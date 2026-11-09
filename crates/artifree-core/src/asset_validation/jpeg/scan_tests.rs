//! Behavior tests for the entropy-coded scan walk, the Huffman tables and the bit reader.
//!
//! The first group runs the whole admission path on files that differ from a valid one by one
//! defect: a truncated scan, bytes after the scan, padding, markers and symbols. Every truncation
//! of the 16x16 4:4:4 file is tried at every byte offset. The second group calls `walk_scan`,
//! `HuffmanTable` and `BitReader` directly, so that each refusal can be traced to the function
//! that makes it.
//!
//! Each refused input has a control with the same header that differs only in the defect, so a
//! refusal cannot come from a header that a test built wrongly. Expected outcomes come from the
//! standard and from the rules in `entropy.rs` and `scan.rs`, not from the code under test.

use artifree_domain::assets::AssetRejection;
use zune_jpeg::zune_core::options::DecoderOptions;

use super::entropy::{BitReader, HuffmanTable};
use super::scan::{HuffmanTables, ScanPlan, walk_scan};
use super::{Reader, ScanInput, read_scan_input, validate_jpeg};
use crate::asset_validation::jpeg_fixtures::{
    AC_CHROMINANCE, AC_LUMINANCE, BitWriter, DC_CHROMINANCE, DC_LUMINANCE, HuffmanSpec, JpegParts,
    JpegSpec, Padding, Sampling, dht_payload, dqt_payload, encode, eoi, segment, sof_payload, soi,
    sos_payload, worst_case_jpeg, zune_decode,
};

/// Marker codes of the segments that the hand-built files need. The fixtures keep theirs private.
const DQT: u8 = 0xDB;
const SOF0: u8 = 0xC0;
const DHT: u8 = 0xC4;
const DRI: u8 = 0xDD;
const SOS: u8 = 0xDA;

/// The first restart marker. RST0 to RST7 are the codes `0xD0` to `0xD7`.
const RST0: u8 = 0xD0;

/// The end-of-block symbol. In the standard luminance AC table its code is `1010`.
const EOB: u8 = 0x00;

/// The zero-run symbol, which skips sixteen zero coefficients.
const ZRL: u8 = 0xF0;

/// A DC table that is the standard one plus category 12, which the standard table does not have.
/// The new symbol takes the first unused code of length 10, `1111111110`.
const DC_WITH_CATEGORY_12: HuffmanSpec = HuffmanSpec {
    counts: [0, 1, 5, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0],
    symbols: &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
};

/// A small incomplete AC table with the symbols that the standard table lacks and these tests
/// need: size 10 and size 11 with run 0, and run 3 with size 0.
const AC_SMALL: HuffmanSpec = HuffmanSpec {
    counts: [0, 2, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    symbols: &[0x00, 0xF0, 0x01, 0x0A, 0x0B, 0x30],
};

// ---- Files and streams -------------------------------------------------------

/// Encodes a 4:4:4 colour frame with the given padding and restart interval.
fn colour(width: u16, height: u16, padding: Padding, restart_interval: u16) -> JpegParts {
    let mut spec = JpegSpec::color(width, height, Sampling::Yuv444);
    spec.padding = padding;
    spec.restart_interval = restart_interval;
    encode(&spec)
}

/// Writes a file around `entropy`: SOI, one DQT for every quantization table the frame names, the
/// frame header, one DHT with `tables`, an optional DRI, the scan header, the entropy data, and EOI.
fn assemble(
    spec: &JpegSpec,
    tables: &[(u8, &HuffmanSpec)],
    restart_interval: Option<u16>,
    entropy: &[u8],
) -> JpegParts {
    let mut quant: Vec<u8> = spec
        .components
        .iter()
        .map(|component| component.tq)
        .collect();
    quant.sort_unstable();
    quant.dedup();
    let mut bytes = soi();
    bytes.extend(segment(DQT, &dqt_payload(&quant)));
    bytes.extend(segment(SOF0, &sof_payload(spec)));
    bytes.extend(segment(DHT, &dht_payload(tables)));
    if let Some(interval) = restart_interval {
        bytes.extend(segment(DRI, &interval.to_be_bytes()));
    }
    bytes.extend(segment(SOS, &sos_payload(&spec.components)));
    let start = bytes.len();
    bytes.extend_from_slice(entropy);
    let end = bytes.len();
    bytes.extend(eoi());
    JpegParts {
        bytes,
        entropy: start..end,
    }
}

/// A one-block 8x8 grayscale file whose scan uses `dc` and `ac` in table slot 0.
fn gray_parts(dc: &HuffmanSpec, ac: &HuffmanSpec, entropy: &[u8]) -> JpegParts {
    assemble(
        &JpegSpec::gray(8, 8),
        &[(0x00, dc), (0x10, ac)],
        None,
        entropy,
    )
}

/// The bytes of [`gray_parts`].
fn gray_file(dc: &HuffmanSpec, ac: &HuffmanSpec, entropy: &[u8]) -> Vec<u8> {
    gray_parts(dc, ac, entropy).bytes
}

/// Returns the entropy-coded bytes of `parts`.
fn entropy_of(parts: &JpegParts) -> &[u8] {
    &parts.bytes[parts.entropy.clone()]
}

/// Returns `parts` with its entropy data replaced by `data`, and EOI after it when `end_with_eoi`
/// is set.
fn replace_entropy(parts: &JpegParts, data: &[u8], end_with_eoi: bool) -> Vec<u8> {
    let mut bytes = parts.bytes[..parts.entropy.start].to_vec();
    bytes.extend_from_slice(data);
    if end_with_eoi {
        bytes.extend(eoi());
    }
    bytes
}

/// Returns each RST marker in `entropy` as `(offset of its FF byte, marker code)`. A data byte FF
/// is always followed by 00, so an FF followed by a code from D0 to D7 is a marker.
fn restart_markers(entropy: &[u8]) -> Vec<(usize, u8)> {
    entropy
        .windows(2)
        .enumerate()
        .filter_map(|(offset, pair)| match pair {
            [0xFF, code @ 0xD0..=0xD7] => Some((offset, *code)),
            _ => None,
        })
        .collect()
}

/// Returns the RST marker code for restart number `index`, which cycles modulo 8.
fn rst_code(index: usize) -> u8 {
    RST0 + u8::try_from(index % 8).unwrap_or(0)
}

/// Returns `entropy` with an FF byte inserted before each RST marker, so that each marker is
/// preceded by a fill byte.
fn with_fill_before_each_marker(entropy: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut start = 0;
    for (offset, _) in restart_markers(entropy) {
        out.extend_from_slice(&entropy[start..offset]);
        out.push(0xFF);
        start = offset;
    }
    out.extend_from_slice(&entropy[start..]);
    out
}

/// Returns the code of `symbol` in `spec` as its bits and their count. Codes of each length are
/// taken in symbol-list order, as in T.81 Annex C. This is a separate builder from the decoder's,
/// so that a test which uses it checks the decoder against the standard, not against itself.
fn code_of(spec: &HuffmanSpec, symbol: u8) -> (u32, u8) {
    let mut code: u32 = 0;
    let mut symbols = spec.symbols.iter();
    for (index, &count) in spec.counts.iter().enumerate() {
        let Ok(length) = u8::try_from(index + 1) else {
            panic!("a DHT has at most 16 code lengths");
        };
        for _ in 0..count {
            let Some(&assigned) = symbols.next() else {
                panic!("DHT counts name more codes than the table has symbols");
            };
            if assigned == symbol {
                return (code, length);
            }
            code += 1;
        }
        code <<= 1;
    }
    panic!("symbol {symbol:#04x} has no code in this table")
}

/// Writes the code of `symbol` in `spec` to `bits`.
fn put_symbol(bits: &mut BitWriter, spec: &HuffmanSpec, symbol: u8) {
    let (code, length) = code_of(spec, symbol);
    bits.put_bits(code, length);
}

/// Returns the entropy of one block of the one-block grayscale file: DC category 0 from the
/// standard DC table, then each AC step as `(symbol, extra bit count, extra bits)`. The bits are
/// padded with 1s.
fn block(ac: &HuffmanSpec, steps: &[(u8, u8, u32)]) -> Vec<u8> {
    let mut bits = BitWriter::new(Padding::Ones);
    put_symbol(&mut bits, &DC_LUMINANCE, 0);
    for &(symbol, extra_bits, extra) in steps {
        put_symbol(&mut bits, ac, symbol);
        bits.put_bits(extra, extra_bits);
    }
    bits.finish()
}

// ---- Decoding helpers --------------------------------------------------------

/// Builds the decoder table for `spec`. The standard tables are valid, so a refusal here is a bug
/// in the test.
fn table_of(spec: &HuffmanSpec) -> HuffmanTable {
    HuffmanTable::build(&spec.counts, spec.symbols)
        .unwrap_or_else(|error| panic!("the table must build, got {error:?}"))
}

/// The decoder tables of the one-block grayscale file, written out by hand.
fn gray_tables() -> HuffmanTables {
    HuffmanTables {
        dc: [Some(table_of(&DC_LUMINANCE)), None, None, None],
        ac: [Some(table_of(&AC_LUMINANCE)), None, None, None],
    }
}

/// The scan plan of the 16x16 4:4:4 file without restarts: four MCUs of three blocks each.
fn colour_plan() -> ScanPlan {
    ScanPlan {
        units: [
            (0, 0),
            (1, 1),
            (1, 1),
            (0, 0),
            (0, 0),
            (0, 0),
            (0, 0),
            (0, 0),
            (0, 0),
            (0, 0),
        ],
        unit_count: 3,
        mcu_count: 4,
        restart_interval: 0,
    }
}

/// Runs the production header parser on `bytes`. Returns the offset of the first entropy byte and
/// the scan input.
fn read_headers(bytes: &[u8]) -> Result<(usize, ScanInput), AssetRejection> {
    let mut reader = Reader::new(bytes);
    reader.take_bytes(2)?;
    let input = read_scan_input(&mut reader)?;
    Ok((reader.offset, input))
}

/// Runs the header parser and the scan walk, but not zune-jpeg. Returns the offset after the
/// scan's EOI.
fn walk_file(bytes: &[u8]) -> Result<usize, AssetRejection> {
    let (start, input) = read_headers(bytes)?;
    walk_scan(bytes, start, &input.plan, &input.tables)
}

// ---- Assertions --------------------------------------------------------------

/// Checks that the scan walk and the whole admission path accept `bytes`, and that the walk ends
/// at the last byte. `case` names the input in the failure message.
fn assert_accepted(case: &str, bytes: &[u8]) {
    assert_eq!(
        walk_file(bytes),
        Ok(bytes.len()),
        "{case}: the scan walk must accept the file and end at its last byte"
    );
    assert_eq!(
        validate_jpeg(bytes),
        Ok(()),
        "{case}: validate_jpeg must accept the file"
    );
}

/// Checks that the scan walk and the whole admission path refuse `bytes` as `Invalid`.
fn assert_refused(case: &str, bytes: &[u8]) {
    assert_eq!(
        walk_file(bytes),
        Err(AssetRejection::Invalid),
        "{case}: the scan walk must refuse the file"
    );
    assert_eq!(
        validate_jpeg(bytes),
        Err(AssetRejection::Invalid),
        "{case}: validate_jpeg must refuse the file"
    );
}

/// Checks that the whole admission path refuses `bytes`. Used for defects outside the scan, where
/// the walk itself ends correctly, such as bytes after the end of image.
fn assert_refused_by_validation(case: &str, bytes: &[u8]) {
    assert_eq!(
        validate_jpeg(bytes),
        Err(AssetRejection::Invalid),
        "{case}: validate_jpeg must refuse the file"
    );
}

/// Checks that the table builder accepts `counts` and `symbols`.
fn build_accepted(case: &str, counts: &[u8; 16], symbols: &[u8]) {
    assert_eq!(
        HuffmanTable::build(counts, symbols).map(|_| ()),
        Ok(()),
        "{case} must be accepted"
    );
}

/// Checks that the table builder refuses `counts` and `symbols` as `Invalid`.
fn build_refused(case: &str, counts: &[u8; 16], symbols: &[u8]) {
    assert_eq!(
        HuffmanTable::build(counts, symbols).map(|_| ()),
        Err(AssetRejection::Invalid),
        "{case} must be refused as Invalid"
    );
}

// ---- Truncation --------------------------------------------------------------

#[test]
fn refuses_every_entropy_cut_followed_by_eoi() {
    let parts = colour(16, 16, Padding::Ones, 0);
    let entropy = entropy_of(&parts);
    assert_eq!(
        entropy.len(),
        60,
        "the sweep below assumes 60 entropy bytes in the 16x16 4:4:4 file"
    );
    assert_accepted("the complete 16x16 4:4:4 file", &parts.bytes);
    for cut in 0..entropy.len() {
        let file = replace_entropy(&parts, &entropy[..cut], true);
        assert_refused(
            &format!("entropy cut to {cut} of {} bytes, then EOI", entropy.len()),
            &file,
        );
    }
}

#[test]
fn refuses_every_entropy_cut_without_eoi() {
    let parts = colour(16, 16, Padding::Ones, 0);
    let entropy = entropy_of(&parts);
    assert_accepted("the complete file", &parts.bytes);
    // The last cut is the whole scan with no EOI, which is a missing end marker.
    for cut in 0..=entropy.len() {
        let file = replace_entropy(&parts, &entropy[..cut], false);
        assert_refused(&format!("entropy cut to {cut} bytes, no EOI"), &file);
    }
}

#[test]
fn refuses_a_scan_that_ends_at_a_restart_boundary() {
    // With interval 1 every MCU after the first starts with a restart marker, so each marker sits
    // on an MCU boundary. The scan is cut on each side of each marker.
    let parts = colour(16, 16, Padding::Ones, 1);
    let entropy = entropy_of(&parts);
    let markers = restart_markers(entropy);
    assert_eq!(
        markers.len(),
        3,
        "four MCUs at interval 1 have three restart markers"
    );
    assert_accepted("the complete interval-1 file", &parts.bytes);
    for (index, &(offset, _)) in markers.iter().enumerate() {
        let mcu = index + 1;
        let before = replace_entropy(&parts, &entropy[..offset], true);
        assert_refused(
            &format!("scan ends before the marker that starts MCU {mcu}, then EOI"),
            &before,
        );
        let after = replace_entropy(&parts, &entropy[..offset + 2], true);
        assert_refused(
            &format!("scan ends after that marker, so MCU {mcu} is missing, then EOI"),
            &after,
        );
    }
}

#[test]
fn walk_scan_refuses_a_truncated_scan_that_zune_decodes_in_default_and_strict_modes() {
    let parts = colour(16, 16, Padding::Ones, 0);
    // The scan is cut to nothing: the SOS segment is followed directly by EOI.
    let truncated = replace_entropy(&parts, &[], true);

    // zune-jpeg checks the headers, tables and geometry. It does not check that the scan holds the
    // bytes it needs, so it decodes this file in both option sets.
    let default_refusal = zune_decode(&truncated, DecoderOptions::default()).err();
    assert!(
        default_refusal.is_none(),
        "zune (default options) refused the truncated file: {default_refusal:?}"
    );
    let strict_refusal =
        zune_decode(&truncated, DecoderOptions::default().set_strict_mode(true)).err();
    assert!(
        strict_refusal.is_none(),
        "zune (strict mode) refused the truncated file: {strict_refusal:?}"
    );

    // The walk alone, with the plan and tables that the header parser gives, refuses it.
    let Ok((start, input)) = read_headers(&truncated) else {
        panic!("the headers of the 16x16 file must parse");
    };
    assert_eq!(
        input.plan,
        colour_plan(),
        "the header parser must give the plan that this test writes out by hand"
    );
    assert_eq!(
        walk_scan(&truncated, start, &input.plan, &input.tables),
        Err(AssetRejection::Invalid),
        "the scan walk must refuse a scan with no data, which zune accepts"
    );
    assert_eq!(
        validate_jpeg(&truncated),
        Err(AssetRejection::Invalid),
        "validate_jpeg must refuse the truncated file"
    );
}

// ---- Data around and inside the scan -----------------------------------------

#[test]
fn refuses_data_bytes_between_the_last_mcu_and_eoi() {
    let parts = colour(16, 16, Padding::Ones, 0);
    let entropy = entropy_of(&parts);
    assert_accepted("the complete file", &parts.bytes);
    let extras: [(&str, &[u8]); 4] = [
        ("one zero byte", &[0x00]),
        ("one data byte 0x80", &[0x80]),
        ("a stuffed FF 00 pair", &[0xFF, 0x00]),
        ("two zero bytes", &[0x00, 0x00]),
    ];
    for (name, extra) in extras {
        let mut data = entropy.to_vec();
        data.extend_from_slice(extra);
        assert_refused(name, &replace_entropy(&parts, &data, true));
    }
}

#[test]
fn refuses_zero_padding_bits_at_the_end_of_the_scan() {
    // DC category 0 (00) and end of block (1010) are six bits, so two padding bits follow them.
    let padded = |padding: Padding| {
        let mut bits = BitWriter::new(padding);
        put_symbol(&mut bits, &DC_LUMINANCE, 0);
        put_symbol(&mut bits, &AC_LUMINANCE, EOB);
        bits.finish()
    };
    assert_eq!(padded(Padding::Ones), [0x2B], "00 1010 then padding 11");
    assert_eq!(padded(Padding::Zeros), [0x28], "00 1010 then padding 00");
    assert_accepted(
        "two padding bits of 1",
        &gray_file(&DC_LUMINANCE, &AC_LUMINANCE, &padded(Padding::Ones)),
    );
    assert_refused(
        "two padding bits of 0",
        &gray_file(&DC_LUMINANCE, &AC_LUMINANCE, &padded(Padding::Zeros)),
    );

    // The same check on the 16x16 file, whose last byte is partly padding.
    let ones = colour(16, 16, Padding::Ones, 0);
    let zeros = colour(16, 16, Padding::Zeros, 0);
    assert_ne!(
        ones.bytes, zeros.bytes,
        "the 16x16 file must end in a partial byte, or the padding check is not exercised"
    );
    assert_accepted("16x16 file with one-bit padding", &ones.bytes);
    assert_refused("16x16 file with zero-bit padding", &zeros.bytes);
}

#[test]
fn refuses_zero_padding_bits_before_a_restart_marker() {
    let ones = colour(16, 16, Padding::Ones, 1);
    let zeros = colour(16, 16, Padding::Zeros, 1);
    assert_ne!(
        ones.bytes, zeros.bytes,
        "the intervals must end in partial bytes, or the padding before RST is not exercised"
    );
    assert_accepted("interval 1 with one-bit padding", &ones.bytes);
    assert_refused("interval 1 with zero-bit padding", &zeros.bytes);
}

#[test]
fn refuses_a_marker_at_every_offset_of_the_entropy_data() {
    let parts = colour(16, 16, Padding::Ones, 0);
    let entropy = entropy_of(&parts);
    assert_accepted("the complete file", &parts.bytes);
    // Each inserted segment is a marker followed by the length 2, so it has an empty payload. The
    // marker is DHT (C4) or DQT (DB).
    for offset in 0..=entropy.len() {
        for marker in [0xC4_u8, 0xDB] {
            let mut data = entropy[..offset].to_vec();
            data.extend_from_slice(&[0xFF, marker, 0x00, 0x02]);
            data.extend_from_slice(&entropy[offset..]);
            assert_refused(
                &format!("{marker:#04x} segment inserted at entropy offset {offset}"),
                &replace_entropy(&parts, &data, true),
            );
        }
    }
}

#[test]
fn refuses_bytes_after_the_end_of_image_marker() {
    let parts = colour(16, 16, Padding::Ones, 0);
    assert_accepted("the complete file", &parts.bytes);
    let trailers: [(&str, &[u8]); 3] = [
        ("one zero byte", &[0x00]),
        ("a newline", b"\n"),
        ("a second EOI", &[0xFF, 0xD9]),
    ];
    for (name, trailer) in trailers {
        let mut file = parts.bytes.clone();
        file.extend_from_slice(trailer);
        // The walk stops at the file's own EOI, so the trailer is outside the scan. zune-jpeg
        // accepts every trailer tried here in both modes, so the length check after the walk is
        // the guard that refuses it.
        assert_eq!(
            walk_file(&file),
            Ok(parts.bytes.len()),
            "{name}: the scan must end right after its first EOI"
        );
        assert_refused_by_validation(name, &file);
    }
}

// ---- Stuffing and fill -------------------------------------------------------

#[test]
fn accepts_stuffed_ff_data_bytes_and_refuses_them_unstuffed() {
    // DC category 10 (code 11111110) with ten 1 bits of magnitude, then end of block (1010). The
    // magnitude makes the second byte 0xFF, which the writer stuffs with 0x00.
    let mut bits = BitWriter::new(Padding::Ones);
    put_symbol(&mut bits, &DC_LUMINANCE, 10);
    bits.put_bits(0x3FF, 10);
    put_symbol(&mut bits, &AC_LUMINANCE, EOB);
    let stuffed = bits.finish();
    assert_eq!(
        stuffed,
        [0xFE, 0xFF, 0x00, 0xEB],
        "the FF data byte must be followed by a stuffed 00"
    );
    assert_accepted(
        "stuffed FF in the scan",
        &gray_file(&DC_LUMINANCE, &AC_LUMINANCE, &stuffed),
    );
    // Without the stuffed zero, FF EB is a marker where the scan needs data.
    assert_refused(
        "FF without its stuffed 00",
        &gray_file(&DC_LUMINANCE, &AC_LUMINANCE, &[0xFE, 0xFF, 0xEB]),
    );
}

#[test]
fn accepts_fill_bytes_before_the_final_eoi() {
    let parts = colour(16, 16, Padding::Ones, 0);
    let entropy = entropy_of(&parts);
    for fill in 1..=3 {
        let mut data = entropy.to_vec();
        data.extend(std::iter::repeat_n(0xFF_u8, fill));
        assert_accepted(
            &format!("{fill} fill byte(s) before EOI"),
            &replace_entropy(&parts, &data, true),
        );
    }
}

#[test]
fn accepts_fill_bytes_before_each_restart_marker() {
    let parts = colour(16, 16, Padding::Ones, 1);
    let entropy = entropy_of(&parts);
    let data = with_fill_before_each_marker(entropy);
    assert_eq!(
        data.len(),
        entropy.len() + 3,
        "control: one fill byte before each of the three markers"
    );
    assert_accepted(
        "fill bytes before each RST marker",
        &replace_entropy(&parts, &data, true),
    );
}

// ---- Restart markers ---------------------------------------------------------

#[test]
fn refuses_a_restart_marker_out_of_sequence() {
    let parts = colour(16, 16, Padding::Ones, 1);
    let entropy = entropy_of(&parts);
    assert_accepted("the complete interval-1 file", &parts.bytes);
    let markers = restart_markers(entropy);
    for (index, &(offset, _)) in markers.iter().enumerate() {
        // Each marker is replaced by the code of the next one, and then by the code of the one
        // before it. The first marker replaced by RST1 is the case that the sequence check exists for.
        for wrong in [rst_code(index + 1), rst_code(index + 7)] {
            let mut data = entropy.to_vec();
            data[offset + 1] = wrong;
            assert_refused(
                &format!(
                    "RST{} where RST{} is expected",
                    wrong - RST0,
                    rst_code(index) - RST0
                ),
                &replace_entropy(&parts, &data, true),
            );
        }
    }
}

#[test]
fn refuses_a_missing_restart_marker() {
    let parts = colour(16, 16, Padding::Ones, 1);
    let entropy = entropy_of(&parts);
    assert_accepted("the complete interval-1 file", &parts.bytes);
    for (index, &(offset, _)) in restart_markers(entropy).iter().enumerate() {
        let mut data = entropy[..offset].to_vec();
        data.extend_from_slice(&entropy[offset + 2..]);
        assert_refused(
            &format!("RST{} removed", index % 8),
            &replace_entropy(&parts, &data, true),
        );
    }
}

#[test]
fn refuses_a_restart_marker_after_the_last_mcu() {
    let parts = colour(16, 16, Padding::Ones, 1);
    let entropy = entropy_of(&parts);
    assert_accepted("the complete interval-1 file", &parts.bytes);
    // Three markers come before MCUs 1 to 3, so the next one would come before MCU 4, which does
    // not exist.
    let mut data = entropy.to_vec();
    data.extend_from_slice(&[0xFF, rst_code(3)]);
    assert_refused(
        "RST3 after the last MCU",
        &replace_entropy(&parts, &data, true),
    );
}

#[test]
fn refuses_restart_markers_when_the_header_has_no_restart_interval() {
    let with_restarts = colour(16, 16, Padding::Ones, 1);
    assert_eq!(
        restart_markers(entropy_of(&with_restarts)).len(),
        3,
        "control: the interval-1 scan has three markers"
    );
    // The same scan with its DRI segment left out, so the header says that there is no interval.
    let parts = assemble(
        &JpegSpec::color(16, 16, Sampling::Yuv444),
        &[
            (0x00, &DC_LUMINANCE),
            (0x10, &AC_LUMINANCE),
            (0x01, &DC_CHROMINANCE),
            (0x11, &AC_CHROMINANCE),
        ],
        None,
        entropy_of(&with_restarts),
    );
    assert_accepted("the same scan with its DRI segment", &with_restarts.bytes);
    assert_refused("RST markers with no DRI segment", &parts.bytes);
}

#[test]
fn accepts_restart_markers_that_wrap_past_rst7() {
    // 32x32 4:4:4 has 16 MCUs, so interval 1 needs 15 markers: RST0 to RST7, then RST0 to RST6.
    // 64x64 4:4:4 has 64 MCUs, so interval 5 needs 12 markers: RST0 to RST7, then RST0 to RST3.
    let cases: [(&str, u16, u16, usize); 2] = [
        ("32x32 4:4:4, interval 1", 32, 1, 15),
        ("64x64 4:4:4, interval 5", 64, 5, 12),
    ];
    for (name, size, interval, expected) in cases {
        let parts = colour(size, size, Padding::Ones, interval);
        let codes: Vec<usize> = restart_markers(entropy_of(&parts))
            .iter()
            .map(|&(_, code)| usize::from(code - RST0))
            .collect();
        let cycle: Vec<usize> = (0..expected).map(|index| index % 8).collect();
        assert_eq!(codes, cycle, "{name}: restart markers must cycle modulo 8");
        assert_accepted(name, &parts.bytes);
    }
}

#[test]
fn accepts_a_restart_interval_larger_than_the_mcu_count() {
    // 16x16 4:4:4 has four MCUs, so no MCU after the first starts a restart interval.
    for interval in [5_u16, 100, u16::MAX] {
        let parts = colour(16, 16, Padding::Ones, interval);
        assert!(
            restart_markers(entropy_of(&parts)).is_empty(),
            "interval {interval}: no restart marker is expected"
        );
        assert_accepted(&format!("interval {interval} over four MCUs"), &parts.bytes);
    }
}

// ---- Symbols -----------------------------------------------------------------

#[test]
fn refuses_a_dc_category_above_eleven() {
    // Category 12 is refused before its twelve magnitude bits are skipped.
    let mut refused = BitWriter::new(Padding::Ones);
    put_symbol(&mut refused, &DC_WITH_CATEGORY_12, 12);
    refused.put_bits(0, 12);
    put_symbol(&mut refused, &AC_LUMINANCE, EOB);
    assert_refused(
        "DC category 12",
        &gray_file(&DC_WITH_CATEGORY_12, &AC_LUMINANCE, &refused.finish()),
    );

    // The same table with category 11. Its magnitude 1024 is the bit 1 followed by ten 0 bits.
    let mut accepted = BitWriter::new(Padding::Ones);
    put_symbol(&mut accepted, &DC_WITH_CATEGORY_12, 11);
    accepted.put_bits(0x400, 11);
    put_symbol(&mut accepted, &AC_LUMINANCE, EOB);
    assert_accepted(
        "DC category 11 with the same table",
        &gray_file(&DC_WITH_CATEGORY_12, &AC_LUMINANCE, &accepted.finish()),
    );
}

#[test]
fn refuses_an_ac_size_above_ten() {
    // Size 11 is refused before its eleven magnitude bits are skipped.
    let mut refused = BitWriter::new(Padding::Ones);
    put_symbol(&mut refused, &DC_LUMINANCE, 0);
    put_symbol(&mut refused, &AC_SMALL, 0x0B);
    refused.put_bits(0, 11);
    assert_refused(
        "AC run 0, size 11",
        &gray_file(&DC_LUMINANCE, &AC_SMALL, &refused.finish()),
    );

    // Size 10 with the bit 1 and nine 0 bits, which is magnitude 512, then end of block.
    let mut accepted = BitWriter::new(Padding::Ones);
    put_symbol(&mut accepted, &DC_LUMINANCE, 0);
    put_symbol(&mut accepted, &AC_SMALL, 0x0A);
    accepted.put_bits(0x200, 10);
    put_symbol(&mut accepted, &AC_SMALL, EOB);
    assert_accepted(
        "AC run 0, size 10",
        &gray_file(&DC_LUMINANCE, &AC_SMALL, &accepted.finish()),
    );
}

#[test]
fn refuses_an_ac_run_that_passes_index_63() {
    // Three ZRLs leave the next index at 49. A run of 15 then reaches index 64, so the walk refuses
    // it. A run of 14 reaches index 63 instead, and its coefficient ends the block with no
    // end-of-block symbol.
    let refused = block(
        &AC_LUMINANCE,
        &[(ZRL, 0, 0), (ZRL, 0, 0), (ZRL, 0, 0), (0xF1, 1, 1)],
    );
    assert_refused(
        "three ZRLs, then run 15 and size 1 (index 64)",
        &gray_file(&DC_LUMINANCE, &AC_LUMINANCE, &refused),
    );
    let accepted = block(
        &AC_LUMINANCE,
        &[(ZRL, 0, 0), (ZRL, 0, 0), (ZRL, 0, 0), (0xE1, 1, 1)],
    );
    assert_accepted(
        "three ZRLs, then run 14 and size 1 (index 63)",
        &gray_file(&DC_LUMINANCE, &AC_LUMINANCE, &accepted),
    );
}

#[test]
fn refuses_a_zrl_that_lands_on_index_64() {
    // Two ZRLs and a run of 14 leave the next index at 48. A ZRL from there reaches index 64. The
    // walk refuses that, where a decoder that stops at 64 would not read another symbol.
    let refused = block(
        &AC_LUMINANCE,
        &[(ZRL, 0, 0), (ZRL, 0, 0), (0xE1, 1, 1), (ZRL, 0, 0)],
    );
    assert_refused(
        "two ZRLs, run 14 and size 1 (index 48), then a ZRL to index 64",
        &gray_file(&DC_LUMINANCE, &AC_LUMINANCE, &refused),
    );
    let accepted = block(
        &AC_LUMINANCE,
        &[(ZRL, 0, 0), (ZRL, 0, 0), (0xE1, 1, 1), (EOB, 0, 0)],
    );
    assert_accepted(
        "two ZRLs, run 14 and size 1 (index 48), then end of block",
        &gray_file(&DC_LUMINANCE, &AC_LUMINANCE, &accepted),
    );
}

#[test]
fn refuses_an_ac_run_with_zero_size_other_than_eob_and_zrl() {
    // In the standard, only the end-of-block and ZRL symbols have size 0. Symbol 0x30 is run 3
    // with size 0, which no table defines as a symbol.
    assert_refused(
        "AC symbol 0x30 (run 3, size 0)",
        &gray_file(&DC_LUMINANCE, &AC_SMALL, &block(&AC_SMALL, &[(0x30, 0, 0)])),
    );
    assert_accepted(
        "AC end of block in the same table",
        &gray_file(&DC_LUMINANCE, &AC_SMALL, &block(&AC_SMALL, &[(EOB, 0, 0)])),
    );
}

#[test]
fn refuses_a_dc_bit_pattern_that_no_code_matches() {
    // Sixteen 1 bits match no DC code. The longest standard code is nine bits, and its all-ones
    // word is reserved, so the decoder reads sixteen bits and gives up.
    let mut bits = BitWriter::new(Padding::Ones);
    bits.put_bits(0xFFFF, 16);
    assert_refused(
        "sixteen 1 bits in the DC position",
        &gray_file(&DC_LUMINANCE, &AC_LUMINANCE, &bits.finish()),
    );
    assert_accepted(
        "control: DC code 00 and an end of block",
        &gray_file(
            &DC_LUMINANCE,
            &AC_LUMINANCE,
            &block(&AC_LUMINANCE, &[(EOB, 0, 0)]),
        ),
    );
}

#[test]
fn refuses_an_ac_bit_pattern_that_no_code_matches() {
    // After DC category 0, sixteen 1 bits match no code of AC_SMALL, whose longest code is six bits.
    let mut bits = BitWriter::new(Padding::Ones);
    put_symbol(&mut bits, &DC_LUMINANCE, 0);
    bits.put_bits(0xFFFF, 16);
    assert_refused(
        "sixteen 1 bits in the AC position",
        &gray_file(&DC_LUMINANCE, &AC_SMALL, &bits.finish()),
    );
    assert_accepted(
        "control: an end of block in AC_SMALL",
        &gray_file(&DC_LUMINANCE, &AC_SMALL, &block(&AC_SMALL, &[(EOB, 0, 0)])),
    );
}

// ---- Sweeps and the largest admitted file ------------------------------------

#[test]
fn byte_flips_in_the_entropy_data_never_panic_and_accepted_flips_decode_with_zune() {
    // A flip that keeps the length can still leave a legal Huffman sequence, because a different
    // symbol can sit where the old one was. No validator without pixel semantics can refuse that,
    // so for an accepted flip this test checks only the length and that zune-jpeg decodes the
    // file, in default and strict modes. A refused flip is checked for a panic and nothing more.
    let parts = colour(16, 16, Padding::Ones, 0);
    let original = parts.bytes.clone();
    let mut accepted = 0_usize;
    for index in parts.entropy.clone() {
        for flip in 1..=u8::MAX {
            let mut mutant = original.clone();
            mutant[index] ^= flip;
            let case = format!("byte {index} XOR {flip:#04x}");
            let Ok(result) = std::panic::catch_unwind(|| validate_jpeg(&mutant)) else {
                panic!("{case}: validate_jpeg panicked");
            };
            match result {
                Ok(()) => {
                    accepted += 1;
                    assert_eq!(
                        mutant.len(),
                        original.len(),
                        "{case}: an accepted flip changed the length"
                    );
                    let strict_refusal =
                        zune_decode(&mutant, DecoderOptions::default().set_strict_mode(true)).err();
                    assert!(
                        strict_refusal.is_none(),
                        "{case}: accepted, but zune (strict) refused it: {strict_refusal:?}"
                    );
                    let default_refusal = zune_decode(&mutant, DecoderOptions::default()).err();
                    assert!(
                        default_refusal.is_none(),
                        "{case}: accepted, but zune (default) refused it: {default_refusal:?}"
                    );
                }
                Err(AssetRejection::Invalid) => {}
                Err(other) => panic!("{case}: unexpected rejection {other:?}"),
            }
        }
    }
    assert!(
        accepted > 0,
        "no flip was accepted, so the zune check ran on nothing"
    );
}

#[test]
fn worst_case_jpeg_is_accepted() {
    // 4096x4096 4:4:4 with a restart after every MCU: 262,144 MCUs and 786,432 blocks, which is the
    // largest scan that the admission limits allow.
    assert_accepted("the worst-case 4096x4096 file", &worst_case_jpeg());
}

// ---- Each function on its own ------------------------------------------------

#[test]
fn huffman_build_refuses_more_codes_of_a_length_than_its_patterns() {
    // Five codes of length 2 need five 2-bit patterns, and there are four.
    build_refused(
        "five codes of length 2",
        &[0, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        &[0, 1, 2, 3, 4],
    );
    // After one code of length 1 (0), the 2-bit codes start at 10, so three of them do not fit in
    // the two patterns that remain.
    build_refused(
        "one code of length 1 and three of length 2",
        &[1, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        &[0, 1, 2, 3],
    );
}

#[test]
fn huffman_build_refuses_the_all_ones_code_of_length_one() {
    // Two codes of length 1 are 0 and 1, and 1 is the reserved all-ones word.
    build_refused(
        "two codes of length 1",
        &[2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        &[0, 1],
    );
    build_accepted(
        "one code of length 1",
        &[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        &[0],
    );
}

#[test]
fn huffman_build_refuses_codes_that_reach_the_all_ones_word_of_length_two() {
    // Four codes of length 2 are 00, 01, 10 and 11, and 11 is reserved.
    build_refused(
        "four codes of length 2",
        &[0, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        &[0, 1, 2, 3],
    );
    build_accepted(
        "three codes of length 2, which leave 11 unassigned",
        &[0, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        &[0, 1, 2],
    );
    // One code of length 1 and two of length 2 give 0, 10 and 11, and then a code of length 3
    // must start at 110 and 111, which is the reserved word again.
    build_refused(
        "codes of length 1, 2, 2 and 3 that reach 111",
        &[1, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        &[0, 1, 2, 3],
    );
    build_accepted(
        "codes of length 1, 2 and 3 that stop before 111",
        &[1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        &[0, 1, 2],
    );
}

#[test]
fn huffman_build_refuses_zero_or_more_than_256_symbols() {
    build_refused("no symbols", &[0; 16], &[]);
    let symbols: Vec<u8> = (0..257_usize)
        .map(|index| u8::try_from(index % 256).unwrap_or(0))
        .collect();
    // 257 codes of lengths 9 and 10 fit the code space, so only the symbol limit refuses this.
    let counts_257 = [0, 0, 0, 0, 0, 0, 0, 0, 128, 129, 0, 0, 0, 0, 0, 0];
    build_refused("257 symbols", &counts_257, &symbols);
    let counts_256 = [0, 0, 0, 0, 0, 0, 0, 0, 128, 128, 0, 0, 0, 0, 0, 0];
    build_accepted("256 symbols", &counts_256, &symbols[..256]);
}

#[test]
fn huffman_build_refuses_a_symbol_list_that_differs_from_the_counts() {
    let counts = [0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    build_refused("one symbol for two codes", &counts, &[0]);
    build_refused("three symbols for two codes", &counts, &[0, 1, 2]);
    build_accepted("two symbols for two codes", &counts, &[0, 1]);
}

#[test]
fn huffman_decode_returns_the_symbol_of_every_code() {
    for spec in [&DC_LUMINANCE, &DC_CHROMINANCE, &AC_SMALL, &AC_LUMINANCE] {
        let table = table_of(spec);
        for &symbol in spec.symbols {
            let (code, length) = code_of(spec, symbol);
            let mut bits = BitWriter::new(Padding::Zeros);
            bits.put_bits(code, length);
            let bytes = bits.finish();
            let mut reader = BitReader::new(&bytes, 0);
            assert_eq!(
                table.decode(&mut reader),
                Ok(symbol),
                "symbol {symbol:#04x}, whose code is {length} bits"
            );
        }
    }
}

#[test]
fn huffman_decode_refuses_input_that_ends_inside_a_code() {
    let table = table_of(&DC_LUMINANCE);
    // Eight 1 bits are not a code, and the input ends before a ninth bit could complete one.
    let bytes = [0xFF, 0x00];
    let mut reader = BitReader::new(&bytes, 0);
    assert_eq!(
        table.decode(&mut reader),
        Err(AssetRejection::Invalid),
        "eight 1 bits, then the end of the input"
    );
    let empty: [u8; 0] = [];
    let mut reader = BitReader::new(&empty, 0);
    assert_eq!(
        table.decode(&mut reader),
        Err(AssetRejection::Invalid),
        "no bits at all"
    );
}

#[test]
fn bit_reader_treats_ff_00_as_the_data_byte_ff() {
    let bytes = [0xFF, 0x00, 0x80];
    let mut reader = BitReader::new(&bytes, 0);
    assert_eq!(
        reader.read_bits(9),
        Ok(0x1FF),
        "FF 00 is the data byte FF, and the top bit of 0x80 is 1"
    );
}

#[test]
fn bit_reader_refuses_ff_followed_by_a_marker_while_a_bit_is_needed() {
    assert_eq!(
        BitReader::new(&[0xFF, 0xD9], 0).read_bits(1),
        Err(AssetRejection::Invalid),
        "FF D9 is a marker, not data"
    );
    assert_eq!(
        BitReader::new(&[0xFF, 0xFF, 0x00], 0).read_bits(1),
        Err(AssetRejection::Invalid),
        "FF FF is a fill byte, not data"
    );
    let bytes = [0xAB, 0xFF, 0xD0];
    let mut reader = BitReader::new(&bytes, 0);
    assert_eq!(
        reader.read_bits(8),
        Ok(0xAB),
        "control: the first byte is read"
    );
    assert_eq!(
        reader.read_bits(1),
        Err(AssetRejection::Invalid),
        "RST0 where a data bit is needed"
    );
}

#[test]
fn bit_reader_refuses_end_of_input_instead_of_zero_filling() {
    let bytes = [0xA5];
    let mut reader = BitReader::new(&bytes, 0);
    assert_eq!(
        reader.read_bits(8),
        Ok(0xA5),
        "control: the one byte is read"
    );
    assert_eq!(
        reader.read_bits(1),
        Err(AssetRejection::Invalid),
        "no bit is supplied past the end of the input"
    );
    let mut reader = BitReader::new(&bytes, 0);
    assert_eq!(
        reader.read_bits(9),
        Err(AssetRejection::Invalid),
        "nine bits from one byte: the ninth is refused, not read as 0"
    );
}

#[test]
fn bit_reader_refuses_more_than_sixteen_bits_per_read() {
    let bytes = [0x00; 4];
    let mut reader = BitReader::new(&bytes, 0);
    assert_eq!(
        reader.read_bits(17),
        Err(AssetRejection::Invalid),
        "17 bits in one read"
    );
    assert_eq!(reader.read_bits(16), Ok(0), "control: 16 bits are read");
}

#[test]
fn finish_interval_refuses_padding_bits_that_are_zero() {
    // The bits 101, then five padding bits that are 0, then the marker.
    let zero_padded = [0b1010_0000, 0xFF, 0xD9];
    let mut reader = BitReader::new(&zero_padded, 0);
    assert_eq!(reader.read_bits(3), Ok(0b101), "control: three data bits");
    assert_eq!(
        reader.finish_interval(),
        Err(AssetRejection::Invalid),
        "padding bits must be 1"
    );
    // The same data with 1 bits as padding is accepted. The marker is EOI, and the offset is
    // just after it.
    let one_padded = [0b1011_1111, 0xFF, 0xD9];
    let mut reader = BitReader::new(&one_padded, 0);
    assert_eq!(reader.read_bits(3), Ok(0b101), "control: three data bits");
    assert_eq!(
        reader.finish_interval(),
        Ok((0xD9, 3)),
        "control: 1-bit padding, then EOI"
    );
}

#[test]
fn finish_interval_refuses_a_data_byte_where_a_marker_belongs() {
    // A data byte 0x12 follows the interval, where its marker should be.
    let extra_data = [0xFF, 0x00, 0x12];
    let mut reader = BitReader::new(&extra_data, 0);
    assert_eq!(
        reader.read_bits(8),
        Ok(0xFF),
        "control: the stuffed data byte"
    );
    assert_eq!(
        reader.finish_interval(),
        Err(AssetRejection::Invalid),
        "0x12 is data, not a marker"
    );
    // The control has the marker there, and the offset after it.
    let with_marker = [0xFF, 0x00, 0xFF, 0xD9];
    let mut reader = BitReader::new(&with_marker, 0);
    assert_eq!(
        reader.read_bits(8),
        Ok(0xFF),
        "control: the stuffed data byte"
    );
    assert_eq!(
        reader.finish_interval(),
        Ok((0xD9, 4)),
        "control: EOI after the interval"
    );
}

#[test]
fn finish_interval_skips_fill_bytes_and_returns_the_marker() {
    // Seven padding 1 bits after one data bit, then two fill bytes, then RST0.
    let bytes = [0b0111_1111, 0xFF, 0xFF, 0xD0];
    let mut reader = BitReader::new(&bytes, 0);
    assert_eq!(reader.read_bits(1), Ok(0), "control: one data bit");
    assert_eq!(
        reader.finish_interval(),
        Ok((0xD0, 4)),
        "fill bytes are skipped, and the offset is after RST0"
    );
}

#[test]
fn finish_interval_refuses_marker_code_zero_and_end_of_input() {
    // FF FF 00 after the interval: the code after the fill bytes is the stuffed zero.
    let stuffed = [0xAB, 0xFF, 0xFF, 0x00];
    let mut reader = BitReader::new(&stuffed, 0);
    assert_eq!(reader.read_bits(8), Ok(0xAB), "control: the data byte");
    assert_eq!(
        reader.finish_interval(),
        Err(AssetRejection::Invalid),
        "marker code 00 is not a marker"
    );
    // The input ends right after the interval, so there is no marker at all.
    let ended = [0xAB];
    let mut reader = BitReader::new(&ended, 0);
    assert_eq!(reader.read_bits(8), Ok(0xAB), "control: the data byte");
    assert_eq!(
        reader.finish_interval(),
        Err(AssetRejection::Invalid),
        "the input ends before a marker"
    );
    let with_marker = [0xAB, 0xFF, 0xD9];
    let mut reader = BitReader::new(&with_marker, 0);
    assert_eq!(reader.read_bits(8), Ok(0xAB), "control: the data byte");
    assert_eq!(
        reader.finish_interval(),
        Ok((0xD9, 3)),
        "control: EOI right after the interval"
    );
}

/// The one-block grayscale file whose only block is DC category 0 followed by end of block.
fn one_block_gray() -> JpegParts {
    gray_parts(
        &DC_LUMINANCE,
        &AC_LUMINANCE,
        &block(&AC_LUMINANCE, &[(EOB, 0, 0)]),
    )
}

#[test]
fn walk_scan_returns_the_offset_just_after_eoi() {
    let parts = one_block_gray();
    let plan = ScanPlan {
        units: [(0, 0); 10],
        unit_count: 1,
        mcu_count: 1,
        restart_interval: 0,
    };
    assert_eq!(
        walk_scan(&parts.bytes, parts.entropy.start, &plan, &gray_tables()),
        Ok(parts.bytes.len()),
        "one block of the gray file: the walk must end after EOI"
    );
}

#[test]
fn walk_scan_refuses_a_plan_with_no_units_or_more_than_ten() {
    let parts = one_block_gray();
    let one_unit = ScanPlan {
        units: [(0, 0); 10],
        unit_count: 1,
        mcu_count: 1,
        restart_interval: 0,
    };
    assert_eq!(
        walk_scan(&parts.bytes, parts.entropy.start, &one_unit, &gray_tables()),
        Ok(parts.bytes.len()),
        "control: a plan with one unit is valid"
    );
    for unit_count in [0_usize, 11] {
        let plan = ScanPlan {
            units: [(0, 0); 10],
            unit_count,
            mcu_count: 1,
            restart_interval: 0,
        };
        assert_eq!(
            walk_scan(&parts.bytes, parts.entropy.start, &plan, &gray_tables()),
            Err(AssetRejection::Invalid),
            "a plan with {unit_count} units"
        );
    }
}

#[test]
fn walk_scan_refuses_a_unit_that_names_an_empty_table_slot() {
    let parts = one_block_gray();
    let plan = ScanPlan {
        units: [(0, 0); 10],
        unit_count: 1,
        mcu_count: 1,
        restart_interval: 0,
    };
    assert_eq!(
        walk_scan(
            &parts.bytes,
            parts.entropy.start,
            &plan,
            &HuffmanTables::default()
        ),
        Err(AssetRejection::Invalid),
        "the unit names DC slot 0 and AC slot 0, which are both empty"
    );
    assert_eq!(
        walk_scan(&parts.bytes, parts.entropy.start, &plan, &gray_tables()),
        Ok(parts.bytes.len()),
        "control: the same plan with the standard tables is accepted"
    );
}
