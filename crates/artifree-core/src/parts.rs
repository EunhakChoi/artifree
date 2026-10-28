//! Bounded reads: a byte range of one file (decision 0016).
//!
//! A part is cut from the file's stored bytes. Read by bytes, it is exactly
//! the range asked for. Read by characters, a file whose bytes are UTF-8
//! text is never cut inside a character: the part ends before a character
//! the range would split, and a range that starts inside one is refused, so
//! that the parts of a text file are text and join into the file.

use std::fmt;
use std::num::NonZeroU64;

use sha2::{Digest, Sha256};

/// The bytes a read asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteRange {
    /// From `offset`, `length` bytes or to the end of the file.
    From {
        /// First byte, counted from 0.
        offset: u64,
        /// How many bytes at most; to the end when absent.
        length: Option<NonZeroU64>,
    },
    /// The last bytes of the file, or all of it when shorter.
    Last(NonZeroU64),
}

impl ByteRange {
    /// The whole file.
    pub const WHOLE: Self = Self::From {
        offset: 0,
        length: None,
    };
}

/// Where a part may end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boundary {
    /// At any byte.
    Byte,
    /// Between characters when the whole file is UTF-8 text; at any byte
    /// otherwise.
    Character,
}

/// Why a range cannot be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeError {
    /// The range starts after the last byte of the file.
    PastEnd {
        /// Size of the whole file in bytes.
        size: u64,
    },
    /// Read by characters, the range starts inside a character.
    InsideCharacter,
    /// Read by characters, the range is shorter than the character it
    /// starts with.
    ShorterThanCharacter,
}

impl fmt::Display for RangeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PastEnd { size } => {
                write!(formatter, "the range starts past the end of the {size}-byte file")
            }
            Self::InsideCharacter => formatter.write_str(
                "the offset is inside a UTF-8 character; start where a part ended (next_offset)",
            ),
            Self::ShorterThanCharacter => formatter.write_str(
                "the length is shorter than the UTF-8 character at the offset; ask for at least 4 bytes",
            ),
        }
    }
}

impl std::error::Error for RangeError {}

/// A part of one file of a pinned revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePart {
    /// The part's bytes.
    pub content: Vec<u8>,
    /// Where the part starts in the file.
    pub offset: u64,
    /// Size of the whole file in bytes.
    pub size: u64,
    /// SHA-256 of the whole file.
    pub sha256: [u8; 32],
    /// Whether the whole file is UTF-8 text.
    pub text: bool,
}

impl FilePart {
    /// Where the next part starts, or `None` when this one ends the file.
    #[must_use]
    pub fn next_offset(&self) -> Option<u64> {
        let end = self.offset + self.content.len() as u64;
        (end < self.size).then_some(end)
    }
}

/// The part of `file` that `range` asks for, ending at `boundary`.
///
/// # Errors
///
/// [`RangeError`] when the range starts past the end of the file, or, read
/// by characters, inside a character or with a length shorter than it.
pub fn part(file: Vec<u8>, range: ByteRange, boundary: Boundary) -> Result<FilePart, RangeError> {
    let size = file.len();
    let text = std::str::from_utf8(&file).is_ok();
    let (start, wanted) = match range {
        ByteRange::From { offset, length } => (
            usize::try_from(offset).unwrap_or(usize::MAX),
            length.map(|length| usize::try_from(length.get()).unwrap_or(usize::MAX)),
        ),
        ByteRange::Last(length) => {
            let length = usize::try_from(length.get()).unwrap_or(usize::MAX);
            (size.saturating_sub(length), None)
        }
    };
    let whole = size as u64;
    if start > size {
        return Err(RangeError::PastEnd { size: whole });
    }
    let mut end = wanted.map_or(size, |length| start.saturating_add(length).min(size));
    if text && boundary == Boundary::Character {
        // In valid UTF-8, a character starts at every byte that is not a
        // continuation byte (0b10xxxxxx).
        let starts_character = |at: usize| at == size || file[at] & 0xC0 != 0x80;
        if !starts_character(start) {
            return Err(RangeError::InsideCharacter);
        }
        while !starts_character(end) {
            end -= 1;
        }
        if end == start && start < size {
            return Err(RangeError::ShorterThanCharacter);
        }
    }
    let sha256 = Sha256::digest(&file).into();
    let mut content = file;
    content.truncate(end);
    content.drain(..start);
    Ok(FilePart {
        content,
        offset: start as u64,
        size: whole,
        sha256,
        text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from(offset: u64, length: u64) -> ByteRange {
        ByteRange::From {
            offset,
            length: NonZeroU64::new(length),
        }
    }

    #[test]
    fn bytes_are_cut_exactly_and_the_last_part_has_no_next_offset() -> Result<(), RangeError> {
        let file = b"0123456789".to_vec();
        let middle = part(file.clone(), from(2, 3), Boundary::Byte)?;
        assert_eq!((middle.content.as_slice(), middle.offset), (&b"234"[..], 2));
        assert_eq!((middle.size, middle.next_offset()), (10, Some(5)));
        assert_eq!(middle.sha256, <[u8; 32]>::from(Sha256::digest(&file)));
        let tail = part(file.clone(), from(8, 5), Boundary::Byte)?;
        assert_eq!(
            (tail.content.as_slice(), tail.next_offset()),
            (&b"89"[..], None)
        );
        let rest = part(file.clone(), from(7, 0), Boundary::Byte)?;
        assert_eq!(rest.content, b"789");
        let last = part(
            file.clone(),
            ByteRange::Last(NonZeroU64::MIN.saturating_add(2)),
            Boundary::Byte,
        )?;
        assert_eq!((last.content.as_slice(), last.offset), (&b"789"[..], 7));
        let all = part(
            file.clone(),
            ByteRange::Last(NonZeroU64::MAX),
            Boundary::Byte,
        )?;
        assert_eq!(all.content, file);
        let whole = part(file.clone(), ByteRange::WHOLE, Boundary::Character)?;
        assert_eq!(
            (whole.next_offset(), whole.text, whole.content),
            (None, true, file.clone())
        );
        Ok(())
    }

    #[test]
    fn the_end_of_the_file_is_an_empty_part_and_past_it_is_refused() -> Result<(), RangeError> {
        let end = part(b"abc".to_vec(), from(3, 4), Boundary::Character)?;
        assert_eq!((end.content.len(), end.next_offset()), (0, None));
        assert_eq!(
            part(b"abc".to_vec(), from(4, 1), Boundary::Byte),
            Err(RangeError::PastEnd { size: 3 })
        );
        assert_eq!(
            part(b"abc".to_vec(), from(u64::MAX, 1), Boundary::Byte),
            Err(RangeError::PastEnd { size: 3 })
        );
        Ok(())
    }

    #[test]
    fn text_parts_end_between_characters_and_join_into_the_file() -> Result<(), RangeError> {
        // 1, 2, 3, and 4-byte characters, with a BOM and CRLF.
        let text = "\u{FEFF}a\u{e9}\u{d55c}\u{1F600}\r\n".as_bytes().to_vec();
        for length in 4..=8 {
            let mut joined = Vec::new();
            let mut offset = 0;
            loop {
                let piece = part(text.clone(), from(offset, length), Boundary::Character)?;
                assert!(
                    std::str::from_utf8(&piece.content).is_ok(),
                    "length {length}"
                );
                assert!(
                    !piece.content.is_empty(),
                    "length {length} made no progress"
                );
                joined.extend_from_slice(&piece.content);
                match piece.next_offset() {
                    Some(next) => offset = next,
                    None => break,
                }
            }
            assert_eq!(joined, text, "length {length}");
        }
        // The emoji starts at byte 9 and takes 4 bytes.
        let cut = part(text.clone(), from(9, 3), Boundary::Character);
        assert_eq!(cut, Err(RangeError::ShorterThanCharacter));
        let inside = part(text.clone(), from(10, 4), Boundary::Character);
        assert_eq!(inside, Err(RangeError::InsideCharacter));
        // Read by bytes, the same ranges are exact.
        let bytes = part(text.clone(), from(10, 2), Boundary::Byte)?;
        assert_eq!(bytes.content, text[10..12]);
        Ok(())
    }

    #[test]
    fn a_file_that_is_not_utf8_is_read_by_bytes_either_way() -> Result<(), RangeError> {
        let binary = vec![0x89, b'P', 0xE9, 0x80, 0xFF, 0x00];
        let piece = part(binary.clone(), from(3, 2), Boundary::Character)?;
        assert_eq!(
            (piece.content.as_slice(), piece.text),
            (&binary[3..5], false)
        );
        Ok(())
    }
}
