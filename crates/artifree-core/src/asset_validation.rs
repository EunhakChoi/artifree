//! Current content admission for new private uploads (0045).
//!
//! Only UTF-8 text/plain and text/csv are admitted. PNG and JPEG remain
//! unsupported until complete validation satisfies decision 0045, rule 3.
//! Call after authority/key/base preflight and outside storage transactions.
//! Replays bypass current admission, and accepted bytes remain unchanged.

use artifree_domain::assets::{AssetMediaType, AssetRejection, MAX_ASSET_BYTES};

/// Validates the current text-only upload policy.
///
/// Text and CSV stay opaque: markup and formulas are not rewritten. No image
/// decoder is linked or invoked; every new PNG/JPEG upload is unsupported.
///
/// # Errors
/// Unsupported types, empty or invalid UTF-8/control-bearing text, or byte overflow.
pub fn validate(content_type: &AssetMediaType, bytes: &[u8]) -> Result<(), AssetRejection> {
    if !matches!(content_type.as_str(), "text/plain" | "text/csv") {
        return Err(AssetRejection::UnsupportedType);
    }
    if bytes.len() > MAX_ASSET_BYTES {
        return Err(AssetRejection::LimitExceeded);
    }
    if bytes.is_empty() {
        return Err(AssetRejection::Invalid);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| AssetRejection::Invalid)?;
    if text
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\t' | '\r' | '\n'))
    {
        return Err(AssetRejection::Invalid);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(kind: &str, bytes: &[u8]) -> Result<(), AssetRejection> {
        let media: AssetMediaType = kind.parse().unwrap_or_else(|error| panic!("{error}"));
        validate(&media, bytes)
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
    fn image_admission_is_disabled_regardless_of_content() {
        for kind in ["image/png", "image/jpeg"] {
            for bytes in [
                b"".as_slice(),
                b"not an image",
                b"\x89PNG\r\n\x1a\n",
                b"\xff\xd8\xff\xd9",
            ] {
                assert_eq!(
                    check(kind, bytes),
                    Err(AssetRejection::UnsupportedType),
                    "{kind}: {bytes:?}"
                );
            }
        }
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
}
