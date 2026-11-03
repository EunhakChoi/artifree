//! The temporary directory of a one-shot server session (decision 0038).
//!
//! A session owns one directory under a session root. It holds a marker file
//! that names the owning process and a `data` directory for the database and
//! the access token. The directory is removed when the session ends normally
//! ([`SessionDir::close`]) or unwinds ([`Drop`]). A process that is killed
//! leaves its directory behind; the next session removes it with
//! [`SessionDir::sweep_stale`], and only when the marker proves that its owner
//! is gone.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::data_dir;

/// Name prefix of every session directory. A directory without it is never
/// touched by a sweep.
const DIR_PREFIX: &str = "artifree-session-";
/// Name of the marker file inside a session directory.
const MARKER_FILE: &str = ".artifree-session";
/// First line of a marker file; the number is the marker format version.
const MARKER_HEADER: &str = "artifree-session 1";
/// Name of the data directory inside a session directory.
const DATA_DIR: &str = "data";

/// A temporary session directory, removed when this value is closed or dropped.
#[derive(Debug)]
pub struct SessionDir {
    path: PathBuf,
    removed: bool,
}

/// What a [`SessionDir::sweep_stale`] pass did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SweepReport {
    /// Directories of ended sessions that were removed.
    pub removed: usize,
    /// Directories of ended sessions that could not be removed. They stay
    /// where they are and the next sweep tries them again.
    pub failed: usize,
}

impl SessionDir {
    /// Creates a new session directory under `root`, creating `root` as a
    /// private directory when it does not exist. The session directory and
    /// its `data` directory are private to the current user (Unix) and the
    /// marker names this process as the owner.
    ///
    /// # Errors
    ///
    /// When the root or the session directory cannot be created, or the
    /// marker cannot be written. A directory created before the failure is
    /// removed again.
    pub fn create(root: &Path) -> io::Result<Self> {
        let mut bytes = [0_u8; 8];
        getrandom::fill(&mut bytes).map_err(|error| io::Error::other(error.to_string()))?;
        let suffix = format!("{:016x}", u64::from_le_bytes(bytes));
        let path = root.join(format!("{DIR_PREFIX}{suffix}"));
        data_dir::create_new_data_dir(&path).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "cannot create the session directory {}: {error}",
                    path.display()
                ),
            )
        })?;
        // From here on, `session` removes the directory if a step fails.
        let session = Self {
            path,
            removed: false,
        };
        data_dir::create_private_dir(&session.data_dir())?;
        session.write_marker()?;
        Ok(session)
    }

    /// The session directory.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The directory for the database and the access token.
    #[must_use]
    pub fn data_dir(&self) -> PathBuf {
        self.path.join(DATA_DIR)
    }

    /// Removes the session directory and everything in it.
    ///
    /// # Errors
    ///
    /// When the directory cannot be removed. It is then left for the sweep of
    /// a later session, once this process has ended.
    pub fn close(mut self) -> io::Result<()> {
        self.removed = true;
        remove_tree(&self.path)
    }

    /// Removes the directories of sessions whose owner has ended without
    /// removing them, from the root this session was created in.
    ///
    /// A directory is removed only when all of these hold: its name has the
    /// session prefix, it is a real directory (not a symbolic link) owned by
    /// the same user as this session's directory (Unix), its marker is a
    /// regular file in the current format, and the marker's owner process no
    /// longer exists. Anything else, including a directory without a marker,
    /// is left alone.
    #[must_use]
    pub fn sweep_stale(&self) -> SweepReport {
        let mut report = SweepReport::default();
        let Some(root) = self.path.parent() else {
            return report;
        };
        let Ok(entries) = fs::read_dir(root) else {
            return report;
        };
        let own_owner = file_owner(&self.path);
        for entry in entries.flatten() {
            let candidate = entry.path();
            if candidate == self.path || !is_stale_session(&candidate, own_owner) {
                continue;
            }
            match remove_tree(&candidate) {
                Ok(()) => report.removed += 1,
                Err(_) => report.failed += 1,
            }
        }
        report
    }

    fn write_marker(&self) -> io::Result<()> {
        let pid = std::process::id();
        let text = match process_start(pid) {
            Some(start) => format!("{MARKER_HEADER}\npid {pid}\nstart {start}\n"),
            None => format!("{MARKER_HEADER}\npid {pid}\n"),
        };
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.path.join(MARKER_FILE))?;
        file.write_all(text.as_bytes())
    }
}

impl Drop for SessionDir {
    fn drop(&mut self) {
        if !self.removed {
            // A failure cannot be reported from here. The directory stays
            // with a marker whose owner has ended, so the next sweep removes it.
            let _ = remove_tree(&self.path);
        }
    }
}

/// Removes a directory tree; a tree that is already gone counts as removed.
fn remove_tree(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Whether `candidate` is a session directory whose owner has ended.
fn is_stale_session(candidate: &Path, own_owner: Option<u32>) -> bool {
    let named_like_a_session = candidate
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(DIR_PREFIX));
    if !named_like_a_session {
        return false;
    }
    // `symlink_metadata` does not follow a link, so a link is not a directory here.
    let Ok(metadata) = fs::symlink_metadata(candidate) else {
        return false;
    };
    if !metadata.is_dir() || file_owner(candidate) != own_owner {
        return false;
    }
    let marker = candidate.join(MARKER_FILE);
    let Ok(marker_metadata) = fs::symlink_metadata(&marker) else {
        return false;
    };
    if !marker_metadata.is_file() {
        return false;
    }
    let Some(owner) = fs::read_to_string(&marker)
        .ok()
        .and_then(|text| Marker::parse(&text))
    else {
        return false;
    };
    owner.has_ended()
}

/// The user that owns a file, where the platform reports one.
#[cfg(unix)]
fn file_owner(path: &Path) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    fs::symlink_metadata(path)
        .ok()
        .map(|metadata| metadata.uid())
}

#[cfg(not(unix))]
fn file_owner(_path: &Path) -> Option<u32> {
    None
}

/// The owner named by a marker file.
#[derive(Debug, PartialEq, Eq)]
struct Marker {
    pid: u32,
    start: Option<u64>,
}

impl Marker {
    fn parse(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        if lines.next()? != MARKER_HEADER {
            return None;
        }
        let pid = lines.next()?.strip_prefix("pid ")?.parse().ok()?;
        let start = match lines.next() {
            Some(line) => Some(line.strip_prefix("start ")?.parse().ok()?),
            None => None,
        };
        Some(Self { pid, start })
    }

    /// Whether the owner is known to be gone. When this platform cannot tell,
    /// the owner counts as running, so a directory is never removed on a guess.
    fn has_ended(&self) -> bool {
        if !process_table_available() {
            return false;
        }
        match process_start_and_state(self.pid) {
            None => true,
            Some((state, start)) => {
                // A zombie has ended; a different start time means that the
                // process id now belongs to another process.
                matches!(state, 'Z' | 'X') || self.start.is_some_and(|marked| marked != start)
            }
        }
    }
}

/// Whether `/proc/<pid>/stat` can be read on this platform.
fn process_table_available() -> bool {
    Path::new("/proc/self/stat").is_file()
}

/// The start time of a running process in clock ticks since boot, where known.
fn process_start(pid: u32) -> Option<u64> {
    process_start_and_state(pid).map(|(_, start)| start)
}

/// The state letter and start time from `/proc/<pid>/stat`, or `None` when
/// the file is missing or unreadable. The command name is skipped by looking
/// for the last `)`, because the name can hold spaces and parentheses.
fn process_start_and_state(pid: u32) -> Option<(char, u64)> {
    parse_stat(&fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

/// Reads the state and start time from the text of a `/proc/<pid>/stat` file.
fn parse_stat(text: &str) -> Option<(char, u64)> {
    let after_name = text.get(text.rfind(')')? + 1..)?;
    let mut fields = after_name.split_whitespace();
    let state = fields.next()?.chars().next()?;
    // Field 22 of the file is the start time; the state is field 3.
    let start = fields.nth(18)?.parse().ok()?;
    Some((state, start))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use std::process::Command;
    use std::sync::atomic::{AtomicU32, Ordering};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// A fresh, empty session root, removed when the test ends.
    struct Root(PathBuf);

    impl Root {
        fn new() -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let path = std::env::temp_dir().join(format!(
                "artifree-sessiondir-test-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&path);
            Self(path)
        }
    }

    impl Drop for Root {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// The id of a process that has already ended and been reaped.
    #[cfg(target_os = "linux")]
    fn ended_pid() -> io::Result<u32> {
        let mut child = Command::new("true").spawn()?;
        let pid = child.id();
        child.wait()?;
        Ok(pid)
    }

    /// Writes a session directory the way a session that was killed leaves it.
    #[cfg(target_os = "linux")]
    fn leave_behind(root: &Path, name: &str, marker: Option<&str>) -> io::Result<PathBuf> {
        let dir = root.join(name);
        data_dir::create_new_data_dir(&dir)?;
        fs::write(dir.join("data-file"), b"secret")?;
        if let Some(marker) = marker {
            fs::write(dir.join(MARKER_FILE), marker)?;
        }
        Ok(dir)
    }

    #[test]
    #[cfg(unix)]
    fn a_session_directory_and_its_data_directory_are_private() -> TestResult {
        use std::os::unix::fs::PermissionsExt;
        let root = Root::new();
        let session = SessionDir::create(&root.0)?;
        for dir in [session.path().to_path_buf(), session.data_dir()] {
            let mode = fs::metadata(&dir)?.permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{} must be private", dir.display());
        }
        Ok(())
    }

    #[test]
    fn close_removes_everything_and_a_dropped_session_does_too() -> TestResult {
        let root = Root::new();
        let closed = SessionDir::create(&root.0)?;
        fs::write(closed.data_dir().join("db"), b"x")?;
        let closed_path = closed.path().to_path_buf();
        closed.close()?;
        assert!(
            !closed_path.exists(),
            "close left {}",
            closed_path.display()
        );

        let dropped = SessionDir::create(&root.0)?;
        let dropped_path = dropped.path().to_path_buf();
        drop(dropped);
        assert!(
            !dropped_path.exists(),
            "drop left {}",
            dropped_path.display()
        );
        assert!(root.0.exists(), "the root itself must stay");
        Ok(())
    }

    #[test]
    fn two_sessions_get_different_directories() -> TestResult {
        let root = Root::new();
        let first = SessionDir::create(&root.0)?;
        let second = SessionDir::create(&root.0)?;
        assert_ne!(first.path(), second.path());
        Ok(())
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn the_sweep_removes_only_directories_whose_owner_has_ended() -> TestResult {
        let root = Root::new();
        let ended = ended_pid()?;
        let gone = leave_behind(
            &root.0,
            "artifree-session-0000000000000001",
            Some(&format!("{MARKER_HEADER}\npid {ended}\n")),
        )?;
        let alive = leave_behind(
            &root.0,
            "artifree-session-0000000000000002",
            Some(&format!("{MARKER_HEADER}\npid {}\n", std::process::id())),
        )?;
        let session = SessionDir::create(&root.0)?;
        let report = session.sweep_stale();
        assert_eq!(
            report,
            SweepReport {
                removed: 1,
                failed: 0
            }
        );
        assert!(!gone.exists(), "the ended session's directory must go");
        assert!(alive.exists(), "a running owner's directory must stay");
        assert!(session.path().exists(), "the sweeping session must stay");
        Ok(())
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn the_sweep_leaves_anything_it_cannot_prove_is_a_dead_session() -> TestResult {
        let root = Root::new();
        let ended = ended_pid()?;
        let good_marker = format!("{MARKER_HEADER}\npid {ended}\n");
        let untouched = [
            // No marker at all.
            leave_behind(&root.0, "artifree-session-0000000000000003", None)?,
            // A marker in another format.
            leave_behind(
                &root.0,
                "artifree-session-0000000000000004",
                Some(&format!("artifree-session 2\npid {ended}\n")),
            )?,
            // A marker without a readable owner.
            leave_behind(
                &root.0,
                "artifree-session-0000000000000005",
                Some(&format!("{MARKER_HEADER}\npid soon\n")),
            )?,
            // The right marker in a directory that is not named like a session.
            leave_behind(&root.0, "other-directory", Some(&good_marker))?,
        ];
        // A link named like a session that points at a stale session
        // directory: neither the link nor its target may be removed.
        let target = leave_behind(&root.0, "link-target", Some(&good_marker))?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, root.0.join("artifree-session-0000000000000006"))?;
        let session = SessionDir::create(&root.0)?;
        assert_eq!(session.sweep_stale(), SweepReport::default());
        for path in untouched.iter().chain([&target]) {
            assert!(path.exists(), "{} must not be removed", path.display());
        }
        assert!(
            target.join("data-file").exists(),
            "a link must not lead the sweep out of the root"
        );
        Ok(())
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn a_reused_process_id_does_not_keep_a_dead_session_alive() -> TestResult {
        // The marker names this very (running) process, but with a start time
        // it cannot have: the process id was reused by another process.
        let Some(start) = process_start(std::process::id()) else {
            return Ok(());
        };
        let root = Root::new();
        let stale = leave_behind(
            &root.0,
            "artifree-session-0000000000000007",
            Some(&format!(
                "{MARKER_HEADER}\npid {}\nstart {}\n",
                std::process::id(),
                start + 1
            )),
        )?;
        let session = SessionDir::create(&root.0)?;
        assert_eq!(session.sweep_stale().removed, 1);
        assert!(!stale.exists());
        Ok(())
    }

    #[test]
    fn a_marker_is_parsed_only_in_the_exact_format() {
        assert_eq!(
            Marker::parse("artifree-session 1\npid 42\nstart 7\n"),
            Some(Marker {
                pid: 42,
                start: Some(7)
            })
        );
        assert_eq!(
            Marker::parse("artifree-session 1\npid 42\n"),
            Some(Marker {
                pid: 42,
                start: None
            })
        );
        for bad in [
            "",
            "artifree-session 1\n",
            "artifree-session 1\nPID 4\n",
            "x\npid 4\n",
        ] {
            assert_eq!(Marker::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_command_name_with_spaces_and_parentheses_does_not_shift_the_fields() {
        let zeros = "0 ".repeat(18);
        let line = format!("42 (we ird) name) R {zeros}777 0 0\n");
        assert_eq!(parse_stat(&line), Some(('R', 777)));
        assert_eq!(parse_stat("42 (short) R 1 2"), None);
    }
}
