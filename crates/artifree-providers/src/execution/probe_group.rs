//! The trusted probe group: limits read back, kill and empty observation (decision 0042 rule 6).

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use super::{AdmissionRefusal, read_bounded, write_control};

/// Decision 0042 rule 6: "a five-second cleanup observation deadline".
pub(super) const CLEANUP_DEADLINE: Duration = Duration::from_secs(5);
/// The pause between two reads of `cgroup.events`.
const POLL_INTERVAL: Duration = Duration::from_millis(10);
/// The largest control file this module reads; the kernel files here hold a few bytes.
const CONTROL_FILE_BYTES: u64 = 4096;
/// The fixture profile's limits (0042 rule 5): 1 GiB with group OOM termination and swap
/// disabled, one CPU at a 100 ms period, and 64 processes and threads.
pub(super) const PROBE_LIMITS: [(&str, &str); 5] = [
    ("memory.max", "1073741824"),
    ("memory.swap.max", "0"),
    ("memory.oom.group", "1"),
    ("cpu.max", "100000 100000"),
    ("pids.max", "64"),
];

/// A probe cgroup that this process created under its delegated group.
///
/// Dropping the value leaves the directory in place. `remove_probe` is the only way
/// to end the group, and only after `kill_and_await_empty` succeeded.
#[derive(Debug)]
pub(super) struct ProbeGroup {
    /// The group's directory in the cgroup filesystem.
    path: PathBuf,
}

/// Creates the child `name` of `parent` with `mkdir`, never reusing a directory.
///
/// Every failure is `UndelegatedCgroup`, because the delegated group did not let this
/// account create a child group (facility 5).
pub(super) fn create_probe(parent: &Path, name: &str) -> Result<ProbeGroup, AdmissionRefusal> {
    let path = parent.join(name);
    fs::create_dir(&path).map_err(|_| AdmissionRefusal::UndelegatedCgroup)?;
    Ok(ProbeGroup { path })
}

/// Writes each `PROBE_LIMITS` entry and reads it back, in table order.
///
/// Call before any process joins the group. A failed write, read or comparison is
/// `LimitNotEnforced`; the applied entries are returned only when all of them succeed.
pub(super) fn apply_limits(
    probe: &ProbeGroup,
) -> Result<Vec<(&'static str, String)>, AdmissionRefusal> {
    let mut applied = Vec::with_capacity(PROBE_LIMITS.len());
    for (file, value) in PROBE_LIMITS {
        let path = probe.path.join(file);
        write_control(&path, value).map_err(|_| AdmissionRefusal::LimitNotEnforced)?;
        let bytes = read_bounded(&path, CONTROL_FILE_BYTES)
            .map_err(|_| AdmissionRefusal::LimitNotEnforced)?;
        verify_read_back(value, &bytes)?;
        applied.push((file, value.to_owned()));
    }
    Ok(applied)
}

/// Accepts `actual` only if it equals `expected` plus at most one newline.
///
/// The kernel ends a control file's value with a newline. Any other difference means
/// the limit was not applied as written.
pub(super) fn verify_read_back(expected: &str, actual: &[u8]) -> Result<(), AdmissionRefusal> {
    let value = actual.strip_suffix(b"\n").unwrap_or(actual);
    if value == expected.as_bytes() {
        Ok(())
    } else {
        Err(AdmissionRefusal::LimitNotEnforced)
    }
}

/// Places the process `pid` in the probe group by writing it to `cgroup.procs`.
///
/// A failed write is `ProbeNotPlaced`.
pub(super) fn add_process(probe: &ProbeGroup, pid: u32) -> Result<(), AdmissionRefusal> {
    write_control(&probe.path.join("cgroup.procs"), &pid.to_string())
        .map_err(|_| AdmissionRefusal::ProbeNotPlaced)
}

/// Reads the `populated` field of a `cgroup.events` file.
///
/// Returns `Some(false)` for exactly one `populated 0` line and `Some(true)` for
/// `populated 1`. A missing, repeated, malformed or non-UTF-8 field gives `None`.
/// Other fields are ignored.
#[must_use]
pub(super) fn parse_populated(events: &[u8]) -> Option<bool> {
    let text = std::str::from_utf8(events).ok()?;
    let mut populated = None;
    for line in text.lines() {
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        if key != "populated" {
            continue;
        }
        if populated.is_some() {
            return None;
        }
        populated = Some(match value {
            "0" => false,
            "1" => true,
            _ => return None,
        });
    }
    populated
}

/// Kills the probe group and waits until `cgroup.events` reports `populated 0`.
///
/// `clock` supplies the observation time. The deadline counts from the first reading
/// taken after the kill, and a clock that moves backwards is also a failure. Every
/// failure is `CleanupNotObserved`. After one, the caller must not remove or reuse
/// the group (0042 rule 6).
pub(super) fn kill_and_await_empty(
    probe: &ProbeGroup,
    deadline: Duration,
    mut clock: impl FnMut() -> Instant,
) -> Result<(), AdmissionRefusal> {
    write_control(&probe.path.join("cgroup.kill"), "1")
        .map_err(|_| AdmissionRefusal::CleanupNotObserved)?;
    let events = probe.path.join("cgroup.events");
    let start = clock();
    loop {
        let bytes = read_bounded(&events, CONTROL_FILE_BYTES)
            .map_err(|_| AdmissionRefusal::CleanupNotObserved)?;
        match parse_populated(&bytes) {
            Some(false) => return Ok(()),
            Some(true) => {}
            None => return Err(AdmissionRefusal::CleanupNotObserved),
        }
        let elapsed = clock()
            .checked_duration_since(start)
            .ok_or(AdmissionRefusal::CleanupNotObserved)?;
        if elapsed >= deadline {
            return Err(AdmissionRefusal::CleanupNotObserved);
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Reads the group's `memory.peak` high-water mark in bytes.
///
/// Call after `kill_and_await_empty` succeeded and before `remove_probe`. The value is
/// evidence, not a facility: a kernel without the file, or a value that does not parse,
/// gives `None` and refuses nothing.
#[must_use]
pub(super) fn read_peak(probe: &ProbeGroup) -> Option<u64> {
    let bytes = read_bounded(&probe.path.join("memory.peak"), CONTROL_FILE_BYTES).ok()?;
    std::str::from_utf8(&bytes).ok()?.trim_end().parse().ok()
}

/// Removes the probe group with `rmdir`.
///
/// Call only after `kill_and_await_empty` succeeded. The removal is never recursive:
/// cgroup control files cannot be unlinked, so a group that still holds anything fails.
/// Any failure is `CleanupNotObserved`.
pub(super) fn remove_probe(probe: ProbeGroup) -> Result<(), AdmissionRefusal> {
    fs::remove_dir(probe.path).map_err(|_| AdmissionRefusal::CleanupNotObserved)
}
