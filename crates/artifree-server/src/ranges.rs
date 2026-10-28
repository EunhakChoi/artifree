//! What a request's `Range` field asks for (RFC 9110, section 14; decision
//! 0016). One byte range is served; every other well-formed request reads
//! the whole file or nothing, and anything else is refused.

use std::fmt;
use std::num::NonZeroU64;

use artifree_core::parts::ByteRange;
use axum::http::{HeaderMap, header};

/// What a request asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Requested {
    /// The whole file: no `Range`, a unit other than bytes, or `If-Range`,
    /// which no validator of this API can match.
    Whole,
    /// One byte range.
    Part(ByteRange),
    /// No bytes of any file (`-0`): well-formed, but never satisfiable.
    Nothing,
}

/// Why a `Range` field was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MalformedRange {
    /// Not `bytes=first-last`, `bytes=first-`, or `bytes=-suffix`.
    Syntax,
    /// A last position before the first.
    Inverted,
    /// More than one range, in one list or in several field lines.
    Several,
}

impl fmt::Display for MalformedRange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Syntax => "Range: expected bytes=first-last, bytes=first-, or bytes=-suffix",
            Self::Inverted => "Range: the last position is before the first",
            Self::Several => "Range: one byte range only",
        })
    }
}

/// The range `headers` ask for.
///
/// The field's lines form one list (RFC 9110, section 5.3), so a second
/// line is a second range. The unit is matched without regard to case;
/// whitespace around the unit, `=`, and the range is ignored, as are empty
/// list elements. A position too large for 64 bits is past the end of any
/// file: a last position there means the end, and a suffix that long the
/// whole file.
pub(crate) fn requested(headers: &HeaderMap) -> Result<Requested, MalformedRange> {
    let mut lines = headers.get_all(header::RANGE).iter();
    let Some(line) = lines.next() else {
        return Ok(Requested::Whole);
    };
    if headers.contains_key(header::IF_RANGE) {
        return Ok(Requested::Whole);
    }
    let value = line.to_str().map_err(|_| MalformedRange::Syntax)?;
    let (unit, set) = value.split_once('=').ok_or(MalformedRange::Syntax)?;
    let unit = unit.trim_matches(is_whitespace);
    if unit.is_empty() || !unit.bytes().all(is_token) {
        return Err(MalformedRange::Syntax);
    }
    if !unit.eq_ignore_ascii_case("bytes") {
        return Ok(Requested::Whole);
    }
    if lines.next().is_some() {
        return Err(MalformedRange::Several);
    }
    let mut specs = set
        .split(',')
        .map(|spec| spec.trim_matches(is_whitespace))
        .filter(|spec| !spec.is_empty());
    let spec = specs.next().ok_or(MalformedRange::Syntax)?;
    if specs.next().is_some() {
        return Err(MalformedRange::Several);
    }
    let (first, last) = spec.split_once('-').ok_or(MalformedRange::Syntax)?;
    match (first, last) {
        ("", suffix) => Ok(match NonZeroU64::new(position(suffix)?) {
            Some(length) => Requested::Part(ByteRange::Last(length)),
            None => Requested::Nothing,
        }),
        (first, "") => Ok(Requested::Part(ByteRange::From {
            offset: position(first)?,
            length: None,
        })),
        (first, last) => {
            let (first, last) = (position(first)?, position(last)?);
            let span = last.checked_sub(first).ok_or(MalformedRange::Inverted)?;
            Ok(Requested::Part(ByteRange::From {
                offset: first,
                length: NonZeroU64::new(span.saturating_add(1)),
            }))
        }
    }
}

/// A position: one or more digits, `u64::MAX` when it does not fit.
fn position(digits: &str) -> Result<u64, MalformedRange> {
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(MalformedRange::Syntax);
    }
    Ok(digits.parse().unwrap_or(u64::MAX))
}

fn is_whitespace(character: char) -> bool {
    character == ' ' || character == '\t'
}

/// A token character (RFC 9110, section 5.6.2).
fn is_token(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn headers(lines: &[&'static str], if_range: bool) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for line in lines {
            headers.append(header::RANGE, HeaderValue::from_static(line));
        }
        if if_range {
            headers.insert(header::IF_RANGE, HeaderValue::from_static("\"tag\""));
        }
        headers
    }

    fn from(offset: u64, length: Option<u64>) -> Requested {
        Requested::Part(ByteRange::From {
            offset,
            length: length.and_then(NonZeroU64::new),
        })
    }

    fn last(length: u64) -> Requested {
        NonZeroU64::new(length).map_or(Requested::Nothing, |length| {
            Requested::Part(ByteRange::Last(length))
        })
    }

    #[test]
    fn every_form_of_one_byte_range_is_read() {
        for (line, expected) in [
            ("bytes=0-0", from(0, Some(1))),
            ("bytes=1-3", from(1, Some(3))),
            ("bytes=7-", from(7, None)),
            ("bytes=-3", last(3)),
            ("BYTES=4-5", from(4, Some(2))),
            ("bytes = 4-5 ", from(4, Some(2))),
            ("bytes=\t4-5", from(4, Some(2))),
            // Empty list elements are ignored.
            ("bytes=, 4-5 ,", from(4, Some(2))),
            // Positions past 64 bits are past the end of any file.
            ("bytes=0-18446744073709551615", from(0, Some(u64::MAX))),
            ("bytes=0-99999999999999999999", from(0, Some(u64::MAX))),
            ("bytes=99999999999999999999-", from(u64::MAX, None)),
            ("bytes=-99999999999999999999", last(u64::MAX)),
            // Nothing is selected, and no file satisfies that.
            ("bytes=-0", Requested::Nothing),
            ("bytes=-000", Requested::Nothing),
        ] {
            assert_eq!(requested(&headers(&[line], false)), Ok(expected), "{line}");
        }
    }

    #[test]
    fn without_a_byte_range_the_whole_file_is_read() {
        assert_eq!(requested(&HeaderMap::new()), Ok(Requested::Whole));
        for line in ["items=0-1", "none=whatever", "bytes=0-1"] {
            let if_range = line == "bytes=0-1";
            assert_eq!(
                requested(&headers(&[line], if_range)),
                Ok(Requested::Whole),
                "{line}"
            );
        }
        // An unknown unit in a second line changes nothing.
        assert_eq!(
            requested(&headers(&["items=0-1", "items=2-3"], false)),
            Ok(Requested::Whole)
        );
    }

    #[test]
    fn anything_else_is_refused() {
        for (lines, expected) in [
            (&["bytes=3-1"][..], MalformedRange::Inverted),
            (&["bytes=0-1,4-5"], MalformedRange::Several),
            (&["bytes=0-1, -2"], MalformedRange::Several),
            (&["bytes=0-1", "bytes=4-5"], MalformedRange::Several),
            (&["bytes"], MalformedRange::Syntax),
            (&["bytes="], MalformedRange::Syntax),
            (&["bytes=,"], MalformedRange::Syntax),
            (&["bytes=-"], MalformedRange::Syntax),
            (&["bytes=a-b"], MalformedRange::Syntax),
            (&["bytes=+1-2"], MalformedRange::Syntax),
            (&["bytes=1 -2"], MalformedRange::Syntax),
            (&["bytes=0x1-2"], MalformedRange::Syntax),
            (&["bytes=1-2-3"], MalformedRange::Syntax),
            (&["=0-1"], MalformedRange::Syntax),
            (&["by tes=0-1"], MalformedRange::Syntax),
        ] {
            assert_eq!(
                requested(&headers(lines, false)),
                Err(expected),
                "{lines:?}"
            );
        }
    }
}
