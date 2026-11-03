//! Removal of session directories that killed one-shot sessions left behind
//! (decision 0039, items 8 and 9).
//!
//! Deleting the data of a session that is still running is the worst failure
//! this code can have, so every doubt keeps the directory: a directory is
//! removed only when every check in [`judge`] passes.

use std::fs;
use std::path::Path;

use crate::process_table::{Liveness, ProcessTable};
use crate::session::{DIR_PREFIX, MARKER_FILE, MAX_MARKER_BYTES, Marker, SUFFIX_DIGITS};

/// What a sweep did. Counts only directories whose name starts with the
/// session prefix; other entries of the root are not looked at further.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Whether the sweep could run at all. `false` where there is no process
    /// table to read, no owner to compare, or the root cannot be listed;
    /// then nothing was removed.
    pub performed: bool,
    /// Directories removed.
    pub removed: u32,
    /// Directories kept: running or unproven owners, and look-alikes.
    pub kept: u32,
    /// Directories judged removable whose removal then failed.
    pub failed: u32,
}

/// Why a directory is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Keep {
    /// Not named like a session directory.
    Name,
    /// Not a real directory (a symbolic link, a file, or unreadable).
    NotDirectory,
    /// Owned by another user, or the owner cannot be compared.
    Owner,
    /// No regular marker file.
    NoMarker,
    /// The marker is not in the current format.
    BadMarker,
    /// The owner process is running.
    OwnerRunning,
    /// The process table cannot tell.
    OwnerUnknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Remove,
    Keep(Keep),
}

/// Sweeps the session directories in `root`, except `own_dir`, the directory
/// of the calling session. Its Unix owner is the only owner that may be swept.
///
/// `stopping` is asked before each entry is looked at; once it says `true` the
/// sweep ends. An entry that was started is always finished: it is never left
/// half-judged or half-removed by a stop.
#[must_use]
pub(crate) fn sweep(
    root: &Path,
    own_dir: &Path,
    table: &ProcessTable,
    stopping: &dyn Fn() -> bool,
) -> SweepReport {
    // Without our own owner there is nothing to compare with: sweep nothing.
    let own_owner = fs::symlink_metadata(own_dir)
        .ok()
        .and_then(|metadata| owner_of(&metadata));
    match own_owner {
        Some(owner) => sweep_as(root, own_dir, owner, table, stopping),
        None => SweepReport::default(),
    }
}

fn sweep_as(
    root: &Path,
    own_dir: &Path,
    own_owner: u32,
    table: &ProcessTable,
    stopping: &dyn Fn() -> bool,
) -> SweepReport {
    if !table.is_readable_here() {
        return SweepReport::default();
    }
    let Ok(entries) = fs::read_dir(root) else {
        return SweepReport::default();
    };
    let mut report = SweepReport {
        performed: true,
        ..SweepReport::default()
    };
    for entry in entries {
        if stopping() {
            break;
        }
        let Ok(entry) = entry else {
            report.kept += 1;
            continue;
        };
        if !entry
            .file_name()
            .as_encoded_bytes()
            .starts_with(DIR_PREFIX.as_bytes())
        {
            continue;
        }
        let path = entry.path();
        if entry.file_name() == own_dir.file_name().unwrap_or_default() {
            continue;
        }
        match judge(&path, own_owner, table) {
            Verdict::Keep(_) => report.kept += 1,
            Verdict::Remove => match fs::remove_dir_all(&path) {
                Ok(()) => report.removed += 1,
                Err(_) => report.failed += 1,
            },
        }
    }
    report
}

/// Decides one entry. Remove only when all of the following hold: the name is
/// the prefix and 16 lowercase hexadecimal digits; it is a real directory
/// (`symlink_metadata`) owned by `own_owner`; it holds a regular marker file,
/// of the same owner and of bounded size, in exactly the current format; and
/// the marker's process is proven gone.
fn judge(path: &Path, own_owner: u32, table: &ProcessTable) -> Verdict {
    if !has_session_name(path) {
        return Verdict::Keep(Keep::Name);
    }
    let Ok(directory) = fs::symlink_metadata(path) else {
        return Verdict::Keep(Keep::NotDirectory);
    };
    if !directory.file_type().is_dir() {
        return Verdict::Keep(Keep::NotDirectory);
    }
    if owner_of(&directory) != Some(own_owner) {
        return Verdict::Keep(Keep::Owner);
    }
    let marker_path = path.join(MARKER_FILE);
    let Ok(file) = fs::symlink_metadata(&marker_path) else {
        return Verdict::Keep(Keep::NoMarker);
    };
    if !file.file_type().is_file() {
        return Verdict::Keep(Keep::NoMarker);
    }
    if owner_of(&file) != Some(own_owner) {
        return Verdict::Keep(Keep::Owner);
    }
    if file.len() > MAX_MARKER_BYTES {
        return Verdict::Keep(Keep::BadMarker);
    }
    let marker = fs::read(&marker_path)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .and_then(|text| Marker::parse(&text));
    let Some(marker) = marker else {
        return Verdict::Keep(Keep::BadMarker);
    };
    match table.liveness(marker.pid, marker.start) {
        Liveness::Gone => Verdict::Remove,
        Liveness::Running => Verdict::Keep(Keep::OwnerRunning),
        Liveness::Unknown => Verdict::Keep(Keep::OwnerUnknown),
    }
}

fn has_session_name(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix(DIR_PREFIX))
        .is_some_and(|suffix| {
            suffix.len() == SUFFIX_DIGITS
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

#[cfg(unix)]
fn owner_of(metadata: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    metadata.uid().into()
}

#[cfg(not(unix))]
fn owner_of(_metadata: &fs::Metadata) -> Option<u32> {
    None
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::error::Error;
    use std::os::unix::fs::{MetadataExt, symlink};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    type TestResult = Result<(), Box<dyn Error>>;

    const OWN_PID: u32 = 100;
    const GONE_PID: u32 = 200;
    const LIVE_PID: u32 = 300;

    /// A session root and a fake process table side by side.
    struct World {
        base: PathBuf,
        root: PathBuf,
        proc_root: PathBuf,
    }

    fn stat(pid: u32, state: &str, start: u64) -> String {
        format!(
            "{pid} (x) {state} 1 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 {start} 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n"
        )
    }

    impl World {
        fn new() -> Result<Self, Box<dyn Error>> {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let base = std::env::temp_dir().join(format!(
                "artifree-sweep-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&base);
            let root = base.join("root");
            let proc_root = base.join("proc");
            fs::create_dir_all(&root)?;
            fs::create_dir_all(&proc_root)?;
            let world = Self {
                base,
                root,
                proc_root,
            };
            world.process(OWN_PID, "S", 1000)?;
            world.process(LIVE_PID, "S", 3000)?;
            Ok(world)
        }

        fn process(&self, pid: u32, state: &str, start: u64) -> Result<(), Box<dyn Error>> {
            let dir = self.proc_root.join(pid.to_string());
            fs::create_dir_all(&dir)?;
            fs::write(dir.join("stat"), stat(pid, state, start))?;
            Ok(())
        }

        fn table(&self) -> ProcessTable {
            ProcessTable::at(&self.proc_root, OWN_PID)
        }

        fn own_uid(&self) -> Result<u32, Box<dyn Error>> {
            Ok(fs::metadata(&self.root)?.uid())
        }

        /// A session directory named like ours with `marker_text` inside.
        fn session(&self, digit: char, marker_text: &str) -> Result<PathBuf, Box<dyn Error>> {
            let dir = self.root.join(format!(
                "{DIR_PREFIX}{}",
                digit.to_string().repeat(SUFFIX_DIGITS)
            ));
            fs::create_dir(&dir)?;
            fs::write(dir.join(MARKER_FILE), marker_text)?;
            fs::write(dir.join("artifree.sqlite3"), b"data")?;
            Ok(dir)
        }

        fn own(&self) -> Result<PathBuf, Box<dyn Error>> {
            self.session(
                'f',
                &Marker {
                    pid: OWN_PID,
                    start: Some(1000),
                }
                .render(),
            )
        }

        fn verdict(&self, dir: &Path) -> Result<Verdict, Box<dyn Error>> {
            Ok(judge(dir, self.own_uid()?, &self.table()))
        }
    }

    impl Drop for World {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    fn marker(pid: u32, start: Option<u64>) -> String {
        Marker { pid, start }.render()
    }

    #[test]
    fn a_directory_whose_owner_is_proven_gone_is_removed() -> TestResult {
        let world = World::new()?;
        let own = world.own()?;
        let dead = world.session('a', &marker(GONE_PID, Some(2000)))?;
        let report = sweep(&world.root, &own, &world.table(), &|| false);
        // `sweep` reads our owner from the directory, as in production.
        assert_eq!(
            report,
            SweepReport {
                performed: true,
                removed: 1,
                kept: 0,
                failed: 0
            }
        );
        assert!(!dead.exists());
        assert!(own.exists(), "our own directory is never swept");
        Ok(())
    }

    #[test]
    fn a_live_owner_is_kept_and_a_dead_one_beside_it_is_removed() -> TestResult {
        let world = World::new()?;
        let own = world.own()?;
        let live = world.session('1', &marker(LIVE_PID, Some(3000)))?;
        let live_unrecorded = world.session('2', &marker(LIVE_PID, None))?;
        let dead = world.session('3', &marker(GONE_PID, None))?;
        let report = sweep(&world.root, &own, &world.table(), &|| false);
        assert_eq!(
            report,
            SweepReport {
                performed: true,
                removed: 1,
                kept: 2,
                failed: 0
            }
        );
        assert!(live.join("artifree.sqlite3").exists());
        assert!(live_unrecorded.exists());
        assert!(!dead.exists());
        Ok(())
    }

    #[test]
    fn a_reused_pid_with_another_start_time_is_removed() -> TestResult {
        let world = World::new()?;
        let reused = world.session('4', &marker(LIVE_PID, Some(2999)))?;
        assert_eq!(world.verdict(&reused)?, Verdict::Remove);
        Ok(())
    }

    #[test]
    fn an_owner_the_table_cannot_judge_is_kept() -> TestResult {
        let world = World::new()?;
        let dir = world.session('5', &marker(GONE_PID, None))?;
        // The same marker, but a table that does not show this process.
        fs::remove_dir_all(world.proc_root.join(OWN_PID.to_string()))?;
        assert_eq!(world.verdict(&dir)?, Verdict::Keep(Keep::OwnerUnknown));
        // A stat that is a directory is an I/O failure, not a missing entry.
        world.process(OWN_PID, "S", 1000)?;
        fs::create_dir_all(world.proc_root.join("200").join("stat"))?;
        assert_eq!(world.verdict(&dir)?, Verdict::Keep(Keep::OwnerUnknown));
        Ok(())
    }

    #[test]
    fn a_running_owner_is_reported_as_such() -> TestResult {
        let world = World::new()?;
        let dir = world.session('6', &marker(LIVE_PID, None))?;
        assert_eq!(world.verdict(&dir)?, Verdict::Keep(Keep::OwnerRunning));
        Ok(())
    }

    #[test]
    fn a_marker_in_any_other_format_keeps_the_directory() -> TestResult {
        let world = World::new()?;
        let gone = marker(GONE_PID, None);
        let cases = [
            ("trailing blank line", format!("{gone}\n")),
            ("extra line", format!("{gone}note\n")),
            ("duplicate pid", format!("{gone}pid {GONE_PID}\n")),
            ("other version", gone.replace("v1", "v2")),
            ("empty", String::new()),
            ("not text", "\u{0}".to_owned()),
            ("too large", format!("{gone}{}", "x".repeat(300))),
        ];
        for (name, text) in cases {
            let dir = world.session('7', &text)?;
            assert_eq!(
                world.verdict(&dir)?,
                Verdict::Keep(Keep::BadMarker),
                "{name}"
            );
            fs::remove_dir_all(&dir)?;
        }
        let dir = world.session('7', "")?;
        fs::write(dir.join(MARKER_FILE), [0xff, 0xfe])?;
        assert_eq!(
            world.verdict(&dir)?,
            Verdict::Keep(Keep::BadMarker),
            "invalid UTF-8"
        );
        Ok(())
    }

    #[test]
    fn a_missing_or_non_regular_marker_keeps_the_directory() -> TestResult {
        let world = World::new()?;
        let no_marker = world.session('8', "")?;
        fs::remove_file(no_marker.join(MARKER_FILE))?;
        assert_eq!(world.verdict(&no_marker)?, Verdict::Keep(Keep::NoMarker));

        let marker_dir = world.session('9', "")?;
        fs::remove_file(marker_dir.join(MARKER_FILE))?;
        fs::create_dir(marker_dir.join(MARKER_FILE))?;
        assert_eq!(world.verdict(&marker_dir)?, Verdict::Keep(Keep::NoMarker));

        // A link to a perfect marker of a dead owner is still not a file.
        let linked = world.session('a', "")?;
        let elsewhere = world.base.join("elsewhere-marker");
        fs::write(&elsewhere, marker(GONE_PID, None))?;
        fs::remove_file(linked.join(MARKER_FILE))?;
        symlink(&elsewhere, linked.join(MARKER_FILE))?;
        assert_eq!(world.verdict(&linked)?, Verdict::Keep(Keep::NoMarker));
        Ok(())
    }

    #[test]
    fn a_symbolic_link_named_like_a_session_is_never_followed_or_removed() -> TestResult {
        let world = World::new()?;
        let own = world.own()?;
        let target = world.base.join("precious");
        fs::create_dir(&target)?;
        fs::write(target.join(MARKER_FILE), marker(GONE_PID, None))?;
        fs::write(target.join("keep.txt"), b"keep")?;
        let link = world
            .root
            .join(format!("{DIR_PREFIX}{}", "b".repeat(SUFFIX_DIGITS)));
        symlink(&target, &link)?;
        assert_eq!(world.verdict(&link)?, Verdict::Keep(Keep::NotDirectory));
        let report = sweep(&world.root, &own, &world.table(), &|| false);
        assert_eq!(report.removed, 0, "{report:?}");
        assert_eq!(report.kept, 1);
        assert!(fs::symlink_metadata(&link)?.file_type().is_symlink());
        assert!(target.join("keep.txt").exists());
        Ok(())
    }

    #[test]
    fn a_file_named_like_a_session_is_kept() -> TestResult {
        let world = World::new()?;
        let file = world
            .root
            .join(format!("{DIR_PREFIX}{}", "c".repeat(SUFFIX_DIGITS)));
        fs::write(&file, marker(GONE_PID, None))?;
        assert_eq!(world.verdict(&file)?, Verdict::Keep(Keep::NotDirectory));
        Ok(())
    }

    #[test]
    fn names_that_only_look_like_sessions_are_kept() -> TestResult {
        let world = World::new()?;
        let own = world.own()?;
        let text = marker(GONE_PID, None);
        let mut kept = Vec::new();
        let digits = "d".repeat(SUFFIX_DIGITS);
        for name in [
            format!("{DIR_PREFIX}{}", "D".repeat(SUFFIX_DIGITS)),
            format!("{DIR_PREFIX}{}", "d".repeat(SUFFIX_DIGITS - 1)),
            format!("{DIR_PREFIX}{digits}0"),
            format!("{DIR_PREFIX}{}", "g".repeat(SUFFIX_DIGITS)),
            DIR_PREFIX.to_owned(),
        ] {
            let dir = world.root.join(&name);
            fs::create_dir(&dir)?;
            fs::write(dir.join(MARKER_FILE), &text)?;
            assert_eq!(world.verdict(&dir)?, Verdict::Keep(Keep::Name), "{name}");
            kept.push(dir);
        }
        // A wrong prefix is not even counted.
        let other = world.root.join(format!("other-session-{digits}"));
        fs::create_dir(&other)?;
        fs::write(other.join(MARKER_FILE), &text)?;
        let report = sweep(&world.root, &own, &world.table(), &|| false);
        assert_eq!(
            report,
            SweepReport {
                performed: true,
                removed: 0,
                kept: 5,
                failed: 0
            }
        );
        assert!(other.exists());
        assert!(kept.iter().all(|dir| dir.exists()));
        Ok(())
    }

    #[test]
    fn a_directory_of_another_owner_is_kept() -> TestResult {
        let world = World::new()?;
        let dir = world.session('e', &marker(GONE_PID, None))?;
        let actual = world.own_uid()?;
        assert_eq!(judge(&dir, actual, &world.table()), Verdict::Remove);
        assert_eq!(
            judge(&dir, actual.wrapping_add(1), &world.table()),
            Verdict::Keep(Keep::Owner)
        );
        Ok(())
    }

    #[test]
    fn nothing_is_swept_when_our_own_owner_cannot_be_read() -> TestResult {
        let world = World::new()?;
        let dead = world.session('a', &marker(GONE_PID, None))?;
        let missing_own = world.root.join("not-created");
        let report = sweep(&world.root, &missing_own, &world.table(), &|| false);
        assert_eq!(report, SweepReport::default());
        assert!(dead.exists());
        Ok(())
    }

    #[test]
    fn nothing_is_swept_without_a_readable_process_table() -> TestResult {
        let world = World::new()?;
        let own = world.own()?;
        let dead = world.session('a', &marker(GONE_PID, None))?;
        let none = ProcessTable::at(world.base.join("no-proc"), OWN_PID);
        // A root that is not there: the table is "readable" but proves nothing.
        let report = sweep(&world.root, &own, &none, &|| false);
        assert_eq!((report.removed, report.kept), (0, 1), "{report:?}");
        assert!(dead.exists());
        Ok(())
    }

    #[test]
    fn a_root_that_cannot_be_listed_sweeps_nothing() -> TestResult {
        let world = World::new()?;
        let own = world.own()?;
        let report = sweep(&world.base.join("absent"), &own, &world.table(), &|| false);
        assert_eq!(report, SweepReport::default());
        Ok(())
    }

    #[test]
    fn a_stop_ends_the_sweep_between_entries_and_never_inside_one() -> TestResult {
        let world = World::new()?;
        let own = world.own()?;
        let dirs = [
            world.session('1', &marker(GONE_PID, None))?,
            world.session('2', &marker(GONE_PID, None))?,
            world.session('3', &marker(GONE_PID, None))?,
        ];
        // The stop arrives once the first removal has happened.
        let stopping = || dirs.iter().any(|dir| !dir.exists());
        let report = sweep(&world.root, &own, &world.table(), &stopping);
        assert_eq!(
            report,
            SweepReport {
                performed: true,
                removed: 1,
                kept: 0,
                failed: 0
            },
            "exactly the entry in hand was finished"
        );
        let left: Vec<&PathBuf> = dirs.iter().filter(|dir| dir.exists()).collect();
        assert_eq!(left.len(), 2, "the others were not looked at: {left:?}");
        for dir in left {
            assert!(
                dir.join("artifree.sqlite3").exists(),
                "{} is whole",
                dir.display()
            );
        }
        let report = sweep(&world.root, &own, &world.table(), &|| true);
        assert_eq!(
            report.removed + report.kept + report.failed,
            0,
            "{report:?}"
        );
        Ok(())
    }
}
