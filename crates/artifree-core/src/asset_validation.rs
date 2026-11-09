//! Current content admission for new private uploads (0045).
//!
//! UTF-8 text/plain and text/csv are admitted as opaque text. PNG is admitted
//! when complete validation passes (`png::validate_png`, decision 0045, rule
//! 3). JPEG remains unsupported until it satisfies that rule.
//! Call after authority/key/base preflight and outside storage transactions.
//! Replays bypass current admission, and accepted bytes remain unchanged.

use std::sync::{Mutex, MutexGuard, PoisonError};

use artifree_domain::assets::{AssetMediaType, AssetRejection, MAX_ASSET_BYTES};

mod png;

/// Largest accepted PNG width or height, in pixels.
const MAX_IMAGE_AXIS: u32 = 4096;

/// Largest accepted PNG pixel count, width times height.
const MAX_IMAGE_PIXELS: u64 = 16_777_216;

/// Byte budget for PNG metadata. The `png` decoder counts its buffered chunks,
/// including text and `iCCP`, against it. Compressed text is inflated within it too.
const PNG_METADATA_BUDGET: usize = 16 * 1024 * 1024;

/// Serializes PNG decoding process-wide, so one image is inflated at a time.
///
/// The lock guards no data, so a poisoned lock is recovered rather than refused.
static IMAGE_DECODE: Mutex<()> = Mutex::new(());

#[cfg(test)]
pub(crate) mod fixtures;

/// Validates the upload policy for a declared content type.
///
/// Text and CSV stay opaque: markup and formulas are not rewritten. PNG is
/// checked by `png::validate_png`, which borrows the bytes and never re-encodes
/// or strips them. JPEG is unsupported for now.
///
/// # Errors
/// `UnsupportedType` for a type outside the allowlist and for `image/jpeg`;
/// `LimitExceeded` above `MAX_ASSET_BYTES`; `Invalid` for empty bytes, invalid
/// or control-bearing text, and a PNG that lacks the signature or fails
/// validation.
pub fn validate(content_type: &AssetMediaType, bytes: &[u8]) -> Result<(), AssetRejection> {
    let kind = content_type.as_str();
    // image/jpeg stays outside the allowlist until it meets rule 3, so every JPEG
    // upload, whatever its size or content, is unsupported (not invalid or oversize).
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
        // Only image/png passes the allowlist above.
        _ => validate_png_upload(bytes),
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

/// Locks `IMAGE_DECODE`, recovering the guard if an earlier holder panicked.
fn lock_image_decode() -> MutexGuard<'static, ()> {
    IMAGE_DECODE.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::fixtures::minimal_png;
    use super::*;

    // Written out here, so the tests do not reuse the constant that the code under test uses.
    const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

    fn check(kind: &str, bytes: &[u8]) -> Result<(), AssetRejection> {
        let media: AssetMediaType = kind.parse().unwrap_or_else(|error| panic!("{error}"));
        validate(&media, bytes)
    }

    /// Returns `length` bytes that start with the PNG signature and are zero after it.
    fn signature_then_zeros(length: usize) -> Vec<u8> {
        let mut bytes = PNG_SIGNATURE.to_vec();
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
        let bytes = minimal_png();
        let original = bytes.clone();
        assert_eq!(check("image/png", &bytes), Ok(()), "a 1x1 PNG is complete");
        // `validate` borrows its input as `&[u8]`, so this comparison cannot fail by
        // construction. It records that admission leaves the uploaded bytes unchanged.
        assert_eq!(bytes, original, "validation must not change the bytes");
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
        assert_eq!(
            check("image/png", &signature_then_zeros(MAX_ASSET_BYTES + 1)),
            Err(AssetRejection::LimitExceeded),
            "one byte over the limit"
        );
        // Exactly at the limit the size check passes, and the zeros then fail PNG validation.
        assert_eq!(
            check("image/png", &signature_then_zeros(MAX_ASSET_BYTES)),
            Err(AssetRejection::Invalid),
            "exactly at the limit"
        );
    }

    #[test]
    fn jpeg_stays_unsupported() {
        for bytes in [
            b"not an image".as_slice(),
            b"\xff\xd8\xff\xd9",
            // One byte, the first byte of a PNG signature: only the declared type can refuse it.
            b"\x89",
        ] {
            assert_eq!(
                check("image/jpeg", bytes),
                Err(AssetRejection::UnsupportedType),
                "image/jpeg over {bytes:?}"
            );
        }
        // The type check comes first: an empty or oversize JPEG is unsupported too,
        // never invalid or over the limit, as before PNG admission existed.
        assert_eq!(
            check("image/jpeg", b""),
            Err(AssetRejection::UnsupportedType),
            "empty image/jpeg"
        );
        assert_eq!(
            check("image/jpeg", &vec![0_u8; MAX_ASSET_BYTES + 1]),
            Err(AssetRejection::UnsupportedType),
            "image/jpeg one byte over the limit"
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
    fn other_image_types_are_unsupported_even_with_png_content() {
        let png_bytes = minimal_png();
        for kind in ["image/gif", "image/webp", "image/svg+xml", "image/apng"] {
            for bytes in [b"GIF89a".as_slice(), png_bytes.as_slice()] {
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
        let valid = minimal_png();
        // Flipping the last byte corrupts the CRC of IEND. The signature still matches, so
        // the corrupt image takes the decode lock, as the valid one does, before it is refused.
        let mut corrupt = minimal_png();
        if let Some(last) = corrupt.last_mut() {
            *last ^= 1;
        }
        std::thread::scope(|scope| {
            let workers: Vec<_> = (0..WORKERS)
                .map(|_| {
                    scope.spawn(|| {
                        for _ in 0..ROUNDS {
                            assert_eq!(check("image/png", &valid), Ok(()), "valid PNG");
                            assert_eq!(
                                check("image/png", &corrupt),
                                Err(AssetRejection::Invalid),
                                "corrupt PNG"
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
}
