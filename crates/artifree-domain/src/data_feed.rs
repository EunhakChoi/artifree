//! What a data subscription sends (decision 0038): the position a client has
//! reached, and the effect of one committed write on a watched path.
//!
//! A position is a data version, never a source revision or a change feed
//! sequence, and its text form differs from a change feed cursor so that one
//! cannot be taken for the other.

use std::fmt;
use std::str::FromStr;

use crate::AppDataVersion;
use crate::change::FeedId;
use crate::data::{DataPath, DataValue};
use crate::identity::InvalidValue;

/// Extra weight of an edit besides its value, in bytes: the path and the
/// bookkeeping it costs to keep.
const EDIT_OVERHEAD: usize = 64;

/// A client's position in the data of one artifact: every write up to
/// `version` was seen. Its text form is `d.`, the feed identity of the
/// database in 32 lowercase hexadecimal digits, a dot, and the version in
/// decimal without leading zeros. Clients treat it as opaque.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataCursor {
    /// The database the position belongs to (decision 0006).
    pub feed: FeedId,
    /// The data version up to which writes were seen.
    pub version: AppDataVersion,
}

impl fmt::Display for DataCursor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "d.{}.{}", self.feed, self.version.0)
    }
}

impl FromStr for DataCursor {
    type Err = InvalidValue;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = InvalidValue {
            what: "data cursor",
            rule: "expected a data cursor received from a data stream",
        };
        let rest = value.strip_prefix("d.").ok_or(invalid)?;
        let (feed, version) = rest.split_once('.').ok_or(invalid)?;
        let canonical_number = !version.is_empty()
            && version.bytes().all(|byte| byte.is_ascii_digit())
            && (version == "0" || !version.starts_with('0'));
        if !canonical_number {
            return Err(invalid);
        }
        Ok(Self {
            feed: FeedId::from_hex(feed).ok_or(invalid)?,
            version: AppDataVersion(version.parse().map_err(|_| invalid)?),
        })
    }
}

/// What one committed write did to the space, stated as its result: after
/// the write at `version`, the subtree at `path` is `value`, or is gone.
#[derive(Debug, Clone, PartialEq)]
pub struct DataEdit {
    /// The data version the write left the space at.
    pub version: AppDataVersion,
    /// The path the write addressed.
    pub path: DataPath,
    /// What is at the path afterwards; `None` where the write removed it.
    pub value: Option<DataValue>,
}

/// The effect of a [`DataEdit`] on a watched path: after the write, the value
/// at `path` is `value`, or is gone.
#[derive(Debug, Clone, PartialEq)]
pub struct Seen {
    /// The watched path, or a path below it that the write addressed.
    pub path: DataPath,
    /// What is at `path` afterwards.
    pub value: Option<DataValue>,
}

impl DataEdit {
    /// Bytes this edit costs to keep: its canonical value and a fixed
    /// overhead.
    #[must_use]
    pub fn weight(&self) -> usize {
        EDIT_OVERHEAD
            + self
                .value
                .as_ref()
                .map_or(0, |value| value.to_canonical().len())
    }

    /// What this write did to `watched`, or `None` when it did not touch it.
    ///
    /// A write below the watched path is reported at its own path. A write at
    /// the watched path or above it is reported at the watched path with the
    /// part of its result that lies there.
    #[must_use]
    pub fn seen_from(&self, watched: &DataPath) -> Option<Seen> {
        if self.path.starts_with(watched) {
            return Some(Seen {
                path: self.path.clone(),
                value: self.value.clone(),
            });
        }
        if watched.starts_with(&self.path) {
            let below = &watched.segments()[self.path.segments().len()..];
            return Some(Seen {
                path: watched.clone(),
                value: self
                    .value
                    .as_ref()
                    .and_then(|value| value.at(below))
                    .cloned(),
            });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(text: &str) -> DataPath {
        text.parse()
            .unwrap_or_else(|error| panic!("{text}: {error}"))
    }

    fn value(text: &str) -> DataValue {
        DataValue::parse(text).unwrap_or_else(|error| panic!("{text}: {error}"))
    }

    fn edit(at: &str, result: Option<&str>) -> DataEdit {
        DataEdit {
            version: AppDataVersion(7),
            path: path(at),
            value: result.map(value),
        }
    }

    fn cursor(version: u64) -> DataCursor {
        DataCursor {
            feed: FeedId::from_random([0x0f; 16]),
            version: AppDataVersion(version),
        }
    }

    #[test]
    fn a_data_cursor_round_trips_and_is_not_a_change_cursor() {
        for version in [0, 1, 42, u64::MAX] {
            let text = cursor(version).to_string();
            assert_eq!(text.parse::<DataCursor>(), Ok(cursor(version)), "{text}");
        }
        assert_eq!(cursor(7).to_string(), format!("d.{}.7", "0f".repeat(16)));
        // A change feed cursor has the same parts without the prefix.
        let change = format!("{}.7", "0f".repeat(16));
        assert!(change.parse::<DataCursor>().is_err(), "accepted {change}");
        assert!(
            cursor(7)
                .to_string()
                .parse::<crate::change::ChangeCursor>()
                .is_err(),
            "a data cursor was read as a change cursor"
        );
    }

    #[test]
    fn noncanonical_data_cursors_are_rejected() {
        let feed = "0f".repeat(16);
        for text in [
            String::new(),
            "d.".to_owned(),
            format!("d.{feed}"),
            format!("d.{feed}."),
            format!("d.{feed}.07"),
            format!("d.{feed}.+7"),
            format!("d.{feed}.18446744073709551616"),
            format!("d.{}.7", "0F".repeat(16)),
            format!("d.{}.7", "0f".repeat(15)),
            format!("D.{feed}.7"),
            format!("d.{feed}.7.1"),
        ] {
            assert!(text.parse::<DataCursor>().is_err(), "accepted {text:?}");
        }
    }

    #[test]
    fn a_write_below_the_watched_path_is_reported_at_its_own_path() {
        let seen = edit("todos/t1/done", Some("true")).seen_from(&path("todos"));
        assert_eq!(
            seen,
            Some(Seen {
                path: path("todos/t1/done"),
                value: Some(value("true")),
            })
        );
        let removed = edit("todos/t1", None).seen_from(&path("todos"));
        assert_eq!(
            removed,
            Some(Seen {
                path: path("todos/t1"),
                value: None,
            })
        );
        // The root is watched by everything.
        assert!(
            edit("a/b", Some("1"))
                .seen_from(&DataPath::root())
                .is_some()
        );
    }

    #[test]
    fn a_write_at_or_above_the_watched_path_is_reported_at_the_watched_path() {
        let at = edit("todos", Some(r#"{"t1":{"done":true}}"#)).seen_from(&path("todos"));
        assert_eq!(
            at,
            Some(Seen {
                path: path("todos"),
                value: Some(value(r#"{"t1":{"done":true}}"#)),
            })
        );
        // Above: the part of the result that lies at the watched path.
        let above = edit("", Some(r#"{"todos":{"t1":1},"other":2}"#));
        assert_eq!(
            above.seen_from(&path("todos/t1")),
            Some(Seen {
                path: path("todos/t1"),
                value: Some(value("1")),
            })
        );
        // The result lacks the watched path, or goes through a value that is
        // not an object: the watched value is gone.
        for result in [r#"{"other":2}"#, r#"{"todos":[1]}"#, r#"{"todos":"text"}"#] {
            assert_eq!(
                edit("", Some(result)).seen_from(&path("todos/t1")),
                Some(Seen {
                    path: path("todos/t1"),
                    value: None,
                }),
                "{result}"
            );
        }
        // A removal above removes what is watched.
        assert_eq!(
            edit("todos", None).seen_from(&path("todos/t1/done")),
            Some(Seen {
                path: path("todos/t1/done"),
                value: None,
            })
        );
    }

    #[test]
    fn a_write_beside_the_watched_path_is_not_reported() {
        assert_eq!(edit("settings", Some("1")).seen_from(&path("todos")), None);
        assert_eq!(
            edit("todos/t2", Some("1")).seen_from(&path("todos/t1")),
            None
        );
        // A shared prefix of characters is not a shared path.
        assert_eq!(edit("todos2", Some("1")).seen_from(&path("todos")), None);
    }

    #[test]
    fn an_edit_weighs_its_value_and_a_fixed_cost() {
        let none = edit("a", None).weight();
        let some = edit("a", Some(r#""abcd""#)).weight();
        assert_eq!(some - none, 6, "the canonical text of the value is counted");
        assert!(none > 0);
    }
}
