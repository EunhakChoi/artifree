//! Checks for PNG uploads (decision 0045, rule 3).
//!
//! `validate_png` is the entry point and runs four passes in order.
//! `scan_container` reads the bytes once and borrows them. It checks each chunk's
//! framing and CRC before the chunk's data is interpreted, refuses animation and
//! unknown critical chunks, records the IDAT ranges, and records the zlib stream
//! of each zTXt, iCCP and compressed iTXt chunk. `verify_zlib` inflates the IDAT
//! ranges and checks the Adler-32 and the exact length. `verify_embedded_streams`
//! inflates each recorded stream against the metadata budget that all of them share.
//! `decode_with_png_crate` decodes every row with the `png` crate and then checks
//! the compressed text.

use std::io::Cursor;
use std::ops::Range;

use artifree_domain::assets::AssetRejection;
use miniz_oxide::inflate::TINFLStatus;
use miniz_oxide::inflate::core::{
    self as inflate_core, DecompressorOxide, TINFL_LZ_DICT_SIZE, inflate_flags,
};
use png::text_metadata::{ITXtChunk, ZTXtChunk};

use super::{MAX_IMAGE_AXIS, MAX_IMAGE_PIXELS, PNG_METADATA_BUDGET};

/// The eight-byte PNG file signature.
pub(super) const SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

/// Bytes a chunk adds around its data: the length, the type and the CRC.
const CHUNK_OVERHEAD: usize = 12;

/// Largest data length a chunk may declare, 2^31 - 1.
const MAX_CHUNK_DATA: u32 = 0x7FFF_FFFF;

/// Largest PLTE payload: 256 entries of three bytes each.
const MAX_PALETTE_BYTES: usize = 768;

/// Longest keyword that a text or iCCP chunk may carry, in bytes.
const MAX_KEYWORD_BYTES: usize = 79;

const IHDR: [u8; 4] = *b"IHDR";
const PLTE: [u8; 4] = *b"PLTE";
const IDAT: [u8; 4] = *b"IDAT";
const IEND: [u8; 4] = *b"IEND";

// Animation chunks. Any of them makes the file an APNG, which is refused.
const ACTL: [u8; 4] = *b"acTL";
const FCTL: [u8; 4] = *b"fcTL";
const FDAT: [u8; 4] = *b"fdAT";

// Chunks whose data may hold a zlib stream.
const ICCP: [u8; 4] = *b"iCCP";
const ITXT: [u8; 4] = *b"iTXt";
const ZTXT: [u8; 4] = *b"zTXt";

// Color type values from IHDR.
const GRAY: u8 = 0;
const RGB: u8 = 2;
const PALETTE: u8 = 3;
const GRAY_ALPHA: u8 = 4;
const RGBA: u8 = 6;

/// Adam7 passes as `(x start, y start, x step, y step)`, in pass order.
const ADAM7_PASSES: [(u64, u64, u64, u64); 7] = [
    (0, 0, 8, 8),
    (4, 0, 8, 8),
    (0, 4, 4, 8),
    (2, 0, 4, 4),
    (0, 2, 2, 4),
    (1, 0, 2, 2),
    (0, 1, 1, 2),
];

/// Image facts that the later checks need, taken from IHDR and the chunks that hold
/// compressed data.
#[derive(Debug)]
struct PngLayout {
    width: u32,
    height: u32,
    bit_depth: u8,
    color_type: u8,
    interlaced: bool,
    /// File ranges of the IDAT payloads, in file order.
    idat: Vec<Range<usize>>,
    /// File ranges of the zlib streams in zTXt, iCCP and compressed iTXt chunks, in file order.
    compressed_streams: Vec<Range<usize>>,
}

/// A chunk whose length, type and CRC have been checked.
struct Chunk<'a> {
    kind: [u8; 4],
    data: &'a [u8],
    /// File offsets of `data`.
    data_range: Range<usize>,
}

/// Forward-only reader over the upload. Every read is bounds-checked, so a
/// declared length cannot reach past the end of the input.
struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

/// Chunk-order state, plus the layout under construction, after IHDR.
struct Scan {
    layout: PngLayout,
    /// Whether a PLTE chunk has appeared so far.
    palette_seen: bool,
    /// Whether a chunk other than IDAT has followed the IDAT run.
    idat_run_ended: bool,
    /// Whether an iCCP chunk has appeared so far.
    iccp_seen: bool,
}

/// Checks a PNG upload: its container, its IDAT stream, its embedded streams, then a
/// full decode.
///
/// # Errors
/// `Invalid` for any input that is not a complete, static PNG within the
/// admitted limits.
pub(super) fn validate_png(bytes: &[u8]) -> Result<(), AssetRejection> {
    let layout = scan_container(bytes)?;
    // The scan already guarantees IDAT data and a representable image size.
    // Both are checked again so that this function stays sound on its own.
    if layout.idat.is_empty() || expected_inflated_len(&layout).is_none() {
        return Err(AssetRejection::Invalid);
    }
    verify_zlib(bytes, &layout)?;
    verify_embedded_streams(bytes, &layout)?;
    decode_with_png_crate(bytes)
}

/// Walks the whole file and returns its layout.
///
/// Every byte must belong to a checked chunk, and IEND must end the file.
fn scan_container(bytes: &[u8]) -> Result<PngLayout, AssetRejection> {
    let mut reader = Reader::new(bytes);
    let signature: [u8; 8] = reader.take_array()?;
    if signature != SIGNATURE {
        return Err(AssetRejection::Invalid);
    }
    let first = read_chunk(&mut reader)?;
    if first.kind != IHDR {
        return Err(AssetRejection::Invalid);
    }
    let mut scan = Scan::new(parse_ihdr(first.data)?);
    loop {
        let chunk = read_chunk(&mut reader)?;
        if chunk.kind == IEND {
            if !chunk.data.is_empty() || !reader.is_at_end() {
                return Err(AssetRejection::Invalid);
            }
            return scan.finish();
        }
        scan.accept(&chunk)?;
    }
}

/// Reads the next chunk and checks its length, type and CRC.
///
/// A chunk needs at least 12 unread bytes, so a truncated file is refused
/// before any field is interpreted.
fn read_chunk<'a>(reader: &mut Reader<'a>) -> Result<Chunk<'a>, AssetRejection> {
    if reader.remaining() < CHUNK_OVERHEAD {
        return Err(AssetRejection::Invalid);
    }
    let length = reader.take_u32()?;
    if length > MAX_CHUNK_DATA {
        return Err(AssetRejection::Invalid);
    }
    let kind: [u8; 4] = reader.take_array()?;
    if !is_valid_type(kind) {
        return Err(AssetRejection::Invalid);
    }
    let data_len = usize::try_from(length).map_err(|_| AssetRejection::Invalid)?;
    let data_start = reader.offset;
    let data = reader.take_bytes(data_len)?;
    let data_range = data_start..reader.offset;
    let stored_crc = reader.take_u32()?;
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&kind);
    hasher.update(data);
    if hasher.finalize() != stored_crc {
        return Err(AssetRejection::Invalid);
    }
    Ok(Chunk {
        kind,
        data,
        data_range,
    })
}

/// Whether `kind` is four ASCII letters with the reserved bit clear.
///
/// Bit 0x20 of the third byte is the reserved bit. For an ASCII letter that bit
/// is clear exactly when the letter is uppercase.
fn is_valid_type(kind: [u8; 4]) -> bool {
    let [_, _, third, _] = kind;
    kind.iter().all(u8::is_ascii_alphabetic) && third.is_ascii_uppercase()
}

/// Whether a chunk after IHDR is refused: a repeated IHDR, an animation chunk,
/// or a critical chunk this validator does not know.
fn is_refused_after_ihdr(kind: [u8; 4]) -> bool {
    let [first, ..] = kind;
    let known_critical = matches!(kind, IHDR | PLTE | IDAT | IEND);
    let unknown_critical = first.is_ascii_uppercase() && !known_critical;
    unknown_critical || matches!(kind, IHDR | ACTL | FCTL | FDAT)
}

/// Parses the 13-byte IHDR payload and checks the fields the validator relies on.
fn parse_ihdr(data: &[u8]) -> Result<PngLayout, AssetRejection> {
    let (width_bytes, rest) = data
        .split_first_chunk::<4>()
        .ok_or(AssetRejection::Invalid)?;
    let (height_bytes, rest) = rest
        .split_first_chunk::<4>()
        .ok_or(AssetRejection::Invalid)?;
    let &[bit_depth, color_type, compression, filter, interlace] = rest else {
        return Err(AssetRejection::Invalid);
    };
    let width = u32::from_be_bytes(*width_bytes);
    let height = u32::from_be_bytes(*height_bytes);
    check_dimensions(width, height)?;
    if !legal_depth(color_type, bit_depth) || compression != 0 || filter != 0 || interlace > 1 {
        return Err(AssetRejection::Invalid);
    }
    Ok(PngLayout {
        width,
        height,
        bit_depth,
        color_type,
        interlaced: interlace == 1,
        idat: Vec::new(),
        compressed_streams: Vec::new(),
    })
}

/// Checks that each axis is in range and that the pixel count is within its limit.
///
/// With a 4096 axis limit the product can never exceed the pixel limit. The
/// product check stays so that the two limits can change independently.
fn check_dimensions(width: u32, height: u32) -> Result<(), AssetRejection> {
    if width == 0 || height == 0 || width > MAX_IMAGE_AXIS || height > MAX_IMAGE_AXIS {
        return Err(AssetRejection::Invalid);
    }
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or(AssetRejection::Invalid)?;
    if pixels > MAX_IMAGE_PIXELS {
        return Err(AssetRejection::Invalid);
    }
    Ok(())
}

/// Whether the PNG specification allows this bit depth for the color type.
fn legal_depth(color_type: u8, bit_depth: u8) -> bool {
    match color_type {
        GRAY => matches!(bit_depth, 1 | 2 | 4 | 8 | 16),
        RGB | GRAY_ALPHA | RGBA => matches!(bit_depth, 8 | 16),
        PALETTE => matches!(bit_depth, 1 | 2 | 4 | 8),
        _ => false,
    }
}

/// Checks a PLTE payload: one to 256 three-byte entries.
fn check_palette(data: &[u8]) -> Result<(), AssetRejection> {
    let len = data.len();
    if len.is_multiple_of(3) && (3..=MAX_PALETTE_BYTES).contains(&len) {
        Ok(())
    } else {
        Err(AssetRejection::Invalid)
    }
}

/// Length of the zlib payload once inflated: a filter byte and the packed pixels
/// for each scanline, summed over the Adam7 passes when the image is interlaced.
///
/// Returns `None` if the size overflows `u64` or the color type is undefined.
fn expected_inflated_len(layout: &PngLayout) -> Option<u64> {
    let bits_per_pixel = u64::from(layout.bit_depth).checked_mul(channels(layout.color_type)?)?;
    let width = u64::from(layout.width);
    let height = u64::from(layout.height);
    if layout.interlaced {
        interlaced_len(width, height, bits_per_pixel)
    } else {
        filtered_len(width, height, bits_per_pixel)
    }
}

/// Samples per pixel for a color type, or `None` for an undefined type.
fn channels(color_type: u8) -> Option<u64> {
    match color_type {
        GRAY | PALETTE => Some(1),
        GRAY_ALPHA => Some(2),
        RGB => Some(3),
        RGBA => Some(4),
        _ => None,
    }
}

/// Filtered bytes for one image or Adam7 pass: each scanline is a filter byte
/// followed by its packed pixels. A pass with no pixels has no scanlines.
fn filtered_len(width: u64, height: u64, bits_per_pixel: u64) -> Option<u64> {
    if width == 0 || height == 0 {
        return Some(0);
    }
    let packed_row_bytes = width.checked_mul(bits_per_pixel)?.div_ceil(8);
    let row_bytes = packed_row_bytes.checked_add(1)?;
    height.checked_mul(row_bytes)
}

/// Filtered bytes summed over the seven Adam7 passes.
///
/// A pass that starts past the edge has no columns or rows, and `saturating_sub`
/// gives zero there.
fn interlaced_len(width: u64, height: u64, bits_per_pixel: u64) -> Option<u64> {
    let mut total: u64 = 0;
    for &(x_start, y_start, x_step, y_step) in &ADAM7_PASSES {
        let pass_width = width.saturating_sub(x_start).div_ceil(x_step);
        let pass_height = height.saturating_sub(y_start).div_ceil(y_step);
        total = total.checked_add(filtered_len(pass_width, pass_height, bits_per_pixel)?)?;
    }
    Some(total)
}

/// Inflates the IDAT payloads as one zlib stream and checks its header, its
/// Adler-32 and its inflated length. The output is counted and then discarded.
fn verify_zlib(bytes: &[u8], layout: &PngLayout) -> Result<(), AssetRejection> {
    let expected = expected_inflated_len(layout).ok_or(AssetRejection::Invalid)?;
    let mut check = ZlibCheck::new(expected);
    let slice_count = layout.idat.len();
    for (index, range) in layout.idat.iter().enumerate() {
        let slice = bytes.get(range.clone()).ok_or(AssetRejection::Invalid)?;
        // IDAT data may not follow the end of the zlib stream, in any slice.
        if !check.feed(slice, index + 1 < slice_count)?.is_empty() {
            return Err(AssetRejection::Invalid);
        }
    }
    check.finish()
}

/// Inflates each zlib stream recorded from a zTXt, iCCP or compressed iTXt chunk.
///
/// Each stream must fill its chunk to the end. Its output is counted against one budget
/// that starts at `PNG_METADATA_BUDGET` and is shared by every stream in the file. The
/// output is discarded. A copy that reaches before the first byte of its stream is refused.
///
/// # Errors
/// `Invalid` for a corrupt stream, bytes after the end of a stream, or output that
/// exceeds the budget that remains.
fn verify_embedded_streams(bytes: &[u8], layout: &PngLayout) -> Result<(), AssetRejection> {
    let mut inflater = EmbeddedInflater::new();
    let mut remaining = PNG_METADATA_BUDGET;
    for range in &layout.compressed_streams {
        let stream = bytes.get(range.clone()).ok_or(AssetRejection::Invalid)?;
        let produced = inflater.inflate(stream, remaining)?;
        remaining = remaining
            .checked_sub(produced)
            .ok_or(AssetRejection::Invalid)?;
    }
    Ok(())
}

/// Size, in bytes, that the output buffer of an embedded stream starts at. It doubles
/// toward the budget that remains, so a small stream never allocates much.
const EMBEDDED_OUTPUT_START: usize = 4096;

/// Inflates the embedded streams of one file, one after another.
///
/// The decompressor and the output buffer are reused, so a file with many small streams
/// pays for no new decompressor or window per stream. The buffer holds all output of the
/// current stream, because miniz checks each back-reference against everything written
/// so far. A copy that reaches before the first byte of its stream is therefore refused.
struct EmbeddedInflater {
    decompressor: Box<DecompressorOxide>,
    /// Output of the current stream, from its first byte. Bytes past that output are left
    /// over from earlier streams and are never read.
    output: Vec<u8>,
}

impl EmbeddedInflater {
    fn new() -> Self {
        Self {
            decompressor: Box::new(DecompressorOxide::new()),
            output: Vec::new(),
        }
    }

    /// Inflates one zlib stream, which must fill `stream` exactly, and returns the number
    /// of bytes it produced. The output may be at most `budget` bytes.
    ///
    /// # Errors
    /// `Invalid` for a corrupt stream, a stream cut short of its end, input left after the
    /// end of the stream, an Adler-32 mismatch, or output past `budget`.
    fn inflate(&mut self, stream: &[u8], budget: usize) -> Result<usize, AssetRejection> {
        // One byte past the budget is the most that is ever stored. Output beyond the
        // budget is refused, and that byte is enough to show it happened.
        let max_len = budget.saturating_add(1);
        self.output.truncate(max_len);
        let start = EMBEDDED_OUTPUT_START.min(max_len);
        if self.output.len() < start {
            self.output.resize(start, 0);
        }
        self.decompressor.init();
        let flags = inflate_flags::TINFL_FLAG_PARSE_ZLIB_HEADER
            | inflate_flags::TINFL_FLAG_COMPUTE_ADLER32
            | inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF;
        let mut input = stream;
        let mut produced: usize = 0;
        loop {
            let (status, consumed, written) = inflate_core::decompress(
                &mut self.decompressor,
                input,
                &mut self.output,
                produced,
                flags,
            );
            input = input.get(consumed..).ok_or(AssetRejection::Invalid)?;
            produced = produced
                .checked_add(written)
                .ok_or(AssetRejection::Invalid)?;
            if produced > budget {
                return Err(AssetRejection::Invalid);
            }
            match status {
                TINFLStatus::Done => break,
                // The buffer is full. Grow it, unless it already holds one byte past the
                // budget, which the check above has refused.
                TINFLStatus::HasMoreOutput if self.output.len() < max_len => {
                    let grown = self
                        .output
                        .len()
                        .saturating_mul(2)
                        .max(EMBEDDED_OUTPUT_START)
                        .min(max_len);
                    self.output.resize(grown, 0);
                }
                _ => return Err(AssetRejection::Invalid),
            }
        }
        if input.is_empty() {
            Ok(produced)
        } else {
            Err(AssetRejection::Invalid)
        }
    }
}

/// State for `verify_zlib`: a decompressor, the 32 KiB window it writes into, and the byte
/// count. Output is never kept.
///
/// The window wraps, so a copy that reaches before the first output byte reads zeros
/// instead of failing. The `png` decoder refuses such copies in IDAT data; this check
/// does not.
struct ZlibCheck {
    decompressor: Box<DecompressorOxide>,
    /// miniz reads this window for back-references. Its contents are never inspected.
    window: Vec<u8>,
    /// Where the next output is written. Always below `TINFL_LZ_DICT_SIZE`.
    window_pos: usize,
    /// Bytes inflated so far.
    produced: u64,
    /// The length that the stream must inflate to, as IHDR implies.
    expected: u64,
    /// Whether the zlib stream has ended.
    finished: bool,
}

impl ZlibCheck {
    /// Starts a check whose output must be exactly `expected` bytes long.
    fn new(expected: u64) -> Self {
        Self {
            decompressor: Box::new(DecompressorOxide::new()),
            window: vec![0; TINFL_LZ_DICT_SIZE],
            window_pos: 0,
            produced: 0,
            expected,
            finished: false,
        }
    }

    /// Inflates one slice of compressed input. `more_input` says whether another slice
    /// follows, so that running out of this one is not reported as truncation.
    ///
    /// Returns the part of `slice` after the end of the stream. It is empty while the
    /// stream is open, and non-empty only when the stream ended in this slice with bytes
    /// left over. The caller decides whether those bytes are allowed.
    ///
    /// # Errors
    /// `Invalid` for a corrupt stream, output past the expected length, or a non-empty
    /// slice after the end of the stream.
    fn feed<'a>(&mut self, slice: &'a [u8], more_input: bool) -> Result<&'a [u8], AssetRejection> {
        if slice.is_empty() {
            // An empty slice adds no bytes, so it is skipped, even after the end.
            return Ok(slice);
        }
        if self.finished {
            // Bytes after the end of the zlib stream are not allowed.
            return Err(AssetRejection::Invalid);
        }
        let mut flags =
            inflate_flags::TINFL_FLAG_PARSE_ZLIB_HEADER | inflate_flags::TINFL_FLAG_COMPUTE_ADLER32;
        if more_input {
            flags |= inflate_flags::TINFL_FLAG_HAS_MORE_INPUT;
        }
        let mut input = slice;
        loop {
            let (status, consumed, written) = inflate_core::decompress(
                &mut self.decompressor,
                input,
                &mut self.window,
                self.window_pos,
                flags,
            );
            input = input.get(consumed..).ok_or(AssetRejection::Invalid)?;
            self.record_output(written)?;
            match status {
                TINFLStatus::Done => {
                    self.finished = true;
                    break;
                }
                TINFLStatus::NeedsMoreInput => break,
                // The window filled, and record_output has wrapped it. Call again
                // with the input left, unless the call made no progress.
                TINFLStatus::HasMoreOutput if consumed > 0 || written > 0 => {}
                _ => return Err(AssetRejection::Invalid),
            }
        }
        Ok(input)
    }

    /// Counts `written` bytes of output and moves `window_pos` past them.
    fn record_output(&mut self, written: usize) -> Result<(), AssetRejection> {
        let added = u64::try_from(written).map_err(|_| AssetRejection::Invalid)?;
        let produced = self
            .produced
            .checked_add(added)
            .ok_or(AssetRejection::Invalid)?;
        // Checked on every call, so a stream that inflates past its length is refused
        // within one window of output.
        if produced > self.expected {
            return Err(AssetRejection::Invalid);
        }
        self.produced = produced;
        let position = self
            .window_pos
            .checked_add(written)
            .ok_or(AssetRejection::Invalid)?;
        self.window_pos = position & (TINFL_LZ_DICT_SIZE - 1);
        Ok(())
    }

    /// Accepts only a stream that has ended, with exactly the expected length.
    fn finish(&self) -> Result<(), AssetRejection> {
        if self.finished && self.produced == self.expected {
            Ok(())
        } else {
            Err(AssetRejection::Invalid)
        }
    }
}

/// Decodes every row with the `png` crate, with its CRC and Adler-32 checks on,
/// and then checks the compressed text chunks.
///
/// The decoder counts its buffered chunks, including text and iCCP, against
/// `PNG_METADATA_BUDGET`. IDAT payloads are streamed and are not counted.
fn decode_with_png_crate(bytes: &[u8]) -> Result<(), AssetRejection> {
    let mut options = png::DecodeOptions::default();
    options.set_ignore_crc(false);
    options.set_ignore_adler32(false);
    options.set_skip_ancillary_crc_failures(false);
    options.set_ignore_text_chunk(false);
    options.set_ignore_iccp_chunk(false);
    let mut decoder = png::Decoder::new_with_options(Cursor::new(bytes), options);
    decoder.set_limits(png::Limits {
        bytes: PNG_METADATA_BUDGET,
    });
    decoder.set_transformations(png::Transformations::IDENTITY);
    let mut reader = decoder.read_info().map_err(|_| AssetRejection::Invalid)?;
    let info = reader.info();
    if info.animation_control.is_some() || info.frame_control.is_some() {
        return Err(AssetRejection::Invalid);
    }
    while let Some(_row) = reader.next_row().map_err(|_| AssetRejection::Invalid)? {}
    reader.finish().map_err(|_| AssetRejection::Invalid)?;
    check_compressed_text(reader.info())
}

/// Inflates each compressed zTXt and iTXt chunk in turn. The decoded text of every
/// chunk is charged against one budget that starts at `PNG_METADATA_BUDGET`.
fn check_compressed_text(info: &png::Info<'_>) -> Result<(), AssetRejection> {
    let mut remaining = PNG_METADATA_BUDGET;
    for chunk in &info.compressed_latin1_text {
        remaining = inflate_text(chunk, remaining)?;
    }
    for chunk in info.utf8_text.iter().filter(|chunk| chunk.compressed) {
        remaining = inflate_text(chunk, remaining)?;
    }
    Ok(())
}

/// Inflates a copy of one compressed text chunk within `remaining` bytes and
/// returns the budget left after its decoded text.
fn inflate_text<T: CompressedText>(chunk: &T, remaining: usize) -> Result<usize, AssetRejection> {
    let mut owned = chunk.clone();
    owned
        .inflate_within(remaining)
        .map_err(|_| AssetRejection::Invalid)?;
    let text = owned.decoded_text().map_err(|_| AssetRejection::Invalid)?;
    remaining
        .checked_sub(text.len())
        .ok_or(AssetRejection::Invalid)
}

/// A compressed zTXt or iTXt payload that can be inflated under a byte limit.
/// Both chunk types implement it, so `inflate_text` serves both.
trait CompressedText: Clone {
    /// Inflates the payload in place. Fails if it decodes to more than `limit` bytes.
    fn inflate_within(&mut self, limit: usize) -> Result<(), png::DecodingError>;

    /// Returns the decoded text of a payload that has been inflated.
    fn decoded_text(&self) -> Result<String, png::DecodingError>;
}

impl CompressedText for ZTXtChunk {
    fn inflate_within(&mut self, limit: usize) -> Result<(), png::DecodingError> {
        self.decompress_text_with_limit(limit)
    }

    fn decoded_text(&self) -> Result<String, png::DecodingError> {
        self.get_text()
    }
}

impl CompressedText for ITXtChunk {
    fn inflate_within(&mut self, limit: usize) -> Result<(), png::DecodingError> {
        self.decompress_text_with_limit(limit)
    }

    fn decoded_text(&self) -> Result<String, png::DecodingError> {
        self.get_text()
    }
}

/// Locates the zlib stream in a zTXt, iCCP or compressed iTXt chunk, and checks the
/// chunk's fields around it.
///
/// Returns `Ok(None)` for every other chunk type, and for an iTXt chunk whose text is
/// not compressed. Otherwise the result is the stream's range in file offsets.
///
/// # Errors
/// `Invalid` for a keyword that is not 1 to 79 bytes ending in a NUL, a compression
/// method other than 0, an iTXt flag other than 0 or 1, an iTXt chunk whose language
/// tag or translated keyword lacks its NUL, or a chunk with no stream bytes.
fn embedded_stream(chunk: &Chunk<'_>) -> Result<Option<Range<usize>>, AssetRejection> {
    if !matches!(chunk.kind, ZTXT | ICCP | ITXT) {
        return Ok(None);
    }
    let mut reader = Reader::new(chunk.data);
    reader.take_keyword()?;
    let compressed = if chunk.kind == ITXT {
        let flag = reader.take_byte()?;
        let method = reader.take_byte()?;
        if flag > 1 || method != 0 {
            return Err(AssetRejection::Invalid);
        }
        // The language tag and the translated keyword each end at a NUL.
        reader.take_until_nul()?;
        reader.take_until_nul()?;
        flag == 1
    } else {
        // zTXt and iCCP are always compressed.
        if reader.take_byte()? != 0 {
            return Err(AssetRejection::Invalid);
        }
        true
    };
    if !compressed {
        return Ok(None);
    }
    let start = chunk
        .data_range
        .start
        .checked_add(reader.offset)
        .ok_or(AssetRejection::Invalid)?;
    let stream = start..chunk.data_range.end;
    if stream.is_empty() {
        return Err(AssetRejection::Invalid);
    }
    Ok(Some(stream))
}

impl Scan {
    fn new(layout: PngLayout) -> Self {
        Self {
            layout,
            palette_seen: false,
            idat_run_ended: false,
            iccp_seen: false,
        }
    }

    /// Applies the rules for a chunk other than IHDR and IEND.
    fn accept(&mut self, chunk: &Chunk<'_>) -> Result<(), AssetRejection> {
        if is_refused_after_ihdr(chunk.kind) {
            return Err(AssetRejection::Invalid);
        }
        if chunk.kind == IDAT {
            return self.accept_idat(chunk);
        }
        self.close_idat_run();
        if chunk.kind == PLTE {
            check_palette(chunk.data)?;
            self.palette_seen = true;
        }
        if chunk.kind == ICCP {
            self.accept_iccp()?;
        }
        if let Some(stream) = embedded_stream(chunk)? {
            self.layout.compressed_streams.push(stream);
        }
        Ok(())
    }

    /// Allows one iCCP chunk, and only before the first IDAT.
    fn accept_iccp(&mut self) -> Result<(), AssetRejection> {
        if self.iccp_seen || !self.layout.idat.is_empty() {
            return Err(AssetRejection::Invalid);
        }
        self.iccp_seen = true;
        Ok(())
    }

    /// Records an IDAT range. The IDAT chunks must form one unbroken run, and a
    /// palette image needs its PLTE before the first of them.
    fn accept_idat(&mut self, chunk: &Chunk<'_>) -> Result<(), AssetRejection> {
        if self.idat_run_ended {
            return Err(AssetRejection::Invalid);
        }
        let first_idat = self.layout.idat.is_empty();
        if first_idat && self.layout.color_type == PALETTE && !self.palette_seen {
            return Err(AssetRejection::Invalid);
        }
        self.layout.idat.push(chunk.data_range.clone());
        Ok(())
    }

    /// Ends the IDAT run once a chunk other than IDAT follows it.
    fn close_idat_run(&mut self) {
        if !self.layout.idat.is_empty() {
            self.idat_run_ended = true;
        }
    }

    /// Returns the layout once IEND has ended the file, provided IDAT data was seen.
    fn finish(self) -> Result<PngLayout, AssetRejection> {
        if self.layout.idat.is_empty() {
            return Err(AssetRejection::Invalid);
        }
        Ok(self.layout)
    }
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    /// Number of bytes not yet read.
    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    /// Whether every byte has been read.
    fn is_at_end(&self) -> bool {
        self.offset == self.bytes.len()
    }

    /// Returns the next `len` bytes, or `Invalid` if they run past the end.
    fn take_bytes(&mut self, len: usize) -> Result<&'a [u8], AssetRejection> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or(AssetRejection::Invalid)?;
        let taken = self
            .bytes
            .get(self.offset..end)
            .ok_or(AssetRejection::Invalid)?;
        self.offset = end;
        Ok(taken)
    }

    /// Returns the next `N` bytes as an array.
    fn take_array<const N: usize>(&mut self) -> Result<[u8; N], AssetRejection> {
        let taken = self.take_bytes(N)?;
        <[u8; N]>::try_from(taken).map_err(|_| AssetRejection::Invalid)
    }

    /// Returns the next four bytes as a big-endian `u32`.
    fn take_u32(&mut self) -> Result<u32, AssetRejection> {
        let bytes: [u8; 4] = self.take_array()?;
        Ok(u32::from_be_bytes(bytes))
    }

    /// Returns the next byte.
    fn take_byte(&mut self) -> Result<u8, AssetRejection> {
        let [byte] = self.take_array()?;
        Ok(byte)
    }

    /// Reads a text keyword and the NUL that ends it. A keyword is 1 to 79 bytes long.
    fn take_keyword(&mut self) -> Result<(), AssetRejection> {
        let keyword = self.take_until_nul()?;
        if (1..=MAX_KEYWORD_BYTES).contains(&keyword.len()) {
            Ok(())
        } else {
            Err(AssetRejection::Invalid)
        }
    }

    /// Returns the bytes up to the next NUL, and moves past that NUL.
    fn take_until_nul(&mut self) -> Result<&'a [u8], AssetRejection> {
        let rest = self
            .bytes
            .get(self.offset..)
            .ok_or(AssetRejection::Invalid)?;
        let len = rest
            .iter()
            .position(|&byte| byte == 0)
            .ok_or(AssetRejection::Invalid)?;
        let text = self.take_bytes(len)?;
        self.take_bytes(1)?;
        Ok(text)
    }
}

#[cfg(test)]
mod tests;
