//! Behavior tests for `validate_jpeg`, and for the layers it is built from: the header parser,
//! the scan walk, the decoder, the dimension checks and the Huffman table builder.
//!
//! Accepted inputs come from the fixture encoder, from headers built in this file, and from
//! Pillow 11.3.0 files that this repository did not write. Each refused input differs from a
//! valid file by one named defect, and its refusal is asserted at the innermost layer that guards
//! it: the header parser for header defects, the scan walk for defects in the entropy-coded data,
//! and `validate_jpeg` for bytes around the image. zune-jpeg does not check the entropy length, so
//! the scan walk is the only guard against truncation, and a refusal that zune-jpeg would make by
//! itself says nothing about our guards. Scan-level cases, such as coefficient patterns and byte
//! flips, are in `scan_tests`.

use artifree_domain::assets::{AssetRejection, MAX_ASSET_BYTES};

use super::entropy::HuffmanTable;
use super::scan::walk_scan;
use super::{
    Frame, Reader, check_dimensions, contain_decoder_panic, decode_with_zune, read_scan_input,
    validate_jpeg,
};
use crate::asset_validation::jpeg_fixtures::{
    AC_CHROMINANCE, AC_LUMINANCE, ComponentSpec, DC_CHROMINANCE, DC_LUMINANCE, HuffmanSpec,
    JpegSpec, Sampling, dht_payload, dqt_payload, encode, eoi, minimal_jpeg, segment, sof_payload,
    soi, sos_payload, worst_case_jpeg,
};

// Marker codes that these tests write by hand. The production constants are private to `jpeg`.
const SOI: u8 = 0xD8;
const EOI: u8 = 0xD9;
const APP0: u8 = 0xE0;
const APP1: u8 = 0xE1;
const APP14: u8 = 0xEE;
const COM: u8 = 0xFE;
const DAC: u8 = 0xCC;
const DHP: u8 = 0xDE;
const DHT: u8 = 0xC4;
const DNL: u8 = 0xDC;
const DQT: u8 = 0xDB;
const DRI: u8 = 0xDD;
const EXP: u8 = 0xDF;
const RST0: u8 = 0xD0;
const SOF0: u8 = 0xC0;
const SOF1: u8 = 0xC1;
const SOS: u8 = 0xDA;

/// The payload of a JFIF APP0 segment: identifier, version 1.1, no density units, 1:1 density, and
/// no thumbnail.
const JFIF_PAYLOAD: [u8; 14] = [b'J', b'F', b'I', b'F', 0, 1, 1, 0, 0, 1, 0, 1, 0, 0];

/// The payload of an Adobe APP14 segment: identifier, version 100, two flag words, and transform 1.
const ADOBE_PAYLOAD: [u8; 12] = [b'A', b'd', b'o', b'b', b'e', 0, 100, 0, 0, 0, 0, 1];

/// A DC table over the standard symbols with every code 4 bits long, so none of its codes is the
/// standard one.
const WRONG_DC: HuffmanSpec = HuffmanSpec {
    counts: [0, 0, 0, 12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    symbols: &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
};

// Independent files from Pillow 11.3.0 (libjpeg-turbo), saved by a script with
// `img.save(buffer, "JPEG", quality=75, ...)`, where the other options are:
//   GRAY_8X8 (L mode, 8x8):             optimize=False
//   COLOR_17X9_420 (RGB, 17x9):         subsampling=2 (4:2:0)
//   RESTART_16X16 (RGB, 16x16):         subsampling=0, restart_marker_blocks=2
//   PROGRESSIVE_8X8 (L mode, 8x8):      progressive=True
// The pixel values do not matter to the validator. The constants are the files' bytes in hex.
const PILLOW_GRAY_8X8: &[&str] = &[
    "ffd8ffe000104a46494600010100000100010000ffdb00430008060607060508",
    "0707070909080a0c140d0c0b0b0c1912130f141d1a1f1e1d1a1c1c20242e2720",
    "222c231c1c2837292c30313434341f27393d38323c2e333432ffc0000b080008",
    "000801011100ffc4001f00000105010101010101000000000000000001020304",
    "05060708090a0bffc400b5100002010303020403050504040000017d01020300",
    "041105122131410613516107227114328191a1082342b1c11552d1f024336272",
    "82090a161718191a25262728292a3435363738393a434445464748494a535455",
    "565758595a636465666768696a737475767778797a838485868788898a929394",
    "95969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4c5c6c7c8c9",
    "cad2d3d4d5d6d7d8d9dae1e2e3e4e5e6e7e8e9eaf1f2f3f4f5f6f7f8f9faffda",
    "0008010100003f003415b7f037849b56962dd70dfb9b38cc658493952503608c",
    "2f049e4700e39c03ffd9",
];

const PILLOW_COLOR_17X9_420: &[&str] = &[
    "ffd8ffe000104a46494600010100000100010000ffdb00430008060607060508",
    "0707070909080a0c140d0c0b0b0c1912130f141d1a1f1e1d1a1c1c20242e2720",
    "222c231c1c2837292c30313434341f27393d38323c2e333432ffdb0043010909",
    "090c0b0c180d0d1832211c213232323232323232323232323232323232323232",
    "323232323232323232323232323232323232323232323232323232323232ffc0",
    "0011080009001103012200021101031101ffc4001f0000010501010101010100",
    "000000000000000102030405060708090a0bffc400b510000201030302040305",
    "0504040000017d01020300041105122131410613516107227114328191a10823",
    "42b1c11552d1f02433627282090a161718191a25262728292a3435363738393a",
    "434445464748494a535455565758595a636465666768696a737475767778797a",
    "838485868788898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7",
    "b8b9bac2c3c4c5c6c7c8c9cad2d3d4d5d6d7d8d9dae1e2e3e4e5e6e7e8e9eaf1",
    "f2f3f4f5f6f7f8f9faffc4001f01000301010101010101010100000000000001",
    "02030405060708090a0bffc400b5110002010204040304070504040001027700",
    "0102031104052131061241510761711322328108144291a1b1c109233352f015",
    "6272d10a162434e125f11718191a262728292a35363738393a43444546474849",
    "4a535455565758595a636465666768696a737475767778797a82838485868788",
    "898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4",
    "c5c6c7c8c9cad2d3d4d5d6d7d8d9dae2e3e4e5e6e7e8e9eaf2f3f4f5f6f7f8f9",
    "faffda000c03010002110311003f00e3f4ef0574fdd7e95d669de0ae9fbafd2b",
    "a3d3bb5759a776ae4c3e2ea1e66499e6274d4e27fe10affa65fa515e9f4577fd",
    "6ea1f69fdb989ee7ffd9",
];

const PILLOW_RESTART_16X16: &[&str] = &[
    "ffd8ffe000104a46494600010100000100010000ffdb00430008060607060508",
    "0707070909080a0c140d0c0b0b0c1912130f141d1a1f1e1d1a1c1c20242e2720",
    "222c231c1c2837292c30313434341f27393d38323c2e333432ffdb0043010909",
    "090c0b0c180d0d1832211c213232323232323232323232323232323232323232",
    "323232323232323232323232323232323232323232323232323232323232ffc0",
    "0011080010001003011100021101031101ffc4001f0000010501010101010100",
    "000000000000000102030405060708090a0bffc400b510000201030302040305",
    "0504040000017d01020300041105122131410613516107227114328191a10823",
    "42b1c11552d1f02433627282090a161718191a25262728292a3435363738393a",
    "434445464748494a535455565758595a636465666768696a737475767778797a",
    "838485868788898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7",
    "b8b9bac2c3c4c5c6c7c8c9cad2d3d4d5d6d7d8d9dae1e2e3e4e5e6e7e8e9eaf1",
    "f2f3f4f5f6f7f8f9faffc4001f01000301010101010101010100000000000001",
    "02030405060708090a0bffc400b5110002010204040304070504040001027700",
    "0102031104052131061241510761711322328108144291a1b1c109233352f015",
    "6272d10a162434e125f11718191a262728292a35363738393a43444546474849",
    "4a535455565758595a636465666768696a737475767778797a82838485868788",
    "898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4",
    "c5c6c7c8c9cad2d3d4d5d6d7d8d9dae2e3e4e5e6e7e8e9eaf2f3f4f5f6f7f8f9",
    "faffdd00040002ffda000c03010002110311003f00f3fb1d0ba7c95f41571665",
    "97e3b6d4e9ac342e9f2579957167dce5f8edb53fffd0b761a174f92b1ab8b3e2",
    "b2fc76da9d3d8e85d3e4af32ae2cfb9cbf1db6a7ffd9",
];

const PILLOW_PROGRESSIVE_8X8: &[&str] = &[
    "ffd8ffe000104a46494600010100000100010000ffdb00430008060607060508",
    "0707070909080a0c140d0c0b0b0c1912130f141d1a1f1e1d1a1c1c20242e2720",
    "222c231c1c2837292c30313434341f27393d38323c2e333432ffc2000b080008",
    "000801011100ffc40014000100000000000000000000000000000000ffda0008",
    "0101000000017fffc400161001010100000000000000000000000000040312ff",
    "da000801010001050286424fffc4001a10010001050000000000000000000000",
    "0001310002111322ffda0008010100063f02da9d458625afffc4001910000300",
    "03000000000000000000000000011121004161ffda0008010100013f21121c2d",
    "c463e67fffda00080101000000107fffc4001710010101010000000000000000",
    "0000000001112131ffda0008010100013f10e8cc04152612615d3066c1ffd9",
];

/// Decodes hex text, written in lines, into bytes.
fn hex_bytes(lines: &[&str]) -> Vec<u8> {
    let text = lines.concat();
    text.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let digits = std::str::from_utf8(pair)
                .unwrap_or_else(|error| panic!("hex text is not UTF-8: {error}"));
            u8::from_str_radix(digits, 16)
                .unwrap_or_else(|error| panic!("{digits:?} is not a hex byte: {error}"))
        })
        .collect()
}

// ---- Accepted inputs ---------------------------------------------------------

#[test]
fn accepts_baseline_and_extended_sequential_frames_in_each_layout() {
    for sof in [SOF0, SOF1] {
        let layouts = [
            ("gray 8x8", JpegSpec::gray(8, 8)),
            ("4:4:4 16x16", JpegSpec::color(16, 16, Sampling::Yuv444)),
            ("4:2:2 16x8", JpegSpec::color(16, 8, Sampling::Yuv422)),
            ("4:2:0 17x9", JpegSpec::color(17, 9, Sampling::Yuv420)),
            ("CMYK 8x8", JpegSpec::cmyk(8, 8)),
        ];
        for (name, mut spec) in layouts {
            spec.sof = sof;
            assert_accepted(
                &format!("{name} with SOF marker {sof:#04x}"),
                &encode(&spec).bytes,
            );
        }
    }
}

#[test]
fn accepts_gray_frames_with_2x2_and_4x1_sampling() {
    for (name, h, v) in [("2x2", 2, 2), ("4x1", 4, 1)] {
        let mut spec = JpegSpec::gray(16, 16);
        spec.components[0].h = h;
        spec.components[0].v = v;
        assert_accepted(
            &format!("gray 16x16 at {name} sampling"),
            &encode(&spec).bytes,
        );
    }
}

#[test]
fn accepts_grayscale_frames_at_the_size_limits() {
    for (width, height) in [(1, 1), (7, 9), (4096, 1), (1, 4096)] {
        let file = encode(&JpegSpec::gray(width, height)).bytes;
        assert_accepted(&format!("gray {width}x{height}"), &file);
    }
}

#[test]
fn accepts_a_4096_by_4096_gray_frame() {
    let file = encode(&JpegSpec::gray(4096, 4096)).bytes;
    assert!(
        file.len() <= MAX_ASSET_BYTES,
        "the 4096x4096 gray file is {} bytes, over the {MAX_ASSET_BYTES}-byte upload limit",
        file.len()
    );
    assert_accepted("gray 4096x4096", &file);
}

#[test]
fn accepts_sampling_sums_of_exactly_ten_blocks_per_mcu() {
    let mut three = JpegSpec::color(16, 16, Sampling::Yuv444);
    set_sampling(&mut three, &[(2, 2), (2, 2), (2, 1)]);
    assert_accepted(
        "3 components at 2x2, 2x2 and 2x1 (sum 10)",
        &encode(&three).bytes,
    );
    let mut four = JpegSpec::cmyk(16, 16);
    set_sampling(&mut four, &[(2, 2), (2, 2), (1, 1), (1, 1)]);
    assert_accepted(
        "4 components at 2x2, 2x2, 1x1 and 1x1 (sum 10)",
        &encode(&four).bytes,
    );
}

#[test]
fn accepts_restart_intervals_of_one_and_five_that_wrap_past_rst7() {
    let mut gray_one = JpegSpec::gray(32, 32);
    gray_one.restart_interval = 1;
    let mut color_one = JpegSpec::color(32, 32, Sampling::Yuv444);
    color_one.restart_interval = 1;
    let mut gray_five = JpegSpec::gray(64, 64);
    gray_five.restart_interval = 5;
    let mut color_five = JpegSpec::color(80, 40, Sampling::Yuv444);
    color_five.restart_interval = 5;
    // A marker comes before each interval start after the first MCU, so the count is
    // (MCUs - 1) divided by the interval, rounded down.
    let cases = [
        ("gray 32x32, interval 1", gray_one, 15_usize),
        ("4:4:4 32x32, interval 1", color_one, 15),
        ("gray 64x64, interval 5", gray_five, 12),
        ("4:4:4 80x40, interval 5", color_five, 9),
    ];
    for (name, spec, markers) in cases {
        let parts = encode(&spec);
        let codes = restart_codes(&parts.bytes[parts.entropy.clone()]);
        assert_eq!(
            codes,
            (0..markers).map(rst_code).collect::<Vec<u8>>(),
            "{name}: the restart markers must cycle RST0 to RST7"
        );
        assert_accepted(name, &parts.bytes);
    }
}

#[test]
fn accepts_restart_intervals_at_or_beyond_the_mcu_count() {
    // A 4:4:4 16x16 frame has four MCUs, so an interval of four or more writes no marker.
    let mut cases = Vec::new();
    for interval in [4_u16, 5, u16::MAX] {
        let mut spec = JpegSpec::color(16, 16, Sampling::Yuv444);
        spec.restart_interval = interval;
        cases.push((format!("4:4:4 16x16, interval {interval}"), spec));
    }
    let mut single = JpegSpec::gray(8, 8);
    single.restart_interval = 5;
    cases.push(("gray 8x8, interval 5".to_owned(), single));
    for (name, spec) in cases {
        let parts = encode(&spec);
        let codes = restart_codes(&parts.bytes[parts.entropy.clone()]);
        assert!(
            codes.is_empty(),
            "{name}: expected no restart marker, found {codes:?}"
        );
        assert_accepted(&name, &parts.bytes);
    }
}

#[test]
fn accepts_application_and_comment_segments_before_and_after_the_frame_header() {
    let spec = JpegSpec::gray(8, 8);
    let entropy = entropy_of(&spec);
    // Every refused case is collected, so that one run lists all of them.
    let mut refused = Vec::new();
    for code in (APP0..=0xEF).chain([COM]) {
        let payload: &[u8] = match code {
            APP0 => &JFIF_PAYLOAD,
            APP1 => b"Exif\0\0 not really exif",
            APP14 => &ADOBE_PAYLOAD,
            _ => b"segment data",
        };
        // Positions 0 and 1 come before the frame header, and positions 2 and 3 after it.
        for position in 0..4 {
            // The decoder refuses an Adobe segment after the frame header of a gray frame; that
            // known limit is pinned by its own test below.
            if code == APP14 && position >= 2 {
                continue;
            }
            let mut headers = header_segments(&spec);
            headers.insert(position, segment(code, payload));
            if validate_jpeg(&assemble(&headers, &entropy)) != Ok(()) {
                refused.push(format!("{code:#04x} at header position {position}"));
            }
        }
    }
    assert!(
        refused.is_empty(),
        "gray 8x8 files with a segment that must be accepted were refused: {refused:?}"
    );
}

#[test]
fn refuses_an_adobe_segment_after_the_frame_header_of_a_gray_frame_as_a_known_limit() {
    // Known limit: the header parser and the walk accept this file (the walk is checked first
    // below), but zune-jpeg in strict mode sets the colorspace from the Adobe transform after the
    // frame header and then refuses "3 components expected, 1 found". The file is refused as a
    // whole. The same segment before the frame header is accepted.
    let spec = JpegSpec::gray(8, 8);
    let entropy = entropy_of(&spec);
    let mut headers = header_segments(&spec);
    headers.insert(2, segment(APP14, &ADOBE_PAYLOAD));
    let bytes = assemble(&headers, &entropy);
    let walked = walk_outcome(&bytes);
    assert_eq!(
        walked,
        Ok(()),
        "the headers and the walk must accept the file"
    );
    assert_eq!(validate_jpeg(&bytes), Err(AssetRejection::Invalid));
}

#[test]
fn accepts_an_adobe_segment_after_the_frame_header_of_a_color_frame() {
    // The frame header sets the colorspace to Luma for one component, and strict mode refuses an
    // Adobe transform that sets three components after it. With three components the two agree.
    let spec = JpegSpec::color(16, 16, Sampling::Yuv444);
    let mut headers = header_segments(&spec);
    headers.insert(2, segment(APP14, &ADOBE_PAYLOAD));
    assert_accepted(
        "3 components with an Adobe APP14 after the frame header",
        &assemble(&headers, &entropy_of(&spec)),
    );
}

#[test]
fn accepts_a_comment_whose_payload_holds_marker_bytes() {
    let spec = JpegSpec::gray(8, 8);
    let mut headers = header_segments(&spec);
    headers.insert(0, segment(COM, b"\xFF\xD9 and \xFF\xD8 inside a comment"));
    assert_accepted(
        "a COM segment whose payload holds FF D9 and FF D8",
        &assemble(&headers, &entropy_of(&spec)),
    );
}

#[test]
fn accepts_fill_bytes_before_each_marker_and_before_end_of_image() {
    let spec = JpegSpec::color(16, 16, Sampling::Yuv444);
    let entropy = entropy_of(&spec);
    let header_count = header_segments(&spec).len();
    for fill in [1_usize, 2, 5] {
        let filler = vec![0xFF_u8; fill];
        for position in 0..header_count {
            let mut headers = header_segments(&spec);
            headers.insert(position, filler.clone());
            assert_accepted(
                &format!("{fill} fill bytes before header position {position}"),
                &assemble(&headers, &entropy),
            );
        }
        let mut end = filler;
        end.extend(eoi());
        assert_accepted(
            &format!("{fill} fill bytes before EOI"),
            &assemble_with_end(&header_segments(&spec), &entropy, &end),
        );
    }
}

#[test]
fn accepts_a_redefined_huffman_table_and_decodes_with_the_last_definition() {
    let spec = JpegSpec::gray(16, 16);
    let entropy = entropy_of(&spec);
    let mut headers = header_segments(&spec);
    headers[2] = segment(
        DHT,
        &dht_payload(&[
            (0x00, &WRONG_DC),
            (0x00, &DC_LUMINANCE),
            (0x10, &AC_LUMINANCE),
        ]),
    );
    assert_accepted(
        "DC table 0 defined twice, with the standard definition last",
        &assemble(&headers, &entropy),
    );
}

#[test]
fn accepts_four_huffman_tables_in_one_dht_segment() {
    let spec = JpegSpec::color(16, 16, Sampling::Yuv420);
    let headers = header_segments(&spec);
    let dht_segments = headers.iter().filter(|part| part[1] == DHT).count();
    assert_eq!(
        dht_segments, 1,
        "the default layout must define all four tables in one DHT segment"
    );
    assert_accepted(
        "4:2:0 with four Huffman tables in one segment",
        &assemble(&headers, &entropy_of(&spec)),
    );
}

#[test]
fn accepts_several_quantization_tables_in_one_segment_and_in_several() {
    let mut gray = JpegSpec::gray(8, 8);
    gray.components[0].tq = 3;
    let entropy = entropy_of(&gray);
    let mut one_segment = header_segments(&gray);
    one_segment[0] = segment(DQT, &dqt_payload(&[0, 1, 2, 3]));
    assert_accepted(
        "tables 0 to 3 in one DQT segment, the frame using table 3",
        &assemble(&one_segment, &entropy),
    );
    let mut several = header_segments(&gray);
    several.insert(1, segment(DQT, &dqt_payload(&[0, 1])));
    assert_accepted(
        "table 3 in one DQT segment, then tables 0 and 1 in another",
        &assemble(&several, &entropy),
    );
    let mut color = JpegSpec::color(16, 16, Sampling::Yuv444);
    color.components[1].tq = 2;
    color.components[2].tq = 3;
    assert_accepted("3 components using tables 0, 2 and 3", &file_of(&color));
}

#[test]
fn accepts_table_selector_two_under_sof1() {
    assert_accepted("SOF1 scan selecting table 2", &slot_two_file(SOF1));
}

#[test]
fn accepts_the_worst_case_frame_within_the_upload_limit() {
    let file = worst_case_jpeg();
    assert!(
        file.len() <= MAX_ASSET_BYTES,
        "worst_case_jpeg is {} bytes, over the {MAX_ASSET_BYTES}-byte upload limit",
        file.len()
    );
    assert_accepted(
        "worst_case_jpeg: 4096x4096, 4:4:4, a restart after every MCU",
        &file,
    );
}

#[test]
fn accepts_the_pillow_baseline_files() {
    let cases: [(&str, &[&str], usize); 3] = [
        ("GRAY_8X8.jpg", PILLOW_GRAY_8X8, 362),
        ("COLOR_17X9_420.jpg", PILLOW_COLOR_17X9_420, 682),
        ("RESTART_16X16.jpg", PILLOW_RESTART_16X16, 694),
    ];
    for (name, lines, len) in cases {
        let bytes = hex_bytes(lines);
        assert_eq!(bytes.len(), len, "{name} decodes to the wrong length");
        assert_accepted(name, &bytes);
    }
}

#[test]
fn the_header_builder_reproduces_the_fixture_encoder() {
    let mut jfif = JpegSpec::color(16, 16, Sampling::Yuv420);
    jfif.jfif = true;
    jfif.restart_interval = 2;
    let mut sof1 = JpegSpec::cmyk(8, 8);
    sof1.sof = SOF1;
    let cases = [
        ("minimal", JpegSpec::default()),
        ("JFIF, 4:2:0, restart 2", jfif),
        ("CMYK, SOF1", sof1),
    ];
    for (name, spec) in cases {
        assert_eq!(
            file_of(&spec),
            encode(&spec).bytes,
            "{name}: the header builder must match `encode`"
        );
    }
}

// ---- Refused inputs: the bytes around the image ------------------------------

#[test]
fn refuses_every_strict_prefix_of_the_minimal_file() {
    let file = minimal_jpeg();
    assert_accepted("the complete minimal file", &file);
    for length in 0..file.len() {
        assert_refused(
            &format!("prefix of {length} of {} bytes", file.len()),
            &file[..length],
        );
    }
}

#[test]
fn refuses_any_byte_after_the_end_of_image() {
    let trailers: [(&str, Vec<u8>); 5] = [
        ("one zero byte", vec![0x00]),
        ("a line feed", vec![0x0A]),
        ("a second EOI marker", vec![0xFF, EOI]),
        ("one fill byte", vec![0xFF]),
        ("a whole second JPEG", minimal_jpeg()),
    ];
    let spec = JpegSpec::gray(8, 8);
    for (name, trailer) in trailers {
        let mut file = minimal_jpeg();
        file.extend(trailer);
        assert_refused_by_end_check(&format!("{name} after EOI"), &file, &spec);
    }
}

#[test]
fn refuses_a_byte_before_the_start_of_image() {
    let file = minimal_jpeg();
    let mut without_third_ff = file.clone();
    without_third_ff.remove(2);
    let cases: [(&str, Vec<u8>); 3] = [
        (
            "one zero byte before SOI",
            [vec![0x00], file.clone()].concat(),
        ),
        (
            "one FF byte before SOI",
            [vec![0xFF], file.clone()].concat(),
        ),
        (
            "SOI without the FF that begins the next marker",
            without_third_ff,
        ),
    ];
    for (name, bytes) in cases {
        assert_refused_by_headers(name, &bytes);
    }
}

#[test]
fn refuses_the_pillow_progressive_file() {
    let bytes = hex_bytes(PILLOW_PROGRESSIVE_8X8);
    assert_eq!(
        bytes.len(),
        319,
        "PROGRESSIVE_8X8.jpg decodes to the wrong length"
    );
    // The frame marker SOF2 is refused in the header, before any scan is read.
    assert_refused_by_headers("PROGRESSIVE_8X8.jpg", &bytes);
}

#[test]
fn refuses_a_scan_that_ends_without_an_end_of_image_marker() {
    let file = minimal_jpeg();
    assert_refused_by_walk("the minimal file without its EOI", &file[..file.len() - 2]);
}

#[test]
fn refuses_a_second_scan_after_the_first() {
    let spec = JpegSpec::gray(8, 8);
    let entropy = entropy_of(&spec);
    let mut bytes = assemble_with_end(&header_segments(&spec), &entropy, &[]);
    bytes.extend(segment(SOS, &sos_payload(&spec.components)));
    bytes.extend(&entropy);
    bytes.extend(eoi());
    assert_refused_by_walk("a second SOS after the first scan", &bytes);
}

#[test]
fn refuses_a_marker_other_than_eoi_where_the_scan_should_end() {
    let spec = JpegSpec::gray(8, 8);
    let entropy = entropy_of(&spec);
    let dnl = [0xFF, DNL, 0x00, 0x04, 0x00, 0x08];
    let mut bytes = assemble_with_end(&header_segments(&spec), &entropy, &dnl);
    bytes.extend(eoi());
    assert_refused_by_walk("DNL in place of EOI after the scan", &bytes);
}

#[test]
fn refuses_a_redefined_huffman_table_whose_last_definition_is_wrong() {
    let spec = JpegSpec::gray(16, 16);
    let entropy = entropy_of(&spec);
    let mut headers = header_segments(&spec);
    headers[2] = segment(
        DHT,
        &dht_payload(&[
            (0x00, &DC_LUMINANCE),
            (0x00, &WRONG_DC),
            (0x10, &AC_LUMINANCE),
        ]),
    );
    assert_refused_by_walk(
        "DC table 0 defined twice, with the wrong definition last",
        &assemble(&headers, &entropy),
    );
}

// ---- Refused inputs: frame, scan and segment headers --------------------------

#[test]
fn refuses_frame_markers_other_than_sof0_and_sof1() {
    let spec = JpegSpec::gray(8, 8);
    let entropy = entropy_of(&spec);
    for code in [
        0xC2_u8, 0xC3, 0xC5, 0xC6, 0xC7, 0xC9, 0xCA, 0xCB, 0xCD, 0xCE, 0xCF,
    ] {
        let mut headers = header_segments(&spec);
        headers[1] = segment(code, &sof_payload(&spec));
        assert_refused_by_headers(
            &format!("frame marker {code:#04x}"),
            &assemble(&headers, &entropy),
        );
    }
}

#[test]
fn refuses_sample_precision_other_than_8() {
    for precision in [0_u8, 12, 16] {
        let mut spec = JpegSpec::gray(8, 8);
        spec.precision = precision;
        assert_refused_by_headers(&format!("precision {precision}"), &file_of(&spec));
    }
}

#[test]
fn refuses_component_counts_other_than_1_3_and_4() {
    let mut two = JpegSpec::color(16, 16, Sampling::Yuv444);
    two.components.truncate(2);
    let mut five = JpegSpec::cmyk(16, 16);
    five.components.push(ComponentSpec {
        id: 5,
        h: 1,
        v: 1,
        tq: 0,
    });
    assert_refused_by_headers("two components", &file_of(&two));
    assert_refused_by_headers("five components", &file_of(&five));
    // A frame header with no components, which `JpegSpec` cannot express.
    let gray = JpegSpec::gray(8, 8);
    let mut headers = header_segments(&gray);
    headers[1] = segment(SOF0, &[8, 0, 8, 0, 8, 0]);
    assert_refused_by_headers("zero components", &assemble(&headers, &entropy_of(&gray)));
}

#[test]
fn refuses_frame_dimensions_of_zero_or_above_4096() {
    for (width, height) in [(0, 8), (8, 0), (4097, 8), (8, 4097), (4097, 4097)] {
        let mut spec = JpegSpec::gray(8, 8);
        spec.width = width;
        spec.height = height;
        assert_refused_by_headers(&format!("gray {width}x{height}"), &file_of(&spec));
    }
}

#[test]
fn refuses_sampling_factors_of_zero_or_above_four() {
    for (name, h, v) in [
        ("H = 0", 0, 1),
        ("V = 0", 1, 0),
        ("H = 5", 5, 1),
        ("V = 5", 1, 5),
    ] {
        let mut spec = JpegSpec::gray(8, 8);
        spec.components[0].h = h;
        spec.components[0].v = v;
        assert_refused_by_headers(name, &file_of(&spec));
    }
}

#[test]
fn refuses_a_sampling_factor_that_does_not_divide_the_largest_factor() {
    // The largest horizontal factor is 3, and 3 is not a multiple of 2.
    let mut horizontal = JpegSpec::color(16, 16, Sampling::Yuv444);
    set_sampling(&mut horizontal, &[(3, 1), (2, 1), (1, 1)]);
    let mut vertical = JpegSpec::color(16, 16, Sampling::Yuv444);
    set_sampling(&mut vertical, &[(1, 3), (1, 2), (1, 1)]);
    let mut four_and_three = JpegSpec::color(16, 16, Sampling::Yuv444);
    set_sampling(&mut four_and_three, &[(4, 1), (3, 1), (1, 1)]);
    for (name, spec) in [
        ("horizontal 3 and 2", horizontal),
        ("vertical 3 and 2", vertical),
        ("horizontal 4 and 3", four_and_three),
    ] {
        assert_refused_by_headers(name, &file_of(&spec));
    }
}

#[test]
fn refuses_frames_with_more_than_ten_blocks_per_mcu() {
    let mut twelve = JpegSpec::color(16, 16, Sampling::Yuv444);
    set_sampling(&mut twelve, &[(2, 2), (2, 2), (2, 2)]);
    let mut thirteen = JpegSpec::cmyk(16, 16);
    set_sampling(&mut thirteen, &[(2, 2), (2, 2), (2, 2), (1, 1)]);
    let mut eighteen = JpegSpec::color(16, 16, Sampling::Yuv444);
    set_sampling(&mut eighteen, &[(4, 4), (1, 1), (1, 1)]);
    for (name, spec) in [
        ("sum 12", twelve),
        ("sum 13", thirteen),
        ("sum 18", eighteen),
    ] {
        assert_refused_by_headers(name, &file_of(&spec));
    }
}

#[test]
fn refuses_a_second_frame_header() {
    let spec = JpegSpec::gray(8, 8);
    let mut headers = header_segments(&spec);
    headers.insert(2, segment(SOF0, &sof_payload(&spec)));
    assert_refused_by_headers(
        "a second SOF0 after the first",
        &assemble(&headers, &entropy_of(&spec)),
    );
}

#[test]
fn refuses_a_scan_header_before_any_frame_header() {
    let spec = JpegSpec::gray(8, 8);
    let mut headers = header_segments(&spec);
    headers.remove(1);
    assert_refused_by_headers(
        "SOS with no frame header before it",
        &assemble(&headers, &entropy_of(&spec)),
    );
}

#[test]
fn refuses_dnl_and_dac_segments_before_the_scan() {
    let spec = JpegSpec::gray(8, 8);
    let entropy = entropy_of(&spec);
    let cases: [(&str, Vec<u8>); 2] = [
        ("DNL", segment(DNL, &[0, 8])),
        ("DAC", segment(DAC, &[0x00, 0x00])),
    ];
    for (name, marker_segment) in cases {
        let mut headers = header_segments(&spec);
        headers.insert(3, marker_segment);
        assert_refused_by_headers(
            &format!("{name} before the scan"),
            &assemble(&headers, &entropy),
        );
    }
}

#[test]
fn refuses_markers_that_are_not_admitted_before_the_scan() {
    let spec = JpegSpec::gray(8, 8);
    let entropy = entropy_of(&spec);
    let codes = (RST0..=0xD7).chain([0x01, 0x02, 0xBF, 0xC8, DHP, EXP, 0xF0, 0xFD, EOI, SOI]);
    for code in codes {
        let mut headers = header_segments(&spec);
        headers.insert(3, empty_marker(code));
        assert_refused_by_headers(
            &format!("marker {code:#04x} before the scan"),
            &assemble(&headers, &entropy),
        );
    }
}

#[test]
fn refuses_a_second_soi_before_the_scan() {
    let spec = JpegSpec::gray(8, 8);
    let mut headers = header_segments(&spec);
    headers.insert(0, vec![0xFF, SOI]);
    assert_refused_by_headers(
        "a second SOI directly after the first",
        &assemble(&headers, &entropy_of(&spec)),
    );
}

#[test]
fn refuses_empty_quantization_and_huffman_segments() {
    let spec = JpegSpec::gray(8, 8);
    let entropy = entropy_of(&spec);
    let mut empty_quant_segment = header_segments(&spec);
    empty_quant_segment[0] = segment(DQT, &[]);
    assert_refused_by_headers("empty DQT", &assemble(&empty_quant_segment, &entropy));
    let mut empty_huffman_segment = header_segments(&spec);
    empty_huffman_segment[2] = segment(DHT, &[]);
    assert_refused_by_headers("empty DHT", &assemble(&empty_huffman_segment, &entropy));
}

#[test]
fn refuses_duplicate_component_identifiers() {
    let mut spec = JpegSpec::color(16, 16, Sampling::Yuv444);
    spec.components[2].id = 1;
    assert_refused_by_headers("component identifiers 1, 2, 1", &file_of(&spec));
}

#[test]
fn refuses_quantization_selectors_above_three() {
    let mut frame = JpegSpec::gray(8, 8);
    frame.components[0].tq = 4;
    let mut headers = header_segments(&frame);
    headers[0] = segment(DQT, &dqt_payload(&[0]));
    assert_refused_by_headers(
        "frame selecting quantization table 4",
        &assemble(&headers, &entropy_of(&frame)),
    );
    let gray = JpegSpec::gray(8, 8);
    let mut defines_four = header_segments(&gray);
    defines_four[0] = segment(DQT, &dqt_payload(&[0, 4]));
    assert_refused_by_headers(
        "DQT defining table 4",
        &assemble(&defines_four, &entropy_of(&gray)),
    );
}

#[test]
fn refuses_scan_tables_that_are_not_defined() {
    let spec = JpegSpec::gray(8, 8);
    let entropy = entropy_of(&spec);
    for (name, table_byte) in [("DC table 1", 0x10_u8), ("AC table 1", 0x01)] {
        let mut headers = header_segments(&spec);
        headers[3] = sos_segment(&[(1, table_byte)], [0, 63, 0]);
        assert_refused_by_headers(
            &format!("scan naming undefined {name}"),
            &assemble(&headers, &entropy),
        );
    }
}

#[test]
fn refuses_a_frame_whose_quantization_table_is_not_defined() {
    let mut spec = JpegSpec::gray(8, 8);
    spec.components[0].tq = 1;
    let mut headers = header_segments(&spec);
    headers[0] = segment(DQT, &dqt_payload(&[0]));
    assert_refused_by_headers(
        "frame using quantization table 1 when only table 0 is defined",
        &assemble(&headers, &entropy_of(&spec)),
    );
}

#[test]
fn refuses_table_selector_two_under_sof0() {
    assert_refused_by_headers("SOF0 scan selecting table 2", &slot_two_file(SOF0));
}

#[test]
fn refuses_scan_headers_that_do_not_match_the_frame_components() {
    let spec = JpegSpec::color(16, 16, Sampling::Yuv444);
    let entropy = entropy_of(&spec);
    let cases: [(&str, Vec<(u8, u8)>); 4] = [
        ("a component missing", vec![(1, 0x00), (2, 0x11)]),
        (
            "components out of order",
            vec![(1, 0x00), (3, 0x11), (2, 0x11)],
        ),
        (
            "an identifier not in the frame",
            vec![(1, 0x00), (2, 0x11), (4, 0x11)],
        ),
        (
            "a fourth component",
            vec![(1, 0x00), (2, 0x11), (3, 0x11), (3, 0x11)],
        ),
    ];
    for (name, components) in cases {
        let mut headers = header_segments(&spec);
        headers[3] = sos_segment(&components, [0, 63, 0]);
        assert_refused_by_headers(name, &assemble(&headers, &entropy));
    }
}

#[test]
fn refuses_scan_fields_other_than_spectral_0_63_and_no_approximation() {
    let spec = JpegSpec::gray(8, 8);
    let entropy = entropy_of(&spec);
    let cases: [(&str, [u8; 3]); 5] = [
        ("Ss = 1", [1, 63, 0]),
        ("Se = 62", [0, 62, 0]),
        ("Ah = 1", [0, 63, 0x10]),
        ("Al = 1", [0, 63, 0x01]),
        ("Ss = 0 and Se = 0, a DC-only scan", [0, 0, 0]),
    ];
    for (name, tail) in cases {
        let mut headers = header_segments(&spec);
        headers[3] = sos_segment(&[(1, 0x00)], tail);
        assert_refused_by_headers(name, &assemble(&headers, &entropy));
    }
}

#[test]
fn refuses_quantization_segments_with_16_bit_precision_zero_values_or_a_wrong_length() {
    let spec = JpegSpec::gray(8, 8);
    let entropy = entropy_of(&spec);
    let mut sixteen_bit = vec![0x10_u8];
    sixteen_bit.extend([16_u8; 128]);
    let mut zero_value = dqt_payload(&[0]);
    zero_value[6] = 0;
    let declared: u16 = 67;
    let cases: [(&str, Vec<u8>); 4] = [
        ("a 16-bit table", segment(DQT, &sixteen_bit)),
        ("a zero value", segment(DQT, &zero_value)),
        (
            "a declared length one byte longer",
            with_declared_length(segment(DQT, &dqt_payload(&[0])), declared + 1),
        ),
        (
            "a declared length one byte shorter",
            with_declared_length(segment(DQT, &dqt_payload(&[0])), declared - 1),
        ),
    ];
    for (name, marker_segment) in cases {
        let mut headers = header_segments(&spec);
        headers[0] = marker_segment;
        assert_refused_by_headers(name, &assemble(&headers, &entropy));
    }
}

#[test]
fn refuses_huffman_entries_with_oversubscribed_or_all_ones_codes() {
    let spec = JpegSpec::gray(8, 8);
    let entropy = entropy_of(&spec);
    let cases: [(&str, [u8; 16], Vec<u8>); 4] = [
        ("three one-bit codes", counts_of(&[3]), symbol_values(3)),
        (
            "two one-bit codes, which use the all-ones word, and four two-bit codes",
            counts_of(&[2, 4]),
            symbol_values(6),
        ),
        (
            "one one-bit code and two two-bit codes, the last being all ones",
            counts_of(&[1, 2]),
            symbol_values(3),
        ),
        ("five two-bit codes", counts_of(&[0, 5]), symbol_values(5)),
    ];
    for (name, counts, symbols) in cases {
        // A valid AC table follows the bad DC table, so the DC table is the only defect.
        let mut headers = header_segments(&spec);
        headers[2] = segment(
            DHT,
            &[
                huffman_entry(0x00, counts, &symbols),
                huffman_entry(0x10, AC_LUMINANCE.counts, AC_LUMINANCE.symbols),
            ]
            .concat(),
        );
        assert_refused_by_headers(name, &assemble(&headers, &entropy));
    }
}

#[test]
fn refuses_huffman_entries_with_zero_or_more_than_256_symbols() {
    let spec = JpegSpec::gray(8, 8);
    let entropy = entropy_of(&spec);
    let mut counts_of_257 = [0_u8; 16];
    counts_of_257[14] = 2;
    counts_of_257[15] = 255;
    let cases: [(&str, [u8; 16], Vec<u8>); 2] = [
        ("no symbols", [0; 16], Vec::new()),
        ("257 symbols", counts_of_257, symbol_values(257)),
    ];
    for (name, counts, symbols) in cases {
        // A valid AC table follows the bad DC table, so the DC table is the only defect.
        let mut headers = header_segments(&spec);
        headers[2] = segment(
            DHT,
            &[
                huffman_entry(0x00, counts, &symbols),
                huffman_entry(0x10, AC_LUMINANCE.counts, AC_LUMINANCE.symbols),
            ]
            .concat(),
        );
        assert_refused_by_headers(name, &assemble(&headers, &entropy));
    }
}

#[test]
fn refuses_huffman_entries_whose_class_or_selector_is_out_of_range() {
    let spec = JpegSpec::gray(8, 8);
    let entropy = entropy_of(&spec);
    for (name, class_and_id) in [("class 2", 0x20_u8), ("selector 4", 0x04)] {
        let mut headers = header_segments(&spec);
        headers[2] = segment(
            DHT,
            &huffman_entry(class_and_id, DC_LUMINANCE.counts, DC_LUMINANCE.symbols),
        );
        assert_refused_by_headers(name, &assemble(&headers, &entropy));
    }
}

#[test]
fn refuses_huffman_segments_whose_entries_do_not_fill_the_segment() {
    let spec = JpegSpec::gray(8, 8);
    let entropy = entropy_of(&spec);
    let payload = dht_payload(&[(0x00, &DC_LUMINANCE), (0x10, &AC_LUMINANCE)]);
    let declared = u16::try_from(payload.len() + 2)
        .unwrap_or_else(|error| panic!("the DHT payload must fit a segment: {error}"));
    for (name, length) in [
        ("a declared length one byte longer", declared + 1),
        ("a declared length one byte shorter", declared - 1),
    ] {
        let mut headers = header_segments(&spec);
        headers[2] = with_declared_length(segment(DHT, &payload), length);
        assert_refused_by_headers(name, &assemble(&headers, &entropy));
    }
    // Symbols that do not match the counts: one symbol short, then one symbol over.
    let mut short = header_segments(&spec);
    short[2] = segment(
        DHT,
        &huffman_entry(0x00, counts_of(&[0, 3]), &symbol_values(2)),
    );
    assert_refused_by_headers("two symbols for three codes", &assemble(&short, &entropy));
    let mut over = header_segments(&spec);
    over[2] = segment(
        DHT,
        &huffman_entry(0x00, counts_of(&[1]), &symbol_values(2)),
    );
    assert_refused_by_headers("two symbols for one code", &assemble(&over, &entropy));
}

#[test]
fn refuses_restart_segments_that_are_not_two_bytes() {
    let mut spec = JpegSpec::gray(8, 8);
    spec.restart_interval = 1;
    let entropy = entropy_of(&spec);
    for (name, payload) in [
        ("a DRI of length 5", vec![0, 1, 0]),
        ("a DRI of length 3", vec![1]),
        ("a DRI of length 2", Vec::new()),
    ] {
        let mut headers = header_segments(&spec);
        headers[3] = segment(DRI, &payload);
        assert_refused_by_headers(name, &assemble(&headers, &entropy));
    }
}

#[test]
fn refuses_segment_lengths_below_two_and_lengths_past_the_end_of_the_file() {
    let spec = JpegSpec::gray(8, 8);
    let entropy = entropy_of(&spec);
    for length in [0_u16, 1] {
        let mut headers = header_segments(&spec);
        headers[0] = with_declared_length(headers[0].clone(), length);
        assert_refused_by_headers(
            &format!("DQT with declared length {length}"),
            &assemble(&headers, &entropy),
        );
    }
    let mut headers = header_segments(&spec);
    headers[3] = with_declared_length(headers[3].clone(), 0xFFFF);
    assert_refused_by_headers(
        "SOS with declared length 65535, past the end of the file",
        &assemble(&headers, &entropy),
    );
}

// ---- Each layer on its own ---------------------------------------------------

// `validate_jpeg` runs the header parser, the scan walk and zune-jpeg in that order. These tests
// call the inner functions directly, so that each layer's own guard is what decides.

#[test]
fn frame_parse_refuses_out_of_range_sizes_before_building_a_frame() {
    for (width, height) in [(0, 8), (8, 0), (4097, 8), (8, 4097), (4097, 4097)] {
        assert_eq!(
            Frame::parse(SOF0, &sof_payload_of(width, height)).map(|_| ()),
            Err(AssetRejection::Invalid),
            "Frame::parse must refuse {width}x{height}"
        );
        assert_eq!(
            check_dimensions(u32::from(width), u32::from(height)),
            Err(AssetRejection::Invalid),
            "check_dimensions must refuse {width}x{height}"
        );
    }
    for (width, height) in [(1, 1), (4096, 1), (1, 4096), (4096, 4096)] {
        assert_eq!(
            Frame::parse(SOF0, &sof_payload_of(width, height)).map(|_| ()),
            Ok(()),
            "Frame::parse must admit {width}x{height}"
        );
        assert_eq!(
            check_dimensions(u32::from(width), u32::from(height)),
            Ok(()),
            "check_dimensions must admit {width}x{height}"
        );
    }
}

#[test]
fn decode_with_zune_refuses_a_size_that_differs_from_the_headers() {
    let file = minimal_jpeg();
    assert_eq!(
        decode_with_zune(&file, 8, 8),
        Ok(()),
        "control: zune decodes the 8x8 file at 8x8"
    );
    for (width, height) in [(7, 8), (9, 8), (8, 7), (8, 9)] {
        assert_eq!(
            decode_with_zune(&file, width, height),
            Err(AssetRejection::Invalid),
            "zune's 8x8 file must not be reported as {width}x{height}"
        );
    }
}

#[test]
fn decode_with_zune_refuses_a_truncated_header() {
    assert_eq!(
        decode_with_zune(&[0xFF, 0xD8, 0xFF, 0xDA], 8, 8),
        Err(AssetRejection::Invalid),
        "a start of scan with no frame or tables"
    );
}

#[test]
fn decode_with_zune_refuses_a_dc_symbol_above_11_that_no_block_uses() {
    // The table builder checks only the code counts, and the walk decodes only the DC categories
    // that blocks use. So zune-jpeg is the layer that refuses symbol 255 in a DC table here.
    let gray = JpegSpec::gray(8, 8);
    assert_accepted("control: the standard DC table", &file_of(&gray));
    let symbols: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 255];
    let mut headers = header_segments(&gray);
    headers[2] = segment(
        DHT,
        &[
            huffman_entry(0x00, DC_LUMINANCE.counts, &symbols),
            huffman_entry(0x10, AC_LUMINANCE.counts, AC_LUMINANCE.symbols),
        ]
        .concat(),
    );
    let file = assemble(&headers, &entropy_of(&gray));
    assert_eq!(
        header_outcome(&file),
        Ok(()),
        "the header parser must admit the table"
    );
    assert_eq!(
        walk_outcome(&file),
        Ok(()),
        "the walk must admit the file, since no block uses symbol 255"
    );
    assert_eq!(
        zune_alone(&file, &gray),
        Err(AssetRejection::Invalid),
        "zune-jpeg alone must refuse symbol 255 in a DC table"
    );
    assert_refused("a DC table with symbol 255", &file);
}

#[test]
fn huffman_table_build_refuses_bad_code_counts_and_symbol_totals_alone() {
    let mut full_256 = [0_u8; 16];
    full_256[14] = 1;
    full_256[15] = 255;
    let mut total_257 = [0_u8; 16];
    total_257[14] = 2;
    total_257[15] = 255;
    let refused: [(&str, [u8; 16], usize); 8] = [
        ("no symbols", [0; 16], 0),
        ("257 symbols", total_257, 257),
        ("three one-bit codes", counts_of(&[3]), 3),
        (
            "two one-bit codes and four two-bit codes",
            counts_of(&[2, 4]),
            6,
        ),
        (
            "one one-bit code and two two-bit codes",
            counts_of(&[1, 2]),
            3,
        ),
        ("five two-bit codes", counts_of(&[0, 5]), 5),
        ("one symbol fewer than the counts", counts_of(&[0, 2]), 1),
        ("one symbol more than the counts", counts_of(&[0, 1]), 3),
    ];
    for (name, counts, count) in refused {
        assert_eq!(
            HuffmanTable::build(&counts, &symbol_values(count)).map(|_| ()),
            Err(AssetRejection::Invalid),
            "{name} must be refused by the table builder"
        );
    }
    let admitted: [(&str, [u8; 16], Vec<u8>); 3] = [
        (
            "the standard DC table",
            DC_LUMINANCE.counts,
            DC_LUMINANCE.symbols.to_vec(),
        ),
        (
            "the standard chrominance AC table",
            AC_CHROMINANCE.counts,
            AC_CHROMINANCE.symbols.to_vec(),
        ),
        ("256 symbols", full_256, symbol_values(256)),
    ];
    for (name, counts, symbols) in admitted {
        assert_eq!(
            HuffmanTable::build(&counts, &symbols).map(|_| ()),
            Ok(()),
            "{name} must be admitted by the table builder"
        );
    }
}

// ---- Helpers -----------------------------------------------------------------

/// Checks that `validate_jpeg` accepts `bytes`. `case` names the input in the failure message.
fn assert_accepted(case: &str, bytes: &[u8]) {
    assert_eq!(validate_jpeg(bytes), Ok(()), "{case} must be accepted");
}

/// Checks that `validate_jpeg` refuses `bytes` as `Invalid`.
fn assert_refused(case: &str, bytes: &[u8]) {
    assert_eq!(
        validate_jpeg(bytes),
        Err(AssetRejection::Invalid),
        "{case} must be refused as Invalid"
    );
}

/// Checks that the header parser refuses `bytes`, and that `validate_jpeg` refuses them too.
fn assert_refused_by_headers(case: &str, bytes: &[u8]) {
    assert_eq!(
        header_outcome(bytes),
        Err(AssetRejection::Invalid),
        "{case}: the header parser must refuse it"
    );
    assert_refused(case, bytes);
}

/// Checks that the headers are valid, that the scan walk refuses `bytes`, and that `validate_jpeg`
/// refuses them too. The headers must be valid, so that the walk is the layer that decides.
fn assert_refused_by_walk(case: &str, bytes: &[u8]) {
    assert_eq!(
        header_outcome(bytes),
        Ok(()),
        "{case}: the headers must be valid, so that the scan walk decides"
    );
    assert_eq!(
        walk_outcome(bytes),
        Err(AssetRejection::Invalid),
        "{case}: the scan walk must refuse it"
    );
    assert_refused(case, bytes);
}

/// Checks a file with bytes after its end-of-image marker. The scan walk accepts the scan, and
/// zune-jpeg alone accepts the file, so only the end-of-file check in `validate_jpeg` can refuse
/// it.
fn assert_refused_by_end_check(case: &str, bytes: &[u8], spec: &JpegSpec) {
    assert_eq!(
        walk_outcome(bytes),
        Ok(()),
        "{case}: the scan walk must accept the scan"
    );
    assert_eq!(
        zune_alone(bytes, spec),
        Ok(()),
        "{case}: zune-jpeg alone must accept the file"
    );
    assert_refused(case, bytes);
}

/// Runs the header parser after the start-of-image marker, with no walk and no decoder.
fn header_outcome(bytes: &[u8]) -> Result<(), AssetRejection> {
    let mut reader = Reader::new(bytes);
    reader.take_bytes(2)?;
    read_scan_input(&mut reader).map(|_| ())
}

/// Runs the header parser and then the scan walk, with no decoder and no check of the bytes after
/// the end-of-image marker.
fn walk_outcome(bytes: &[u8]) -> Result<(), AssetRejection> {
    let mut reader = Reader::new(bytes);
    reader.take_bytes(2)?;
    let input = read_scan_input(&mut reader)?;
    walk_scan(bytes, reader.offset, &input.plan, &input.tables).map(|_| ())
}

/// Runs zune-jpeg alone, at the size that `spec` declares.
fn zune_alone(bytes: &[u8], spec: &JpegSpec) -> Result<(), AssetRejection> {
    decode_with_zune(bytes, u32::from(spec.width), u32::from(spec.height))
}

/// The file that `encode` writes for `spec`, rebuilt from its header segments and its entropy data.
fn file_of(spec: &JpegSpec) -> Vec<u8> {
    assemble(&header_segments(spec), &entropy_of(spec))
}

/// Returns the entropy-coded data that `encode` writes for `spec`.
fn entropy_of(spec: &JpegSpec) -> Vec<u8> {
    let parts = encode(spec);
    parts.bytes[parts.entropy.clone()].to_vec()
}

/// Returns the segments that `encode` writes before the entropy-coded data: the optional JFIF
/// segment, DQT, the frame header, DHT, the optional DRI, and SOS.
fn header_segments(spec: &JpegSpec) -> Vec<Vec<u8>> {
    let mut headers = Vec::new();
    if spec.jfif {
        headers.push(segment(APP0, &JFIF_PAYLOAD));
    }
    let mut quant: Vec<u8> = spec
        .components
        .iter()
        .map(|component| component.tq)
        .collect();
    quant.sort_unstable();
    quant.dedup();
    headers.push(segment(DQT, &dqt_payload(&quant)));
    headers.push(segment(spec.sof, &sof_payload(spec)));
    let mut huffman: Vec<(u8, &HuffmanSpec)> = vec![(0x00, &DC_LUMINANCE), (0x10, &AC_LUMINANCE)];
    if spec.components.len() > 1 {
        huffman.push((0x01, &DC_CHROMINANCE));
        huffman.push((0x11, &AC_CHROMINANCE));
    }
    headers.push(segment(DHT, &dht_payload(&huffman)));
    if spec.restart_interval > 0 {
        headers.push(segment(DRI, &spec.restart_interval.to_be_bytes()));
    }
    headers.push(segment(SOS, &sos_payload(&spec.components)));
    headers
}

/// Joins SOI, the header segments, the entropy-coded data and EOI, in that order.
fn assemble(headers: &[Vec<u8>], entropy: &[u8]) -> Vec<u8> {
    assemble_with_end(headers, entropy, &eoi())
}

/// Joins SOI, the header segments, the entropy-coded data and `end`, in that order.
fn assemble_with_end(headers: &[Vec<u8>], entropy: &[u8], end: &[u8]) -> Vec<u8> {
    let mut bytes = soi();
    for part in headers {
        bytes.extend_from_slice(part);
    }
    bytes.extend_from_slice(entropy);
    bytes.extend_from_slice(end);
    bytes
}

/// Returns a 16x16 gray file whose scan selects Huffman tables 2. The DHT defines tables 2 as
/// copies of the luminance tables, so the entropy data decodes either way. Under SOF1 a scan may
/// select tables up to 3, and under SOF0 only up to 1.
fn slot_two_file(sof: u8) -> Vec<u8> {
    let mut spec = JpegSpec::gray(16, 16);
    spec.sof = sof;
    let mut headers = header_segments(&spec);
    headers[2] = segment(
        DHT,
        &dht_payload(&[
            (0x00, &DC_LUMINANCE),
            (0x10, &AC_LUMINANCE),
            (0x02, &DC_LUMINANCE),
            (0x12, &AC_LUMINANCE),
        ]),
    );
    headers[3] = sos_segment(&[(1, 0x22)], [0, 63, 0]);
    assemble(&headers, &entropy_of(&spec))
}

/// Builds an SOS segment from `(component identifier, table byte)` pairs, and the three bytes
/// that follow them: spectral start, spectral end, and approximation.
fn sos_segment(components: &[(u8, u8)], tail: [u8; 3]) -> Vec<u8> {
    let count = u8::try_from(components.len())
        .unwrap_or_else(|error| panic!("a scan header holds at most 255 components: {error}"));
    let mut payload = vec![count];
    for &(id, table) in components {
        payload.extend_from_slice(&[id, table]);
    }
    payload.extend_from_slice(&tail);
    segment(SOS, &payload)
}

/// Encodes one DHT entry: the class and identifier byte, 16 code counts, then the symbols.
fn huffman_entry(class_and_id: u8, counts: [u8; 16], symbols: &[u8]) -> Vec<u8> {
    let mut entry = vec![class_and_id];
    entry.extend_from_slice(&counts);
    entry.extend_from_slice(symbols);
    entry
}

/// Returns 16 code counts that start with `lengths`, and are zero after them.
fn counts_of(lengths: &[u8]) -> [u8; 16] {
    let mut counts = [0_u8; 16];
    for (count, &value) in counts.iter_mut().zip(lengths) {
        *count = value;
    }
    counts
}

/// Returns `count` symbol values, which repeat every 256.
fn symbol_values(count: usize) -> Vec<u8> {
    (0..count)
        .map(|index| {
            u8::try_from(index % 256)
                .unwrap_or_else(|error| panic!("{index} modulo 256 must fit a byte: {error}"))
        })
        .collect()
}

/// Returns `segment_bytes` with its two-byte length field replaced by `length`.
fn with_declared_length(mut segment_bytes: Vec<u8>, length: u16) -> Vec<u8> {
    segment_bytes[2..4].copy_from_slice(&length.to_be_bytes());
    segment_bytes
}

/// A marker followed by a segment length of 2, so that the marker code itself is what gets
/// refused.
fn empty_marker(code: u8) -> Vec<u8> {
    vec![0xFF, code, 0x00, 0x02]
}

/// The payload of a gray SOF0 header with the given size.
fn sof_payload_of(width: u16, height: u16) -> Vec<u8> {
    let mut spec = JpegSpec::gray(8, 8);
    spec.width = width;
    spec.height = height;
    sof_payload(&spec)
}

/// Sets the sampling factors of each component, in frame order.
fn set_sampling(spec: &mut JpegSpec, factors: &[(u8, u8)]) {
    for (component, &(h, v)) in spec.components.iter_mut().zip(factors) {
        component.h = h;
        component.v = v;
    }
}

/// Returns the RST codes, RST0 to RST7, of the restart markers in `data`, in order.
///
/// Stuffing makes every `0xFF` data byte be followed by `0x00`, so an `FF Dn` pair is a marker.
fn restart_codes(data: &[u8]) -> Vec<u8> {
    data.windows(2)
        .filter(|pair| pair[0] == 0xFF && (RST0..RST0 + 8).contains(&pair[1]))
        .map(|pair| pair[1])
        .collect()
}

/// The RST code of the `index`-th restart marker of a scan, which cycles through RST0 to RST7.
fn rst_code(index: usize) -> u8 {
    let cycle = u8::try_from(index % 8)
        .unwrap_or_else(|error| panic!("{index} modulo 8 must fit a byte: {error}"));
    RST0 + cycle
}

#[test]
fn a_panic_inside_the_decoder_is_refused_as_invalid_and_does_not_unwind() {
    // zune-jpeg can panic in `decode_into` on some input; a stand-in closure panics the same way.
    // Mutation check: calling the closure directly in `contain_decoder_panic` lets this unwind.
    let outcome = contain_decoder_panic(|| panic!("the decoder panicked"));
    assert_eq!(outcome, Err(AssetRejection::Invalid));
    // An ordinary result passes through unchanged.
    assert_eq!(contain_decoder_panic(|| Ok(())), Ok(()));
    assert_eq!(
        contain_decoder_panic(|| Err(AssetRejection::Invalid)),
        Err(AssetRejection::Invalid)
    );
}
