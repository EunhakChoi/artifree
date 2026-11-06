//! Bounded content admission for new private uploads (0045).
//!
//! Call only after authority/key/base preflight and outside storage transactions.
//! Successful validation preserves the original bytes; it does not sanitize them.

use std::io::Cursor;

use artifree_domain::assets::{AssetMediaType, AssetRejection, MAX_ASSET_BYTES};

const MAX_AXIS: usize = 4096;
const MAX_PIXELS: usize = 16_777_216;
const PNG_INTERNAL_BYTES: usize = 64 * 1024 * 1024;

/// Validates an upload's current type, byte and complete image/text policy.
///
/// Text/CSV stay opaque UTF-8; formulas and markup are not rewritten. PNG undergoes
/// strict checks for RGB/RGBA8, noninterlaced IHDR/IDAT/IEND-only images, plus
/// maintained pixel decoding. Other PNG features are refused. JPEG admission
/// stays disabled because reviewed decoders silently accept truncated entropy.
///
/// # Errors
/// Returns the current admission refusal. Dimension, malformed image and image
/// decoder budget refusals are invalid content; the byte cap is a size refusal.
pub fn validate(content_type: &AssetMediaType, bytes: &[u8]) -> Result<(), AssetRejection> {
    let kind = content_type.as_str();
    if !matches!(kind, "text/plain" | "text/csv" | "image/png") {
        return Err(AssetRejection::UnsupportedType);
    }
    if bytes.len() > MAX_ASSET_BYTES {
        return Err(AssetRejection::LimitExceeded);
    }
    if bytes.is_empty() {
        return Err(AssetRejection::Invalid);
    }
    match kind {
        "text/plain" | "text/csv" => validate_text(bytes),
        "image/png" => validate_png(bytes),
        _ => Err(AssetRejection::UnsupportedType),
    }
}

fn validate_text(bytes: &[u8]) -> Result<(), AssetRejection> {
    let text = std::str::from_utf8(bytes).map_err(|_| AssetRejection::Invalid)?;
    if text
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\t' | '\r' | '\n'))
    {
        return Err(AssetRejection::Invalid);
    }
    Ok(())
}

fn dimensions(width: usize, height: usize) -> Result<(), AssetRejection> {
    if width == 0
        || height == 0
        || width > MAX_AXIS
        || height > MAX_AXIS
        || width
            .checked_mul(height)
            .is_none_or(|pixels| pixels > MAX_PIXELS)
    {
        return Err(AssetRejection::Invalid);
    }
    Ok(())
}

fn be_u32(bytes: &[u8]) -> Result<usize, AssetRejection> {
    let array: [u8; 4] = bytes.try_into().map_err(|_| AssetRejection::Invalid)?;
    usize::try_from(u32::from_be_bytes(array)).map_err(|_| AssetRejection::Invalid)
}

// The deliberately narrow subset avoids depending on the decoder's lenient
// palette/ancillary policies: RGB/RGBA8, no interlace, IHDR/IDAT/IEND only.
fn png_framing(bytes: &[u8]) -> Result<(), AssetRejection> {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(AssetRejection::Invalid);
    }
    let mut offset = 8;
    let mut expected_output = 0;
    let mut image_data = Vec::new();
    let mut seen_data = false;
    while let Some(header) = bytes.get(offset..offset + 8) {
        let length = be_u32(&header[..4])?;
        let kind = &header[4..];
        let end = offset
            .checked_add(12)
            .and_then(|n| n.checked_add(length))
            .filter(|&n| n <= bytes.len())
            .ok_or(AssetRejection::Invalid)?;
        let data = &bytes[offset + 8..end - 4];
        if offset == 8 {
            if kind != b"IHDR" || length != 13 {
                return Err(AssetRejection::Invalid);
            }
            let width = be_u32(&data[..4])?;
            let height = be_u32(&data[4..8])?;
            dimensions(width, height)?;
            if data[8] != 8 || !matches!(data[9], 2 | 6) || data[10..] != [0, 0, 0] {
                return Err(AssetRejection::Invalid);
            }
            let channels = if data[9] == 2 { 3 } else { 4 };
            expected_output = (width * channels + 1) * height;
        } else if kind == b"IDAT" {
            seen_data = true;
            image_data.extend_from_slice(data);
        } else if kind == b"IEND" && length == 0 && end == bytes.len() && seen_data {
            let output = strict_zlib(&image_data, expected_output)?;
            return if output == expected_output {
                Ok(())
            } else {
                Err(AssetRejection::Invalid)
            };
        } else {
            return Err(AssetRejection::Invalid);
        }
        offset = end;
    }
    Err(AssetRejection::Invalid)
}

fn strict_zlib(bytes: &[u8], output_limit: usize) -> Result<usize, AssetRejection> {
    let mut decoder = flate2::Decompress::new(true);
    let mut scratch = [0; 16384];
    let mut input = 0;
    loop {
        let before_in = decoder.total_in();
        let before_out = decoder.total_out();
        let status = decoder
            .decompress(&bytes[input..], &mut scratch, flate2::FlushDecompress::None)
            .map_err(|_| AssetRejection::Invalid)?;
        input = usize::try_from(decoder.total_in()).map_err(|_| AssetRejection::Invalid)?;
        let output = usize::try_from(decoder.total_out()).map_err(|_| AssetRejection::Invalid)?;
        if output > output_limit {
            return Err(AssetRejection::Invalid);
        }
        if status == flate2::Status::StreamEnd {
            return if input == bytes.len() {
                Ok(output)
            } else {
                Err(AssetRejection::Invalid)
            };
        }
        if before_in == decoder.total_in() && before_out == decoder.total_out() {
            return Err(AssetRejection::Invalid);
        }
    }
}

fn validate_png(bytes: &[u8]) -> Result<(), AssetRejection> {
    png_framing(bytes)?;
    let mut options = png::DecodeOptions::default();
    options.set_ignore_checksums(false);
    let mut decoder = png::Decoder::new_with_options(Cursor::new(bytes), options);
    decoder.set_limits(png::Limits {
        bytes: PNG_INTERNAL_BYTES,
    });
    let mut reader = decoder.read_info().map_err(|_| AssetRejection::Invalid)?;
    // Row decoding verifies every filter/pixel row without a full frame allocation.
    while reader
        .next_row()
        .map_err(|_| AssetRejection::Invalid)?
        .is_some()
    {}
    reader.finish().map_err(|_| AssetRejection::Invalid)
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "Tests fail immediately on invalid fixture construction."
)]
mod tests {
    use std::io::Write;

    use super::*;

    fn check(kind: &str, bytes: &[u8]) -> Result<(), AssetRejection> {
        validate(&kind.parse().unwrap(), bytes)
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, width, height);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer
                .write_image_data(&vec![128; (width * height * 3) as usize])
                .unwrap();
        }
        bytes
    }

    fn jpeg() -> Vec<u8> {
        fn segment(bytes: &mut Vec<u8>, marker: u8, data: &[u8]) {
            bytes.extend_from_slice(&[0xff, marker]);
            bytes.extend_from_slice(&u16::try_from(data.len() + 2).unwrap().to_be_bytes());
            bytes.extend_from_slice(data);
        }
        // Original 1x1, 8-bit grayscale baseline fixture: DC zero and AC EOB.
        let mut bytes = vec![0xff, 0xd8];
        let mut quantization = vec![0];
        quantization.extend_from_slice(&[1; 64]);
        segment(&mut bytes, 0xdb, &quantization);
        segment(&mut bytes, 0xc0, &[8, 0, 1, 0, 1, 1, 1, 0x11, 0]);
        for class in [0, 0x10] {
            let mut huffman = vec![class, 1];
            huffman.extend_from_slice(&[0; 15]);
            huffman.push(0);
            segment(&mut bytes, 0xc4, &huffman);
        }
        segment(&mut bytes, 0xda, &[1, 1, 0, 0, 63, 0]);
        bytes.extend_from_slice(&[0x3f, 0xff, 0xd9]);
        bytes
    }

    fn chunk(kind: &[u8], data: &[u8]) -> Vec<u8> {
        let mut chunk = Vec::new();
        chunk.extend_from_slice(&u32::try_from(data.len()).unwrap().to_be_bytes());
        chunk.extend_from_slice(kind);
        chunk.extend_from_slice(data);
        chunk.extend_from_slice(&crc32fast::hash(&chunk[4..]).to_be_bytes());
        chunk
    }

    fn rewrite_chunk(bytes: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
        let start = bytes.windows(4).position(|s| s == kind).unwrap() - 4;
        let length = be_u32(&bytes[start..start + 4]).unwrap();
        bytes.splice(start..start + 12 + length, chunk(kind, data));
    }

    fn zlib(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn text_byte_bounds_and_opaque_csv() {
        assert_eq!(check("text/plain", b"x"), Ok(()));
        assert_eq!(check("text/plain", &vec![b'x'; MAX_ASSET_BYTES]), Ok(()));
        assert_eq!(
            check("text/plain", &vec![b'x'; MAX_ASSET_BYTES + 1]),
            Err(AssetRejection::LimitExceeded)
        );
        assert_eq!(check("text/plain", b""), Err(AssetRejection::Invalid));
        assert_eq!(check("text/csv", b"=1+1,\"<script>\"\r\n\t"), Ok(()));
        assert_eq!(check("text/plain", "한글😀".as_bytes()), Ok(()));
    }

    #[test]
    fn rejects_invalid_utf8_and_unicode_controls() {
        for bytes in [
            b"\xff".as_slice(),
            b"\0",
            b"\x01",
            b"\x7f",
            "\u{85}".as_bytes(),
        ] {
            assert_eq!(
                check("text/plain", bytes),
                Err(AssetRejection::Invalid),
                "{bytes:?}"
            );
            assert_eq!(
                check("text/csv", bytes),
                Err(AssetRejection::Invalid),
                "{bytes:?}"
            );
        }
    }

    #[test]
    fn unsupported_types_are_policy_refusals() {
        for kind in [
            "image/svg+xml",
            "application/json",
            "text/html",
            "image/gif",
            "image/webp",
        ] {
            assert_eq!(
                check(kind, b"x"),
                Err(AssetRejection::UnsupportedType),
                "{kind}"
            );
        }
    }

    #[test]
    fn decodes_png_but_refuses_jpeg_and_false_declared_type() {
        let png = png(2, 3);
        let jpeg = jpeg();
        assert_eq!(check("image/png", &png), Ok(()));
        assert_eq!(
            check("image/jpeg", &jpeg),
            Err(AssetRejection::UnsupportedType)
        );
        assert_eq!(check("image/png", &jpeg), Err(AssetRejection::Invalid));
        assert_eq!(
            check("image/jpeg", &png),
            Err(AssetRejection::UnsupportedType)
        );
    }

    #[test]
    fn rejects_every_truncated_image_prefix_and_trailing_bytes() {
        for (kind, bytes) in [("image/png", png(2, 3))] {
            for end in 0..bytes.len() {
                assert_eq!(
                    check(kind, &bytes[..end]),
                    Err(AssetRejection::Invalid),
                    "{kind} prefix {end}"
                );
            }
            let mut trailing = bytes.clone();
            trailing.push(0);
            assert_eq!(check(kind, &trailing), Err(AssetRejection::Invalid));
            trailing.extend_from_slice(&bytes);
            assert_eq!(check(kind, &trailing), Err(AssetRejection::Invalid));
        }
    }

    #[test]
    fn rejects_image_data_corruption_after_valid_headers() {
        let mut png = png(2, 3);
        let idat = png.windows(4).position(|s| s == b"IDAT").unwrap();
        png[idat + 5] ^= 128;
        assert_eq!(check("image/png", &png), Err(AssetRejection::Invalid));
    }

    #[test]
    fn rejects_animation_and_unbounded_dimensions_before_decode() {
        let png = png(2, 3);
        for kind in [b"acTL", b"fcTL", b"fdAT"] {
            let mut animated = png.clone();
            animated.splice(33..33, chunk(kind, &[0, 0, 0, 1, 0, 0, 0, 0]));
            assert_eq!(check("image/png", &animated), Err(AssetRejection::Invalid));
        }
        for (width, height) in [(0, 1), (1, 0), (4097, 1), (1, 4097), (u32::MAX, u32::MAX)] {
            let mut oversized = png.clone();
            oversized[16..20].copy_from_slice(&width.to_be_bytes());
            oversized[20..24].copy_from_slice(&height.to_be_bytes());
            let header = oversized[16..29].to_vec();
            rewrite_chunk(&mut oversized, b"IHDR", &header);
            assert_eq!(check("image/png", &oversized), Err(AssetRejection::Invalid));
        }
        assert_eq!(dimensions(4096, 4096), Ok(()));
        assert_eq!(
            check("image/png", &self::png(4097, 1)),
            Err(AssetRejection::Invalid)
        );
        assert_eq!(
            check("image/png", &self::png(1, 4097)),
            Err(AssetRejection::Invalid)
        );
    }

    #[test]
    fn accepts_dimension_limit_decompression_heavy_png() {
        assert_eq!(check("image/png", &png(4096, 4096)), Ok(()));
    }

    #[test]
    fn rejects_corrupt_and_truncated_deflate_with_valid_chunk_crc() {
        let bytes = png(4, 4);
        let start = bytes.windows(4).position(|s| s == b"IDAT").unwrap();
        let length = be_u32(&bytes[start - 4..start]).unwrap();
        let data = &bytes[start + 4..start + 4 + length];
        for end in 0..data.len() {
            let mut truncated = bytes.clone();
            rewrite_chunk(&mut truncated, b"IDAT", &data[..end]);
            assert_eq!(
                check("image/png", &truncated),
                Err(AssetRejection::Invalid),
                "IDAT length {end}"
            );
        }
        let mut bad_adler = data.to_vec();
        let last = bad_adler.len() - 1;
        bad_adler[last] ^= 1;
        let mut corrupt = bytes;
        rewrite_chunk(&mut corrupt, b"IDAT", &bad_adler);
        assert_eq!(check("image/png", &corrupt), Err(AssetRejection::Invalid));
    }

    #[test]
    fn rejects_malformed_ancillary_chunks_and_crc() {
        let bytes = png(1, 1);
        for (kind, data) in [
            (b"gAMA", b"x".as_slice()),
            (b"iCCP", b"Profile\0\0bad-deflate"),
            (b"zTXt", b"Comment\0\0bad-deflate"),
        ] {
            let mut malformed = bytes.clone();
            malformed.splice(33..33, chunk(kind, data));
            assert_eq!(
                check("image/png", &malformed),
                Err(AssetRejection::Invalid),
                "{kind:?}"
            );
        }
        let mut malformed = bytes;
        let mut text = chunk(b"tEXt", b"Comment\0value");
        let last = text.len() - 1;
        text[last] ^= 1;
        malformed.splice(33..33, text);
        assert_eq!(check("image/png", &malformed), Err(AssetRejection::Invalid));
    }

    #[test]
    fn rejects_zlib_trailers_extra_rows_and_invalid_filter_semantics() {
        let bytes = png(1, 1);
        for raw in [b"\0\x80\x80\x80\0".as_slice(), b"\x05\x80\x80\x80"] {
            let mut invalid = bytes.clone();
            rewrite_chunk(&mut invalid, b"IDAT", &zlib(raw));
            assert_eq!(
                check("image/png", &invalid),
                Err(AssetRejection::Invalid),
                "{raw:?}"
            );
        }
        let compressed = zlib(b"\0\x80\x80\x80");
        for suffix in [b"\0".as_slice(), compressed.as_slice()] {
            let mut invalid = bytes.clone();
            let mut appended = compressed.clone();
            appended.extend_from_slice(suffix);
            rewrite_chunk(&mut invalid, b"IDAT", &appended);
            assert_eq!(check("image/png", &invalid), Err(AssetRejection::Invalid));
        }
    }

    #[test]
    fn rejects_duplicate_headers_nonconsecutive_idat_and_invalid_chunk_order() {
        let bytes = png(1, 1);
        let header = &bytes[16..29];
        let mut duplicated = bytes.clone();
        duplicated.splice(33..33, chunk(b"IHDR", header));
        assert_eq!(
            check("image/png", &duplicated),
            Err(AssetRejection::Invalid)
        );

        let mut separated = bytes.clone();
        let start = separated.windows(4).position(|s| s == b"IDAT").unwrap() - 4;
        let length = be_u32(&separated[start..start + 4]).unwrap();
        let data = separated[start + 8..start + 8 + length].to_vec();
        let mut parts = chunk(b"IDAT", &data[..1]);
        parts.extend(chunk(b"tEXt", b"Comment\0split"));
        parts.extend(chunk(b"IDAT", &data[1..]));
        separated.splice(start..start + length + 12, parts);
        assert_eq!(check("image/png", &separated), Err(AssetRejection::Invalid));

        let mut after_end = bytes;
        after_end.extend(chunk(b"IEND", b""));
        assert_eq!(check("image/png", &after_end), Err(AssetRejection::Invalid));
    }

    #[test]
    fn refuses_indexed_png_without_palette_and_malformed_palette() {
        let mut bytes = png(1, 1);
        // Retain the original review reproducer's one-channel scanline, so
        // these files are not refused merely for a mismatched row byte count.
        let mut gray_header = bytes[16..29].to_vec();
        gray_header[9] = 0;
        rewrite_chunk(&mut bytes, b"IHDR", &gray_header);
        rewrite_chunk(&mut bytes, b"IDAT", &zlib(b"\0\x80"));
        let mut indexed = bytes.clone();
        let mut header = bytes[16..29].to_vec();
        header[9] = 3;
        rewrite_chunk(&mut indexed, b"IHDR", &header);
        assert_eq!(check("image/png", &indexed), Err(AssetRejection::Invalid));
        let mut malformed = bytes;
        malformed.splice(33..33, chunk(b"PLTE", &[0, 0, 0, 0]));
        assert_eq!(check("image/png", &malformed), Err(AssetRejection::Invalid));
    }

    #[test]
    fn accepts_rgba_transparency_and_preserves_exact_bytes() {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 2, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer
                .write_image_data(&[255, 0, 0, 0, 0, 255, 0, 128])
                .unwrap();
        }
        let original = bytes.clone();
        assert_eq!(check("image/png", &bytes), Ok(()));
        assert_eq!(bytes, original);
    }

    #[test]
    fn refuses_other_png_features_and_every_unrecognized_chunk() {
        let bytes = png(1, 1);
        for (field, value) in [(8, 16), (9, 0), (9, 3), (9, 4), (12, 1), (10, 1), (11, 1)] {
            let mut invalid = bytes.clone();
            let mut header = bytes[16..29].to_vec();
            header[field] = value;
            rewrite_chunk(&mut invalid, b"IHDR", &header);
            assert_eq!(
                check("image/png", &invalid),
                Err(AssetRejection::Invalid),
                "field {field}: {value}"
            );
        }
        for kind in [
            b"PLTE", b"tRNS", b"tEXt", b"gAMA", b"iCCP", b"iTXt", b"zTXt", b"eXIf", b"a1BC",
            b"abcd", b"ABCD",
        ] {
            let mut invalid = bytes.clone();
            invalid.splice(33..33, chunk(kind, b""));
            assert_eq!(
                check("image/png", &invalid),
                Err(AssetRejection::Invalid),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn accepts_consecutive_idat_fragments_and_rejects_idat_crc_corruption() {
        let mut bytes = png(1, 1);
        let start = bytes.windows(4).position(|s| s == b"IDAT").unwrap() - 4;
        let length = be_u32(&bytes[start..start + 4]).unwrap();
        let data = bytes[start + 8..start + 8 + length].to_vec();
        let mut parts = chunk(b"IDAT", &[]);
        parts.extend(chunk(b"IDAT", &data[..1]));
        parts.extend(chunk(b"IDAT", &data[1..]));
        bytes.splice(start..start + length + 12, parts);
        assert_eq!(check("image/png", &bytes), Ok(()));
        bytes[start + 8] ^= 1;
        assert_eq!(check("image/png", &bytes), Err(AssetRejection::Invalid));
    }
}
