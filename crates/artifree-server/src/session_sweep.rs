//! The marker of a one-shot session directory, and the removal of the
//! directories of dead sessions (decision 0039).
//!
//! The sweep fails closed: a directory is removed only when every test proves
//! it is ours and its owner is gone, and any doubt keeps it. It is effective on
//! Linux only (it reads `/proc`); elsewhere it removes nothing.

use std::fmt::Write as _;

#[cfg(target_os = "linux")]
use std::fs::{self, File};
#[cfg(target_os = "linux")]
use std::io::Read;
#[cfg(target_os = "linux")]
use std::os::unix::fs::MetadataExt;
#[cfg(target_os = "linux")]
use std::path::Path;

#[cfg(target_os = "linux")]
use crate::one_shot::{self, SESSION_PREFIX, StopLatch};

/// File name of the marker inside a session directory.
pub const MARKER_FILE: &str = "artifree-session";
/// First line of a marker; the number is the format version.
const MARKER_HEADER: &str = "artifree-session 1";
/// A marker is a few dozen bytes; anything longer is not ours.
#[cfg(target_os = "linux")]
const MAX_MARKER_BYTES: u64 = 256;

/// The text of the marker of the current process: the format header, the
/// process id, and the process start time when it can be read.
#[must_use]
pub fn marker_text(pid: u32, start: Option<u64>) -> String {
    let mut text = format!("{MARKER_HEADER}\npid {pid}\n");
    if let Some(start) = start {
        // Writing to a String cannot fail.
        writeln!(text, "start {start}").ok();
    }
    text
}

/// What one sweep did. Counters only; the names of the directories are not
/// recorded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Whether this platform can sweep at all (Linux).
    pub supported: bool,
    /// Directories of dead sessions that were removed.
    pub removed: u32,
    /// Directories kept because their owner is running.
    pub kept_running: u32,
    /// Directories kept because the state of their owner could not be proven.
    pub kept_unknown: u32,
    /// Look-alike directories kept because they are not ours.
    pub kept_not_ours: u32,
    /// Directories or entries that could not be read or removed.
    pub failed: u32,
    /// Whether a stop request ended the sweep early.
    pub stopped: bool,
}

impl SweepReport {
    /// One line for a person, or `None` when the sweep found nothing worth
    /// saying.
    #[must_use]
    pub fn summary(&self) -> Option<String> {
        if !self.supported {
            return None;
        }
        let kept = self.kept_running + self.kept_unknown + self.kept_not_ours + self.failed;
        if self.removed == 0 && kept == 0 {
            return None;
        }
        Some(format!(
            "Sweep of earlier one-shot sessions: {} removed, {} owner still running, {} owner state unknown (kept), {} not recognized (kept), {} failed.",
            self.removed, self.kept_running, self.kept_unknown, self.kept_not_ours, self.failed
        ))
    }
}

/// A parsed marker.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Marker {
    pid: u32,
    start: Option<u64>,
}

/// Parses a marker in exactly the current format: the header line, a `pid`
/// line, an optional `start` line, each ended by a newline, and nothing else.
#[cfg(target_os = "linux")]
fn parse_marker(text: &str) -> Option<Marker> {
    fn number<T: std::str::FromStr>(line: &str, key: &str) -> Option<T> {
        let digits = line.strip_prefix(key)?.strip_prefix(' ')?;
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        digits.parse().ok()
    }
    let body = text.strip_suffix('\n')?;
    let mut lines = body.split('\n');
    if lines.next()? != MARKER_HEADER {
        return None;
    }
    let pid: u32 = number(lines.next()?, "pid")?;
    let start: Option<u64> = match lines.next() {
        Some(line) => Some(number(line, "start")?),
        None => None,
    };
    if lines.next().is_some() || pid == 0 {
        return None;
    }
    Some(Marker { pid, start })
}

/// What `/proc/<pid>/stat` says about a process.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProcStat {
    state: char,
    start: u64,
}

/// Parses a stat line: `pid (name) state ... starttime ...`. The name may
/// contain spaces and parentheses, so fields are counted after the last `)`.
#[cfg(target_os = "linux")]
fn parse_stat(text: &str) -> Option<ProcStat> {
    let (_, after) = text.rsplit_once(')')?;
    let mut fields = after.split_whitespace();
    let state = fields.next()?.chars().next()?;
    if !state.is_ascii_alphabetic() {
        return None;
    }
    // Field 22 is the start time; the state was field 3.
    let start: u64 = fields.nth(18)?.parse().ok()?;
    Some(ProcStat { state, start })
}

/// The answer of one lookup in the process table.
#[cfg(target_os = "linux")]
enum Lookup {
    Found(ProcStat),
    /// The process entry does not exist.
    Missing,
    /// Anything else: permission, I/O error, unparsable text.
    Unreadable,
}

#[cfg(target_os = "linux")]
fn lookup(proc_root: &Path, pid: u32) -> Lookup {
    match fs::read_to_string(proc_root.join(pid.to_string()).join("stat")) {
        Ok(text) => parse_stat(&text).map_or(Lookup::Unreadable, Lookup::Found),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Lookup::Missing,
        Err(_) => Lookup::Unreadable,
    }
}

/// The start time of this process, for the marker, or `None` when `/proc`
/// cannot say (and on platforms without it).
#[cfg(target_os = "linux")]
#[must_use]
pub fn own_start_time() -> Option<u64> {
    match lookup(Path::new("/proc"), std::process::id()) {
        Lookup::Found(stat) => Some(stat.start),
        Lookup::Missing | Lookup::Unreadable => None,
    }
}

/// Only Linux can say when a process started.
#[cfg(not(target_os = "linux"))]
#[must_use]
pub fn own_start_time() -> Option<u64> {
    None
}

/// Whether the process that wrote a marker is still there.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owner {
    Running,
    Gone,
    Unknown,
}

/// Three-way lookup. `Gone` only for a missing process entry, a zombie or
/// dead state, or a different start time (the pid was reused). A table that does
/// not show this very process is not a process table to trust.
#[cfg(target_os = "linux")]
fn owner_of(proc_root: &Path, own_pid: u32, marker: Marker) -> Owner {
    if !matches!(lookup(proc_root, own_pid), Lookup::Found(_)) {
        return Owner::Unknown;
    }
    match lookup(proc_root, marker.pid) {
        Lookup::Missing => Owner::Gone,
        Lookup::Unreadable => Owner::Unknown,
        Lookup::Found(stat) => {
            if matches!(stat.state, 'Z' | 'X' | 'x')
                || marker.start.is_some_and(|s| s != stat.start)
            {
                Owner::Gone
            } else {
                Owner::Running
            }
        }
    }
}

/// A regular marker file of bounded size, read as text.
#[cfg(target_os = "linux")]
fn read_marker(dir: &Path) -> Option<Marker> {
    let path = dir.join(MARKER_FILE);
    let metadata = fs::symlink_metadata(&path).ok()?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_MARKER_BYTES {
        return None;
    }
    let mut text = String::new();
    File::open(&path)
        .ok()?
        .take(MAX_MARKER_BYTES + 1)
        .read_to_string(&mut text)
        .ok()?;
    if text.len() as u64 > MAX_MARKER_BYTES {
        return None;
    }
    parse_marker(&text)
}

#[cfg(target_os = "linux")]
enum Verdict {
    NotOurs,
    Running,
    Unknown,
    Dead,
}

#[cfg(target_os = "linux")]
fn verdict(path: &Path, uid: u32, proc_root: &Path, own_pid: u32) -> Verdict {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Verdict::NotOurs;
    };
    if !name.starts_with(SESSION_PREFIX) {
        return Verdict::NotOurs;
    }
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Verdict::Unknown;
    };
    if !metadata.file_type().is_dir() || metadata.uid() != uid {
        return Verdict::NotOurs;
    }
    let Some(marker) = read_marker(path) else {
        return Verdict::NotOurs;
    };
    match owner_of(proc_root, own_pid, marker) {
        Owner::Running => Verdict::Running,
        Owner::Unknown => Verdict::Unknown,
        Owner::Gone => Verdict::Dead,
    }
}

/// Removes the directories of dead one-shot sessions found directly under
/// `root`. `own_dir` is the directory of the running session: it is skipped and
/// its owner is the user whose directories may be removed. The `stop` latch is
/// checked before every entry.
#[cfg(target_os = "linux")]
#[must_use]
pub fn sweep(root: &Path, own_dir: &Path, stop: &StopLatch) -> SweepReport {
    sweep_in(root, own_dir, Path::new("/proc"), std::process::id(), stop)
}

/// [`sweep`] with an injectable process table and process id.
#[cfg(target_os = "linux")]
fn sweep_in(
    root: &Path,
    own_dir: &Path,
    proc_root: &Path,
    own_pid: u32,
    stop: &StopLatch,
) -> SweepReport {
    let mut report = SweepReport {
        supported: true,
        ..SweepReport::default()
    };
    // Without our own user there is no way to tell whose a directory is.
    let Ok(own) = fs::symlink_metadata(own_dir) else {
        report.failed += 1;
        return report;
    };
    let uid = own.uid();
    let Ok(entries) = fs::read_dir(root) else {
        report.failed += 1;
        return report;
    };
    for entry in entries {
        one_shot::hold_for_test("sweep-entry", stop);
        if stop.is_set() {
            report.stopped = true;
            break;
        }
        let Ok(entry) = entry else {
            report.failed += 1;
            continue;
        };
        let path = entry.path();
        if path == own_dir {
            continue;
        }
        match verdict(&path, uid, proc_root, own_pid) {
            Verdict::NotOurs => {
                // Only names that look like ours are worth counting.
                if entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with(SESSION_PREFIX))
                {
                    report.kept_not_ours += 1;
                }
            }
            Verdict::Running => report.kept_running += 1,
            Verdict::Unknown => report.kept_unknown += 1,
            Verdict::Dead => match fs::remove_dir_all(&path) {
                Ok(()) => report.removed += 1,
                Err(_) => report.failed += 1,
            },
        }
    }
    report
}

/// Elsewhere the owner of a directory cannot be proven gone, so nothing is
/// removed.
#[cfg(not(target_os = "linux"))]
#[must_use]
pub fn sweep(
    _root: &std::path::Path,
    _own_dir: &std::path::Path,
    _stop: &crate::one_shot::StopLatch,
) -> SweepReport {
    SweepReport::default()
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::sync::atomic::{AtomicU32, Ordering};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// A scratch directory holding a session root and a proc-shaped table.
    struct Scratch {
        base: std::path::PathBuf,
    }

    impl Scratch {
        fn new() -> Result<Self, std::io::Error> {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let base = std::env::temp_dir().join(format!(
                "artifree-sweep-test-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(base.join("root"))?;
            fs::create_dir_all(base.join("proc"))?;
            let scratch = Self { base };
            scratch.process(OWN, 'S', 1000)?;
            fs::create_dir(scratch.root().join(format!("{SESSION_PREFIX}own")))?;
            Ok(scratch)
        }
        fn root(&self) -> std::path::PathBuf {
            self.base.join("root")
        }
        fn own_dir(&self) -> std::path::PathBuf {
            self.root().join(format!("{SESSION_PREFIX}own"))
        }
        fn proc_root(&self) -> std::path::PathBuf {
            self.base.join("proc")
        }
        /// A process entry with a real-looking stat line (name with spaces
        /// and a parenthesis, to prove the field counting).
        fn process(&self, pid: u32, state: char, start: u64) -> std::io::Result<()> {
            let dir = self.proc_root().join(pid.to_string());
            fs::create_dir_all(&dir)?;
            let tail: Vec<String> = (4..=21).map(|n| n.to_string()).collect();
            fs::write(
                dir.join("stat"),
                format!(
                    "{pid} (we (ird) name) {state} {} {start} 0 0\n",
                    tail.join(" ")
                ),
            )
        }
        fn session(&self, name: &str, marker: &str) -> std::io::Result<std::path::PathBuf> {
            let dir = self.root().join(name);
            fs::create_dir(&dir)?;
            fs::write(dir.join(MARKER_FILE), marker)?;
            Ok(dir)
        }
        fn sweep(&self) -> SweepReport {
            sweep_in(
                &self.root(),
                &self.own_dir(),
                &self.proc_root(),
                OWN,
                &StopLatch::new(),
            )
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.base).ok();
        }
    }

    const OWN: u32 = 4242;
    const DEAD: u32 = 900_001;

    fn name(suffix: &str) -> String {
        format!("{SESSION_PREFIX}{suffix}")
    }

    #[test]
    fn marker_round_trips_and_rejects_anything_else() {
        let text = marker_text(77, Some(5));
        assert_eq!(text, "artifree-session 1\npid 77\nstart 5\n");
        assert_eq!(
            parse_marker(&text),
            Some(Marker {
                pid: 77,
                start: Some(5)
            })
        );
        assert_eq!(
            parse_marker("artifree-session 1\npid 77\n"),
            Some(Marker {
                pid: 77,
                start: None
            })
        );
        for bad in [
            "",
            "artifree-session 1\npid 77",
            "artifree-session 2\npid 77\n",
            "artifree-session 1\npid +77\n",
            "artifree-session 1\npid 0\n",
            "artifree-session 1\npid 77\nstart 5\n\n",
            "artifree-session 1\npid 77\nstart 5\nstart 6\n",
            "artifree-session 1\npid 77\nextra\n",
            "artifree-session 1\npid 77\nstart \n",
            "artifree-session 1\npid 77\nstart 5\nx\n",
            "artifree-session 1\npid 77 \n",
            "pid 77\nartifree-session 1\n",
        ] {
            assert_eq!(parse_marker(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn stat_fields_are_counted_after_the_last_parenthesis() {
        let text = "12 (a b) c) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 777 0\n";
        assert_eq!(
            parse_stat(text),
            Some(ProcStat {
                state: 'S',
                start: 777
            })
        );
        assert_eq!(parse_stat("12 (a) S 1 2"), None);
        assert_eq!(parse_stat("garbage"), None);
    }

    #[test]
    fn a_dead_owner_is_removed_and_a_live_one_is_not() -> TestResult {
        let scratch = Scratch::new()?;
        scratch.process(DEAD + 1, 'S', 50)?;
        let dead = scratch.session(&name("dead"), &marker_text(DEAD, Some(50)))?;
        let live = scratch.session(&name("live"), &marker_text(DEAD + 1, Some(50)))?;
        let no_start = scratch.session(&name("nostart"), &marker_text(DEAD + 1, None))?;
        let report = scratch.sweep();
        assert!(!dead.exists(), "the dead owner's directory must go");
        assert!(live.exists() && no_start.exists());
        assert!(scratch.own_dir().exists(), "never our own directory");
        assert_eq!((report.removed, report.kept_running), (1, 2), "{report:?}");
        Ok(())
    }

    #[test]
    fn zombie_and_reused_pid_count_as_gone() -> TestResult {
        let scratch = Scratch::new()?;
        scratch.process(DEAD, 'Z', 50)?;
        scratch.process(DEAD + 1, 'S', 51)?;
        let zombie = scratch.session(&name("zombie"), &marker_text(DEAD, Some(50)))?;
        let reused = scratch.session(&name("reused"), &marker_text(DEAD + 1, Some(50)))?;
        assert_eq!(scratch.sweep().removed, 2);
        assert!(!zombie.exists() && !reused.exists());
        Ok(())
    }

    #[test]
    fn look_alikes_are_never_removed() -> TestResult {
        let scratch = Scratch::new()?;
        let good = marker_text(DEAD, Some(50));
        let wrong_prefix = scratch.session("other-prefix-1", &good)?;
        let wrong_format = scratch.session(&name("format"), "artifree-session 9\npid 900001\n")?;
        let trailing = scratch.session(&name("trailing"), &format!("{good}\n"))?;
        let extra = scratch.session(&name("extra"), &format!("{good}junk\n"))?;
        let no_marker = scratch.root().join(name("nomarker"));
        fs::create_dir(&no_marker)?;
        let big = scratch.session(&name("big"), &format!("{good}{}", " ".repeat(300)))?;
        // A symbolic link to a directory that would otherwise qualify.
        let target = scratch.session("target-dir", &good)?;
        let link = scratch.root().join(name("link"));
        symlink(&target, &link)?;
        // A marker that is itself a link.
        let linked_marker = scratch.root().join(name("linkedmarker"));
        fs::create_dir(&linked_marker)?;
        fs::write(scratch.base.join("real-marker"), &good)?;
        symlink(
            scratch.base.join("real-marker"),
            linked_marker.join(MARKER_FILE),
        )?;
        // A plain file with the prefix.
        let file = scratch.root().join(name("file"));
        fs::write(&file, &good)?;

        let report = scratch.sweep();
        for path in [
            &wrong_prefix,
            &wrong_format,
            &trailing,
            &extra,
            &no_marker,
            &big,
            &target,
            &linked_marker,
            &file,
        ] {
            assert!(path.exists(), "{} must be kept", path.display());
        }
        assert!(
            fs::symlink_metadata(&link).is_ok(),
            "the link itself must be kept"
        );
        assert_eq!(report.removed, 0, "{report:?}");
        Ok(())
    }

    #[test]
    fn an_unknown_owner_state_keeps_the_directory() -> TestResult {
        let scratch = Scratch::new()?;
        let good = marker_text(DEAD, Some(50));
        // Unparsable stat.
        let unparsable = scratch.session(&name("unparsable"), &good)?;
        fs::create_dir_all(scratch.proc_root().join(DEAD.to_string()))?;
        fs::write(
            scratch.proc_root().join(DEAD.to_string()).join("stat"),
            "garbage",
        )?;
        // The process entry is not a directory: another I/O error than NotFound.
        let odd = scratch.session(&name("odd"), &marker_text(DEAD + 2, Some(50)))?;
        fs::write(scratch.proc_root().join((DEAD + 2).to_string()), "a file")?;
        let report = scratch.sweep();
        assert!(unparsable.exists() && odd.exists());
        assert_eq!((report.removed, report.kept_unknown), (0, 2), "{report:?}");
        Ok(())
    }

    #[test]
    fn an_unreadable_entry_keeps_the_directory() -> TestResult {
        let scratch = Scratch::new()?;
        let dir = scratch.session(&name("denied"), &marker_text(DEAD, Some(50)))?;
        scratch.process(DEAD, 'S', 50)?;
        let stat = scratch.proc_root().join(DEAD.to_string()).join("stat");
        fs::set_permissions(&stat, fs::Permissions::from_mode(0o000))?;
        if fs::read_to_string(&stat).is_ok() {
            writeln!(
                std::io::stderr(),
                "skipped: this user can read a mode-000 file (root?)"
            )
            .ok();
            return Ok(());
        }
        let report = scratch.sweep();
        assert!(dir.exists(), "permission denied is unknown, not gone");
        assert_eq!(report.kept_unknown, 1, "{report:?}");
        Ok(())
    }

    #[test]
    fn a_missing_or_foreign_process_table_keeps_everything() -> TestResult {
        let scratch = Scratch::new()?;
        let dir = scratch.session(&name("dead"), &marker_text(DEAD, Some(50)))?;
        // No table at all.
        let none = sweep_in(
            &scratch.root(),
            &scratch.own_dir(),
            &scratch.base.join("no-such-proc"),
            OWN,
            &StopLatch::new(),
        );
        // A table that does not list this very process.
        let other = sweep_in(
            &scratch.root(),
            &scratch.own_dir(),
            &scratch.proc_root(),
            OWN + 1,
            &StopLatch::new(),
        );
        assert!(dir.exists());
        assert_eq!((none.removed, none.kept_unknown), (0, 1), "{none:?}");
        assert_eq!((other.removed, other.kept_unknown), (0, 1), "{other:?}");
        Ok(())
    }

    #[test]
    fn without_our_own_user_nothing_is_swept() -> TestResult {
        // Another user's directory cannot be made here; the same rule is
        // that an unreadable own directory gives no uid, so no sweep.
        let scratch = Scratch::new()?;
        let dir = scratch.session(&name("dead"), &marker_text(DEAD, Some(50)))?;
        let report = sweep_in(
            &scratch.root(),
            &scratch.root().join(name("missing-own")),
            &scratch.proc_root(),
            OWN,
            &StopLatch::new(),
        );
        assert!(dir.exists());
        assert_eq!((report.removed, report.failed), (0, 1), "{report:?}");
        Ok(())
    }

    #[test]
    fn a_latched_stop_ends_the_sweep_before_any_removal() -> TestResult {
        let scratch = Scratch::new()?;
        let dir = scratch.session(&name("dead"), &marker_text(DEAD, Some(50)))?;
        let stop = StopLatch::new();
        stop.set();
        let report = sweep_in(
            &scratch.root(),
            &scratch.own_dir(),
            &scratch.proc_root(),
            OWN,
            &stop,
        );
        assert!(dir.exists());
        assert!(report.stopped && report.removed == 0, "{report:?}");
        Ok(())
    }

    #[test]
    fn the_real_process_table_knows_this_process() {
        assert!(own_start_time().is_some());
    }
}
