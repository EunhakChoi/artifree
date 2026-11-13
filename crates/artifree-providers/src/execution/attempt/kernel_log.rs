//! Reader of the kernel's seccomp records for the job tests (decision 0056, amendment of
//! 2026-11-13). A refusal counts only when a kernel record names the expected program, `sig=31`,
//! and the expected architecture and number. The test reads the kernel log from its end before
//! the launch and to its end after it.
//!
//! Compiled only into the test build, like its siblings. The product reads no kernel log. Each
//! `/dev/kmsg` read returns one record, and the kernel prints the audit record of a refusal from
//! its own thread after the refused call, so `await_records` polls until the records arrive.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::thread;
use std::time::{Duration, Instant};

/// The kernel's log device. The runner's account reads it once `kernel-log-access` has run.
const KMSG: &str = "/dev/kmsg";
/// The audit type of a seccomp refusal record, as one token of the record's message.
const SECCOMP_TYPE: &str = "type=1326";
/// The buffer of one `/dev/kmsg` read. A read into a smaller buffer than the record fails with
/// `EINVAL`.
const RECORD_BYTES: usize = 8192;
/// The interval between reads while records are awaited.
const POLL: Duration = Duration::from_millis(20);
/// How long the reader keeps reading after the awaited records arrived, to catch extra records.
const SETTLE: Duration = Duration::from_millis(300);
/// How long a job test waits for the records of one launch.
pub(super) const RECORD_WAIT: Duration = Duration::from_secs(3);

/// One seccomp refusal that the kernel recorded for a process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SeccompRecord {
    /// The quoted `exe` path of the refused process.
    pub(super) exe: String,
    /// The signal that the kernel sent, `31` (`SIGSYS`) for a refusal.
    pub(super) sig: u32,
    /// The audit architecture value, read from hexadecimal.
    pub(super) arch: u32,
    /// The refused system call number, as the kernel reports it.
    pub(super) syscall: u64,
}

/// Parses one kernel log record as a seccomp refusal.
///
/// Only the first line of the record is read. A record that is not a seccomp record, or whose
/// `exe`, `sig`, `arch` or `syscall` field is missing or malformed, gives `None`. An `exe` value
/// that the kernel hex-encodes gives `None` too, because the expected programs have quoted paths.
pub(super) fn parse_record(line: &str) -> Option<SeccompRecord> {
    if !is_seccomp(line) {
        return None;
    }
    let first = line.lines().next()?;
    let (_, message) = first.split_once(';')?;
    let exe = quoted_exe(message)?;
    let sig: u32 = field(message, "sig")?.parse().ok()?;
    let arch: u32 = u32::from_str_radix(field(message, "arch")?, 16).ok()?;
    let syscall: u64 = field(message, "syscall")?.parse().ok()?;
    Some(SeccompRecord {
        exe,
        sig,
        arch,
        syscall,
    })
}

/// Whether the first line of a kernel record carries the seccomp audit type. Every such record is
/// a seccomp refusal, whether or not its fields parse.
fn is_seccomp(line: &str) -> bool {
    line.lines()
        .next()
        .is_some_and(|first| first.split_whitespace().any(|token| token == SECCOMP_TYPE))
}

/// The text between the quotes of the `exe` field, or `None` when the value is not quoted.
fn quoted_exe(message: &str) -> Option<String> {
    let (_, quoted) = message.split_once(" exe=\"")?;
    let (exe, _) = quoted.split_once('"')?;
    Some(exe.to_owned())
}

/// The value of the first `key=` field in `message`: the text up to the next whitespace.
fn field<'a>(message: &'a str, key: &str) -> Option<&'a str> {
    let (_, rest) = message.split_once(&format!(" {key}="))?;
    rest.split(char::is_whitespace).next()
}

/// The kernel log, read from the position where it was opened.
#[derive(Debug)]
pub(super) struct KernelLog {
    /// The non-blocking descriptor of `/dev/kmsg`, positioned at the end of the log.
    file: File,
}

impl KernelLog {
    /// Opens the kernel log at its end, so that only the records printed afterwards are read.
    ///
    /// # Errors
    ///
    /// Returns the error of opening the device, of switching it to non-blocking reads, or of
    /// seeking to its end.
    pub(super) fn open_at_end() -> io::Result<Self> {
        let mut file = File::open(KMSG)?;
        rustix::io::ioctl_fionbio(&file, true)?;
        file.seek(SeekFrom::End(0))?;
        Ok(Self { file })
    }

    /// Reads every record printed since the last call, as the kernel's text, until no record is
    /// left to read.
    ///
    /// # Errors
    ///
    /// Returns a `BrokenPipe` error when the kernel overwrote records before they were read,
    /// since the window then misses records. Any other read error is returned unchanged.
    pub(super) fn read_new(&mut self) -> io::Result<Vec<String>> {
        let mut records = Vec::new();
        let mut buffer = vec![0_u8; RECORD_BYTES];
        loop {
            match self.file.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    let bytes = buffer.get(..count).ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            "the kernel log reported more bytes than the buffer holds",
                        )
                    })?;
                    records.push(String::from_utf8_lossy(bytes).into_owned());
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "the kernel log overwrote records before they were read",
                    ));
                }
                Err(error) => return Err(error),
            }
        }
        Ok(records)
    }
}

/// Appends the seccomp refusals among `lines` to `records`. A line that carries the seccomp type
/// but whose fields do not parse is an error, never a skipped line, so a window that holds such a
/// record cannot pass as clean.
///
/// # Errors
///
/// Returns `InvalidData` naming the first seccomp line that cannot be parsed.
fn collect_lines(lines: &[String], records: &mut Vec<SeccompRecord>) -> io::Result<()> {
    for line in lines {
        if !is_seccomp(line) {
            continue;
        }
        let record = parse_record(line).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("the kernel logged a seccomp record that cannot be parsed: {line}"),
            )
        })?;
        records.push(record);
    }
    Ok(())
}

/// Reads the new records of `log` and appends the seccomp refusals among them to `records`.
fn collect_seccomp(log: &mut KernelLog, records: &mut Vec<SeccompRecord>) -> io::Result<()> {
    let lines = log.read_new()?;
    collect_lines(&lines, records)
}

/// Returns the seccomp refusals printed after `log` was opened, once `expected` of them arrived
/// (at least one when `expected` is 0) or `within` passed, followed by a settling window that
/// catches extra records.
///
/// Kernel lines that are not seccomp records are ignored. A record that arrives during the
/// settling window is returned too, so the caller sees every seccomp record of the window.
///
/// # Errors
///
/// Returns the error of [`KernelLog::read_new`], or `InvalidData` for a seccomp record that cannot
/// be parsed. Such a record fails the whole window, so it is never dropped.
pub(super) fn await_records(
    log: &mut KernelLog,
    expected: usize,
    within: Duration,
) -> io::Result<Vec<SeccompRecord>> {
    let mut records = Vec::new();
    let started = Instant::now();
    loop {
        collect_seccomp(log, &mut records)?;
        // With no record expected, the wait runs through the whole window unless one arrives: an
        // early break would leave only the settling window to catch a late refusal.
        if records.len() >= expected.max(1) || started.elapsed() >= within {
            break;
        }
        thread::sleep(POLL);
    }
    let settling = Instant::now();
    while settling.elapsed() < SETTLE {
        thread::sleep(POLL);
        collect_seccomp(log, &mut records)?;
    }
    Ok(records)
}

#[cfg(test)]
mod tests;
