//! Behavior tests for `validate_png`.
//!
//! Each accepted input is a valid PNG built from the fixtures. Each refused input
//! differs from a valid file by one named defect, so a refusal can be traced to
//! the rule it breaks. Outcomes are compared exactly.

use std::time::{Duration, Instant};

use artifree_domain::assets::AssetRejection;

use super::{
    ZlibCheck, decode_with_png_crate, scan_container, validate_png, verify_embedded_streams,
    verify_zlib,
};
use crate::asset_validation::PNG_METADATA_BUDGET;
use crate::asset_validation::fixtures::{
    IhdrSpec, adler32, chunk, chunk_with_bad_crc, iend, ihdr, minimal_png, plte, png,
    raw_scanlines, rebuild, valid_adam7_png, valid_png, zlib_compressed, zlib_stored,
    zlib_stored_with_adler,
};

/// One mebibyte, used for the metadata and bomb sizes.
const MIB: usize = 1024 * 1024;

/// Number of scanline rows in the period of `repeating_gray8_scanlines`.
const REPEAT_ROWS: usize = 8;

/// Gray 8-bit sizes whose scanlines inflate to 32768, 32769 and 65536 bytes.
const WINDOW_SIZES: [(u32, u32, usize); 3] =
    [(255, 128, 32_768), (330, 99, 32_769), (255, 256, 65_536)];

/// The (color type, bit depth) pairs that the PNG specification allows.
const LEGAL_PAIRS: [(u8, u8); 15] = [
    (0, 1),
    (0, 2),
    (0, 4),
    (0, 8),
    (0, 16),
    (2, 8),
    (2, 16),
    (3, 1),
    (3, 2),
    (3, 4),
    (3, 8),
    (4, 8),
    (4, 16),
    (6, 8),
    (6, 16),
];

/// A zlib stream whose only block copies three bytes from distance 1 before any byte has
/// been written. Reference zlib refuses it as "invalid distance too far back". Its Adler-32
/// is that of the three zero bytes that a zero-filled window would supply.
const BACK_REFERENCE_BEFORE_START: [u8; 9] = [0x78, 0x9C, 0x03, 0x02, 0x00, 0x00, 0x03, 0x00, 0x01];

/// A zlib stream of four zero bytes: a literal zero, then a copy of three bytes from
/// distance 1, which reaches the first byte. Reference zlib accepts it.
const BACK_REFERENCE_TO_FIRST_BYTE: [u8; 10] =
    [0x78, 0x9C, 0x63, 0x00, 0x02, 0x00, 0x00, 0x04, 0x00, 0x01];

/// Like `BACK_REFERENCE_TO_FIRST_BYTE`, but the copy is from distance 2, one byte before the
/// first byte. Reference zlib refuses it.
const BACK_REFERENCE_ONE_BYTE_BEFORE_FIRST: [u8; 10] =
    [0x78, 0x9C, 0x63, 0x00, 0x42, 0x00, 0x00, 0x04, 0x00, 0x01];

// ---- Accepted inputs ---------------------------------------------------------

#[test]
fn accepts_the_smallest_rgba8_image() {
    assert_accepted("1x1 RGBA 8", &minimal_png());
}

#[test]
fn accepts_1x4096_and_4096x1_gray8_images() {
    for (width, height) in [(4096, 1), (1, 4096)] {
        assert_accepted(
            &format!("{width}x{height} gray 8"),
            &valid_png(width, height, 8, 0),
        );
    }
}

#[test]
fn accepts_every_legal_color_type_and_bit_depth_pair() {
    for (color_type, bit_depth) in LEGAL_PAIRS {
        assert_accepted(
            &format!("3x2 color type {color_type}, bit depth {bit_depth}"),
            &valid_png(3, 2, bit_depth, color_type),
        );
    }
}

#[test]
fn accepts_adam7_interlaced_images_of_small_and_odd_sizes() {
    for (color_type, bit_depth) in [(0, 8), (3, 4), (6, 16)] {
        for (width, height) in [(1, 1), (5, 3), (9, 9)] {
            assert_accepted(
                &format!("Adam7 {width}x{height}, color type {color_type}, bit depth {bit_depth}"),
                &valid_adam7_png(width, height, bit_depth, color_type),
            );
        }
    }
}

#[test]
fn accepts_idat_data_split_after_every_interior_byte() {
    let zlib = zlib_stored(&raw_scanlines(1, 1, 8, 6));
    for split in 1..zlib.len() {
        let (head, tail) = zlib.split_at(split);
        let file = rebuild(
            vec![ihdr(non_interlaced(1, 1, 8, 6))],
            head,
            vec![chunk(*b"IDAT", tail)],
        );
        assert_accepted(&format!("IDAT split after byte {split}"), &file);
    }
}

#[test]
fn accepts_idat_data_in_one_byte_chunks() {
    let zlib = zlib_stored(&raw_scanlines(1, 1, 8, 6));
    let mut chunks = vec![ihdr(non_interlaced(1, 1, 8, 6))];
    for byte in &zlib {
        chunks.push(chunk(*b"IDAT", std::slice::from_ref(byte)));
    }
    chunks.push(iend());
    assert_accepted("IDAT split into one-byte chunks", &png(&chunks));
}

#[test]
fn accepts_text_and_unknown_ancillary_chunks_before_idat() {
    for (name, extra) in ancillary_chunks() {
        assert_accepted(
            name,
            &rgba_1x1_file(vec![extra], &raw_scanlines(1, 1, 8, 6)),
        );
    }
    let all: Vec<Vec<u8>> = ancillary_chunks()
        .into_iter()
        .map(|(_, bytes)| bytes)
        .collect();
    assert_accepted(
        "all ancillary chunks together",
        &rgba_1x1_file(all, &raw_scanlines(1, 1, 8, 6)),
    );
}

#[test]
fn accepts_text_and_unknown_ancillary_chunks_after_idat() {
    let file = rebuild(
        vec![ihdr(non_interlaced(1, 1, 8, 6))],
        &zlib_stored(&raw_scanlines(1, 1, 8, 6)),
        vec![
            chunk(*b"tEXt", b"Comment\0after"),
            chunk(*b"prVt", b"trailing"),
        ],
    );
    assert_accepted("tEXt and prVt after the IDAT run", &file);
}

#[test]
fn accepts_scanline_filter_types_0_through_4() {
    for filter in 0_u8..=4 {
        let mut raw = raw_scanlines(1, 1, 8, 6);
        raw[0] = filter;
        assert_accepted(
            &format!("scanline filter type {filter}"),
            &rgba_1x1_file(vec![], &raw),
        );
    }
}

#[test]
fn accepts_palette_sizes_from_one_to_256_entries() {
    for entries in [1_usize, 256] {
        assert_accepted(
            &format!("palette of {entries} entries"),
            &palette_1x1_file(&vec![0_u8; entries * 3]),
        );
    }
}

#[test]
fn accepts_4096_by_4096_gray8_image() {
    let file = rebuild(
        vec![ihdr(non_interlaced(4096, 4096, 8, 0))],
        &zlib_compressed(&raw_scanlines(4096, 4096, 8, 0)),
        vec![],
    );
    assert_accepted("4096x4096 gray 8", &file);
}

#[test]
fn accepts_inflated_sizes_of_32768_32769_and_65536_bytes() {
    for (width, height, inflated) in WINDOW_SIZES {
        let raw = repeating_gray8_scanlines(width, height);
        assert_eq!(
            raw.len(),
            inflated,
            "fixture {width}x{height} must inflate to {inflated} bytes"
        );
        assert_accepted(
            &format!("{width}x{height} gray 8, repeating rows"),
            &gray8_file(width, height, &raw),
        );
    }
}

#[test]
fn accepts_compressed_text_that_exactly_fills_the_metadata_budget() {
    let half = chunk(*b"zTXt", &ztxt_data(&zlib_compressed(&vec![0_u8; 8 * MIB])));
    let file = rgba_1x1_file(vec![half.clone(), half], &raw_scanlines(1, 1, 8, 6));
    assert_accepted("two zTXt chunks of 8 MiB each", &file);
}

// ---- Refused inputs ----------------------------------------------------------

#[test]
fn refuses_every_strict_prefix_of_a_valid_file() {
    let file = rgba_1x1_file(
        vec![chunk(*b"tEXt", b"Comment\0prefix")],
        &raw_scanlines(1, 1, 8, 6),
    );
    assert_accepted("the complete file", &file);
    let total = file.len();
    for length in 0..total {
        assert_refused(
            &format!("prefix of {length} of {total} bytes"),
            &file[..length],
        );
    }
}

#[test]
fn refuses_any_byte_after_iend() {
    let trailers: [(&str, Vec<u8>); 3] = [
        ("one zero byte", vec![0]),
        ("a second IEND chunk", iend()),
        ("a tEXt chunk", chunk(*b"tEXt", b"Comment\0late")),
    ];
    for (name, trailer) in trailers {
        let mut file = minimal_png();
        file.extend(trailer);
        assert_refused(name, &file);
    }
}

#[test]
fn refuses_idat_chunk_with_a_bad_crc() {
    let file = png(&[
        ihdr(non_interlaced(1, 1, 8, 6)),
        chunk_with_bad_crc(*b"IDAT", &zlib_stored(&raw_scanlines(1, 1, 8, 6))),
        iend(),
    ]);
    assert_refused("IDAT with a bad CRC", &file);
}

#[test]
fn refuses_ancillary_chunk_with_a_bad_crc() {
    let file = rgba_1x1_file(
        vec![chunk_with_bad_crc(*b"tEXt", b"Comment\0x")],
        &raw_scanlines(1, 1, 8, 6),
    );
    assert_refused("tEXt with a bad CRC", &file);
}

#[test]
fn refuses_wrong_adler32_trailer() {
    let raw = raw_scanlines(1, 1, 8, 6);
    let file = rebuild(
        vec![ihdr(non_interlaced(1, 1, 8, 6))],
        &zlib_stored_with_adler(&raw, adler32(&raw) ^ 1),
        vec![],
    );
    assert_refused("zlib Adler-32 off by one", &file);
}

#[test]
fn refuses_zlib_stream_cut_before_its_adler32() {
    let zlib = zlib_stored(&raw_scanlines(1, 1, 8, 6));
    let file = rebuild(
        vec![ihdr(non_interlaced(1, 1, 8, 6))],
        &zlib[..zlib.len() - 4],
        vec![],
    );
    assert_refused("zlib stream without its four-byte Adler-32", &file);
}

#[test]
fn refuses_bytes_after_the_zlib_stream_inside_idat() {
    let mut zlib = zlib_stored(&raw_scanlines(1, 1, 8, 6));
    zlib.push(0);
    let file = rebuild(vec![ihdr(non_interlaced(1, 1, 8, 6))], &zlib, vec![]);
    assert_refused("one byte after the zlib trailer", &file);
}

#[test]
fn refuses_a_later_idat_chunk_holding_bytes_after_the_zlib_end() {
    let file = png(&[
        ihdr(non_interlaced(1, 1, 8, 6)),
        chunk(*b"IDAT", &zlib_stored(&raw_scanlines(1, 1, 8, 6))),
        chunk(*b"IDAT", &[0]),
        iend(),
    ]);
    assert_refused("extra IDAT after the zlib stream has ended", &file);
}

#[test]
fn refuses_one_inflated_byte_more_than_the_ihdr_implies() {
    let mut raw = raw_scanlines(1, 1, 8, 6);
    raw.push(0);
    assert_refused(
        "6 inflated bytes for a 5-byte image",
        &rgba_1x1_file(vec![], &raw),
    );
}

#[test]
fn refuses_one_inflated_byte_fewer_than_the_ihdr_implies() {
    let raw = raw_scanlines(1, 1, 8, 6);
    let shorter = &raw[..raw.len() - 1];
    assert_refused(
        "4 inflated bytes for a 5-byte image",
        &rgba_1x1_file(vec![], shorter),
    );
}

#[test]
fn refuses_one_byte_more_or_fewer_than_the_ihdr_implies_at_the_window_sizes() {
    for (width, height, inflated) in WINDOW_SIZES {
        let raw = repeating_gray8_scanlines(width, height);
        let mut longer = raw.clone();
        longer.push(0);
        let shorter = &raw[..raw.len() - 1];
        assert_refused(
            &format!("{width}x{height} gray 8 with {inflated} + 1 inflated bytes"),
            &gray8_file(width, height, &longer),
        );
        assert_refused(
            &format!("{width}x{height} gray 8 with {inflated} - 1 inflated bytes"),
            &gray8_file(width, height, shorter),
        );
    }
}

#[test]
fn refuses_stored_block_whose_nlen_is_not_the_complement_of_len() {
    let raw = raw_scanlines(1, 1, 8, 6);
    let adler = adler32(&raw).to_be_bytes();
    // One final stored block with LEN 5, then the NLEN under test, the data and the Adler-32.
    let stored_block = |nlen: [u8; 2]| {
        let mut stream = vec![0x78, 0x01, 0x01, 0x05, 0x00];
        stream.extend_from_slice(&nlen);
        stream.extend_from_slice(&raw);
        stream.extend_from_slice(&adler);
        stream
    };
    let with_nlen = |nlen: [u8; 2]| {
        rebuild(
            vec![ihdr(non_interlaced(1, 1, 8, 6))],
            &stored_block(nlen),
            vec![],
        )
    };
    // The NLEN for LEN 5 is 0xFFFA, stored little-endian.
    assert_accepted("stored block with NLEN 0xFFFA", &with_nlen([0xFA, 0xFF]));
    assert_refused("stored block with NLEN 0x0000", &with_nlen([0x00, 0x00]));
}

#[test]
fn refuses_animation_chunks_in_a_static_png() {
    let animation = [
        ("acTL", chunk(*b"acTL", &[0, 0, 0, 1, 0, 0, 0, 0])),
        ("fcTL", chunk(*b"fcTL", &[0_u8; 26])),
        ("fdAT", chunk(*b"fdAT", &[0, 0, 0, 0])),
    ];
    for (name, animation_chunk) in animation {
        assert_refused(
            name,
            &rgba_1x1_file(vec![animation_chunk], &raw_scanlines(1, 1, 8, 6)),
        );
    }
}

#[test]
fn refuses_zero_width_or_zero_height() {
    for (width, height) in [(0, 1), (1, 0)] {
        let file = rebuild(
            vec![ihdr(non_interlaced(width, height, 8, 6))],
            &zlib_stored(&raw_scanlines(width, height, 8, 6)),
            vec![],
        );
        assert_refused(&format!("{width}x{height} RGBA 8"), &file);
    }
}

#[test]
fn refuses_width_or_height_above_4096() {
    for (width, height) in [(4097, 1), (1, 4097)] {
        let file = rebuild(
            vec![ihdr(non_interlaced(width, height, 8, 0))],
            &zlib_stored(&raw_scanlines(width, height, 8, 0)),
            vec![],
        );
        assert_refused(&format!("{width}x{height} gray 8"), &file);
    }
}

#[test]
fn refuses_color_type_and_bit_depth_pairs_the_specification_forbids() {
    for (color_type, bit_depth) in [(0, 3), (2, 4), (3, 16), (4, 1), (6, 4)] {
        let mut before_idat = vec![ihdr(non_interlaced(1, 1, bit_depth, color_type))];
        if color_type == 3 {
            // A full palette, so that the illegal pair is the only defect.
            before_idat.push(plte(256));
        }
        let file = rebuild(
            before_idat,
            &zlib_stored(&raw_scanlines(1, 1, bit_depth, color_type)),
            vec![],
        );
        assert_refused(
            &format!("color type {color_type} with bit depth {bit_depth}"),
            &file,
        );
    }
}

#[test]
fn refuses_undefined_color_types() {
    for color_type in [1_u8, 5] {
        let file = rebuild(
            vec![ihdr(non_interlaced(1, 1, 8, color_type))],
            &zlib_stored(&[0_u8; 5]),
            vec![],
        );
        assert_refused(&format!("color type {color_type}"), &file);
    }
}

#[test]
fn refuses_nonzero_compression_filter_or_interlace_method() {
    let fields = non_interlaced(1, 1, 8, 6);
    let zlib = zlib_stored(&raw_scanlines(1, 1, 8, 6));
    let cases = [
        (
            "compression method 1",
            chunk(*b"IHDR", &ihdr_data(fields, 1, 0)),
        ),
        ("filter method 1", chunk(*b"IHDR", &ihdr_data(fields, 0, 1))),
        (
            "interlace method 2",
            ihdr(IhdrSpec {
                interlace: 2,
                ..fields
            }),
        ),
    ];
    for (name, header_chunk) in cases {
        let file = png(&[header_chunk, chunk(*b"IDAT", &zlib), iend()]);
        assert_refused(name, &file);
    }
}

#[test]
fn refuses_unknown_critical_chunk_before_or_after_idat() {
    let raw = raw_scanlines(1, 1, 8, 6);
    assert_refused(
        "ABCD before IDAT",
        &rgba_1x1_file(vec![chunk(*b"ABCD", b"critical")], &raw),
    );
    let after = rebuild(
        vec![ihdr(non_interlaced(1, 1, 8, 6))],
        &zlib_stored(&raw),
        vec![chunk(*b"ABCD", b"critical")],
    );
    assert_refused("ABCD after IDAT", &after);
}

#[test]
fn refuses_chunk_type_whose_third_letter_is_lowercase() {
    for (name, kind) in [("tExt", *b"tExt"), ("IDat", *b"IDat")] {
        assert_refused(
            name,
            &rgba_1x1_file(vec![chunk(kind, b"x")], &raw_scanlines(1, 1, 8, 6)),
        );
    }
}

#[test]
fn refuses_idat_run_interrupted_by_another_chunk() {
    let zlib = zlib_stored(&raw_scanlines(1, 1, 8, 6));
    let (head, tail) = zlib.split_at(8);
    let file = png(&[
        ihdr(non_interlaced(1, 1, 8, 6)),
        chunk(*b"IDAT", head),
        chunk(*b"tEXt", b"Comment\0between"),
        chunk(*b"IDAT", tail),
        iend(),
    ]);
    assert_refused("tEXt between two IDAT chunks", &file);
}

#[test]
fn refuses_file_without_idat() {
    let file = png(&[ihdr(non_interlaced(1, 1, 8, 6)), iend()]);
    assert_refused("IHDR and IEND only", &file);
}

#[test]
fn refuses_palette_image_without_plte_before_idat() {
    let zlib = zlib_stored(&raw_scanlines(1, 1, 8, 3));
    let without_plte = rebuild(vec![ihdr(non_interlaced(1, 1, 8, 3))], &zlib, vec![]);
    assert_refused("palette image without PLTE", &without_plte);
    let plte_after_idat = rebuild(
        vec![ihdr(non_interlaced(1, 1, 8, 3))],
        &zlib,
        vec![chunk(*b"PLTE", &[0; 3])],
    );
    assert_refused("PLTE after the IDAT run", &plte_after_idat);
}

#[test]
fn refuses_plte_payload_that_is_not_one_to_256_three_byte_entries() {
    for length in [0_usize, 1, 2, 4, 769, 771] {
        assert_refused(
            &format!("PLTE of {length} bytes"),
            &palette_1x1_file(&vec![0_u8; length]),
        );
    }
}

#[test]
fn refuses_scanline_filter_types_above_4() {
    for filter in [5_u8, 6, 255] {
        let mut raw = raw_scanlines(1, 1, 8, 6);
        raw[0] = filter;
        assert_refused(
            &format!("scanline filter type {filter}"),
            &rgba_1x1_file(vec![], &raw),
        );
    }
}

#[test]
fn refuses_compressed_text_whose_stream_is_not_zlib() {
    let cases = [
        ("zTXt", chunk(*b"zTXt", &ztxt_data(b"not a zlib stream"))),
        (
            "compressed iTXt",
            chunk(*b"iTXt", &itxt_data(true, b"not a zlib stream")),
        ),
    ];
    for (name, text_chunk) in cases {
        assert_refused(
            name,
            &rgba_1x1_file(vec![text_chunk], &raw_scanlines(1, 1, 8, 6)),
        );
    }
}

#[test]
fn refuses_ihdr_whose_length_is_not_13() {
    let data = ihdr_data(non_interlaced(1, 1, 8, 6), 0, 0);
    let mut longer = data.clone();
    longer.push(0);
    let zlib = zlib_stored(&raw_scanlines(1, 1, 8, 6));
    for (name, payload) in [("12 bytes", data[..12].to_vec()), ("14 bytes", longer)] {
        let file = png(&[chunk(*b"IHDR", &payload), chunk(*b"IDAT", &zlib), iend()]);
        assert_refused(&format!("IHDR of {name}"), &file);
    }
}

#[test]
fn refuses_second_ihdr() {
    let file = rgba_1x1_file(
        vec![ihdr(non_interlaced(1, 1, 8, 6))],
        &raw_scanlines(1, 1, 8, 6),
    );
    assert_refused("two IHDR chunks", &file);
}

#[test]
fn refuses_first_chunk_that_is_not_ihdr() {
    let file = png(&[
        chunk(*b"tEXt", b"Comment\0early"),
        ihdr(non_interlaced(1, 1, 8, 6)),
        chunk(*b"IDAT", &zlib_stored(&raw_scanlines(1, 1, 8, 6))),
        iend(),
    ]);
    assert_refused("tEXt before IHDR", &file);
}

#[test]
fn refuses_missing_or_malformed_iend() {
    let header_chunk = ihdr(non_interlaced(1, 1, 8, 6));
    let idat = chunk(*b"IDAT", &zlib_stored(&raw_scanlines(1, 1, 8, 6)));
    assert_refused("no IEND", &png(&[header_chunk.clone(), idat.clone()]));
    assert_refused(
        "IEND with one data byte",
        &png(&[header_chunk.clone(), idat.clone(), chunk(*b"IEND", &[0])]),
    );
    assert_refused(
        "IEND with a bad CRC",
        &png(&[header_chunk, idat, chunk_with_bad_crc(*b"IEND", &[])]),
    );
}

#[test]
fn refuses_chunk_length_past_the_end_or_above_2_pow_31_minus_1() {
    for declared in [1_000_u32, 0x8000_0000, u32::MAX] {
        let mut text = chunk(*b"tEXt", b"abcd");
        text[..4].copy_from_slice(&declared.to_be_bytes());
        assert_refused(
            &format!("tEXt with length field {declared}"),
            &rgba_1x1_file(vec![text], &raw_scanlines(1, 1, 8, 6)),
        );
    }
}

#[test]
fn refuses_a_damaged_or_missing_signature() {
    let valid = minimal_png();
    for index in [0_usize, 3, 7] {
        let mut file = valid.clone();
        file[index] ^= 0xFF;
        assert_refused(&format!("signature byte {index} inverted"), &file);
    }
    assert_refused(
        "text in place of a PNG signature",
        b"not a PNG file, just text",
    );
}

// ---- Compressed text and ICC profile streams ---------------------------------

#[test]
fn accepts_zlib_streams_in_ztxt_iccp_and_compressed_itxt_chunks() {
    let zlib = zlib_compressed(b"compressed text and profile bytes");
    for (name, kind, data) in compressed_chunks(&zlib) {
        let file = file_with_chunk(kind, &data);
        assert_accepted(name, &file);
        assert_eq!(
            embedded_only(&file),
            Ok(()),
            "{name}: the embedded-stream check must accept a valid stream"
        );
    }
    // 79 bytes is the longest keyword that the specification allows.
    let longest_keyword = [0x6B_u8; 79];
    for (name, kind) in [("zTXt", *b"zTXt"), ("iCCP", *b"iCCP")] {
        let file = file_with_chunk(kind, &keyword_method_data(&longest_keyword, 0, &zlib));
        assert_accepted(&format!("{name} with a 79-byte keyword"), &file);
        assert_eq!(
            scan_only(&file),
            Ok(()),
            "{name} with a 79-byte keyword must scan"
        );
    }
}

#[test]
fn refuses_bytes_after_the_zlib_stream_in_compressed_text_and_icc_chunks() {
    let zlib = zlib_compressed(b"compressed text and profile bytes");
    let mut padded = zlib.clone();
    padded.push(0);
    for (name, kind, data) in compressed_chunks(&padded) {
        assert_stream_refused(
            &format!("{name} with one byte after its zlib stream"),
            &file_with_chunk(kind, &data),
        );
    }
}

#[test]
fn refuses_a_bad_adler32_in_compressed_text_and_icc_chunks() {
    let mut zlib = zlib_compressed(b"compressed text and profile bytes");
    if let Some(last) = zlib.last_mut() {
        *last ^= 1;
    }
    for (name, kind, data) in compressed_chunks(&zlib) {
        assert_stream_refused(
            &format!("{name} with a bad Adler-32"),
            &file_with_chunk(kind, &data),
        );
    }
}

#[test]
fn refuses_a_zlib_stream_cut_before_its_adler32_in_compressed_chunks() {
    let zlib = zlib_compressed(b"compressed text and profile bytes");
    let cut = &zlib[..zlib.len() - 4];
    for (name, kind, data) in compressed_chunks(cut) {
        assert_stream_refused(
            &format!("{name} stream without its Adler-32"),
            &file_with_chunk(kind, &data),
        );
    }
}

#[test]
fn refuses_an_empty_zlib_stream_in_compressed_chunks() {
    for (name, kind, data) in compressed_chunks(&[]) {
        assert_layout_refused(
            &format!("{name} with an empty stream"),
            &file_with_chunk(kind, &data),
        );
    }
}

#[test]
fn refuses_compression_method_1_in_compressed_chunks() {
    let zlib = zlib_compressed(b"compressed text and profile bytes");
    let cases = [
        (
            "zTXt with compression method 1",
            *b"zTXt",
            keyword_method_data(b"Comment", 1, &zlib),
        ),
        (
            "iCCP with compression method 1",
            *b"iCCP",
            keyword_method_data(b"icc", 1, &zlib),
        ),
        (
            "compressed iTXt with compression method 1",
            *b"iTXt",
            itxt_fields(b"Comment", 1, 1, &zlib),
        ),
    ];
    for (name, kind, data) in cases {
        assert_layout_refused(name, &file_with_chunk(kind, &data));
    }
}

#[test]
fn refuses_an_icc_profile_stream_that_copies_from_before_its_first_byte() {
    assert_stream_refused(
        "iCCP whose stream copies from before its first byte",
        &file_with_chunk(*b"iCCP", &iccp_data(&BACK_REFERENCE_BEFORE_START)),
    );
}

#[test]
fn refuses_text_streams_that_copy_from_before_their_first_byte() {
    let cases = [
        ("zTXt", *b"zTXt", ztxt_data(&BACK_REFERENCE_BEFORE_START)),
        (
            "compressed iTXt",
            *b"iTXt",
            itxt_data(true, &BACK_REFERENCE_BEFORE_START),
        ),
    ];
    for (name, kind, data) in cases {
        assert_stream_refused(
            &format!("{name} whose stream copies from before its first byte"),
            &file_with_chunk(kind, &data),
        );
    }
}

#[test]
fn accepts_a_copy_that_reaches_the_first_byte_of_its_stream() {
    for (name, kind, data) in compressed_chunks(&BACK_REFERENCE_TO_FIRST_BYTE) {
        assert_accepted(
            &format!("{name} whose copy reaches the first byte"),
            &file_with_chunk(kind, &data),
        );
    }
}

#[test]
fn refuses_a_copy_that_reaches_one_byte_before_the_first_byte_of_its_stream() {
    for (name, kind, data) in compressed_chunks(&BACK_REFERENCE_ONE_BYTE_BEFORE_FIRST) {
        assert_stream_refused(
            &format!("{name} whose copy reaches one byte before the first byte"),
            &file_with_chunk(kind, &data),
        );
    }
}

#[test]
fn accepts_embedded_streams_whose_copies_reach_back_across_output_buffer_growth() {
    // 64 KiB of printable ASCII that repeats every eight rows. Copies reach back about one
    // period, so some of them span the points where the output buffer grows. The text is
    // valid UTF-8, which compressed iTXt requires.
    let text: Vec<u8> = repeating_gray8_scanlines(255, 256)
        .iter()
        .map(|&byte| b' ' + byte % 95)
        .collect();
    let zlib = zlib_compressed(&text);
    for (name, kind, data) in compressed_chunks(&zlib) {
        let file = file_with_chunk(kind, &data);
        assert_eq!(
            embedded_only(&file),
            Ok(()),
            "{name} of 64 KiB must pass the embedded-stream check"
        );
        assert_accepted(name, &file);
    }
}

#[test]
fn refuses_keywords_of_0_or_80_bytes_and_a_keyword_without_its_nul() {
    let zlib = zlib_compressed(b"compressed text and profile bytes");
    let too_long = [0x6B_u8; 80];
    let cases = [
        (
            "zTXt with an empty keyword",
            *b"zTXt",
            keyword_method_data(b"", 0, &zlib),
        ),
        (
            "zTXt with an 80-byte keyword",
            *b"zTXt",
            keyword_method_data(&too_long, 0, &zlib),
        ),
        ("zTXt with no NUL", *b"zTXt", b"Comment".to_vec()),
        (
            "iCCP with an empty keyword",
            *b"iCCP",
            keyword_method_data(b"", 0, &zlib),
        ),
        (
            "iCCP with an 80-byte keyword",
            *b"iCCP",
            keyword_method_data(&too_long, 0, &zlib),
        ),
        ("iCCP with no NUL", *b"iCCP", b"icc".to_vec()),
        (
            "compressed iTXt with an empty keyword",
            *b"iTXt",
            itxt_fields(b"", 1, 0, &zlib),
        ),
        (
            "compressed iTXt with an 80-byte keyword",
            *b"iTXt",
            itxt_fields(&too_long, 1, 0, &zlib),
        ),
        ("compressed iTXt with no NUL", *b"iTXt", b"Comment".to_vec()),
    ];
    for (name, kind, data) in cases {
        assert_layout_refused(name, &file_with_chunk(kind, &data));
    }
}

#[test]
fn refuses_compressed_itxt_fields_outside_the_specification() {
    let zlib = zlib_compressed(b"compressed text and profile bytes");
    let cases = [
        ("compression flag 2", itxt_fields(b"Comment", 2, 0, &zlib)),
        (
            "method 1 on uncompressed text",
            itxt_fields(b"Comment", 0, 1, b"plain text"),
        ),
        (
            "language tag without its NUL",
            b"Comment\0\x01\x00en".to_vec(),
        ),
        (
            "translated keyword without its NUL",
            b"Comment\0\x01\x00\0translated".to_vec(),
        ),
    ];
    for (name, data) in cases {
        assert_layout_refused(name, &file_with_chunk(*b"iTXt", &data));
    }
}

#[test]
fn refuses_a_second_iccp_chunk() {
    let zlib = zlib_compressed(b"compressed text and profile bytes");
    let image = zlib_stored(&raw_scanlines(1, 1, 8, 6));
    let file = rebuild(
        vec![
            ihdr(non_interlaced(1, 1, 8, 6)),
            chunk(*b"iCCP", &iccp_data(&zlib)),
            chunk(*b"iCCP", &iccp_data(&zlib)),
        ],
        &image,
        vec![],
    );
    assert_layout_refused("two iCCP chunks before the IDAT", &file);
}

#[test]
fn refuses_an_iccp_chunk_after_the_idat() {
    let zlib = zlib_compressed(b"compressed text and profile bytes");
    let image = zlib_stored(&raw_scanlines(1, 1, 8, 6));
    let file = rebuild(
        vec![ihdr(non_interlaced(1, 1, 8, 6))],
        &image,
        vec![chunk(*b"iCCP", &iccp_data(&zlib))],
    );
    assert_layout_refused("iCCP after the IDAT", &file);
}

#[test]
fn refuses_an_icc_chunk_whose_stream_is_not_zlib() {
    assert_stream_refused(
        "iCCP with a stream that is not zlib",
        &file_with_chunk(*b"iCCP", &iccp_data(b"not a zlib stream")),
    );
}

#[test]
fn container_scan_records_the_zlib_stream_of_each_compressed_chunk() {
    let zlib = zlib_compressed(b"recorded stream");
    let file = rgba_1x1_file(
        vec![
            chunk(*b"tEXt", b"Comment\0plain"),
            chunk(*b"zTXt", &ztxt_data(&zlib)),
            chunk(*b"iTXt", &itxt_data(false, b"plain UTF-8")),
            chunk(*b"iTXt", &itxt_data(true, &zlib)),
            chunk(*b"iCCP", &iccp_data(&zlib)),
        ],
        &raw_scanlines(1, 1, 8, 6),
    );
    let layout = scan_container(&file)
        .unwrap_or_else(|error| panic!("a valid file must scan, got {error:?}"));
    let streams: Vec<&[u8]> = layout
        .compressed_streams
        .iter()
        .filter_map(|range| file.get(range.clone()))
        .collect();
    assert_eq!(
        streams,
        vec![zlib.as_slice(); 3],
        "the zTXt, compressed iTXt and iCCP streams are recorded, and nothing else is"
    );
}

// ---- Decompression bombs and the metadata budget -----------------------------

#[test]
fn refuses_one_pixel_image_whose_idat_inflates_to_16_mib_at_once() {
    let file = rebuild(
        vec![ihdr(non_interlaced(1, 1, 8, 6))],
        &zlib_compressed(&vec![0_u8; 16 * MIB]),
        vec![],
    );
    let started = Instant::now();
    let result = validate_png(&file);
    let elapsed = started.elapsed();
    assert_eq!(
        result,
        Err(AssetRejection::Invalid),
        "16 MiB of output for a 5-byte image must be refused"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "refusing the 16 MiB bomb took {elapsed:?}, expected under 5 s"
    );
}

#[test]
fn refuses_one_pixel_image_whose_idat_inflates_past_the_32_kib_window() {
    let file = rebuild(
        vec![ihdr(non_interlaced(1, 1, 8, 6))],
        &zlib_compressed(&vec![0_u8; 33 * 1024]),
        vec![],
    );
    assert_refused("33 KiB of output for a 5-byte image", &file);
}

#[test]
fn refuses_an_idat_stream_that_copies_from_before_its_first_byte() {
    // A 2x1 gray image inflates to three bytes. The stream supplies three zero bytes with
    // the right Adler-32, so the copy from before the first byte is the only defect. The
    // control shows that the same image with a valid stream is accepted.
    let header = vec![ihdr(non_interlaced(2, 1, 8, 0))];
    assert_accepted(
        "control: the image with a stored block of three zero bytes",
        &rebuild(header.clone(), &zlib_stored(&[0_u8; 3]), vec![]),
    );
    assert_refused(
        "IDAT whose copy reaches before its first byte",
        &rebuild(header, &BACK_REFERENCE_BEFORE_START, vec![]),
    );
}

#[test]
fn refuses_compressed_text_one_byte_past_the_metadata_budget() {
    let half = chunk(*b"zTXt", &ztxt_data(&zlib_compressed(&vec![0_u8; 8 * MIB])));
    let one_over = chunk(
        *b"zTXt",
        &ztxt_data(&zlib_compressed(&vec![0_u8; 8 * MIB + 1])),
    );
    let file = rgba_1x1_file(vec![half, one_over], &raw_scanlines(1, 1, 8, 6));
    assert_refused("8 MiB of zTXt text, then 8 MiB + 1 byte", &file);
}

#[test]
fn refuses_compressed_text_past_the_budget_across_two_chunks() {
    let half = chunk(*b"zTXt", &ztxt_data(&zlib_compressed(&vec![0_u8; 8 * MIB])));
    let one_over = chunk(
        *b"zTXt",
        &ztxt_data(&zlib_compressed(&vec![0_u8; 8 * MIB + 1])),
    );
    let exact = rgba_1x1_file(vec![half.clone(), half.clone()], &raw_scanlines(1, 1, 8, 6));
    assert_eq!(
        embedded_only(&exact),
        Ok(()),
        "control: 8 MiB and then 8 MiB exactly fill the budget"
    );
    let over = rgba_1x1_file(vec![half.clone(), one_over], &raw_scanlines(1, 1, 8, 6));
    assert_eq!(scan_only(&over), Ok(()), "control: the container scans");
    assert_eq!(
        embedded_only(&over),
        Err(AssetRejection::Invalid),
        "8 MiB, then 8 MiB + 1 byte, exceeds the budget that the first stream leaves"
    );
    assert_refused("8 MiB of zTXt text, then 8 MiB + 1 byte", &over);
    // The budget is shared by every kind of embedded stream, so compressed iTXt counts too.
    let itxt_one_over = chunk(
        *b"iTXt",
        &itxt_data(true, &zlib_compressed(&vec![0_u8; 8 * MIB + 1])),
    );
    let mixed = rgba_1x1_file(vec![half, itxt_one_over], &raw_scanlines(1, 1, 8, 6));
    assert_eq!(
        embedded_only(&mixed),
        Err(AssetRejection::Invalid),
        "8 MiB of zTXt, then 8 MiB + 1 byte of compressed iTXt, exceeds the shared budget"
    );
    assert_refused(
        "8 MiB of zTXt, then 8 MiB + 1 byte of compressed iTXt",
        &mixed,
    );
}

#[test]
fn embedded_streams_fill_a_remaining_budget_that_is_not_a_power_of_two_exactly() {
    let budget = PNG_METADATA_BUDGET;
    let first = chunk(*b"zTXt", &ztxt_data(&zlib_stored(&[0_u8; 3])));
    let exact = chunk(*b"zTXt", &ztxt_data(&zlib_stored(&vec![0_u8; budget - 3])));
    let one_over = chunk(*b"zTXt", &ztxt_data(&zlib_stored(&vec![0_u8; budget - 2])));
    let raw = raw_scanlines(1, 1, 8, 6);
    assert_eq!(
        embedded_only(&rgba_1x1_file(vec![first.clone(), exact], &raw)),
        Ok(()),
        "3 bytes, then exactly the rest of the budget, must pass"
    );
    assert_eq!(
        embedded_only(&rgba_1x1_file(vec![first, one_over], &raw)),
        Err(AssetRejection::Invalid),
        "3 bytes, then one byte more than the rest of the budget, must be refused"
    );
}

// ---- Each pass on its own ----------------------------------------------------

// `validate_png` runs the container scan, the zlib check and the `png` decode in
// order. The decoder repeats many of the same checks, so these tests call each
// pass directly and show that it refuses without help from the others.

/// Outcome of the container scan alone.
fn scan_only(bytes: &[u8]) -> Result<(), AssetRejection> {
    scan_container(bytes).map(|_| ())
}

/// Outcome of the zlib check alone, on the layout that the container scan reports.
fn zlib_only(bytes: &[u8]) -> Result<(), AssetRejection> {
    let layout = scan_container(bytes)?;
    verify_zlib(bytes, &layout)
}

#[test]
fn container_scan_refuses_bad_crc_on_idat_and_on_unknown_ancillary_chunks() {
    let zlib = zlib_stored(&raw_scanlines(1, 1, 8, 6));
    let bad_idat = png(&[
        ihdr(non_interlaced(1, 1, 8, 6)),
        chunk_with_bad_crc(*b"IDAT", &zlib),
        iend(),
    ]);
    let bad_prvt = rgba_1x1_file(
        vec![chunk_with_bad_crc(*b"prVt", b"private")],
        &raw_scanlines(1, 1, 8, 6),
    );
    assert_eq!(
        scan_only(&minimal_png()),
        Ok(()),
        "control: a valid file scans"
    );
    assert_eq!(
        scan_only(&bad_idat),
        Err(AssetRejection::Invalid),
        "IDAT with a bad CRC"
    );
    assert_eq!(
        scan_only(&bad_prvt),
        Err(AssetRejection::Invalid),
        "prVt with a bad CRC"
    );
}

#[test]
fn container_scan_refuses_any_byte_after_iend() {
    let mut trailing = minimal_png();
    trailing.push(0);
    let mut second_iend = minimal_png();
    second_iend.extend(iend());
    assert_eq!(
        scan_only(&trailing),
        Err(AssetRejection::Invalid),
        "one byte after IEND"
    );
    assert_eq!(
        scan_only(&second_iend),
        Err(AssetRejection::Invalid),
        "a second IEND chunk"
    );
}

#[test]
fn container_scan_refuses_animation_chunks() {
    let animation = [
        ("acTL", chunk(*b"acTL", &[0, 0, 0, 1, 0, 0, 0, 0])),
        ("fcTL", chunk(*b"fcTL", &[0_u8; 26])),
        ("fdAT", chunk(*b"fdAT", &[0, 0, 0, 0])),
    ];
    for (name, animation_chunk) in animation {
        let file = rgba_1x1_file(vec![animation_chunk], &raw_scanlines(1, 1, 8, 6));
        assert_eq!(
            scan_only(&file),
            Err(AssetRejection::Invalid),
            "{name} alone"
        );
    }
}

#[test]
fn container_scan_limits_each_axis_to_4096_and_refuses_zero() {
    assert_eq!(
        scan_only(&valid_png(4096, 1, 8, 0)),
        Ok(()),
        "4096x1 gray 8"
    );
    assert_eq!(
        scan_only(&valid_png(1, 4096, 8, 0)),
        Ok(()),
        "1x4096 gray 8"
    );
    for (width, height) in [(4097, 1), (1, 4097), (0, 1), (1, 0)] {
        let file = rebuild(
            vec![ihdr(non_interlaced(width, height, 8, 0))],
            &zlib_stored(&raw_scanlines(width, height, 8, 0)),
            vec![],
        );
        assert_eq!(
            scan_only(&file),
            Err(AssetRejection::Invalid),
            "{width}x{height} gray 8"
        );
    }
}

#[test]
fn container_scan_refuses_illegal_header_fields() {
    let fields = non_interlaced(1, 1, 8, 6);
    let zlib = zlib_stored(&raw_scanlines(1, 1, 8, 6));
    let headers = [
        ("gray with bit depth 3", ihdr(non_interlaced(1, 1, 3, 0))),
        (
            "interlace method 2",
            ihdr(IhdrSpec {
                interlace: 2,
                ..fields
            }),
        ),
        (
            "compression method 1",
            chunk(*b"IHDR", &ihdr_data(fields, 1, 0)),
        ),
        ("filter method 1", chunk(*b"IHDR", &ihdr_data(fields, 0, 1))),
    ];
    for (name, header_chunk) in headers {
        let file = png(&[header_chunk, chunk(*b"IDAT", &zlib), iend()]);
        assert_eq!(scan_only(&file), Err(AssetRejection::Invalid), "{name}");
    }
}

#[test]
fn container_scan_refuses_reserved_bit_types_and_unknown_critical_chunks() {
    for (name, extra) in [
        ("tExt, third letter lowercase", chunk(*b"tExt", b"x")),
        ("ABCD, unknown critical", chunk(*b"ABCD", b"x")),
    ] {
        let file = rgba_1x1_file(vec![extra], &raw_scanlines(1, 1, 8, 6));
        assert_eq!(scan_only(&file), Err(AssetRejection::Invalid), "{name}");
    }
}

#[test]
fn container_scan_enforces_the_idat_run_and_palette_order() {
    let zlib = zlib_stored(&raw_scanlines(1, 1, 8, 6));
    let split_run = png(&[
        ihdr(non_interlaced(1, 1, 8, 6)),
        chunk(*b"IDAT", &zlib[..8]),
        chunk(*b"tEXt", b"Comment\0between"),
        chunk(*b"IDAT", &zlib[8..]),
        iend(),
    ]);
    assert_eq!(
        scan_only(&split_run),
        Err(AssetRejection::Invalid),
        "tEXt between two IDAT chunks"
    );
    let palette_zlib = zlib_stored(&raw_scanlines(1, 1, 8, 3));
    let without_plte = rebuild(
        vec![ihdr(non_interlaced(1, 1, 8, 3))],
        &palette_zlib,
        vec![],
    );
    assert_eq!(
        scan_only(&without_plte),
        Err(AssetRejection::Invalid),
        "palette image without PLTE"
    );
    let plte_after = rebuild(
        vec![ihdr(non_interlaced(1, 1, 8, 3))],
        &palette_zlib,
        vec![chunk(*b"PLTE", &[0; 3])],
    );
    assert_eq!(
        scan_only(&plte_after),
        Err(AssetRejection::Invalid),
        "PLTE after the IDAT run"
    );
}

#[test]
fn container_scan_bounds_the_plte_payload_to_one_to_256_entries() {
    assert_eq!(
        scan_only(&palette_1x1_file(&[0; 768])),
        Ok(()),
        "256 entries"
    );
    for length in [0_usize, 2, 769, 771] {
        assert_eq!(
            scan_only(&palette_1x1_file(&vec![0_u8; length])),
            Err(AssetRejection::Invalid),
            "PLTE of {length} bytes"
        );
    }
}

#[test]
fn container_scan_requires_the_signature_and_an_ihdr_first() {
    let mut damaged = minimal_png();
    damaged[3] ^= 0xFF;
    assert_eq!(
        scan_only(&damaged),
        Err(AssetRejection::Invalid),
        "signature byte 3 inverted"
    );
    let zlib = zlib_stored(&raw_scanlines(1, 1, 8, 6));
    let header_shaped_text = png(&[
        chunk(*b"tEXt", &ihdr_data(non_interlaced(1, 1, 8, 6), 0, 0)),
        chunk(*b"IDAT", &zlib),
        iend(),
    ]);
    assert_eq!(
        scan_only(&header_shaped_text),
        Err(AssetRejection::Invalid),
        "tEXt first, with IHDR-shaped data"
    );
}

#[test]
fn zlib_check_refuses_an_adler32_mismatch() {
    let raw = raw_scanlines(1, 1, 8, 6);
    let bad = rebuild(
        vec![ihdr(non_interlaced(1, 1, 8, 6))],
        &zlib_stored_with_adler(&raw, adler32(&raw) ^ 1),
        vec![],
    );
    assert_eq!(
        zlib_only(&rgba_1x1_file(vec![], &raw)),
        Ok(()),
        "control: the valid stream passes"
    );
    assert_eq!(
        zlib_only(&bad),
        Err(AssetRejection::Invalid),
        "Adler-32 off by one"
    );
}

#[test]
fn zlib_check_refuses_output_one_byte_short_or_long_of_the_ihdr_length() {
    let raw = raw_scanlines(1, 1, 8, 6);
    let mut longer = raw.clone();
    longer.push(0);
    assert_eq!(
        zlib_only(&rgba_1x1_file(vec![], &raw[..raw.len() - 1])),
        Err(AssetRejection::Invalid),
        "one byte short"
    );
    assert_eq!(
        zlib_only(&rgba_1x1_file(vec![], &longer)),
        Err(AssetRejection::Invalid),
        "one byte long"
    );
}

#[test]
fn zlib_check_stops_a_16_mib_stream_for_a_5_byte_image() {
    let mut check = ZlibCheck::new(5);
    let bomb = zlib_compressed(&vec![0_u8; 16 * MIB]);
    assert_eq!(
        check.feed(&bomb, false),
        Err(AssetRejection::Invalid),
        "16 MiB of output"
    );
}

#[test]
fn png_decoder_refuses_a_bad_ancillary_crc_without_the_container_scan() {
    let raw = raw_scanlines(1, 1, 8, 6);
    let good = rgba_1x1_file(vec![chunk(*b"tEXt", b"Comment\0ok")], &raw);
    let bad = rgba_1x1_file(vec![chunk_with_bad_crc(*b"tEXt", b"Comment\0x")], &raw);
    assert_eq!(
        decode_with_png_crate(&good),
        Ok(()),
        "control: a valid file decodes"
    );
    assert_eq!(
        decode_with_png_crate(&bad),
        Err(AssetRejection::Invalid),
        "tEXt with a bad CRC"
    );
}

// ---- Helpers -----------------------------------------------------------------

/// Checks that `bytes` validates. `case` names the input in the failure message.
fn assert_accepted(case: &str, bytes: &[u8]) {
    assert_eq!(validate_png(bytes), Ok(()), "{case} must be accepted");
}

/// Checks that `bytes` is refused as `Invalid`. `case` names the input in the failure message.
fn assert_refused(case: &str, bytes: &[u8]) {
    assert_eq!(
        validate_png(bytes),
        Err(AssetRejection::Invalid),
        "{case} must be refused as Invalid"
    );
}

/// Outcome of the container scan followed by the embedded-stream check, without the
/// IDAT check or the `png` decode.
fn embedded_only(bytes: &[u8]) -> Result<(), AssetRejection> {
    let layout = scan_container(bytes)?;
    verify_embedded_streams(bytes, &layout)
}

/// Checks that a defect in a compressed chunk's stream is refused by the embedded-stream
/// check. The container must scan first, so that the stream is the part that fails.
fn assert_stream_refused(case: &str, bytes: &[u8]) {
    assert_eq!(
        scan_only(bytes),
        Ok(()),
        "{case}: the container must scan, so that the stream is what fails"
    );
    assert_eq!(
        embedded_only(bytes),
        Err(AssetRejection::Invalid),
        "{case} must be refused by the embedded-stream check"
    );
    assert_refused(case, bytes);
}

/// Checks that a malformed field of a compressed chunk is refused by the container scan.
fn assert_layout_refused(case: &str, bytes: &[u8]) {
    assert_eq!(
        scan_only(bytes),
        Err(AssetRejection::Invalid),
        "{case} must be refused by the container scan"
    );
    assert_refused(case, bytes);
}

/// A header with no interlacing.
fn non_interlaced(width: u32, height: u32, bit_depth: u8, color_type: u8) -> IhdrSpec {
    IhdrSpec {
        width,
        height,
        bit_depth,
        color_type,
        interlace: 0,
    }
}

/// Converts a header dimension to `usize` for building buffers.
fn to_usize(value: u32) -> usize {
    usize::try_from(value).unwrap_or_else(|error| panic!("{value} does not fit in usize: {error}"))
}

/// The IHDR payload for `fields`, with the compression and filter method bytes set to `compression` and `filter`.
fn ihdr_data(fields: IhdrSpec, compression: u8, filter: u8) -> Vec<u8> {
    let mut data = Vec::with_capacity(13);
    data.extend_from_slice(&fields.width.to_be_bytes());
    data.extend_from_slice(&fields.height.to_be_bytes());
    data.extend_from_slice(&[
        fields.bit_depth,
        fields.color_type,
        compression,
        filter,
        fields.interlace,
    ]);
    data
}

/// Data of a `zTXt` chunk: the keyword `Comment`, a separator, method 0, then `stream`.
fn ztxt_data(stream: &[u8]) -> Vec<u8> {
    keyword_method_data(b"Comment", 0, stream)
}

/// Data of an `iCCP` chunk: the keyword `icc`, a separator, method 0, then `stream`.
fn iccp_data(stream: &[u8]) -> Vec<u8> {
    keyword_method_data(b"icc", 0, stream)
}

/// Data of a `zTXt` or `iCCP` chunk: `keyword`, a NUL, the method byte, then `stream`.
/// The fields are written as given, so a test can break any one of them.
fn keyword_method_data(keyword: &[u8], method: u8, stream: &[u8]) -> Vec<u8> {
    let mut data = keyword.to_vec();
    data.extend_from_slice(&[0, method]);
    data.extend_from_slice(stream);
    data
}

/// Data of an `iTXt` chunk: the keyword `Comment`, the flag and method bytes, an
/// empty language tag and translated keyword, then `text`.
fn itxt_data(compressed: bool, text: &[u8]) -> Vec<u8> {
    itxt_fields(b"Comment", u8::from(compressed), 0, text)
}

/// Data of an `iTXt` chunk with `keyword`, and the flag and method bytes as given.
/// An empty language tag and translated keyword follow, then `text`.
fn itxt_fields(keyword: &[u8], flag: u8, method: u8, text: &[u8]) -> Vec<u8> {
    let mut data = keyword.to_vec();
    data.extend_from_slice(&[0, flag, method]);
    data.extend_from_slice(b"\0\0");
    data.extend_from_slice(text);
    data
}

/// The three chunk types that carry a zlib stream, each with valid fields around `stream`.
fn compressed_chunks(stream: &[u8]) -> [(&'static str, [u8; 4], Vec<u8>); 3] {
    [
        ("zTXt", *b"zTXt", ztxt_data(stream)),
        ("iCCP", *b"iCCP", iccp_data(stream)),
        ("compressed iTXt", *b"iTXt", itxt_data(true, stream)),
    ]
}

/// A 1x1 RGBA file with one chunk of `kind` and `data` before the IDAT.
fn file_with_chunk(kind: [u8; 4], data: &[u8]) -> Vec<u8> {
    rgba_1x1_file(vec![chunk(kind, data)], &raw_scanlines(1, 1, 8, 6))
}

/// Text and unknown ancillary chunks that the validator must accept, each with a valid CRC.
fn ancillary_chunks() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("tEXt", chunk(*b"tEXt", b"Comment\0plain latin-1 text")),
        (
            "zTXt",
            chunk(
                *b"zTXt",
                &ztxt_data(&zlib_compressed(b"compressed latin-1 text")),
            ),
        ),
        (
            "iTXt",
            chunk(*b"iTXt", &itxt_data(false, b"plain UTF-8 text")),
        ),
        (
            "compressed iTXt",
            chunk(
                *b"iTXt",
                &itxt_data(true, &zlib_compressed(b"compressed UTF-8 text")),
            ),
        ),
        ("unknown ancillary prVt", chunk(*b"prVt", b"private data")),
    ]
}

/// A 1x1 RGBA file: IHDR, `before_idat`, one IDAT holding `raw` as stored zlib, and IEND.
fn rgba_1x1_file(before_idat: Vec<Vec<u8>>, raw: &[u8]) -> Vec<u8> {
    let mut chunks = vec![ihdr(non_interlaced(1, 1, 8, 6))];
    chunks.extend(before_idat);
    rebuild(chunks, &zlib_stored(raw), vec![])
}

/// A 1x1 palette file whose PLTE chunk carries `palette`, placed before the IDAT.
fn palette_1x1_file(palette: &[u8]) -> Vec<u8> {
    rebuild(
        vec![ihdr(non_interlaced(1, 1, 8, 3)), chunk(*b"PLTE", palette)],
        &zlib_stored(&raw_scanlines(1, 1, 8, 3)),
        vec![],
    )
}

/// A gray 8-bit file of the given size, with `raw` compressed into its IDAT.
fn gray8_file(width: u32, height: u32, raw: &[u8]) -> Vec<u8> {
    rebuild(
        vec![ihdr(non_interlaced(width, height, 8, 0))],
        &zlib_compressed(raw),
        vec![],
    )
}

/// Gray 8-bit scanlines whose rows repeat every `REPEAT_ROWS` rows.
///
/// Each repeat is a back-reference about one period long, so inflating the stream
/// reads the decoder's 32 KiB window across its wrap. The pixels are pseudo-random,
/// so a wrong window byte changes the output and fails the Adler-32 check.
fn repeating_gray8_scanlines(width: u32, height: u32) -> Vec<u8> {
    let row_len = 1 + to_usize(width);
    let mut state: u32 = 0x9E37_79B9;
    let mut period = Vec::with_capacity(REPEAT_ROWS * row_len);
    for _ in 0..REPEAT_ROWS {
        // Each row is filter byte 0 followed by pseudo-random pixels.
        let mut row = vec![0_u8];
        row.extend((1..row_len).map(|_| next_byte(&mut state)));
        period.extend(row);
    }
    let total = to_usize(height) * row_len;
    period.iter().copied().cycle().take(total).collect()
}

/// One step of an xorshift32 generator. Returns the low byte of the new state.
fn next_byte(state: &mut u32) -> u8 {
    *state ^= *state << 13;
    *state ^= *state >> 17;
    *state ^= *state << 5;
    state.to_le_bytes()[0]
}
