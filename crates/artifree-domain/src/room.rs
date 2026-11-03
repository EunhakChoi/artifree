//! Room vocabulary (decision 0034): validated labels and texts, and the
//! counters a room hands out. Nothing here is stored; a room lives in the
//! memory of one server process.

use std::fmt;
use std::str::FromStr;

use crate::identity::InvalidValue;

/// Longest slot or event kind, in characters.
pub const MAX_LABEL_CHARS: usize = 32;
/// Largest presence state, in UTF-8 bytes.
pub const MAX_STATE_BYTES: usize = 256;
/// Largest event data, in UTF-8 bytes.
pub const MAX_EVENT_DATA_BYTES: usize = 512;

/// A label made of ASCII letters, digits, `.`, `_` and `-`, 1 to
/// [`MAX_LABEL_CHARS`] characters.
fn label(value: &str, what: &'static str) -> Result<String, InvalidValue> {
    let valid = (1..=MAX_LABEL_CHARS).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if valid {
        Ok(value.to_owned())
    } else {
        Err(InvalidValue {
            what,
            rule: "expected 1-32 ASCII letters, digits, '.', '_' or '-'",
        })
    }
}

/// The name a tab gives one of its participants, such as `page`. A slot is
/// only a name inside the tab that chose it: another tab's slot of the same
/// spelling is another participant. It identifies nothing across tabs and is
/// never shown to other participants.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Slot(String);

impl Slot {
    /// The canonical string form.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Slot {
    type Err = InvalidValue;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        label(value, "room slot").map(Self)
    }
}

/// What an ephemeral event is about, chosen by the sender, such as `cursor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventKind(String);

impl EventKind {
    /// The canonical string form.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for EventKind {
    type Err = InvalidValue;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        label(value, "event kind").map(Self)
    }
}

/// Text of at most `MAX` UTF-8 bytes. The room does not read it: a sender
/// and its readers agree on its meaning, and nothing parses it as JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedText<const MAX: usize>(String);

impl<const MAX: usize> BoundedText<MAX> {
    /// The empty text.
    #[must_use]
    pub fn empty() -> Self {
        Self(String::new())
    }

    /// The text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<const MAX: usize> FromStr for BoundedText<MAX> {
    type Err = InvalidValue;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() <= MAX {
            Ok(Self(value.to_owned()))
        } else {
            Err(InvalidValue {
                what: "room text",
                rule: "longer than its limit in bytes",
            })
        }
    }
}

impl<const MAX: usize> fmt::Display for BoundedText<MAX> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// What a participant tells the others about itself, at most
/// [`MAX_STATE_BYTES`] bytes. The latest value replaces the earlier one.
pub type PresenceState = BoundedText<MAX_STATE_BYTES>;
/// The data of one ephemeral event, at most [`MAX_EVENT_DATA_BYTES`] bytes.
pub type EventData = BoundedText<MAX_EVENT_DATA_BYTES>;

/// A participant of a room: one connection of a tab, for example the
/// workspace page, and later a frame in it. The server assigns it; it is a
/// label other participants see, never a credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ParticipantId(pub u64);

/// A tab as one room tells tabs apart. The label is comparable only inside
/// one room while the tab has a participant there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TabLabel(pub u64);

/// The generation of a participant's connection. A join that resumes a
/// participant gets a newer one, and the older connection can no longer act.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Epoch(pub u64);

/// The position of an ephemeral event in one room, from 1. It is not a
/// change feed sequence and not a source revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EventSeq(pub u64);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_accept_only_the_documented_characters_and_length() {
        for ok in ["page", "frame-1", "a.b_c", &"x".repeat(MAX_LABEL_CHARS)] {
            assert!(ok.parse::<Slot>().is_ok(), "{ok}");
            assert!(ok.parse::<EventKind>().is_ok(), "{ok}");
        }
        for bad in [
            "",
            "has space",
            "slash/",
            "é",
            "tab\t",
            &"x".repeat(MAX_LABEL_CHARS + 1),
        ] {
            assert!(bad.parse::<Slot>().is_err(), "{bad:?}");
            assert!(bad.parse::<EventKind>().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn text_is_bounded_in_bytes_not_characters() {
        assert!("x".repeat(MAX_STATE_BYTES).parse::<PresenceState>().is_ok());
        assert!(
            "x".repeat(MAX_STATE_BYTES + 1)
                .parse::<PresenceState>()
                .is_err()
        );
        // 129 two-byte characters are 258 bytes and 129 characters.
        assert!("é".repeat(129).parse::<PresenceState>().is_err());
        assert!("é".repeat(128).parse::<PresenceState>().is_ok());
        assert!(
            "x".repeat(MAX_EVENT_DATA_BYTES + 1)
                .parse::<EventData>()
                .is_err()
        );
    }
}
