//! Parts of a file (decision 0016): a range of a file's bytes, cut exactly
//! or, for a UTF-8 file, between characters, with what identifies the whole
//! file.

use std::fmt;
use std::num::NonZeroU64;

use sha2::{Digest, Sha256};

/// Which bytes of a file a read asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteRange {
    /// From `offset`, `length` bytes or to the end of the file.
    From {
        /// The first byte, counted from 0.
        offset: u64,
        /// How many bytes at most; none reads to the end.
        length: Option<NonZeroU64>,
    },
    /// The last bytes of the file, or all of it when it is shorter.
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
    /// Between characters, when the whole file is UTF-8; at any byte
    /// otherwise.
    Character,
}

/// Why a range was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeError {
    /// The range starts past the end of a file of `size` bytes.
    PastEnd {
        /// The file's size in bytes.
        size: u64,
    },
    /// Read between characters, the range starts inside one.
    InsideCharacter,
    /// Read between characters, the range is shorter than the character it
    /// starts with, so the part would be empty before the end of the file.
    ShorterThanCharacter,
}

impl fmt::Display for RangeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PastEnd { size } => write!(
                formatter,
                "the range starts past the end of the file, which has {size} bytes"
            ),
            Self::InsideCharacter => formatter.write_str(
                "the offset is inside a UTF-8 character; start at the offset a part returned",
            ),
            Self::ShorterThanCharacter => formatter.write_str(
                "the length is shorter than the UTF-8 character at the offset; ask for at least 4 bytes",
            ),
        }
    }
}

impl std::error::Error for RangeError {}

/// Part of a file, with what identifies the whole file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePart {
    /// The part's bytes.
    pub content: Vec<u8>,
    /// Where the part starts in the file.
    pub offset: u64,
    /// The whole file's size in bytes.
    pub size: u64,
    /// The whole file's SHA-256 digest.
    pub sha256: [u8; 32],
    /// Whether the whole file is UTF-8 text.
    pub text: bool,
}

impl FilePart {
    /// Where the next part starts, or `None` when this one reaches the end.
    #[must_use]
    pub fn next_offset(&self) -> Option<u64> {
        let end = self.offset + self.content.len() as u64;
        (end < self.size).then_some(end)
    }
}

/// The part of `file` that `range` selects, cut at `boundary`.
///
/// A range that starts at the end of the file selects an empty part; one
/// that starts past it is refused. A range that ends past the end stops
/// there. Read between characters, a part of a UTF-8 file ends before a
/// character the range would split, so the parts read from each
/// [`FilePart::next_offset`] join into the file.
///
/// # Errors
///
/// [`RangeError`] for a range that starts past the end, or, read between
/// characters, inside a character or with a length shorter than it.
pub fn part(file: Vec<u8>, range: ByteRange, boundary: Boundary) -> Result<FilePart, RangeError> {
    let size = file.len() as u64;
    let (start, end) = match range {
        ByteRange::From { offset, length } => {
            if offset > size {
                return Err(RangeError::PastEnd { size });
            }
            let end = length.map_or(size, |length| offset.saturating_add(length.get()).min(size));
            (offset, end)
        }
        ByteRange::Last(length) => (size.saturating_sub(length.get()), size),
    };
    // Both are at most `size`, the length of `file`.
    let at = |position: u64| usize::try_from(position).map_or(file.len(), |at| at.min(file.len()));
    let (start, mut end) = (at(start), at(end));
    let text = std::str::from_utf8(&file).ok();
    if let (Boundary::Character, Some(text)) = (boundary, text) {
        if !text.is_char_boundary(start) {
            return Err(RangeError::InsideCharacter);
        }
        // `start` is a boundary, so this stops at it at the latest.
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if end == start && start < file.len() {
            return Err(RangeError::ShorterThanCharacter);
        }
    }
    let sha256 = Sha256::digest(&file).into();
    let text = text.is_some();
    let mut content = file;
    content.truncate(end);
    content.drain(..start);
    Ok(FilePart {
        content,
        offset: start as u64,
        size,
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
    fn bytes_are_cut_exactly_with_the_whole_file_identified() -> Result<(), RangeError> {
        let file = b"0123456789".to_vec();
        let middle = part(file.clone(), from(2, 3), Boundary::Byte)?;
        assert_eq!(
            (middle.content.as_slice(), middle.offset, middle.size),
            (&b"234"[..], 2, 10)
        );
        assert_eq!(middle.next_offset(), Some(5));
        assert_eq!(middle.sha256, <[u8; 32]>::from(Sha256::digest(&file)));
        assert!(middle.text);
        let tail = part(file.clone(), from(8, 5), Boundary::Byte)?;
        assert_eq!(
            (tail.content.as_slice(), tail.next_offset()),
            (&b"89"[..], None)
        );
        let rest = part(file.clone(), from(7, 0), Boundary::Byte)?;
        assert_eq!(rest.content, b"789");
        let huge = part(file.clone(), from(1, u64::MAX), Boundary::Byte)?;
        assert_eq!(huge.content, b"123456789");
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
        assert_eq!((all.content, all.offset), (file, 0));
        Ok(())
    }

    #[test]
    fn the_end_is_an_empty_part_and_past_it_is_refused() -> Result<(), RangeError> {
        let end = part(b"abc".to_vec(), from(3, 4), Boundary::Character)?;
        assert_eq!((end.content.len(), end.next_offset()), (0, None));
        let empty = part(Vec::new(), ByteRange::WHOLE, Boundary::Character)?;
        assert_eq!((empty.size, empty.next_offset()), (0, None));
        for offset in [4, u64::MAX] {
            assert_eq!(
                part(b"abc".to_vec(), from(offset, 1), Boundary::Byte),
                Err(RangeError::PastEnd { size: 3 })
            );
        }
        Ok(())
    }

    #[test]
    fn every_text_part_ends_between_characters_and_the_parts_join() -> Result<(), RangeError> {
        // 1, 2, 3, and 4-byte characters, with a byte order mark and CRLF.
        let text = "\u{FEFF}a\u{e9}\u{d55c}\u{1F600}\r\n";
        let bytes = text.as_bytes().to_vec();
        let size = bytes.len() as u64;
        for offset in 0..=size + 1 {
            for length in 1..=size + 1 {
                let read = part(bytes.clone(), from(offset, length), Boundary::Character);
                let start = usize::try_from(offset).unwrap_or(usize::MAX);
                if offset > size {
                    assert_eq!(read, Err(RangeError::PastEnd { size }));
                } else if !text.is_char_boundary(start) {
                    assert_eq!(read, Err(RangeError::InsideCharacter), "{offset}+{length}");
                } else {
                    let end = (offset + length).min(size);
                    let expected = (start..=usize::try_from(end).unwrap_or(usize::MAX))
                        .rev()
                        .find(|&end| text.is_char_boundary(end))
                        .unwrap_or(start);
                    if expected == start && start < bytes.len() {
                        assert_eq!(
                            read,
                            Err(RangeError::ShorterThanCharacter),
                            "{offset}+{length}"
                        );
                    } else {
                        let piece = read.map_err(|error| format!("{offset}+{length}: {error}"));
                        assert_eq!(
                            piece.map(|piece| piece.content),
                            Ok(bytes[start..expected].to_vec()),
                            "{offset}+{length}"
                        );
                    }
                }
            }
        }
        // Read from each next offset, parts of every length from 4 bytes
        // make progress and join into the file.
        for length in 4..=size {
            let mut joined = Vec::new();
            let mut offset = 0;
            loop {
                let piece = part(bytes.clone(), from(offset, length), Boundary::Character)?;
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
            assert_eq!(joined, bytes, "length {length}");
        }
        Ok(())
    }

    #[test]
    fn a_file_that_is_not_utf8_is_cut_by_bytes_either_way() -> Result<(), RangeError> {
        let binary = vec![0x89, b'P', 0xE9, 0x80, 0xFF, 0x00];
        for boundary in [Boundary::Byte, Boundary::Character] {
            let piece = part(binary.clone(), from(3, 2), boundary)?;
            assert_eq!(
                (piece.content.as_slice(), piece.text),
                (&binary[3..5], false)
            );
        }
        Ok(())
    }
}
