//! ZIP archives of source files (decision 0015).
//!
//! Entries are stored without compression, in the order given, with one
//! modification time, so the same entries always give the same bytes and
//! each file's bytes appear unchanged inside the archive. Only the classic
//! ZIP format is written: an archive larger than its 32-bit sizes and
//! offsets, or with more than 65535 entries, is refused rather than written
//! with ZIP64 extensions.

use std::fmt;

use artifree_domain::identity::UnixMillis;

const LOCAL_HEADER: u32 = 0x0403_4b50;
const CENTRAL_HEADER: u32 = 0x0201_4b50;
const END_OF_DIRECTORY: u32 = 0x0605_4b50;
/// Version 1.0 of the format suffices for stored files without directories.
const VERSION_NEEDED: u16 = 10;
/// Made by a Unix system (high byte 3) with format version 2.0 (low byte
/// 20), so that the external attributes carry Unix permissions.
const VERSION_MADE_BY: u16 = 0x0314;
/// General purpose flag bit 11: names are UTF-8.
const UTF8_NAMES: u16 = 1 << 11;
/// A regular file readable by everyone and writable by its owner.
const REGULAR_FILE: u32 = 0o100_644 << 16;
const LOCAL_HEADER_BYTES: usize = 30;
const CENTRAL_HEADER_BYTES: usize = 46;
const END_OF_DIRECTORY_BYTES: usize = 22;

/// One file of an archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveEntry {
    /// Name inside the archive, with `/` between directories.
    pub name: String,
    /// The file's bytes.
    pub content: Vec<u8>,
}

/// The entries do not fit the classic ZIP format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveTooLarge;

impl fmt::Display for ArchiveTooLarge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the files exceed the size or count limits of a ZIP archive")
    }
}

impl std::error::Error for ArchiveTooLarge {}

/// Writes `entries` as a ZIP archive, consuming each entry's bytes as it is
/// copied. Every entry gets the modification time `modified`, in UTC,
/// within the range the format can record (1980 to 2107).
///
/// # Errors
///
/// [`ArchiveTooLarge`] when a name, a file, the archive, or the number of
/// entries exceeds the classic format's limits.
pub fn stored_zip(
    entries: impl IntoIterator<Item = ArchiveEntry>,
    modified: UnixMillis,
) -> Result<Vec<u8>, ArchiveTooLarge> {
    let (time, date) = dos_time(modified);
    let mut archive = Vec::new();
    let mut directory = Vec::new();
    let mut count: u16 = 0;
    for entry in entries {
        count = count.checked_add(1).ok_or(ArchiveTooLarge)?;
        let name = entry.name.as_bytes();
        let name_length = u16::try_from(name.len()).map_err(|_| ArchiveTooLarge)?;
        let size = u32::try_from(entry.content.len()).map_err(|_| ArchiveTooLarge)?;
        let offset = u32::try_from(archive.len()).map_err(|_| ArchiveTooLarge)?;
        let crc = crc32(&entry.content);

        archive.reserve(LOCAL_HEADER_BYTES + name.len() + entry.content.len());
        put_u32(&mut archive, LOCAL_HEADER);
        put_u16(&mut archive, VERSION_NEEDED);
        put_u16(&mut archive, UTF8_NAMES);
        put_u16(&mut archive, 0); // stored
        put_u16(&mut archive, time);
        put_u16(&mut archive, date);
        put_u32(&mut archive, crc);
        put_u32(&mut archive, size); // compressed size
        put_u32(&mut archive, size);
        put_u16(&mut archive, name_length);
        put_u16(&mut archive, 0); // extra field length
        archive.extend_from_slice(name);
        archive.extend_from_slice(&entry.content);

        directory.reserve(CENTRAL_HEADER_BYTES + name.len());
        put_u32(&mut directory, CENTRAL_HEADER);
        put_u16(&mut directory, VERSION_MADE_BY);
        put_u16(&mut directory, VERSION_NEEDED);
        put_u16(&mut directory, UTF8_NAMES);
        put_u16(&mut directory, 0); // stored
        put_u16(&mut directory, time);
        put_u16(&mut directory, date);
        put_u32(&mut directory, crc);
        put_u32(&mut directory, size);
        put_u32(&mut directory, size);
        put_u16(&mut directory, name_length);
        put_u16(&mut directory, 0); // extra field length
        put_u16(&mut directory, 0); // comment length
        put_u16(&mut directory, 0); // disk number
        put_u16(&mut directory, 0); // internal attributes
        put_u32(&mut directory, REGULAR_FILE);
        put_u32(&mut directory, offset);
        directory.extend_from_slice(name);
    }
    let directory_offset = u32::try_from(archive.len()).map_err(|_| ArchiveTooLarge)?;
    let directory_size = u32::try_from(directory.len()).map_err(|_| ArchiveTooLarge)?;
    // The end record's own offset must stay addressable too.
    u32::try_from(archive.len() + directory.len() + END_OF_DIRECTORY_BYTES)
        .map_err(|_| ArchiveTooLarge)?;
    archive.append(&mut directory);
    put_u32(&mut archive, END_OF_DIRECTORY);
    put_u16(&mut archive, 0); // this disk
    put_u16(&mut archive, 0); // disk of the directory
    put_u16(&mut archive, count);
    put_u16(&mut archive, count);
    put_u32(&mut archive, directory_size);
    put_u32(&mut archive, directory_offset);
    put_u16(&mut archive, 0); // comment length
    Ok(archive)
}

fn put_u16(buffer: &mut Vec<u8>, value: u16) {
    buffer.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(buffer: &mut Vec<u8>, value: u32) {
    buffer.extend_from_slice(&value.to_le_bytes());
}

/// The CRC-32 of ZIP (reflected polynomial `0xEDB88320`).
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &byte in bytes {
        crc = CRC_TABLE[usize::from(crc.to_le_bytes()[0] ^ byte)] ^ (crc >> 8);
    }
    !crc
}

const CRC_TABLE: [u32; 256] = crc_table();

const fn crc_table() -> [u32; 256] {
    let mut table = [0; 256];
    let mut index = 0;
    while index < 256 {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "index is below 256, so it fits in a u32"
        )]
        let mut value = index as u32;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 1 == 1 {
                0xEDB8_8320 ^ (value >> 1)
            } else {
                value >> 1
            };
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
}

/// MS-DOS time and date fields of `at` in UTC, clamped to 1980-01-01
/// 00:00:00 through 2107-12-31 23:59:58. The format counts seconds in
/// twos, so odd seconds round down.
fn dos_time(at: UnixMillis) -> (u16, u16) {
    const FIRST: i64 = 315_532_800; // 1980-01-01T00:00:00Z
    const LAST: i64 = 4_354_819_198; // 2107-12-31T23:59:58Z
    let seconds = at.0.div_euclid(1000).clamp(FIRST, LAST);
    let (year, month, day) = civil_date(seconds.div_euclid(86_400));
    let of_day = seconds.rem_euclid(86_400);
    let time = ((of_day / 3600) << 11) | ((of_day / 60 % 60) << 5) | ((of_day % 60) / 2);
    let date = ((year - 1980) << 9) | (month << 5) | day;
    // Both fit: the clamp keeps the year within 1980-2107.
    (
        u16::try_from(time).unwrap_or(u16::MAX),
        u16::try_from(date).unwrap_or(u16::MAX),
    )
}

/// The proleptic Gregorian (year, month, day) of a count of days since
/// 1970-01-01, after Howard Hinnant's `civil_from_days`.
fn civil_date(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;

    fn entry(name: &str, content: &[u8]) -> ArchiveEntry {
        ArchiveEntry {
            name: name.to_owned(),
            content: content.to_vec(),
        }
    }

    fn u16_at(bytes: &[u8], at: usize) -> u16 {
        u16::from_le_bytes([bytes[at], bytes[at + 1]])
    }

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
    }

    #[test]
    fn crc_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn times_are_utc_and_clamped_to_the_dos_range() {
        // Year, month, day, hour, minute, second.
        let fields = |at: i64| {
            let (time, date) = dos_time(UnixMillis(at));
            (
                1980 + (date >> 9),
                (date >> 5) & 0xF,
                date & 0x1F,
                time >> 11,
                (time >> 5) & 0x3F,
                (time & 0x1F) * 2,
            )
        };
        // 2026-10-28T09:49:27.999Z: odd seconds round down.
        assert_eq!(fields(1_793_180_967_999), (2026, 10, 28, 9, 49, 26));
        // 2024-02-29, a leap day.
        assert_eq!(fields(1_709_164_800_000), (2024, 2, 29, 0, 0, 0));
        assert_eq!(fields(0), (1980, 1, 1, 0, 0, 0));
        assert_eq!(fields(-1), (1980, 1, 1, 0, 0, 0));
        assert_eq!(fields(i64::MAX), (2107, 12, 31, 23, 59, 58));
    }

    #[test]
    fn entries_are_stored_in_order_and_listed_at_their_offsets() -> Result<(), Box<dyn Error>> {
        let first = b"<p>first</p>".as_slice();
        let archive = stored_zip(
            [entry("index.html", first), entry("lib/app.js", b"")],
            UnixMillis(1_793_180_967_999),
        )?;

        // The first local header holds the bytes unchanged.
        assert_eq!(u32_at(&archive, 0), LOCAL_HEADER);
        assert_eq!(u16_at(&archive, 8), 0, "stored, not compressed");
        assert_eq!(u32_at(&archive, 14), crc32(first));
        assert_eq!(u32_at(&archive, 18), 12);
        assert_eq!(&archive[30..40], b"index.html");
        assert_eq!(&archive[40..52], first);
        let second = 52;
        assert_eq!(u32_at(&archive, second), LOCAL_HEADER);
        assert_eq!(&archive[second + 30..second + 40], b"lib/app.js");

        let end = archive.len() - END_OF_DIRECTORY_BYTES;
        assert_eq!(u32_at(&archive, end), END_OF_DIRECTORY);
        assert_eq!(u16_at(&archive, end + 10), 2);
        let directory = usize::try_from(u32_at(&archive, end + 16))?;
        assert_eq!(directory, second + 30 + 10);
        assert_eq!(
            usize::try_from(u32_at(&archive, end + 12))?,
            end - directory
        );
        assert_eq!(u32_at(&archive, directory), CENTRAL_HEADER);
        assert_eq!(u32_at(&archive, directory + 42), 0);
        assert_eq!(u32_at(&archive, directory + 38) >> 16, 0o100_644);
        let next = directory + CENTRAL_HEADER_BYTES + 10;
        assert_eq!(usize::try_from(u32_at(&archive, next + 42))?, second);
        Ok(())
    }

    #[test]
    fn the_same_entries_give_the_same_bytes() {
        let make = || stored_zip([entry("index.html", b"x")], UnixMillis(5_000_000_000_000));
        assert_eq!(make(), make());
    }

    #[test]
    fn what_the_classic_format_cannot_hold_is_refused() -> Result<(), ArchiveTooLarge> {
        let long_name = "a".repeat(usize::from(u16::MAX) + 1);
        assert_eq!(
            stored_zip([entry(&long_name, b"")], UnixMillis(0)),
            Err(ArchiveTooLarge)
        );
        let many = (0..=u32::from(u16::MAX)).map(|_| entry("a", b""));
        assert_eq!(stored_zip(many, UnixMillis(0)), Err(ArchiveTooLarge));
        let empty = stored_zip([], UnixMillis(0))?;
        assert_eq!(empty.len(), END_OF_DIRECTORY_BYTES);
        Ok(())
    }
}
