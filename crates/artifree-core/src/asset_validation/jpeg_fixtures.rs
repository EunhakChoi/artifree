//! Deterministic baseline JPEG writer for the admission tests.
//!
//! `encode` writes one 8-bit frame with a single scan that contains every
//! component. Coefficients go straight into Huffman symbols: there is no DCT,
//! no sample data, and no reconstruction. Each block takes its DC category and
//! its AC pattern from its index. Every pattern is valid under the entropy rules
//! the admission walker checks, so a test can change one field of an accepted
//! file to make a refused one.
//!
//! The Huffman tables are the standard Annex K tables, written out as constants.
//! Codes are assigned by this module's own canonical builder, not by the decoder
//! under test. Nothing is read from disk, and no third-party image is used.

use std::fmt::Display;
use std::ops::Range;

use zune_jpeg::errors::DecodeErrors;
use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::zune_core::options::DecoderOptions;
use zune_jpeg::{ImageInfo, JpegDecoder};

/// Marker byte of SOI, the first marker of every file.
const SOI: u8 = 0xD8;
/// Marker byte of EOI, the last marker of every file.
const EOI: u8 = 0xD9;
/// Marker byte of APP0, which carries the JFIF identifier.
const APP0: u8 = 0xE0;
/// Marker byte of DQT.
const DQT: u8 = 0xDB;
/// Marker byte of DHT.
const DHT: u8 = 0xC4;
/// Marker byte of DRI.
const DRI: u8 = 0xDD;
/// Marker byte of SOS.
const SOS: u8 = 0xDA;
/// Marker byte of RST0. RST markers run from here to RST7.
const RST0: u8 = 0xD0;

/// Value written for every entry of every quantization table.
const QUANT_VALUE: u8 = 16;

/// Number of DC categories that blocks cycle through, 0 to 11 inclusive.
const DC_CATEGORIES: usize = 12;

/// Payload of the JFIF APP0 segment: identifier, version 1.1, no density units,
/// 1:1 density, and no thumbnail.
const JFIF_PAYLOAD: [u8; 14] = [b'J', b'F', b'I', b'F', 0, 1, 1, 0, 0, 1, 0, 1, 0, 0];

/// A Huffman table as a DHT segment stores it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct HuffmanSpec {
    /// Number of codes of each length, for lengths 1 through 16.
    pub(crate) counts: [u8; 16],
    /// Symbols in code order. There must be one symbol per code.
    pub(crate) symbols: &'static [u8],
}

/// The standard luminance DC table, for categories 0 to 11.
pub(crate) const DC_LUMINANCE: HuffmanSpec = HuffmanSpec {
    counts: [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0],
    symbols: &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
};

/// The standard chrominance DC table, for categories 0 to 11.
pub(crate) const DC_CHROMINANCE: HuffmanSpec = HuffmanSpec {
    counts: [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0],
    symbols: &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
};

/// The standard luminance AC table, with 162 run/size symbols and the end-of-block and ZRL symbols.
pub(crate) const AC_LUMINANCE: HuffmanSpec = HuffmanSpec {
    counts: [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d],
    symbols: &[
        0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, //
        0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07, //
        0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, //
        0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52, 0xd1, 0xf0, //
        0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0a, 0x16, //
        0x17, 0x18, 0x19, 0x1a, 0x25, 0x26, 0x27, 0x28, //
        0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, //
        0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, //
        0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, //
        0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, //
        0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, //
        0x7a, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, //
        0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, //
        0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, //
        0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, //
        0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, //
        0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, //
        0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2, //
        0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, //
        0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, //
        0xf9, 0xfa,
    ],
};

/// The standard chrominance AC table, with 162 run/size symbols and the end-of-block and ZRL symbols.
pub(crate) const AC_CHROMINANCE: HuffmanSpec = HuffmanSpec {
    counts: [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77],
    symbols: &[
        0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, //
        0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71, //
        0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, //
        0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33, 0x52, 0xf0, //
        0x15, 0x62, 0x72, 0xd1, 0x0a, 0x16, 0x24, 0x34, //
        0xe1, 0x25, 0xf1, 0x17, 0x18, 0x19, 0x1a, 0x26, //
        0x27, 0x28, 0x29, 0x2a, 0x35, 0x36, 0x37, 0x38, //
        0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, //
        0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, //
        0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, //
        0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, //
        0x79, 0x7a, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, //
        0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, //
        0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, //
        0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, //
        0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, //
        0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, //
        0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, //
        0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, //
        0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, //
        0xf9, 0xfa,
    ],
};

/// One step of a block's AC coefficient sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AcStep {
    /// Sixteen zero coefficients, written as symbol `0xF0`.
    Zrl,
    /// `run` zero coefficients, then one non-zero coefficient of `size` magnitude bits.
    Coefficient { run: u8, size: u8 },
    /// The rest of the block is zero, written as symbol `0x00`.
    EndOfBlock,
}

/// The AC sequences that blocks cycle through, chosen by block index modulo five.
///
/// Each one either ends with an end-of-block before index 63, or reaches index 63
/// with its last coefficient and writes no end-of-block. Run fields stay at most 15.
const AC_PATTERNS: [&[AcStep]; 5] = [
    &[AcStep::EndOfBlock],
    &[
        AcStep::Zrl,
        AcStep::Coefficient { run: 0, size: 1 },
        AcStep::EndOfBlock,
    ],
    &[
        AcStep::Zrl,
        AcStep::Zrl,
        AcStep::Zrl,
        AcStep::Coefficient { run: 14, size: 1 },
    ],
    &[
        AcStep::Zrl,
        AcStep::Coefficient { run: 13, size: 10 },
        AcStep::EndOfBlock,
    ],
    &[
        AcStep::Coefficient { run: 15, size: 2 },
        AcStep::Coefficient { run: 0, size: 10 },
        AcStep::EndOfBlock,
    ],
];

/// One component of a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ComponentSpec {
    /// Component identifier, written as given.
    pub(crate) id: u8,
    /// Horizontal sampling factor. Only its low four bits are written.
    pub(crate) h: u8,
    /// Vertical sampling factor. Only its low four bits are written.
    pub(crate) v: u8,
    /// Quantization table identifier, written as given.
    pub(crate) tq: u8,
}

/// Chroma layout of a three-component frame. Chroma components are always 1x1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sampling {
    /// No subsampling: luminance is 1x1.
    Yuv444,
    /// Luminance is 2x1, so chroma is halved horizontally.
    Yuv422,
    /// Luminance is 2x2, so chroma is halved in both directions.
    Yuv420,
}

/// Which bit fills the unused bits of a partial byte before a marker or at the end of the scan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Padding {
    /// Fill with 1 bits, as the standard requires.
    Ones,
    /// Fill with 0 bits. A valid decoder reads this fine, but the admission walker must refuse it.
    Zeros,
}

/// Everything `encode` writes. Fields are written as given, so a test can build an illegal header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct JpegSpec {
    /// Image width in pixels.
    pub(crate) width: u16,
    /// Image height in pixels.
    pub(crate) height: u16,
    /// Frame marker byte: `0xC0` for SOF0 or `0xC1` for SOF1. Other values are written too.
    pub(crate) sof: u8,
    /// Sample precision byte. A valid file has 8.
    pub(crate) precision: u8,
    /// Components, in frame order, which is also scan order.
    pub(crate) components: Vec<ComponentSpec>,
    /// Restart interval in MCUs. Zero writes no DRI segment and no RST markers.
    pub(crate) restart_interval: u16,
    /// Whether to write a JFIF APP0 segment before the tables.
    pub(crate) jfif: bool,
    /// Fill value for the unused bits before each marker and at the end of the scan.
    pub(crate) padding: Padding,
}

impl JpegSpec {
    /// Returns a baseline frame with one 1x1 component, no JFIF segment, and no restarts.
    pub(crate) fn gray(width: u16, height: u16) -> Self {
        Self::with_components(
            width,
            height,
            vec![ComponentSpec {
                id: 1,
                h: 1,
                v: 1,
                tq: 0,
            }],
        )
    }

    /// Returns a three-component YCbCr frame with ids 1, 2 and 3 and the given luminance sampling.
    pub(crate) fn color(width: u16, height: u16, sampling: Sampling) -> Self {
        let (h, v) = match sampling {
            Sampling::Yuv444 => (1, 1),
            Sampling::Yuv422 => (2, 1),
            Sampling::Yuv420 => (2, 2),
        };
        Self::with_components(
            width,
            height,
            vec![
                ComponentSpec { id: 1, h, v, tq: 0 },
                ComponentSpec {
                    id: 2,
                    h: 1,
                    v: 1,
                    tq: 1,
                },
                ComponentSpec {
                    id: 3,
                    h: 1,
                    v: 1,
                    tq: 1,
                },
            ],
        )
    }

    /// Returns a four-component frame with ids 1 to 4 and 1x1 sampling for every component.
    ///
    /// No Adobe APP14 segment is written, so a decoder must infer the color model.
    pub(crate) fn cmyk(width: u16, height: u16) -> Self {
        let component = |id: u8, tq: u8| ComponentSpec { id, h: 1, v: 1, tq };
        Self::with_components(
            width,
            height,
            vec![
                component(1, 0),
                component(2, 1),
                component(3, 1),
                component(4, 1),
            ],
        )
    }

    /// Returns a baseline frame with the given components and every optional segment turned off.
    fn with_components(width: u16, height: u16, components: Vec<ComponentSpec>) -> Self {
        Self {
            width,
            height,
            sof: 0xC0,
            precision: 8,
            components,
            restart_interval: 0,
            jfif: false,
            padding: Padding::Ones,
        }
    }
}

impl Default for JpegSpec {
    /// Returns the 8x8 grayscale frame that `minimal_jpeg` encodes.
    fn default() -> Self {
        Self::gray(8, 8)
    }
}

/// An encoded file and the byte range of its entropy-coded data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct JpegParts {
    /// The whole file, from SOI through EOI.
    pub(crate) bytes: Vec<u8>,
    /// The entropy-coded data, including its RST markers and final padding.
    ///
    /// The range starts right after the SOS segment and ends right before EOI.
    pub(crate) entropy: Range<usize>,
}

/// Encodes `spec` as a complete file.
///
/// The segments come out in this order: SOI, optional JFIF APP0, DQT, the frame
/// header, one DHT with the tables the scan needs, optional DRI, SOS, entropy
/// data, and EOI.
///
/// # Panics
///
/// Panics if there are more than 255 components, or if a segment would be too long.
pub(crate) fn encode(spec: &JpegSpec) -> JpegParts {
    let mut bytes = soi();
    if spec.jfif {
        bytes.extend(segment(APP0, &JFIF_PAYLOAD));
    }
    let mut quant_tables: Vec<u8> = spec
        .components
        .iter()
        .map(|component| component.tq)
        .collect();
    quant_tables.sort_unstable();
    quant_tables.dedup();
    bytes.extend(segment(DQT, &dqt_payload(&quant_tables)));
    bytes.extend(segment(spec.sof, &sof_payload(spec)));
    let mut huffman: Vec<(u8, &HuffmanSpec)> = vec![(0x00, &DC_LUMINANCE), (0x10, &AC_LUMINANCE)];
    if spec.components.len() > 1 {
        huffman.push((0x01, &DC_CHROMINANCE));
        huffman.push((0x11, &AC_CHROMINANCE));
    }
    bytes.extend(segment(DHT, &dht_payload(&huffman)));
    if spec.restart_interval > 0 {
        bytes.extend(segment(DRI, &spec.restart_interval.to_be_bytes()));
    }
    bytes.extend(segment(SOS, &sos_payload(&spec.components)));
    let start = bytes.len();
    bytes.extend(scan_data(spec));
    let end = bytes.len();
    bytes.extend(eoi());
    JpegParts {
        bytes,
        entropy: start..end,
    }
}

/// Encodes one marker segment: `FF`, the marker byte, a big-endian length that counts itself, and `payload`.
///
/// # Panics
///
/// Panics if `payload` is longer than 65,533 bytes.
pub(crate) fn segment(marker: u8, payload: &[u8]) -> Vec<u8> {
    let length: u16 = fit(payload.len() + 2);
    let mut out = vec![0xFF, marker];
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// Returns the start-of-image marker, `FF D8`.
pub(crate) fn soi() -> Vec<u8> {
    vec![0xFF, SOI]
}

/// Returns the end-of-image marker, `FF D9`.
pub(crate) fn eoi() -> Vec<u8> {
    vec![0xFF, EOI]
}

/// Returns the smallest file the tests use: the 8x8 grayscale frame of `JpegSpec::default`.
pub(crate) fn minimal_jpeg() -> Vec<u8> {
    encode(&JpegSpec::default()).bytes
}

/// Returns the largest frame the admission limits allow in practice: 4096x4096, three
/// components at 4:4:4, and a restart after every MCU.
///
/// The file is about 4.85 MiB, just under the 5 MiB upload limit. Its size depends on the
/// AC pattern cycle: pattern 3 has one ZRL, where three would push the file over the limit.
pub(crate) fn worst_case_jpeg() -> Vec<u8> {
    let mut spec = JpegSpec::color(4096, 4096, Sampling::Yuv444);
    spec.restart_interval = 1;
    spec.jfif = true;
    encode(&spec).bytes
}

/// Decodes `bytes` with zune-jpeg: the headers first, then the pixels.
///
/// Returns the image info and the number of decoded bytes. zune-jpeg checks the headers,
/// tables and geometry. It does not check that the scan uses exactly the bytes present,
/// so a passing decode does not prove the entropy length.
///
/// # Errors
///
/// Returns zune's error if it refuses the file.
///
/// # Panics
///
/// Panics if zune reports decoded headers but no image info.
pub(crate) fn zune_decode(
    bytes: &[u8],
    options: DecoderOptions,
) -> Result<(ImageInfo, usize), DecodeErrors> {
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    decoder.decode_headers()?;
    let Some(info) = decoder.info() else {
        panic!("zune decoded the headers but reports no image info");
    };
    let pixels = decoder.decode()?;
    Ok((info, pixels.len()))
}

/// Packs bits, most significant first, into entropy-coded data.
///
/// A `0xFF` data byte is followed by a stuffed `0x00`. Restart markers are written
/// raw, after the partial byte is padded.
#[derive(Debug)]
pub(crate) struct BitWriter {
    out: Vec<u8>,
    partial: u8,
    filled: u8,
    padding: Padding,
}

impl BitWriter {
    /// Returns an empty writer that pads partial bytes with `padding`.
    pub(crate) fn new(padding: Padding) -> Self {
        Self {
            out: Vec::new(),
            partial: 0,
            filled: 0,
            padding,
        }
    }

    /// Writes the low `count` bits of `value`, most significant first.
    ///
    /// # Panics
    ///
    /// Panics if `count` is greater than 32.
    pub(crate) fn put_bits(&mut self, value: u32, count: u8) {
        assert!(count <= 32, "cannot write {count} bits at once");
        for shift in (0..count).rev() {
            self.push_bit(((value >> shift) & 1) == 1);
        }
    }

    /// Pads the partial byte, then writes the restart marker `RSTn`, where `n` is `index` modulo 8.
    ///
    /// `index` counts the restart markers already written in the scan, starting at 0.
    pub(crate) fn restart(&mut self, index: usize) {
        self.pad();
        let marker = RST0 | fit::<usize, u8>(index % 8);
        self.out.extend_from_slice(&[0xFF, marker]);
    }

    /// Pads the partial byte, then returns the entropy-coded bytes.
    #[must_use]
    pub(crate) fn finish(mut self) -> Vec<u8> {
        self.pad();
        self.out
    }

    /// Appends one bit, emitting a byte once eight bits have been collected.
    fn push_bit(&mut self, bit: bool) {
        self.partial = (self.partial << 1) | u8::from(bit);
        self.filled += 1;
        if self.filled == 8 {
            let byte = self.partial;
            self.push_byte(byte);
            self.partial = 0;
            self.filled = 0;
        }
    }

    /// Appends one entropy-coded byte, stuffing a zero after `0xFF`.
    fn push_byte(&mut self, byte: u8) {
        self.out.push(byte);
        if byte == 0xFF {
            self.out.push(0x00);
        }
    }

    /// Fills the unused bits of the partial byte with the padding bit, if there is one.
    fn pad(&mut self) {
        while self.filled != 0 {
            self.push_bit(self.padding == Padding::Ones);
        }
    }
}

/// One Huffman code: the low `len` bits of `bits`, most significant first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Code {
    bits: u32,
    len: u8,
}

/// The codes of one table, indexed by symbol.
#[derive(Debug)]
struct CodeTable {
    codes: [Option<Code>; 256],
}

impl CodeTable {
    /// Assigns canonical codes as JPEG Annex C does.
    ///
    /// Codes of each length are consecutive, taken in symbol-list order. Moving to
    /// the next length shifts the next free code left by one bit.
    ///
    /// # Panics
    ///
    /// Panics if the counts and the symbols disagree, or if the counts oversubscribe
    /// the code space. The tables are constants here, so either is a typo.
    fn new(table: &HuffmanSpec) -> Self {
        let mut codes: [Option<Code>; 256] = [None; 256];
        let mut symbols = table.symbols.iter();
        let mut code: u32 = 0;
        for (index, &count) in table.counts.iter().enumerate() {
            let len: u8 = fit(index + 1);
            for _ in 0..count {
                let Some(&symbol) = symbols.next() else {
                    panic!("DHT counts name more codes than the table has symbols");
                };
                codes[usize::from(symbol)] = Some(Code { bits: code, len });
                code += 1;
            }
            assert!(
                code <= 1 << len,
                "codes of length {len} oversubscribe the table"
            );
            code <<= 1;
        }
        assert!(
            symbols.next().is_none(),
            "the table has more symbols than its counts"
        );
        Self { codes }
    }

    /// Returns the code for `symbol`.
    ///
    /// # Panics
    ///
    /// Panics if the table has no code for `symbol`.
    fn code(&self, symbol: u8) -> Code {
        self.codes[usize::from(symbol)]
            .unwrap_or_else(|| panic!("symbol {symbol:#04x} has no code in this table"))
    }
}

/// The DC and AC code tables for one class of components.
#[derive(Debug)]
struct TablePair {
    dc: CodeTable,
    ac: CodeTable,
}

/// Returns the code tables for pair 0 (luminance) and pair 1 (chrominance).
fn table_pairs() -> [TablePair; 2] {
    [
        TablePair {
            dc: CodeTable::new(&DC_LUMINANCE),
            ac: CodeTable::new(&AC_LUMINANCE),
        },
        TablePair {
            dc: CodeTable::new(&DC_CHROMINANCE),
            ac: CodeTable::new(&AC_CHROMINANCE),
        },
    ]
}

/// Converts `value` to `T`, panicking if it does not fit.
///
/// # Panics
///
/// Panics if `value` is out of range for `T`.
fn fit<S, T>(value: S) -> T
where
    S: Copy + Display,
    T: TryFrom<S>,
    T::Error: Display,
{
    T::try_from(value)
        .unwrap_or_else(|error| panic!("fixture value {value} does not fit its field: {error}"))
}

/// Encodes a frame header payload: precision, height, width, component count, then each component.
pub(crate) fn sof_payload(spec: &JpegSpec) -> Vec<u8> {
    let count: u8 = fit(spec.components.len());
    let mut payload = vec![spec.precision];
    payload.extend_from_slice(&spec.height.to_be_bytes());
    payload.extend_from_slice(&spec.width.to_be_bytes());
    payload.push(count);
    for component in &spec.components {
        payload.push(component.id);
        payload.push(((component.h & 0x0F) << 4) | (component.v & 0x0F));
        payload.push(component.tq);
    }
    payload
}

/// Encodes 8-bit quantization tables with the given identifiers, every entry `QUANT_VALUE`.
///
/// Only the low four bits of each identifier are written.
pub(crate) fn dqt_payload(tables: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(tables.len() * 65);
    for &table in tables {
        payload.push(table & 0x0F);
        payload.extend_from_slice(&[QUANT_VALUE; 64]);
    }
    payload
}

/// Encodes Huffman tables. Each entry is a class and identifier byte (`Tc << 4 | Th`),
/// 16 code counts, then the symbols.
pub(crate) fn dht_payload(tables: &[(u8, &HuffmanSpec)]) -> Vec<u8> {
    let mut payload = Vec::new();
    for &(class_and_id, table) in tables {
        payload.push(class_and_id);
        payload.extend_from_slice(&table.counts);
        payload.extend_from_slice(table.symbols);
    }
    payload
}

/// Encodes the scan header for every component in order.
///
/// Component 0 uses table pair 0 and the others use pair 1. The spectral range is
/// 0 to 63 with no successive approximation, so the scan covers the whole block.
pub(crate) fn sos_payload(components: &[ComponentSpec]) -> Vec<u8> {
    let count: u8 = fit(components.len());
    let mut payload = vec![count];
    for (index, component) in components.iter().enumerate() {
        let pair = u8::from(index > 0);
        payload.push(component.id);
        payload.push((pair << 4) | pair);
    }
    payload.extend_from_slice(&[0, 63, 0]);
    payload
}

/// Returns `(component index, blocks per MCU)` in scan order.
///
/// A single-component scan is not interleaved, so each MCU is one block whatever the
/// sampling factors are.
fn mcu_layout(spec: &JpegSpec) -> Vec<(usize, usize)> {
    if spec.components.len() == 1 {
        return vec![(0, 1)];
    }
    spec.components
        .iter()
        .enumerate()
        .map(|(index, component)| {
            (
                index,
                usize::from(component.h.max(1)) * usize::from(component.v.max(1)),
            )
        })
        .collect()
}

/// Returns the number of MCUs in the scan.
///
/// A single-component scan has one MCU per 8x8 block. Otherwise the MCU is
/// `8 * Hmax` by `8 * Vmax` pixels. Sampling factors below 1 count as 1 here.
fn mcu_count(spec: &JpegSpec) -> usize {
    let width = usize::from(spec.width);
    let height = usize::from(spec.height);
    if spec.components.len() == 1 {
        return width.div_ceil(8) * height.div_ceil(8);
    }
    let h_max = spec
        .components
        .iter()
        .map(|component| usize::from(component.h.max(1)))
        .max()
        .unwrap_or(1);
    let v_max = spec
        .components
        .iter()
        .map(|component| usize::from(component.v.max(1)))
        .max()
        .unwrap_or(1);
    width.div_ceil(8 * h_max) * height.div_ceil(8 * v_max)
}

/// Writes the entropy-coded data of the scan.
///
/// Every MCU is written in order. Before each MCU that starts a new restart
/// interval, the writer emits an RST marker and resets the DC predictors.
fn scan_data(spec: &JpegSpec) -> Vec<u8> {
    let pairs = table_pairs();
    let layout = mcu_layout(spec);
    let interval = usize::from(spec.restart_interval);
    let mut bits = BitWriter::new(spec.padding);
    let mut predictors = vec![0_i32; spec.components.len()];
    let mut block = 0_usize;
    let mut restarts = 0_usize;
    for mcu in 0..mcu_count(spec) {
        if interval > 0 && mcu > 0 && mcu.is_multiple_of(interval) {
            bits.restart(restarts);
            restarts += 1;
            predictors.fill(0);
        }
        for &(component, blocks) in &layout {
            let pair = &pairs[usize::from(component > 0)];
            for _ in 0..blocks {
                write_block(&mut bits, pair, &mut predictors[component], block);
                block += 1;
            }
        }
    }
    bits.finish()
}

/// Writes one block: its DC category and difference, then its AC steps.
fn write_block(bits: &mut BitWriter, pair: &TablePair, predictor: &mut i32, block: usize) {
    let category: u8 = fit(block % DC_CATEGORIES);
    let difference = dc_difference(category, block, *predictor);
    *predictor += difference;
    write_symbol(bits, &pair.dc, category);
    put_value(bits, difference, category);
    for &step in AC_PATTERNS[block % AC_PATTERNS.len()] {
        match step {
            AcStep::Zrl => write_symbol(bits, &pair.ac, 0xF0),
            AcStep::Coefficient { run, size } => {
                write_symbol(bits, &pair.ac, (run << 4) | size);
                put_value(bits, coefficient(size, block), size);
            }
            AcStep::EndOfBlock => write_symbol(bits, &pair.ac, 0x00),
        }
    }
}

/// Returns a magnitude that has exactly `size` bits, varied by `block` so that the
/// extra bits change from block to block. Size 0 gives 0.
fn magnitude(size: u8, block: usize) -> i32 {
    if size == 0 {
        return 0;
    }
    let low = 1_usize << (size - 1);
    fit(low | (block % low))
}

/// Returns a DC difference of `category` size. Its sign opposes `predictor`, so the
/// predictor stays within the category range instead of drifting.
fn dc_difference(category: u8, block: usize, predictor: i32) -> i32 {
    let value = magnitude(category, block);
    if predictor > 0 { -value } else { value }
}

/// Returns an AC coefficient of `size` magnitude bits. Its sign alternates by block.
fn coefficient(size: u8, block: usize) -> i32 {
    let value = magnitude(size, block);
    if block.is_multiple_of(2) {
        value
    } else {
        -value
    }
}

/// Writes the code for `symbol` from `table`.
fn write_symbol(bits: &mut BitWriter, table: &CodeTable, symbol: u8) {
    let code = table.code(symbol);
    bits.put_bits(code.bits, code.len);
}

/// Writes the additional bits for `value` in a category of `size` bits.
///
/// A positive value is written as it is. A negative value is written as the low
/// `size` bits of `value - 1`, which is its one's complement.
///
/// # Panics
///
/// Panics if `value` does not have exactly `size` magnitude bits, or if `size` is 0 and
/// `value` is not 0.
fn put_value(bits: &mut BitWriter, value: i32, size: u8) {
    if size == 0 {
        assert!(value == 0, "a category-0 value must be 0, got {value}");
        return;
    }
    let magnitude = value.unsigned_abs();
    assert!(
        magnitude >= 1 << (size - 1) && magnitude < 1 << size,
        "value {value} does not have {size} magnitude bits"
    );
    let raw = if value >= 0 {
        value
    } else {
        value + (1 << size) - 1
    };
    bits.put_bits(fit(raw), size);
}

#[cfg(test)]
mod tests {
    use artifree_domain::assets::MAX_ASSET_BYTES;
    use zune_jpeg::zune_core::options::DecoderOptions;

    use super::{
        AC_CHROMINANCE, AC_LUMINANCE, AC_PATTERNS, AcStep, BitWriter, Code, CodeTable,
        DC_CATEGORIES, DC_CHROMINANCE, DC_LUMINANCE, EOI, JpegSpec, Padding, RST0, SOI, Sampling,
        coefficient, dc_difference, encode, eoi, fit, minimal_jpeg, put_value, soi,
        worst_case_jpeg, zune_decode,
    };

    /// Returns the shapes that the accept tests use, each with a name for failure messages.
    fn accepted_shapes() -> Vec<(&'static str, JpegSpec)> {
        let mut gray_2x2 = JpegSpec::gray(16, 16);
        gray_2x2.components[0].h = 2;
        gray_2x2.components[0].v = 2;

        let mut gray_sof1 = JpegSpec::gray(8, 8);
        gray_sof1.sof = 0xC1;

        let mut color_sof1 = JpegSpec::color(17, 9, Sampling::Yuv420);
        color_sof1.sof = 0xC1;

        let mut color_with_jfif = JpegSpec::color(16, 16, Sampling::Yuv444);
        color_with_jfif.jfif = true;

        let mut restart_one = JpegSpec::color(32, 16, Sampling::Yuv444);
        restart_one.restart_interval = 1;

        let mut restart_three = JpegSpec::color(32, 16, Sampling::Yuv444);
        restart_three.restart_interval = 3;

        vec![
            ("gray 8x8", JpegSpec::gray(8, 8)),
            ("gray 7x9", JpegSpec::gray(7, 9)),
            ("gray 16x16 with 2x2 sampling", gray_2x2),
            ("gray 8x8 SOF1", gray_sof1),
            ("4:4:4 16x16 with JFIF", color_with_jfif),
            ("4:2:0 17x9", JpegSpec::color(17, 9, Sampling::Yuv420)),
            ("4:2:0 17x9 SOF1", color_sof1),
            ("4:2:2 16x8", JpegSpec::color(16, 8, Sampling::Yuv422)),
            ("CMYK 8x8", JpegSpec::cmyk(8, 8)),
            ("4:4:4 32x16 restart 1", restart_one),
            ("4:4:4 32x16 restart 3", restart_three),
        ]
    }

    /// Returns the RST markers that appear in `data`, in order.
    ///
    /// Stuffing makes every `0xFF` in entropy data followed by `0x00`, so `FF Dn`
    /// pairs are always markers.
    fn restart_markers(data: &[u8]) -> Vec<u8> {
        data.windows(2)
            .filter(|pair| pair[0] == 0xFF && (RST0..RST0 + 8).contains(&pair[1]))
            .map(|pair| pair[1])
            .collect()
    }

    /// zune-jpeg must read the headers, tables and frame geometry of every shape, in both
    /// option sets. It does not check that the scan uses exactly the bytes present: strict
    /// mode accepted a scan with its entropy data removed. Exact entropy length is the walker's job.
    #[test]
    fn zune_decodes_every_accepted_shape_in_default_and_strict_modes() {
        for (name, spec) in accepted_shapes() {
            let parts = encode(&spec);
            let modes = [
                ("default", DecoderOptions::default()),
                ("strict", DecoderOptions::default().set_strict_mode(true)),
            ];
            for (mode, options) in modes {
                let (info, decoded) = zune_decode(&parts.bytes, options).unwrap_or_else(|error| {
                    panic!("{name} ({mode}): zune refused the fixture: {error:?}")
                });
                assert_eq!(
                    (info.width, info.height, usize::from(info.components)),
                    (spec.width, spec.height, spec.components.len()),
                    "{name} ({mode}): zune read a different frame"
                );
                assert!(decoded > 0, "{name} ({mode}): zune produced no pixels");
            }
        }
    }

    #[test]
    fn layout_brackets_the_entropy_data_with_soi_and_eoi() {
        let parts = encode(&JpegSpec::color(16, 16, Sampling::Yuv444));
        assert_eq!(&parts.bytes[..2], &[0xFF, SOI], "file must start with SOI");
        assert_eq!(
            &parts.bytes[parts.entropy.end..],
            &[0xFF, EOI],
            "EOI must follow the entropy range directly"
        );
        assert!(!parts.entropy.is_empty(), "entropy range must not be empty");
    }

    #[test]
    fn restart_markers_follow_the_interval_and_cycle_modulo_eight() {
        // The expected counts come from the MCU grid, counted by hand. A frame of m MCUs
        // with interval n has floor((m - 1) / n) restart markers: one before each interval
        // start after the first MCU.
        //   32x32 4:4:4: 4x4 MCUs of 8x8 pixels, so m = 16.
        //   33x17 4:2:0: 3x2 MCUs of 16x16 pixels, so m = 6.
        //   33x17 4:2:2: 3x3 MCUs of 16x8 pixels, so m = 9.
        let cases = [
            (Sampling::Yuv444, 32, 32, 1, 15),
            (Sampling::Yuv444, 32, 32, 3, 5),
            (Sampling::Yuv444, 32, 32, 5, 3),
            (Sampling::Yuv444, 32, 32, 16, 0),
            (Sampling::Yuv444, 32, 32, 17, 0),
            (Sampling::Yuv420, 33, 17, 1, 5),
            (Sampling::Yuv420, 33, 17, 4, 1),
            (Sampling::Yuv422, 33, 17, 2, 4),
        ];
        for (sampling, width, height, interval, expected_count) in cases {
            let mut spec = JpegSpec::color(width, height, sampling);
            spec.restart_interval = interval;
            let parts = encode(&spec);
            let found = restart_markers(&parts.bytes[parts.entropy.clone()]);
            let expected: Vec<u8> = (0..expected_count)
                .map(|index| RST0 + fit::<usize, u8>(index % 8))
                .collect();
            assert_eq!(
                found, expected,
                "{sampling:?} {width}x{height}, interval {interval}: restart markers differ \
                 from RST0 cycling modulo 8"
            );
        }
    }

    #[test]
    fn bit_writer_stuffs_ff_and_pads_with_the_selected_bit() {
        let mut stuffed = BitWriter::new(Padding::Ones);
        stuffed.put_bits(0xFF, 8);
        assert_eq!(
            stuffed.finish(),
            vec![0xFF, 0x00],
            "0xFF data must be stuffed"
        );

        let mut ones = BitWriter::new(Padding::Ones);
        ones.put_bits(0b101, 3);
        assert_eq!(ones.finish(), vec![0b1011_1111], "ones padding");

        let mut zeros = BitWriter::new(Padding::Zeros);
        zeros.put_bits(0b101, 3);
        assert_eq!(zeros.finish(), vec![0b1010_0000], "zeros padding");
    }

    #[test]
    fn restart_pads_then_writes_an_unstuffed_marker_numbered_modulo_eight() {
        let mut writer = BitWriter::new(Padding::Ones);
        writer.put_bits(0, 1);
        // Index 9 is RST1. The marker bytes are written raw, so FF D1 is not stuffed.
        writer.restart(9);
        writer.put_bits(0xFF, 8);
        assert_eq!(
            writer.finish(),
            vec![0x7F, 0xFF, 0xD1, 0xFF, 0x00],
            "restart must pad, write FF D1 raw, and still stuff the next 0xFF"
        );
    }

    #[test]
    fn additional_bits_store_negative_values_as_ones_complement() {
        // Category 2 holds -3, -2, 2 and 3. Their additional bits are 00, 01, 10 and 11.
        let cases: [(i32, u8); 4] = [(-3, 0b00), (-2, 0b01), (2, 0b10), (3, 0b11)];
        for (value, bits) in cases {
            let mut writer = BitWriter::new(Padding::Zeros);
            put_value(&mut writer, value, 2);
            assert_eq!(
                writer.finish(),
                vec![bits << 6],
                "value {value} in category 2"
            );
        }
    }

    #[test]
    fn canonical_codes_match_the_standard_examples() {
        let code = |bits: u32, len: u8| Code { bits, len };
        let dc = CodeTable::new(&DC_LUMINANCE);
        let dc_chroma = CodeTable::new(&DC_CHROMINANCE);
        let ac = CodeTable::new(&AC_LUMINANCE);
        let ac_chroma = CodeTable::new(&AC_CHROMINANCE);
        assert_eq!(dc.code(0), code(0b00, 2), "luminance DC category 0");
        assert_eq!(dc.code(6), code(0b1110, 4), "luminance DC category 6");
        assert_eq!(dc.code(11), code(0x1FE, 9), "luminance DC category 11");
        assert_eq!(
            dc_chroma.code(0),
            code(0b00, 2),
            "chrominance DC category 0"
        );
        assert_eq!(ac.code(0x00), code(0b1010, 4), "luminance end of block");
        assert_eq!(ac.code(0xF0), code(0x7F9, 11), "luminance ZRL");
        assert_eq!(
            ac_chroma.code(0x00),
            code(0b00, 2),
            "chrominance end of block"
        );
        assert_eq!(ac_chroma.code(0xF0), code(0x3FA, 10), "chrominance ZRL");
    }

    #[test]
    fn ac_patterns_end_inside_the_block_as_the_walker_requires() {
        for (index, pattern) in AC_PATTERNS.iter().enumerate() {
            // `position` is the zig-zag index of the next coefficient.
            let mut position = 1_usize;
            let mut ended = false;
            for &step in *pattern {
                assert!(!ended, "pattern {index} has a step after its end of block");
                match step {
                    AcStep::Zrl => {
                        position += 16;
                        assert!(
                            position <= 63,
                            "pattern {index}: ZRL reaches index {position}"
                        );
                    }
                    AcStep::Coefficient { run, .. } => {
                        assert!(
                            run <= 15,
                            "pattern {index}: run {run} does not fit a symbol"
                        );
                        position += usize::from(run) + 1;
                        assert!(position <= 64, "pattern {index}: coefficient past index 63");
                    }
                    AcStep::EndOfBlock => {
                        assert!(
                            position < 64,
                            "pattern {index}: end of block at index {position}"
                        );
                        ended = true;
                    }
                }
            }
            assert!(
                ended || position == 64,
                "pattern {index} neither ends with an end of block nor reaches index 63"
            );
        }
    }

    #[test]
    fn dc_and_ac_values_have_exactly_their_category_bits_and_keep_the_predictor_small() {
        let mut predictor = 0_i32;
        for block in 0..10_000_usize {
            let category: u8 = fit(block % DC_CATEGORIES);
            let difference = dc_difference(category, block, predictor);
            predictor += difference;
            let bit_length = u32::BITS - difference.unsigned_abs().leading_zeros();
            assert_eq!(
                bit_length,
                u32::from(category),
                "block {block}: DC difference {difference} is not category {category}"
            );
            assert!(
                predictor.unsigned_abs() <= 2047,
                "block {block}: DC predictor {predictor} left the 11-bit range"
            );
        }
        for size in 1..=10_u8 {
            for block in 0..64 {
                let value = coefficient(size, block);
                let bit_length = u32::BITS - value.unsigned_abs().leading_zeros();
                assert_eq!(
                    bit_length,
                    u32::from(size),
                    "block {block}: AC value {value}"
                );
            }
        }
    }

    #[test]
    fn minimal_jpeg_is_the_default_frame_bracketed_by_soi_and_eoi() {
        let minimal = minimal_jpeg();
        assert_eq!(minimal, encode(&JpegSpec::default()).bytes);
        assert_eq!(
            &minimal[..2],
            &soi()[..],
            "minimal file must start with SOI"
        );
        assert_eq!(
            &minimal[minimal.len() - 2..],
            &eoi()[..],
            "minimal file must end with EOI"
        );
    }

    #[test]
    fn worst_case_is_near_the_upload_limit_and_zune_accepts_it_in_strict_mode() {
        let worst = worst_case_jpeg();
        assert!(
            worst.len() <= MAX_ASSET_BYTES,
            "worst case is {} bytes, over the {MAX_ASSET_BYTES}-byte limit",
            worst.len()
        );
        assert!(
            worst.len() >= 4 * 1024 * 1024,
            "worst case is only {} bytes; it should approach the limit",
            worst.len()
        );
        let (info, decoded) = zune_decode(&worst, DecoderOptions::default().set_strict_mode(true))
            .unwrap_or_else(|error| panic!("zune refused the worst case: {error:?}"));
        assert_eq!(
            (info.width, info.height, usize::from(info.components)),
            (4096, 4096, 3),
            "zune must read the worst-case frame"
        );
        assert_eq!(decoded, 4096 * 4096 * 3, "zune must decode every pixel");
    }
}
