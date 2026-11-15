//! The type an artifact is made from (decision 0059): a catalog type's id and
//! the release of it that the artifact keeps for good.

use std::fmt;
use std::str::FromStr;

use crate::identity::InvalidValue;

/// The longest type id, in bytes (decision 0059 rule 1).
pub const MAX_TYPE_ID: usize = 32;

/// A catalog type's id: a lowercase slug of 1 to 32 ASCII letters, digits and
/// hyphens, such as `react` (decision 0059 rule 1).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TypeId(String);

impl TypeId {
    /// The id as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for TypeId {
    type Err = InvalidValue;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let allowed = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-';
        if (1..=MAX_TYPE_ID).contains(&value.len()) && value.bytes().all(allowed) {
            Ok(Self(value.to_owned()))
        } else {
            Err(InvalidValue {
                what: "type id",
                rule: "expected 1 to 32 lowercase letters, digits, or hyphens",
            })
        }
    }
}

impl fmt::Display for TypeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A release of a type: a positive integer. A larger one is newer (decision
/// 0059 rule 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TypeRelease(u32);

impl TypeRelease {
    /// The release numbered `value`, or an error for 0.
    ///
    /// # Errors
    ///
    /// [`InvalidValue`] when `value` is 0.
    pub fn new(value: u32) -> Result<Self, InvalidValue> {
        if value == 0 {
            Err(InvalidValue {
                what: "type release",
                rule: "expected a positive integer",
            })
        } else {
            Ok(Self(value))
        }
    }

    /// The release number.
    #[must_use]
    pub fn get(self) -> u32 {
        self.0
    }
}

impl FromStr for TypeRelease {
    type Err = InvalidValue;

    /// Parses a decimal release without a sign or leading zeros.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = InvalidValue {
            what: "type release",
            rule: "expected a positive integer",
        };
        if value.starts_with('0') || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(invalid);
        }
        value
            .parse::<u32>()
            .map_err(|_| invalid)
            .and_then(Self::new)
    }
}

impl fmt::Display for TypeRelease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// The type an artifact was made from: the id and the release it keeps
/// (decision 0059 rules 5 and 6). An untyped artifact has none.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ArtifactType {
    /// The type's id.
    pub id: TypeId,
    /// The release the artifact was created with. It never changes.
    pub release: TypeRelease,
}

#[cfg(test)]
mod tests {
    use super::{TypeId, TypeRelease};

    #[test]
    fn type_ids_are_short_lowercase_slugs() {
        for valid in ["react", "a", "slide-deck-2", &"a".repeat(32)] {
            assert!(valid.parse::<TypeId>().is_ok(), "{valid}");
        }
        for invalid in ["", "React", "re act", "react_1", "é", &"a".repeat(33)] {
            assert!(invalid.parse::<TypeId>().is_err(), "{invalid}");
        }
    }

    #[test]
    fn releases_are_positive_decimals() {
        assert_eq!("1".parse::<TypeRelease>().map(TypeRelease::get), Ok(1));
        assert_eq!("42".parse::<TypeRelease>().map(TypeRelease::get), Ok(42));
        for invalid in ["0", "01", "-1", "+1", "", "1.0", "4294967296"] {
            assert!(invalid.parse::<TypeRelease>().is_err(), "{invalid}");
        }
        assert!(TypeRelease::new(0).is_err());
    }
}
