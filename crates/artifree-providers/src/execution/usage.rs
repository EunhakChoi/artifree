//! The attempt group's cgroup counters: memory, task and CPU use (decisions 0042 rules 5 and
//! 6, 0056 rule 14). The attempt reads them after the group was seen empty and before it is
//! removed.
//!
//! These counters are evidence of what an attempt used, not limits. The limits were written
//! to the group before the payload started, and the kernel enforces them whatever this module
//! reads. Only `memory.peak` and `pids.peak` may be absent, because the kernel may not offer
//! them; any other missing, repeated or unparsable value makes the whole read fail.

use std::fmt;
use std::io;
use std::path::Path;

/// The largest usage file that the read takes, in bytes. Each file holds a few short lines or
/// one number.
const USAGE_FILE_BYTES: u64 = 4096;

// Control files of the attempt group.
const MEMORY_EVENTS: &str = "memory.events";
const PIDS_EVENTS: &str = "pids.events";
const CPU_STAT: &str = "cpu.stat";
const MEMORY_PEAK: &str = "memory.peak";
const PIDS_PEAK: &str = "pids.peak";

// Keys of the flat keyed files, matched exactly.
const OOM_KILL: &str = "oom_kill";
const OOM_GROUP_KILL: &str = "oom_group_kill";
const MAX: &str = "max";
const USAGE_USEC: &str = "usage_usec";
const NR_PERIODS: &str = "nr_periods";
const NR_THROTTLED: &str = "nr_throttled";

/// The attempt group's counters, read after the group was seen empty and before removal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct GroupUsage {
    /// The peak memory in bytes, from `memory.peak`, when the kernel offers the file.
    pub(super) memory_peak_bytes: Option<u64>,
    /// The number of OOM kills, from the `oom_kill` key of `memory.events`.
    pub(super) oom_kills: u64,
    /// The number of group OOM kills, from the `oom_group_kill` key of `memory.events`.
    pub(super) oom_group_kills: u64,
    /// The number of task creations that the limit refused, from the `max` key of
    /// `pids.events`.
    pub(super) task_refusals: u64,
    /// The peak number of tasks, from `pids.peak`, when the kernel offers the file.
    pub(super) tasks_peak: Option<u64>,
    /// The CPU time that the group used in microseconds, from `usage_usec` of `cpu.stat`.
    pub(super) cpu_usage_usec: u64,
    /// The enforcement periods, from `nr_periods` of `cpu.stat`.
    pub(super) cpu_periods: u64,
    /// The enforcement periods in which the group was throttled, from `nr_throttled` of
    /// `cpu.stat`.
    pub(super) cpu_throttled_periods: u64,
}

impl fmt::Display for GroupUsage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            formatter,
            "attempt_usage.memory_peak_bytes={}",
            optional_word(self.memory_peak_bytes)
        )?;
        writeln!(formatter, "attempt_usage.oom_kills={}", self.oom_kills)?;
        writeln!(
            formatter,
            "attempt_usage.oom_group_kills={}",
            self.oom_group_kills
        )?;
        writeln!(
            formatter,
            "attempt_usage.task_refusals={}",
            self.task_refusals
        )?;
        writeln!(
            formatter,
            "attempt_usage.tasks_peak={}",
            optional_word(self.tasks_peak)
        )?;
        writeln!(
            formatter,
            "attempt_usage.cpu_usage_usec={}",
            self.cpu_usage_usec
        )?;
        writeln!(formatter, "attempt_usage.cpu_periods={}", self.cpu_periods)?;
        writeln!(
            formatter,
            "attempt_usage.cpu_throttled_periods={}",
            self.cpu_throttled_periods
        )
    }
}

/// Why the counters of an attempt group could not be read.
///
/// The payload is the file or key concerned, never a value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum UsageUnreadable {
    /// A control file could not be read, was larger than its bound or was not UTF-8.
    File(&'static str),
    /// A required key is on no line.
    Missing(&'static str),
    /// A required key is on more than one line.
    Repeated(&'static str),
    /// A value is not a decimal `u64`. The payload is the key, or the file name for a file
    /// that holds one number.
    Malformed(&'static str),
}

impl fmt::Display for UsageUnreadable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File(file) => write!(formatter, "usage file {file} could not be read"),
            Self::Missing(key) => write!(formatter, "usage key {key} is missing"),
            Self::Repeated(key) => write!(formatter, "usage key {key} is repeated"),
            Self::Malformed(key) => write!(formatter, "usage value {key} is malformed"),
        }
    }
}

/// The texts of one attempt group's usage files. The two peaks are `None` when the kernel
/// does not offer the file.
#[derive(Clone, Copy, Debug)]
pub(super) struct UsageTexts<'a> {
    /// The text of `memory.events`.
    pub(super) memory_events: &'a str,
    /// The text of `pids.events`.
    pub(super) pids_events: &'a str,
    /// The text of `cpu.stat`.
    pub(super) cpu_stat: &'a str,
    /// The text of `memory.peak`, when the kernel offers the file.
    pub(super) memory_peak: Option<&'a str>,
    /// The text of `pids.peak`, when the kernel offers the file.
    pub(super) pids_peak: Option<&'a str>,
}

/// The value of the one line of a flat keyed file whose key is exactly `key`.
///
/// Each line splits at its first space, and a line without one is ignored.
///
/// # Errors
/// Returns `Missing` or `Repeated` unless exactly one line holds the key, and `Malformed` for
/// a value that is not a decimal number.
pub(super) fn keyed_value(text: &str, key: &'static str) -> Result<u64, UsageUnreadable> {
    let values: Vec<&str> = text
        .lines()
        .filter_map(|line| line.split_once(' '))
        .filter(|&(name, _)| name == key)
        .map(|(_, value)| value)
        .collect();
    decimal(single_value(&values, key)?, key)
}

/// The number in a file that holds one decimal number, after at most one trailing newline.
///
/// # Errors
/// Returns `Malformed` with the file name for an empty text or any character other than an
/// ASCII digit.
pub(super) fn single_number(text: &str, file: &'static str) -> Result<u64, UsageUnreadable> {
    decimal(text.strip_suffix('\n').unwrap_or(text), file)
}

/// The counters that the usage texts hold.
///
/// # Errors
/// Returns the first `UsageUnreadable` among the required keys and the present peaks.
pub(super) fn usage_from_texts(texts: &UsageTexts<'_>) -> Result<GroupUsage, UsageUnreadable> {
    Ok(GroupUsage {
        memory_peak_bytes: optional_number(texts.memory_peak, MEMORY_PEAK)?,
        oom_kills: keyed_value(texts.memory_events, OOM_KILL)?,
        oom_group_kills: keyed_value(texts.memory_events, OOM_GROUP_KILL)?,
        task_refusals: keyed_value(texts.pids_events, MAX)?,
        tasks_peak: optional_number(texts.pids_peak, PIDS_PEAK)?,
        cpu_usage_usec: keyed_value(texts.cpu_stat, USAGE_USEC)?,
        cpu_periods: keyed_value(texts.cpu_stat, NR_PERIODS)?,
        cpu_throttled_periods: keyed_value(texts.cpu_stat, NR_THROTTLED)?,
    })
}

/// Reads the usage files of one attempt group directory. It only reads.
///
/// # Errors
/// Returns `File` for a required file that cannot be read, and for a peak file that cannot be
/// read for any reason other than its absence. Otherwise returns the errors of
/// `usage_from_texts`.
pub(super) fn read_usage(group_dir: &Path) -> Result<GroupUsage, UsageUnreadable> {
    let memory_events = read_text(group_dir, MEMORY_EVENTS)?;
    let pids_events = read_text(group_dir, PIDS_EVENTS)?;
    let cpu_stat = read_text(group_dir, CPU_STAT)?;
    let memory_peak = read_optional_text(group_dir, MEMORY_PEAK)?;
    let pids_peak = read_optional_text(group_dir, PIDS_PEAK)?;
    usage_from_texts(&UsageTexts {
        memory_events: &memory_events,
        pids_events: &pids_events,
        cpu_stat: &cpu_stat,
        memory_peak: memory_peak.as_deref(),
        pids_peak: pids_peak.as_deref(),
    })
}

/// The text of one required usage file, read within its bound.
fn read_text(group_dir: &Path, file: &'static str) -> Result<String, UsageUnreadable> {
    let bytes = super::read_bounded(&group_dir.join(file), USAGE_FILE_BYTES)
        .map_err(|_| UsageUnreadable::File(file))?;
    String::from_utf8(bytes).map_err(|_| UsageUnreadable::File(file))
}

/// The text of one peak file within its bound, or `None` when the kernel does not offer it.
fn read_optional_text(
    group_dir: &Path,
    file: &'static str,
) -> Result<Option<String>, UsageUnreadable> {
    match super::read_bounded(&group_dir.join(file), USAGE_FILE_BYTES) {
        Ok(bytes) => String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| UsageUnreadable::File(file)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(UsageUnreadable::File(file)),
    }
}

/// The one value that the lines for a key hold, and no other number of lines.
fn single_value<'a>(values: &[&'a str], key: &'static str) -> Result<&'a str, UsageUnreadable> {
    match values {
        [] => Err(UsageUnreadable::Missing(key)),
        [value] => Ok(*value),
        _ => Err(UsageUnreadable::Repeated(key)),
    }
}

/// A non-empty run of ASCII digits that fits in `u64`.
fn decimal(value: &str, key: &'static str) -> Result<u64, UsageUnreadable> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(UsageUnreadable::Malformed(key));
    }
    let number: u64 = value.parse().map_err(|_| UsageUnreadable::Malformed(key))?;
    Ok(number)
}

/// The number of an optional peak, or `None` when the file is absent.
fn optional_number(text: Option<&str>, file: &'static str) -> Result<Option<u64>, UsageUnreadable> {
    text.map(|value| single_number(value, file)).transpose()
}

/// A counter as the evidence prints it: its number, or `unavailable`.
fn optional_word(value: Option<u64>) -> String {
    value.map_or_else(|| "unavailable".to_owned(), |number| number.to_string())
}

#[cfg(test)]
mod tests;
