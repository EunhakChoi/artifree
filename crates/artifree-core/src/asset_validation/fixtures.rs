//! Deterministic PNG byte builders for the image validation tests.
//!
//! Each fixture is assembled from explicit chunk and zlib structure, so a test
//! can corrupt one field at a time. Nothing is read from disk, and no
//! third-party image is used.

/// The eight bytes that start every PNG file.
const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

/// Largest payload one stored (uncompressed) deflate block can hold.
const MAX_STORED_BLOCK: usize = 65_535;

/// Adler-32 modulus: the largest prime below 2^16.
const ADLER_MODULUS: u32 = 65_521;

/// Adam7 passes as `(x_origin, y_origin, x_step, y_step)`, in transmission order.
const ADAM7: [(usize, usize, usize, usize); 7] = [
    (0, 0, 8, 8),
    (4, 0, 8, 8),
    (0, 4, 4, 8),
    (2, 0, 4, 4),
    (0, 2, 2, 4),
    (1, 0, 2, 2),
    (0, 1, 1, 2),
];

/// Header fields for an IHDR chunk.
///
/// Values are written as given, with no range or pairing checks, so tests can
/// build illegal headers. Compression and filter method bytes are always 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct IhdrSpec {
    /// Width in pixels.
    pub(crate) width: u32,
    /// Height in pixels.
    pub(crate) height: u32,
    /// Bit depth byte.
    pub(crate) bit_depth: u8,
    /// Color type byte.
    pub(crate) color_type: u8,
    /// Interlace method byte: 0 for none, 1 for Adam7.
    pub(crate) interlace: u8,
}

/// Returns the CRC-32 that PNG uses for chunk integrity.
pub(crate) fn crc32(bytes: &[u8]) -> u32 {
    crc32fast::hash(bytes)
}

/// Returns the Adler-32 checksum of `bytes`, computed directly from RFC 1950.
pub(crate) fn adler32(bytes: &[u8]) -> u32 {
    let mut low: u32 = 1;
    let mut high: u32 = 0;
    for &byte in bytes {
        low = (low + u32::from(byte)) % ADLER_MODULUS;
        high = (high + low) % ADLER_MODULUS;
    }
    (high << 16) | low
}

/// Encodes one chunk: big-endian length, type, data, and a CRC over type and data.
///
/// # Panics
///
/// Panics if `data` is longer than `u32::MAX` bytes.
pub(crate) fn chunk(kind: [u8; 4], data: &[u8]) -> Vec<u8> {
    let mut body = kind.to_vec();
    body.extend_from_slice(data);
    let mut out = length_field(data.len()).to_vec();
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(&body).to_be_bytes());
    out
}

/// Like [`chunk`], but with the lowest bit of the stored CRC flipped.
pub(crate) fn chunk_with_bad_crc(kind: [u8; 4], data: &[u8]) -> Vec<u8> {
    let mut bytes = chunk(kind, data);
    if let Some(last) = bytes.last_mut() {
        *last ^= 1;
    }
    bytes
}

/// Wraps `raw` in a zlib stream of stored deflate blocks with a correct Adler-32.
pub(crate) fn zlib_stored(raw: &[u8]) -> Vec<u8> {
    zlib_stored_with_adler(raw, adler32(raw))
}

/// Like [`zlib_stored`], but writes `adler` as the trailer instead of the real checksum.
pub(crate) fn zlib_stored_with_adler(raw: &[u8], adler: u32) -> Vec<u8> {
    let mut blocks: Vec<&[u8]> = raw.chunks(MAX_STORED_BLOCK).collect();
    if blocks.is_empty() {
        // An empty payload still needs one final block.
        blocks.push(raw);
    }
    let final_index = blocks.len() - 1;
    // 0x78 0x01: deflate with a 32 KiB window, no preset dictionary, fastest level.
    let mut out = vec![0x78, 0x01];
    for (index, block) in blocks.into_iter().enumerate() {
        let len = u16::try_from(block.len())
            .unwrap_or_else(|error| panic!("stored block is too long: {error}"));
        // The first byte holds BFINAL in bit 0 and BTYPE 00 (stored) in bits 1 and 2.
        out.push(u8::from(index == final_index));
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(block);
    }
    out.extend_from_slice(&adler.to_be_bytes());
    out
}

/// Compresses `raw` into a zlib stream with `miniz_oxide` at level 6.
pub(crate) fn zlib_compressed(raw: &[u8]) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec_zlib(raw, 6)
}

/// Encodes a complete IHDR chunk for `spec`.
pub(crate) fn ihdr(spec: IhdrSpec) -> Vec<u8> {
    let mut data = Vec::with_capacity(13);
    data.extend_from_slice(&spec.width.to_be_bytes());
    data.extend_from_slice(&spec.height.to_be_bytes());
    data.extend_from_slice(&[spec.bit_depth, spec.color_type, 0, 0, spec.interlace]);
    chunk(*b"IHDR", &data)
}

/// Returns non-interlaced scanline data: each row is filter byte 0 and zero pixels.
///
/// The buffer is entirely zero, which is the inflated payload of a blank image.
///
/// # Panics
///
/// Panics if `color_type` is not 0, 2, 3, 4 or 6.
pub(crate) fn raw_scanlines(width: u32, height: u32, bit_depth: u8, color_type: u8) -> Vec<u8> {
    let row = 1 + row_bytes(dimension(width), bit_depth, color_type);
    vec![0; dimension(height) * row]
}

/// Returns Adam7 scanline data for seven passes in transmission order.
///
/// Each row is filter byte 0 and zero pixels. A pass with no pixels is omitted,
/// as the format requires.
///
/// # Panics
///
/// Panics if `color_type` is not 0, 2, 3, 4 or 6.
pub(crate) fn raw_adam7(width: u32, height: u32, bit_depth: u8, color_type: u8) -> Vec<u8> {
    let (width, height) = (dimension(width), dimension(height));
    let mut total = 0;
    for (x_origin, y_origin, x_step, y_step) in ADAM7 {
        let pass_width = pass_pixels(width, x_origin, x_step);
        let pass_height = pass_pixels(height, y_origin, y_step);
        if pass_width > 0 && pass_height > 0 {
            total += pass_height * (1 + row_bytes(pass_width, bit_depth, color_type));
        }
    }
    vec![0; total]
}

/// Encodes a PLTE chunk with `entries` palette entries, all black.
pub(crate) fn plte(entries: usize) -> Vec<u8> {
    chunk(*b"PLTE", &vec![0_u8; entries * 3])
}

/// Encodes the empty IEND chunk that ends a PNG file.
pub(crate) fn iend() -> Vec<u8> {
    chunk(*b"IEND", &[])
}

/// Concatenates the PNG signature and `chunks`, adding nothing else.
///
/// The caller supplies IHDR and IEND, so tests can omit or reorder either one.
pub(crate) fn png(chunks: &[Vec<u8>]) -> Vec<u8> {
    let mut out = SIGNATURE.to_vec();
    for part in chunks {
        out.extend_from_slice(part);
    }
    out
}

/// Builds a non-interlaced PNG: IHDR, PLTE for color type 3, one stored IDAT, and IEND.
///
/// Stored blocks make the file as large as its raw pixels, so keep images small.
/// For a large valid image, pass [`zlib_compressed`] output to [`rebuild`].
pub(crate) fn valid_png(width: u32, height: u32, bit_depth: u8, color_type: u8) -> Vec<u8> {
    let raw = raw_scanlines(width, height, bit_depth, color_type);
    let spec = IhdrSpec {
        width,
        height,
        bit_depth,
        color_type,
        interlace: 0,
    };
    build_png(spec, &raw)
}

/// Like [`valid_png`], but Adam7 interlaced.
pub(crate) fn valid_adam7_png(width: u32, height: u32, bit_depth: u8, color_type: u8) -> Vec<u8> {
    let raw = raw_adam7(width, height, bit_depth, color_type);
    let spec = IhdrSpec {
        width,
        height,
        bit_depth,
        color_type,
        interlace: 1,
    };
    build_png(spec, &raw)
}

/// Returns a 1x1 RGBA PNG, the smallest valid image.
pub(crate) fn minimal_png() -> Vec<u8> {
    valid_png(1, 1, 8, 6)
}

/// Assembles a PNG from explicit parts, so a test can reorder or split chunks.
///
/// The result is `chunks_before_idat`, one IDAT holding `idat_payload`,
/// `extra_after_idat`, and IEND. The payload is written as given, not wrapped
/// or checked, so pass a zlib stream.
pub(crate) fn rebuild(
    chunks_before_idat: Vec<Vec<u8>>,
    idat_payload: &[u8],
    extra_after_idat: Vec<Vec<u8>>,
) -> Vec<u8> {
    let mut chunks = chunks_before_idat;
    chunks.push(chunk(*b"IDAT", idat_payload));
    chunks.extend(extra_after_idat);
    chunks.push(iend());
    png(&chunks)
}

/// Assembles the chunks of a valid image around `raw`.
fn build_png(spec: IhdrSpec, raw: &[u8]) -> Vec<u8> {
    let mut chunks = vec![ihdr(spec)];
    if spec.color_type == 3 {
        chunks.push(plte(palette_entries(spec.bit_depth)));
    }
    chunks.push(chunk(*b"IDAT", &zlib_stored(raw)));
    chunks.push(iend());
    png(&chunks)
}

/// Returns the palette size for a color type 3 image: `2^bit_depth`, capped at 256.
fn palette_entries(bit_depth: u8) -> usize {
    match bit_depth {
        0..=7 => 1_usize << bit_depth,
        _ => 256,
    }
}

/// Encodes a chunk length as the big-endian bytes PNG requires.
///
/// # Panics
///
/// Panics if `len` does not fit in `u32`.
fn length_field(len: usize) -> [u8; 4] {
    u32::try_from(len)
        .unwrap_or_else(|error| panic!("chunk data of {len} bytes: {error}"))
        .to_be_bytes()
}

/// Returns the number of pixels an Adam7 pass places along an axis of `size` pixels.
fn pass_pixels(size: usize, origin: usize, step: usize) -> usize {
    size.saturating_sub(origin).div_ceil(step)
}

/// Returns the bytes one row of `pixels` pixels occupies, excluding the filter byte.
fn row_bytes(pixels: usize, bit_depth: u8, color_type: u8) -> usize {
    (pixels * samples(color_type) * usize::from(bit_depth)).div_ceil(8)
}

/// Returns the number of samples per pixel for `color_type`.
///
/// # Panics
///
/// Panics if `color_type` is not 0, 2, 3, 4 or 6.
fn samples(color_type: u8) -> usize {
    match color_type {
        0 | 3 => 1,
        2 => 3,
        4 => 2,
        6 => 4,
        other => panic!("fixtures do not build color type {other}"),
    }
}

/// Converts a header dimension to `usize`.
///
/// # Panics
///
/// Panics if the value does not fit in `usize`.
fn dimension(value: u32) -> usize {
    usize::try_from(value).unwrap_or_else(|error| panic!("dimension {value}: {error}"))
}
