//! Current content admission for new private uploads (0045).
//!
//! UTF-8 text/plain and text/csv are admitted as opaque text. PNG and JPEG are
//! admitted when complete validation passes (`png::validate_png`,
//! `jpeg::validate_jpeg`, decision 0045, rule 3).
//! Call after authority/key/base preflight and outside storage transactions.
//! Replays bypass current admission, and accepted bytes remain unchanged.

use std::sync::{Mutex, MutexGuard, PoisonError};

use artifree_domain::assets::{AssetMediaType, AssetRejection, MAX_ASSET_BYTES};

mod jpeg;
mod png;

/// Largest accepted PNG or JPEG width or height, in pixels.
const MAX_IMAGE_AXIS: u32 = 4096;

/// Largest accepted PNG or JPEG pixel count, width times height.
const MAX_IMAGE_PIXELS: u64 = 16_777_216;

/// Byte budget for PNG metadata. The `png` decoder counts its buffered chunks,
/// including text and `iCCP`, against it. Compressed text is inflated within it too.
const PNG_METADATA_BUDGET: usize = 16 * 1024 * 1024;

/// Serializes image validation (PNG and JPEG) process-wide, so one image is
/// decoded at a time.
///
/// The lock guards no data, so a poisoned lock is recovered rather than refused.
static IMAGE_DECODE: Mutex<()> = Mutex::new(());

#[cfg(test)]
pub(crate) mod fixtures;

#[cfg(test)]
pub(crate) mod jpeg_fixtures;

/// Validates the upload policy for a declared content type.
///
/// Text and CSV stay opaque: markup and formulas are not rewritten. PNG is
/// checked by `png::validate_png` and JPEG by `jpeg::validate_jpeg`, which borrow
/// the bytes and never re-encode or strip them.
///
/// # Errors
/// `UnsupportedType` for a type outside the allowlist; `LimitExceeded` above
/// `MAX_ASSET_BYTES`; `Invalid` for empty bytes, invalid or control-bearing text,
/// and a PNG or JPEG that lacks the signature or fails validation (including a
/// valid JPEG variant outside the admitted subset).
pub fn validate(content_type: &AssetMediaType, bytes: &[u8]) -> Result<(), AssetRejection> {
    let kind = content_type.as_str();
    if !matches!(kind, "text/plain" | "text/csv" | "image/png" | "image/jpeg") {
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
        "image/png" => validate_png_upload(bytes),
        // Only image/jpeg is left after the allowlist above.
        _ => validate_jpeg_upload(bytes),
    }
}

/// Checks UTF-8 text, refusing control characters other than tab, CR and LF.
fn validate_text(bytes: &[u8]) -> Result<(), AssetRejection> {
    let text = std::str::from_utf8(bytes).map_err(|_| AssetRejection::Invalid)?;
    if text
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\t' | '\r' | '\n'))
    {
        return Err(AssetRejection::Invalid);
    }
    Ok(())
}

/// Checks the PNG signature, then runs the PNG validator under the decode lock.
///
/// The signature check comes first so that non-PNG bytes never wait for the lock.
fn validate_png_upload(bytes: &[u8]) -> Result<(), AssetRejection> {
    if !bytes.starts_with(&png::SIGNATURE) {
        return Err(AssetRejection::Invalid);
    }
    let _decode = lock_image_decode();
    png::validate_png(bytes)
}

/// Checks the JPEG signature, then runs the JPEG validator under the decode lock.
///
/// The signature check comes first so that non-JPEG bytes never wait for the lock.
fn validate_jpeg_upload(bytes: &[u8]) -> Result<(), AssetRejection> {
    if !bytes.starts_with(&jpeg::SIGNATURE) {
        return Err(AssetRejection::Invalid);
    }
    let _decode = lock_image_decode();
    jpeg::validate_jpeg(bytes)
}

/// Locks `IMAGE_DECODE`, recovering the guard if an earlier holder panicked.
fn lock_image_decode() -> MutexGuard<'static, ()> {
    IMAGE_DECODE.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::fixtures::minimal_png;
    use super::jpeg_fixtures::minimal_jpeg;
    use super::*;

    // Written out here, so the tests do not reuse the constants that the code under test uses.
    const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    const JPEG_SIGNATURE: &[u8] = b"\xff\xd8\xff";

    fn check(kind: &str, bytes: &[u8]) -> Result<(), AssetRejection> {
        let media: AssetMediaType = kind.parse().unwrap_or_else(|error| panic!("{error}"));
        validate(&media, bytes)
    }

    /// Returns `length` bytes that start with `signature` and are zero after it.
    fn signature_then_zeros(signature: &[u8], length: usize) -> Vec<u8> {
        let mut bytes = signature.to_vec();
        bytes.resize(length, 0);
        bytes
    }

    #[test]
    fn text_bounds_preserve_opaque_unicode_markup_and_csv_formulas() {
        assert_eq!(check("text/plain", b""), Err(AssetRejection::Invalid));
        assert_eq!(check("text/plain", b"a"), Ok(()));
        assert_eq!(check("text/plain", &vec![b'a'; MAX_ASSET_BYTES]), Ok(()));
        assert_eq!(
            check("text/plain", &vec![b'a'; MAX_ASSET_BYTES + 1]),
            Err(AssetRejection::LimitExceeded)
        );
        assert_eq!(check("text/csv", b"=1+1,\"<script>\"\r\n\t"), Ok(()));
        assert_eq!(check("text/plain", "한글😀".as_bytes()), Ok(()));
    }

    #[test]
    fn invalid_utf8_and_controls_are_refused_for_both_text_types() {
        for bytes in [
            b"\xff".as_slice(),
            b"\0",
            b"\x01",
            b"\x7f",
            "\u{85}".as_bytes(),
        ] {
            for kind in ["text/plain", "text/csv"] {
                assert_eq!(
                    check(kind, bytes),
                    Err(AssetRejection::Invalid),
                    "{kind}: {bytes:?}"
                );
            }
        }
    }

    #[test]
    fn a_complete_png_is_admitted() {
        // Byte preservation is guarded by the server test that compares stored bytes.
        assert_eq!(
            check("image/png", &minimal_png()),
            Ok(()),
            "a 1x1 PNG is complete"
        );
    }

    #[test]
    fn png_that_does_not_match_the_declared_type_is_invalid() {
        for bytes in [
            b"not an image".as_slice(),
            b"\xff\xd8\xff\xd9",
            // A JPEG start-of-image and APP0 header, with no PNG signature.
            b"\xff\xd8\xff\xe0\x00\x10JFIF\x00",
            b"hello, world\n",
            // The signature alone is truncated: a PNG needs at least one chunk after it.
            PNG_SIGNATURE,
        ] {
            assert_eq!(
                check("image/png", bytes),
                Err(AssetRejection::Invalid),
                "image/png over {bytes:?}"
            );
        }
        // The PNG signature byte is not UTF-8, and the bytes include control characters.
        assert_eq!(
            check("text/plain", &minimal_png()),
            Err(AssetRejection::Invalid),
            "a PNG declared as text/plain"
        );
        assert_eq!(
            check("image/png", b""),
            Err(AssetRejection::Invalid),
            "empty bytes declared as image/png"
        );
    }

    #[test]
    fn png_over_the_asset_size_limit_is_a_limit_error_before_decoding() {
        // Signature then zeros: the PNG validator refuses this as Invalid, so a
        // LimitExceeded result shows that the size check runs before the PNG branch.
        let over_limit = signature_then_zeros(PNG_SIGNATURE, MAX_ASSET_BYTES + 1);
        assert_eq!(
            check("image/png", &over_limit),
            Err(AssetRejection::LimitExceeded),
            "one byte over the limit"
        );
        // Exactly at the limit the size check passes, and the zeros then fail PNG validation.
        let at_limit = signature_then_zeros(PNG_SIGNATURE, MAX_ASSET_BYTES);
        assert_eq!(
            check("image/png", &at_limit),
            Err(AssetRejection::Invalid),
            "exactly at the limit"
        );
    }

    #[test]
    fn a_complete_jpeg_is_admitted() {
        assert_eq!(
            check("image/jpeg", &minimal_jpeg()),
            Ok(()),
            "an 8x8 grayscale JPEG is complete"
        );
    }

    #[test]
    fn jpeg_that_does_not_match_the_declared_type_is_invalid() {
        let png_bytes = minimal_png();
        for bytes in [
            png_bytes.as_slice(),
            b"not an image",
            b"hello, world\n",
            // The signature alone is truncated: markers must follow it.
            JPEG_SIGNATURE,
            // Start of image and end of image, with no frame or scan between them.
            b"\xff\xd8\xff\xd9",
        ] {
            assert_eq!(
                check("image/jpeg", bytes),
                Err(AssetRejection::Invalid),
                "image/jpeg over {bytes:?}"
            );
        }
        // A JPEG declared as a PNG has no PNG signature.
        assert_eq!(
            check("image/png", &minimal_jpeg()),
            Err(AssetRejection::Invalid),
            "a JPEG declared as image/png"
        );
        // A JPEG starts with 0xFF, which is not UTF-8, and it contains control bytes.
        assert_eq!(
            check("text/plain", &minimal_jpeg()),
            Err(AssetRejection::Invalid),
            "a JPEG declared as text/plain"
        );
        assert_eq!(
            check("image/jpeg", b""),
            Err(AssetRejection::Invalid),
            "empty bytes declared as image/jpeg"
        );
    }

    #[test]
    fn jpeg_over_the_asset_size_limit_is_a_limit_error_before_validation() {
        // Signature then zeros: the JPEG validator refuses this as Invalid, so a LimitExceeded
        // result shows that the size check runs before the JPEG branch.
        let over_limit = signature_then_zeros(JPEG_SIGNATURE, MAX_ASSET_BYTES + 1);
        assert_eq!(
            check("image/jpeg", &over_limit),
            Err(AssetRejection::LimitExceeded),
            "one byte over the limit"
        );
        // Exactly at the limit the size check passes, and the zeros then fail JPEG validation.
        let at_limit = signature_then_zeros(JPEG_SIGNATURE, MAX_ASSET_BYTES);
        assert_eq!(
            check("image/jpeg", &at_limit),
            Err(AssetRejection::Invalid),
            "exactly at the limit"
        );
    }

    #[test]
    fn a_valid_jpeg_variant_outside_the_admitted_subset_is_invalid() {
        // Every refused JPEG is `Invalid` (`invalid_asset`), including a valid variant that is
        // outside the admitted subset. `UnsupportedType` is only for a declared type that is not
        // on the allowlist.
        let admitted = minimal_jpeg();
        assert_eq!(
            check("image/jpeg", &admitted),
            Ok(()),
            "the unchanged file must be admitted, so the refusal below comes from the marker"
        );
        // The only edit is the frame marker: SOF0 (0xC0) becomes SOF2 (0xC2), which is progressive.
        let mut progressive = admitted;
        let frame = progressive
            .windows(2)
            .position(|pair| pair == [0xFF, 0xC0])
            .unwrap_or_else(|| panic!("the minimal JPEG has an SOF0 marker"));
        progressive[frame + 1] = 0xC2;
        assert_eq!(
            check("image/jpeg", &progressive),
            Err(AssetRejection::Invalid),
            "a progressive (SOF2) frame is outside the admitted subset"
        );
    }

    #[test]
    fn other_types_remain_unsupported() {
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
    fn other_image_types_are_unsupported_even_with_png_or_jpeg_content() {
        let png_bytes = minimal_png();
        let jpeg_bytes = minimal_jpeg();
        for kind in ["image/gif", "image/webp", "image/svg+xml", "image/apng"] {
            for bytes in [
                b"GIF89a".as_slice(),
                png_bytes.as_slice(),
                jpeg_bytes.as_slice(),
            ] {
                assert_eq!(
                    check(kind, bytes),
                    Err(AssetRejection::UnsupportedType),
                    "{kind} over {bytes:?}"
                );
            }
        }
    }

    #[test]
    fn concurrent_image_validations_do_not_deadlock_or_cross_talk() {
        const WORKERS: usize = 8;
        const ROUNDS: usize = 20;
        let valid_png = minimal_png();
        // Flipping the last byte corrupts the CRC of IEND. The signature still matches, so
        // the corrupt image takes the decode lock, as the valid one does, before it is refused.
        let mut corrupt_png = minimal_png();
        if let Some(last) = corrupt_png.last_mut() {
            *last ^= 1;
        }
        let valid_jpeg = minimal_jpeg();
        // Dropping the final byte leaves the file ending in 0xFF, the first byte of EOI.
        // The signature still matches, so the truncated image takes the decode lock first.
        let truncated_jpeg = &valid_jpeg[..valid_jpeg.len() - 1];
        std::thread::scope(|scope| {
            let workers: Vec<_> = (0..WORKERS)
                .map(|_| {
                    scope.spawn(|| {
                        for _ in 0..ROUNDS {
                            assert_eq!(check("image/png", &valid_png), Ok(()), "valid PNG");
                            assert_eq!(
                                check("image/png", &corrupt_png),
                                Err(AssetRejection::Invalid),
                                "corrupt PNG"
                            );
                            assert_eq!(check("image/jpeg", &valid_jpeg), Ok(()), "valid JPEG");
                            assert_eq!(
                                check("image/jpeg", truncated_jpeg),
                                Err(AssetRejection::Invalid),
                                "truncated JPEG"
                            );
                        }
                    })
                })
                .collect();
            for worker in workers {
                worker
                    .join()
                    .unwrap_or_else(|_| panic!("a validation worker panicked"));
            }
        });
    }

    #[test]
    fn non_png_bytes_are_refused_without_waiting_for_the_decode_lock() {
        // The decode lock is held while a non-PNG upload is validated on another thread.
        // The upload must be refused without the lock, so the result arrives well inside
        // the timeout. The timeout only bounds the wait when the lock is taken.
        let decode = lock_image_decode();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            sender.send(check("image/png", b"not an image")).ok();
        });
        let outcome = receiver.recv_timeout(Duration::from_secs(10));
        drop(decode);
        assert_eq!(
            outcome,
            Ok(Err(AssetRejection::Invalid)),
            "a non-PNG upload waited for the decode lock or was refused differently"
        );
    }

    #[test]
    fn non_jpeg_bytes_are_refused_without_waiting_for_the_decode_lock() {
        // The decode lock is held while a non-JPEG upload is validated on another thread.
        // The upload must be refused without the lock, so the result arrives well inside
        // the timeout. The timeout only bounds the wait when the lock is taken.
        let decode = lock_image_decode();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            sender.send(check("image/jpeg", b"not an image")).ok();
        });
        let outcome = receiver.recv_timeout(Duration::from_secs(10));
        drop(decode);
        assert_eq!(
            outcome,
            Ok(Err(AssetRejection::Invalid)),
            "a non-JPEG upload waited for the decode lock or was refused differently"
        );
    }
}
