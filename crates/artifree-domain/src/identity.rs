//! Validated identifiers and labels. Parsing never trusts a caller's spelling.

use std::fmt;
use std::str::FromStr;

/// A value that does not satisfy its documented format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidValue {
    /// The kind of value that failed validation.
    pub what: &'static str,
    /// The rule that the value violated.
    pub rule: &'static str,
}

impl fmt::Display for InvalidValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid {}: {}", self.what, self.rule)
    }
}

impl std::error::Error for InvalidValue {}

const ARTIFACT_PREFIX: &str = "art_";
const ARTIFACT_HEX_LEN: usize = 32;

const TOKEN_ID_PREFIX: &str = "tok_";
const TOKEN_ID_HEX_LEN: usize = 32;

/// An opaque token identifier: `tok_` followed by 32 lowercase hex digits.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TokenId(String);

impl TokenId {
    /// Builds an identifier from caller-supplied random bytes.
    #[must_use]
    pub fn from_random(bytes: [u8; 16]) -> Self {
        let mut value = String::with_capacity(TOKEN_ID_PREFIX.len() + TOKEN_ID_HEX_LEN);
        value.push_str(TOKEN_ID_PREFIX);
        for byte in bytes {
            value.push(hex_digit(byte >> 4));
            value.push(hex_digit(byte & 0x0f));
        }
        Self(value)
    }

    /// The canonical string form.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for TokenId {
    type Err = InvalidValue;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let valid = value.strip_prefix(TOKEN_ID_PREFIX).is_some_and(|hex| {
            hex.len() == TOKEN_ID_HEX_LEN
                && hex
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        });
        if valid {
            Ok(Self(value.to_owned()))
        } else {
            Err(InvalidValue {
                what: "token id",
                rule: "expected tok_ followed by 32 lowercase hexadecimal digits",
            })
        }
    }
}

impl fmt::Display for TokenId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// An opaque artifact identifier: `art_` followed by 32 lowercase hex digits.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ArtifactId(String);

impl ArtifactId {
    /// Builds an identifier from caller-supplied random bytes.
    #[must_use]
    pub fn from_random(bytes: [u8; 16]) -> Self {
        let mut value = String::with_capacity(ARTIFACT_PREFIX.len() + ARTIFACT_HEX_LEN);
        value.push_str(ARTIFACT_PREFIX);
        for byte in bytes {
            value.push(hex_digit(byte >> 4));
            value.push(hex_digit(byte & 0x0f));
        }
        Self(value)
    }

    /// The canonical string form.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The lowercase hexadecimal digit of the low four bits of `nibble`.
pub(crate) fn hex_digit(nibble: u8) -> char {
    char::from(b"0123456789abcdef"[usize::from(nibble & 0x0f)])
}

impl FromStr for ArtifactId {
    type Err = InvalidValue;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let valid = value.strip_prefix(ARTIFACT_PREFIX).is_some_and(|hex| {
            hex.len() == ARTIFACT_HEX_LEN
                && hex
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        });
        if valid {
            Ok(Self(value.to_owned()))
        } else {
            Err(InvalidValue {
                what: "artifact id",
                rule: "expected art_ followed by 32 lowercase hexadecimal digits",
            })
        }
    }
}

impl fmt::Display for ArtifactId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The authenticated principal performing an operation.
///
/// Only the server's authentication boundary may construct an actor for a
/// request; an identifier received from artifact content never becomes one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ActorId(String);

impl ActorId {
    /// The canonical string form.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for ActorId {
    type Err = InvalidValue;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if (1..=64).contains(&value.len())
            && value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            Ok(Self(value.to_owned()))
        } else {
            Err(InvalidValue {
                what: "actor id",
                rule: "expected 1-64 lowercase letters, digits, or hyphens",
            })
        }
    }
}

impl fmt::Display for ActorId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A client-chosen key that makes a mutating request safe to retry.
///
/// Keys are scoped to the actor that sent them.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RequestKey(String);

impl RequestKey {
    /// The canonical string form.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for RequestKey {
    type Err = InvalidValue;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if (8..=128).contains(&value.len())
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
        {
            Ok(Self(value.to_owned()))
        } else {
            Err(InvalidValue {
                what: "request key",
                rule: "expected 8-128 ASCII letters, digits, '-', '_', '.', or ':'",
            })
        }
    }
}

/// Maximum title length in Unicode scalar values.
pub const MAX_TITLE_CHARS: usize = 200;

/// A display title without control characters or surrounding whitespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Title(String);

impl Title {
    /// The stored title.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The default title of a copy: this title followed by " (Copy)". A
    /// title too long for the suffix is shortened by whole characters, which
    /// can split a sequence the reader sees as one character.
    #[must_use]
    pub fn copy_of(&self) -> Self {
        const SUFFIX: &str = " (Copy)";
        let kept = MAX_TITLE_CHARS - SUFFIX.chars().count();
        let end = self
            .0
            .char_indices()
            .nth(kept)
            .map_or(self.0.len(), |(index, _)| index);
        // A valid title has no leading whitespace or control characters, so
        // neither has its prefix, and the suffix ends without whitespace.
        Self(format!("{}{SUFFIX}", &self.0[..end]))
    }
}

impl FromStr for Title {
    type Err = InvalidValue;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = |rule| {
            Err(InvalidValue {
                what: "title",
                rule,
            })
        };
        if value.trim() != value {
            return invalid("must not start or end with whitespace");
        }
        let length = value.chars().count();
        if length == 0 || length > MAX_TITLE_CHARS {
            return invalid("must contain 1-200 characters");
        }
        if value.chars().any(char::is_control) {
            return invalid("must not contain control characters");
        }
        Ok(Self(value.to_owned()))
    }
}

/// Milliseconds since the Unix epoch, as recorded by durable storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct UnixMillis(pub i64);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_id_round_trips_random_bytes() {
        let id = ArtifactId::from_random([0xab; 16]);
        assert_eq!(id.as_str(), format!("art_{}", "ab".repeat(16)));
        assert_eq!(id.as_str().parse::<ArtifactId>(), Ok(id));
    }

    #[test]
    fn token_id_formatting_and_validation() {
        let id = TokenId::from_random([0xab; 16]);
        assert_eq!(id.as_str(), &format!("tok_{}", "ab".repeat(16)));
        assert_eq!(id.to_string(), format!("tok_{}", "ab".repeat(16)));
        assert_eq!(id.as_str().parse::<TokenId>(), Ok(id));

        for value in [
            "",
            "tok_",
            &format!("tok_{}", "AB".repeat(16)),
            &format!("tok_{}", "a".repeat(31)),
            &format!("tok_{}", "a".repeat(33)),
            &format!("TOK_{}", "a".repeat(32)),
            &format!("tok_{}/", "a".repeat(31)),
        ] {
            assert!(value.parse::<TokenId>().is_err(), "accepted {value:?}");
        }
    }

    #[test]
    fn artifact_id_rejects_noncanonical_spellings() {
        for value in [
            "",
            "art_",
            &format!("art_{}", "AB".repeat(16)),
            &format!("art_{}", "a".repeat(31)),
            &format!("art_{}", "a".repeat(33)),
            &format!("ART_{}", "a".repeat(32)),
            &format!("art_{}/", "a".repeat(31)),
        ] {
            assert!(value.parse::<ArtifactId>().is_err(), "accepted {value:?}");
        }
    }

    #[test]
    fn title_rejects_padding_controls_and_length() {
        assert!("Quarterly plan".parse::<Title>().is_ok());
        assert!("가".repeat(200).parse::<Title>().is_ok());
        for value in ["", " padded", "line\nbreak", "bell\u{7}", &"x".repeat(201)] {
            assert!(value.parse::<Title>().is_err(), "accepted {value:?}");
        }
    }

    #[test]
    fn a_copy_title_is_a_valid_title_that_names_the_copy() {
        let copy = |title: &str| title.parse::<Title>().map(|title| title.copy_of());
        assert_eq!(
            copy("Plan").map(|title| title.0),
            Ok("Plan (Copy)".to_owned())
        );
        for title in ["가".repeat(193), "가".repeat(194), "x".repeat(200)] {
            let copied = copy(&title).map(|title| title.0).unwrap_or_default();
            assert_eq!(copied.chars().count(), title.chars().count().min(193) + 7);
            assert!(copied.ends_with(" (Copy)"), "{copied:?}");
            assert!(
                copied.parse::<Title>().is_ok(),
                "invalid copy title {copied:?}"
            );
        }
    }

    #[test]
    fn request_key_requires_bounded_portable_text() {
        assert!("cli-1f2e3d4c".parse::<RequestKey>().is_ok());
        for value in ["short", "has space-1234", "slash/123456", &"k".repeat(129)] {
            assert!(value.parse::<RequestKey>().is_err(), "accepted {value:?}");
        }
    }
}
