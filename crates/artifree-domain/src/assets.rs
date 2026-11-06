//! Private immutable upload vocabulary, independent of source history (0045).

use std::fmt;
use std::str::FromStr;

use crate::identity::{InvalidValue, UnixMillis};

/// Largest accepted decoded upload.
pub const MAX_ASSET_BYTES: usize = 5 * 1024 * 1024;
/// Maximum associations in one space.
pub const MAX_SPACE_ASSETS: u64 = 64;
/// Maximum logical bytes in one space.
pub const MAX_SPACE_BYTES: u64 = 25 * 1024 * 1024;
/// Maximum associations per actor, including trash.
pub const MAX_ACTOR_ASSETS: u64 = 256;
/// Maximum logical bytes per actor, including trash.
pub const MAX_ACTOR_BYTES: u64 = 100 * 1024 * 1024;

/// An independently versioned actor/artifact asset space.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AssetVersion(pub u64);

/// An opaque identifier reserved for the lifetime of the database.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AssetId(String);

impl AssetId {
    /// Encodes a cryptographically random 256-bit value supplied by the provider.
    #[must_use]
    pub fn from_random(bytes: [u8; 32]) -> Self {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut text = String::with_capacity(64);
        for byte in bytes {
            text.push(char::from(HEX[usize::from(byte >> 4)]));
            text.push(char::from(HEX[usize::from(byte & 15)]));
        }
        Self(text)
    }

    /// The lowercase hexadecimal transport spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for AssetId {
    type Err = InvalidValue;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err(InvalidValue {
                what: "asset ID",
                rule: "expected 64 lowercase hexadecimal characters",
            });
        }
        Ok(Self(value.to_owned()))
    }
}

/// Stable envelope MIME spelling; admission policy is deliberately separate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetMediaType(String);
impl AssetMediaType {
    /// The declared, parameter-free MIME spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl FromStr for AssetMediaType {
    type Err = InvalidValue;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let valid_part = |part: &str| {
            !part.is_empty()
                && part.bytes().all(|c| {
                    c.is_ascii_lowercase() || c.is_ascii_digit() || b"!#$%&'*+-.^_`|~".contains(&c)
                })
        };
        let valid = value.len() <= 127
            && value
                .split_once('/')
                .is_some_and(|(left, right)| valid_part(left) && valid_part(right));
        if !valid {
            return Err(InvalidValue {
                what: "content_type",
                rule: "expected lowercase parameter-free MIME type",
            });
        }
        Ok(Self(value.to_owned()))
    }
}

/// Immutable metadata; the digest is a fingerprint, never an access credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetMetadata {
    /// Opaque scoped identifier.
    pub id: AssetId,
    /// Validated normalized media type.
    pub content_type: AssetMediaType,
    /// Exact stored byte count.
    pub size: u64,
    /// SHA-256 of the exact bytes.
    pub digest: [u8; 32],
    /// Server time of the first commit.
    pub created_at: UnixMillis,
}

/// Current content admission refusal; rejected writes reserve nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AssetRejection {
    /// The MIME type has no enabled validator.
    UnsupportedType,
    /// Empty, malformed, or disallowed content.
    Invalid,
    /// A decoded byte, association count, or logical storage budget was exceeded.
    LimitExceeded,
}
impl fmt::Display for AssetRejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnsupportedType => "asset type is unsupported",
            Self::Invalid => "invalid asset content",
            Self::LimitExceeded => "asset limit exceeded",
        })
    }
}
impl std::error::Error for AssetRejection {}
