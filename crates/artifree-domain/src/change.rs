//! Positions in the committed change feed of one database.
//!
//! A feed sequence counts commits in one database. It is not a source
//! revision: one sequence spans all artifacts. A cursor pairs it with the
//! feed's identity, so a cursor kept from another database is recognized
//! instead of being read as a position in this one.

use std::fmt;
use std::str::FromStr;

use crate::identity::{InvalidValue, hex_digit};

/// The number of commits recorded in a feed up to and including one change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChangeSequence(pub u64);

const FEED_BYTES: usize = 16;

/// The identity of one database's feed, chosen at random when the feed is
/// created.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FeedId([u8; FEED_BYTES]);

impl FeedId {
    /// Builds an identity from caller-supplied random bytes.
    #[must_use]
    pub fn from_random(bytes: [u8; FEED_BYTES]) -> Self {
        Self(bytes)
    }

    /// The identity's bytes, as stored.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; FEED_BYTES] {
        &self.0
    }
}

impl fmt::Display for FeedId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{}{}", hex_digit(byte >> 4), hex_digit(byte))?;
        }
        Ok(())
    }
}

/// A client's position in a feed: every change up to `sequence` was seen.
///
/// Its text form is the feed identity in 32 lowercase hexadecimal digits, a
/// dot, and the sequence in decimal without leading zeros. Clients treat it
/// as opaque.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangeCursor {
    /// The feed the position belongs to.
    pub feed: FeedId,
    /// The last change seen; 0 before the first commit.
    pub sequence: ChangeSequence,
}

impl fmt::Display for ChangeCursor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}", self.feed, self.sequence.0)
    }
}

impl FromStr for ChangeCursor {
    type Err = InvalidValue;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = InvalidValue {
            what: "change cursor",
            rule: "expected a cursor received from this feed",
        };
        let (feed, sequence) = value.split_once('.').ok_or(invalid)?;
        let canonical_number = !sequence.is_empty()
            && sequence.bytes().all(|byte| byte.is_ascii_digit())
            && (sequence == "0" || !sequence.starts_with('0'));
        if feed.len() != 2 * FEED_BYTES || !canonical_number {
            return Err(invalid);
        }
        let mut bytes = [0; FEED_BYTES];
        for (byte, pair) in bytes.iter_mut().zip(feed.as_bytes().chunks_exact(2)) {
            *byte = (nibble(pair[0]).ok_or(invalid)? << 4) | nibble(pair[1]).ok_or(invalid)?;
        }
        Ok(Self {
            feed: FeedId(bytes),
            sequence: ChangeSequence(sequence.parse().map_err(|_| invalid)?),
        })
    }
}

/// The value of a lowercase hexadecimal digit.
fn nibble(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cursor(sequence: u64) -> ChangeCursor {
        ChangeCursor {
            feed: FeedId::from_random([0x0f; 16]),
            sequence: ChangeSequence(sequence),
        }
    }

    #[test]
    fn a_cursor_round_trips_through_its_text_form() {
        for value in [cursor(0), cursor(42), cursor(u64::MAX)] {
            let text = value.to_string();
            assert_eq!(text.parse::<ChangeCursor>(), Ok(value), "{text}");
        }
        assert_eq!(cursor(7).to_string(), format!("{}.7", "0f".repeat(16)));
    }

    #[test]
    fn noncanonical_cursors_are_rejected() {
        let feed = "0f".repeat(16);
        for value in [
            String::new(),
            "7".to_owned(),
            feed.clone(),
            format!("{feed}."),
            format!("{feed}.07"),
            format!("{feed}.+7"),
            format!("{feed}.-1"),
            format!("{feed}.18446744073709551616"),
            format!("{}.7", "0F".repeat(16)),
            format!("{}.7", "0f".repeat(15)),
            format!("{feed}0.7"),
            format!("{feed}.7.1"),
        ] {
            assert!(value.parse::<ChangeCursor>().is_err(), "accepted {value:?}");
        }
    }
}
