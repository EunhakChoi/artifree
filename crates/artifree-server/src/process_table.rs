//! Whether a process is still running, read from a Linux-style process table
//! (decision 0039, item 9).
//!
//! The answer has three values on purpose. Only [`Liveness::Gone`] allows a
//! caller to delete anything; every doubt is [`Liveness::Unknown`].

use std::fs;
use std::io;
use std::path::PathBuf;

/// What the process table says about one process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// The process exists and is not a zombie.
    Running,
    /// The table proves the process ended: its entry does not exist, it is a
    /// zombie or dead, or the pid now belongs to a process that started later.
    Gone,
    /// The table cannot tell. Treat as running.
    Unknown,
}

/// A process table: `/proc` on Linux, an injected directory in tests, nothing
/// elsewhere.
#[derive(Debug, Clone)]
pub struct ProcessTable {
    root: Option<PathBuf>,
    own_pid: u32,
}

impl ProcessTable {
    /// The table of this machine. Only Linux has one this module can read;
    /// elsewhere every answer is [`Liveness::Unknown`].
    #[must_use]
    pub fn system() -> Self {
        Self {
            root: cfg!(target_os = "linux").then(|| PathBuf::from("/proc")),
            own_pid: std::process::id(),
        }
    }

    /// A table read from `root`, laid out like `/proc` (`<root>/<pid>/stat`),
    /// in which `own_pid` names the calling process. Tests build their own.
    #[cfg(test)]
    #[must_use]
    pub fn at(root: impl Into<PathBuf>, own_pid: u32) -> Self {
        Self {
            root: Some(root.into()),
            own_pid,
        }
    }

    /// Whether this table can be read at all on this platform.
    #[must_use]
    pub fn is_readable_here(&self) -> bool {
        self.root.is_some()
    }

    /// The start time, in clock ticks since boot, of this process, if the table
    /// shows it. A marker records it so that a reused pid is recognized.
    #[must_use]
    pub fn own_start_time(&self) -> Option<u64> {
        let root = self.root.as_ref()?;
        let stat = read_stat(root, self.own_pid).ok()?;
        Some(stat.start_time)
    }

    /// What the table says about `pid`; `noted_start` is the start time
    /// recorded when the pid was noted, if any.
    #[must_use]
    pub fn liveness(&self, pid: u32, noted_start: Option<u64>) -> Liveness {
        let Some(root) = self.root.as_ref() else {
            return Liveness::Unknown;
        };
        // The table must show this very process alive. Otherwise "no entry"
        // could only mean that the table is not mounted or not ours.
        match read_stat(root, self.own_pid) {
            Ok(me) if !me.state.is_dead() => {}
            _ => return Liveness::Unknown,
        }
        match read_stat(root, pid) {
            Err(StatError::Missing) => Liveness::Gone,
            Err(StatError::Unreadable) => Liveness::Unknown,
            Ok(entry) if entry.state.is_dead() => Liveness::Gone,
            Ok(entry) => match noted_start {
                Some(noted) if noted != entry.start_time => Liveness::Gone,
                _ => Liveness::Running,
            },
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum StatError {
    /// A definite "no such file or directory".
    Missing,
    /// Any other failure, including a stat that does not parse.
    Unreadable,
}

#[derive(Debug, PartialEq, Eq)]
struct Stat {
    state: State,
    start_time: u64,
}

#[derive(Debug, PartialEq, Eq)]
enum State {
    Alive,
    /// Zombie (`Z`) or dead (`X`, `x`).
    Dead,
}

impl State {
    fn is_dead(&self) -> bool {
        *self == Self::Dead
    }
}

fn read_stat(root: &std::path::Path, pid: u32) -> Result<Stat, StatError> {
    let bytes = match fs::read(root.join(pid.to_string()).join("stat")) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Err(StatError::Missing),
        Err(_) => return Err(StatError::Unreadable),
    };
    parse_stat(pid, &bytes).ok_or(StatError::Unreadable)
}

/// Reads `pid (name) state ... starttime ...` (proc(5)). The name may hold
/// spaces and parentheses, so fields are counted after the last `)`.
fn parse_stat(pid: u32, bytes: &[u8]) -> Option<Stat> {
    let head = format!("{pid} (");
    if !bytes.starts_with(head.as_bytes()) {
        return None;
    }
    let close = bytes.iter().rposition(|&byte| byte == b')')?;
    let rest = std::str::from_utf8(bytes.get(close + 1..)?).ok()?;
    let mut fields = rest.split_ascii_whitespace();
    let state = fields.next()?;
    let state = match state {
        "Z" | "X" | "x" => State::Dead,
        _ if state.len() == 1 && state.bytes().all(|byte| byte.is_ascii_alphabetic()) => {
            State::Alive
        }
        _ => return None,
    };
    // Field 22 is the 20th after the name; `state` was the first of them.
    let start_time = fields.nth(18)?;
    if !start_time.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(Stat {
        state,
        start_time: start_time.parse().ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;
    use std::io::Write;
    use std::path::Path;
    use std::sync::atomic::{AtomicU32, Ordering};

    type TestResult = Result<(), Box<dyn Error>>;

    /// A `stat` line with the given name, state and start time (field 22).
    fn stat_line(pid: u32, name: &str, state: &str, start: u64) -> String {
        format!(
            "{pid} ({name}) {state} 1 1 1 0 -1 4194560 100 0 0 0 1 1 0 0 20 0 1 0 {start} 1000 100 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0\n"
        )
    }

    struct Table {
        root: PathBuf,
    }

    const OWN: u32 = 100;

    impl Table {
        fn new() -> io::Result<Self> {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let root = std::env::temp_dir().join(format!(
                "artifree-proc-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root)?;
            let table = Self { root };
            table.put(OWN, &stat_line(OWN, "artifree-server", "S", 5000))?;
            Ok(table)
        }

        fn put(&self, pid: u32, content: &str) -> io::Result<()> {
            let dir = self.root.join(pid.to_string());
            fs::create_dir_all(&dir)?;
            fs::write(dir.join("stat"), content)
        }

        fn view(&self) -> ProcessTable {
            ProcessTable::at(&self.root, OWN)
        }

        fn liveness(&self, pid: u32, start: Option<u64>) -> Liveness {
            self.view().liveness(pid, start)
        }
    }

    impl Drop for Table {
        fn drop(&mut self) {
            // A test may leave a mode-000 entry behind.
            restore_modes(&self.root);
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn restore_modes(path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
            if let Ok(entries) = fs::read_dir(path) {
                for entry in entries.flatten() {
                    restore_modes(&entry.path());
                }
            }
        }
        #[cfg(not(unix))]
        let _ = path;
    }

    /// Says on stderr that a test could not run here (it did not fail).
    fn skipped(reason: &str) {
        let _ = writeln!(std::io::stderr(), "skipped: {reason}");
    }

    #[test]
    fn a_live_process_is_running_with_or_without_a_matching_start_time() -> TestResult {
        let table = Table::new()?;
        table.put(200, &stat_line(200, "sleep", "S", 7000))?;
        assert_eq!(table.liveness(200, None), Liveness::Running);
        assert_eq!(table.liveness(200, Some(7000)), Liveness::Running);
        Ok(())
    }

    #[test]
    fn a_missing_entry_is_gone() -> TestResult {
        let table = Table::new()?;
        assert_eq!(table.liveness(4242, None), Liveness::Gone);
        assert_eq!(table.liveness(4242, Some(1)), Liveness::Gone);
        Ok(())
    }

    #[test]
    fn zombie_and_dead_states_are_gone() -> TestResult {
        let table = Table::new()?;
        for (pid, state) in [(201, "Z"), (202, "X"), (203, "x")] {
            table.put(pid, &stat_line(pid, "x", state, 7000))?;
            assert_eq!(table.liveness(pid, None), Liveness::Gone, "state {state}");
        }
        for (pid, state) in [(204, "R"), (205, "D"), (206, "T"), (207, "I")] {
            table.put(pid, &stat_line(pid, "x", state, 7000))?;
            assert_eq!(
                table.liveness(pid, None),
                Liveness::Running,
                "state {state}"
            );
        }
        Ok(())
    }

    #[test]
    fn a_different_start_time_means_the_pid_was_reused() -> TestResult {
        let table = Table::new()?;
        table.put(200, &stat_line(200, "sleep", "S", 7000))?;
        assert_eq!(table.liveness(200, Some(6999)), Liveness::Gone);
        Ok(())
    }

    #[test]
    fn a_name_with_spaces_and_parentheses_is_skipped_by_the_last_parenthesis() -> TestResult {
        let table = Table::new()?;
        table.put(200, &stat_line(200, "a b) S 1 (c", "S", 7000))?;
        assert_eq!(table.liveness(200, Some(7000)), Liveness::Running);
        assert_eq!(table.liveness(200, Some(7001)), Liveness::Gone);
        Ok(())
    }

    #[test]
    fn unparsable_stat_is_unknown() -> TestResult {
        let table = Table::new()?;
        let good = stat_line(200, "x", "S", 7000);
        let cases = [
            ("empty", String::new()),
            ("no parenthesis", "200 sleep S 1".to_owned()),
            ("too few fields", "200 (x) S 1 2 3\n".to_owned()),
            ("another pid", stat_line(999, "x", "S", 7000)),
            ("state not a letter", stat_line(200, "x", "5", 7000)),
            ("state too long", stat_line(200, "x", "SS", 7000)),
            ("start not a number", good.replace(" 7000 ", " 70x0 ")),
            ("start negative", good.replace(" 7000 ", " -7000 ")),
        ];
        for (name, content) in cases {
            table.put(200, &content)?;
            assert_eq!(
                table.liveness(200, Some(7000)),
                Liveness::Unknown,
                "{name}: {content:?}"
            );
        }
        let mut inside = good.into_bytes();
        inside.splice(10..10, [0xff]);
        fs::write(table.root.join("200").join("stat"), inside)?;
        assert_eq!(
            table.liveness(200, None),
            Liveness::Unknown,
            "not UTF-8 among the fields"
        );
        Ok(())
    }

    #[test]
    fn a_stat_that_is_a_directory_is_unknown() -> TestResult {
        let table = Table::new()?;
        fs::create_dir_all(table.root.join("200").join("stat"))?;
        assert_eq!(table.liveness(200, None), Liveness::Unknown);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_stat_that_cannot_be_read_is_unknown_not_gone() -> TestResult {
        use std::os::unix::fs::PermissionsExt;
        let table = Table::new()?;
        table.put(200, &stat_line(200, "sleep", "S", 7000))?;
        let file = table.root.join("200").join("stat");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o000))?;
        if fs::read(&file).is_ok() {
            // Root (or a capability) reads any file, so no denial can be made.
            skipped("this user can read a mode-000 file");
            return Ok(());
        }
        assert_eq!(table.liveness(200, Some(7000)), Liveness::Unknown);
        assert_eq!(table.liveness(200, None), Liveness::Unknown);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn an_entry_directory_that_cannot_be_entered_is_unknown_not_gone() -> TestResult {
        use std::os::unix::fs::PermissionsExt;
        let table = Table::new()?;
        table.put(200, &stat_line(200, "sleep", "S", 7000))?;
        let entry = table.root.join("200");
        fs::set_permissions(&entry, fs::Permissions::from_mode(0o000))?;
        if fs::read(entry.join("stat")).is_ok() {
            skipped("this user can enter a mode-000 directory");
            return Ok(());
        }
        assert_eq!(table.liveness(200, None), Liveness::Unknown);
        Ok(())
    }

    #[test]
    fn a_table_that_does_not_show_this_process_is_unknown() -> TestResult {
        let table = Table::new()?;
        fs::remove_dir_all(table.root.join(OWN.to_string()))?;
        // 4242 has no entry either, which here must not read as "gone".
        assert_eq!(table.liveness(4242, None), Liveness::Unknown);
        table.put(OWN, &stat_line(OWN, "artifree-server", "Z", 5000))?;
        assert_eq!(table.liveness(4242, None), Liveness::Unknown);
        table.put(OWN, "garbage")?;
        assert_eq!(table.liveness(4242, None), Liveness::Unknown);
        Ok(())
    }

    #[test]
    fn a_table_root_that_does_not_exist_is_unknown() {
        let missing = ProcessTable::at("/nonexistent/artifree-proc-root", OWN);
        assert_eq!(missing.liveness(1, None), Liveness::Unknown);
    }

    #[test]
    fn a_platform_without_a_table_knows_nothing() {
        let none = ProcessTable {
            root: None,
            own_pid: OWN,
        };
        assert!(!none.is_readable_here());
        assert_eq!(none.liveness(1, None), Liveness::Unknown);
        assert_eq!(none.own_start_time(), None);
    }

    #[test]
    fn own_start_time_comes_from_the_table() -> TestResult {
        let table = Table::new()?;
        assert_eq!(table.view().own_start_time(), Some(5000));
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_system_table_knows_this_process_and_a_finished_child() -> TestResult {
        let system = ProcessTable::system();
        let start = system.own_start_time().ok_or("Linux shows our own stat")?;
        assert_eq!(
            system.liveness(std::process::id(), Some(start)),
            Liveness::Running
        );
        let mut child = std::process::Command::new("true").spawn()?;
        let pid = child.id();
        child.wait()?;
        assert_eq!(system.liveness(pid, None), Liveness::Gone);
        Ok(())
    }
}
