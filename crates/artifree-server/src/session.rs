//! The temporary data directory of a one-shot session and the marker that
//! names its owner (decision 0039, items 3 and 8).

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use artifree_providers::data_dir;

use crate::process_table::ProcessTable;

/// Every session directory name starts with this.
pub(crate) const DIR_PREFIX: &str = "artifree-session-";
/// Number of random hexadecimal digits after [`DIR_PREFIX`].
pub(crate) const SUFFIX_DIGITS: usize = 16;
/// The file inside a session directory that names its owner process.
pub(crate) const MARKER_FILE: &str = ".artifree-session";
/// A marker larger than this is not ours.
pub(crate) const MAX_MARKER_BYTES: u64 = 256;
const HEADER: &str = "artifree-session v1";
const CREATE_ATTEMPTS: u32 = 16;

/// The content of [`MARKER_FILE`]: which process owns the directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Marker {
    /// The owner's process id; never 0.
    pub pid: u32,
    /// The owner's start time in clock ticks since boot, when it was known.
    pub start: Option<u64>,
}

impl Marker {
    /// The marker of the calling process.
    pub(crate) fn of_this_process(table: &ProcessTable) -> Self {
        Self {
            pid: std::process::id(),
            start: table.own_start_time(),
        }
    }

    /// The file content: header, `pid N`, and `start T` when known, one per
    /// line, ending in one newline.
    pub(crate) fn render(&self) -> String {
        let start = self
            .start
            .map_or_else(String::new, |start| format!("start {start}\n"));
        format!("{HEADER}\npid {}\n{start}", self.pid)
    }

    /// Reads exactly the format [`Marker::render`] writes (one final newline
    /// is optional); anything else is not a marker and gives `None`: a
    /// repeated or extra line, a blank line, a carriage return, a number with
    /// a sign, a leading zero, or `pid 0`.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let body = text.strip_suffix('\n').unwrap_or(text);
        let mut lines = body.split('\n');
        if lines.next()? != HEADER {
            return None;
        }
        let pid = canonical_number(lines.next()?.strip_prefix("pid ")?)?;
        let pid = u32::try_from(pid).ok().filter(|&pid| pid != 0)?;
        let start = match lines.next() {
            None => None,
            Some(line) => Some(canonical_number(line.strip_prefix("start ")?)?),
        };
        if lines.next().is_some() {
            return None;
        }
        Some(Self { pid, start })
    }
}

/// A decimal number written the way `to_string` writes it.
fn canonical_number(text: &str) -> Option<u64> {
    let number: u64 = text.parse().ok()?;
    (number.to_string() == text).then_some(number)
}

/// Owns the session directory from the moment it exists until it is removed.
///
/// Declare it **before** the runtime and the database handles: locals drop in
/// reverse order, so on a panic they are closed before the directory is
/// deleted. The directory is removed by [`Session::finish`], which reports a
/// failure, or else by `Drop`, which cannot.
#[derive(Debug, Default)]
pub(crate) struct Session {
    dir: Option<PathBuf>,
}

impl Session {
    /// A session whose directory does not exist yet.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The directory, once [`Session::begin`] has created it.
    pub(crate) fn path(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// Creates a new private directory under `root` (which must exist) and
    /// writes the marker of this process into it.
    ///
    /// The directory is owned by this value from the moment it exists, so a
    /// failure after that point still removes it.
    pub(crate) fn begin(&mut self, root: &Path, table: &ProcessTable) -> Result<&Path, String> {
        if self.dir.is_some() {
            return Err("a session directory already exists".to_owned());
        }
        let path = create_unique_dir(root)?;
        let dir = self.dir.insert(path);
        data_dir::prepare_data_dir(dir)?;
        write_marker(dir, &Marker::of_this_process(table)).map_err(|error| {
            format!(
                "cannot write the session marker in {}: {error}",
                dir.display()
            )
        })?;
        Ok(dir)
    }

    /// Removes the directory and everything in it. A directory that is
    /// already gone is a success.
    ///
    /// # Errors
    ///
    /// When removal fails; the directory is then removed again by `Drop`.
    pub(crate) fn finish(&mut self) -> io::Result<()> {
        let Some(dir) = self.dir.take() else {
            return Ok(());
        };
        match fs::remove_dir_all(&dir) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => {
                self.dir = Some(dir);
                Err(error)
            }
            Ok(()) | Err(_) => Ok(()),
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Err(error) = self.finish() {
            let dir = self.dir.take().unwrap_or_default();
            writeln!(
                io::stderr(),
                "Could not remove the session directory {}: {error}",
                dir.display()
            )
            .ok();
        }
    }
}

fn create_unique_dir(root: &Path) -> Result<PathBuf, String> {
    for _ in 0..CREATE_ATTEMPTS {
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).map_err(|error| format!("no system randomness: {error}"))?;
        let suffix = format!("{:016x}", u64::from_le_bytes(random));
        let path = root.join(format!("{DIR_PREFIX}{suffix}"));
        // `mkdir` fails for an existing name, so nothing existing is adopted.
        match data_dir::create_private_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(format!(
                    "cannot create a session directory in {}: {error}",
                    root.display()
                ));
            }
        }
    }
    Err(format!(
        "cannot find an unused session directory name in {}",
        root.display()
    ))
}

fn write_marker(dir: &Path, marker: &Marker) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(dir.join(MARKER_FILE))?;
    file.write_all(marker.render().as_bytes())?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;
    use std::panic::{self, AssertUnwindSafe};
    use std::sync::atomic::{AtomicU32, Ordering};

    type TestResult = Result<(), Box<dyn Error>>;

    struct Root(PathBuf);

    impl Root {
        fn new() -> io::Result<Self> {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let path = std::env::temp_dir().join(format!(
                "artifree-session-test-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path)?;
            Ok(Self(path))
        }

        fn entries(&self) -> io::Result<Vec<String>> {
            let mut names = Vec::new();
            for entry in fs::read_dir(&self.0)? {
                names.push(entry?.file_name().to_string_lossy().into_owned());
            }
            Ok(names)
        }
    }

    impl Drop for Root {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_marker_round_trips_with_and_without_a_start_time() {
        for marker in [
            Marker {
                pid: 42,
                start: Some(9_000_000),
            },
            Marker {
                pid: 4_194_304,
                start: None,
            },
        ] {
            assert_eq!(Marker::parse(&marker.render()), Some(marker.clone()));
        }
        assert_eq!(
            Marker {
                pid: 7,
                start: Some(8)
            }
            .render(),
            "artifree-session v1\npid 7\nstart 8\n"
        );
    }

    #[test]
    fn a_marker_has_exactly_the_current_format() {
        let ok = |text: &str| Marker::parse(text).is_some();
        assert!(ok("artifree-session v1\npid 5"));
        assert!(ok("artifree-session v1\npid 5\n"));
        assert!(ok("artifree-session v1\npid 5\nstart 6"));
        assert!(ok("artifree-session v1\npid 5\nstart 6\n"));
        let not_ours = [
            ("empty", ""),
            ("header only", "artifree-session v1\n"),
            ("other version", "artifree-session v2\npid 5\n"),
            ("other header", "session v1\npid 5\n"),
            ("blank line after pid", "artifree-session v1\npid 5\n\n"),
            (
                "blank line after start",
                "artifree-session v1\npid 5\nstart 6\n\n",
            ),
            ("extra line", "artifree-session v1\npid 5\nstart 6\nnote\n"),
            ("duplicate pid", "artifree-session v1\npid 5\npid 5\n"),
            (
                "duplicate start",
                "artifree-session v1\npid 5\nstart 6\nstart 6\n",
            ),
            ("start before pid", "artifree-session v1\nstart 6\npid 5\n"),
            (
                "pid then pid",
                "artifree-session v1\npid 5\npid 6\nstart 6\n",
            ),
            ("carriage return", "artifree-session v1\r\npid 5\r\n"),
            ("leading blank", "\nartifree-session v1\npid 5\n"),
            ("pid zero", "artifree-session v1\npid 0\n"),
            ("pid too large", "artifree-session v1\npid 4294967296\n"),
            ("pid signed", "artifree-session v1\npid +5\n"),
            ("pid leading zero", "artifree-session v1\npid 05\n"),
            ("pid negative", "artifree-session v1\npid -5\n"),
            ("pid spaced", "artifree-session v1\npid  5\n"),
            ("pid missing", "artifree-session v1\npid \n"),
            ("start empty", "artifree-session v1\npid 5\nstart \n"),
            ("start words", "artifree-session v1\npid 5\nstart soon\n"),
            ("trailing text on pid", "artifree-session v1\npid 5 x\n"),
            (
                "two trailing newlines",
                "artifree-session v1\npid 5\nstart 6\n\n",
            ),
        ];
        for (name, text) in not_ours {
            assert_eq!(Marker::parse(text), None, "{name}: {text:?}");
        }
    }

    #[test]
    fn begin_creates_a_private_directory_with_a_marker() -> TestResult {
        let root = Root::new()?;
        let table = ProcessTable::system();
        let mut session = Session::new();
        let dir = session.begin(&root.0, &table)?.to_path_buf();
        assert_eq!(dir.parent(), Some(root.0.as_path()));
        let name = dir
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("name")?;
        let suffix = name.strip_prefix(DIR_PREFIX).ok_or("prefix")?;
        assert_eq!(suffix.len(), SUFFIX_DIGITS);
        assert!(
            suffix
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        let marker = Marker::parse(&fs::read_to_string(dir.join(MARKER_FILE))?)
            .ok_or("the marker we wrote is not ours")?;
        assert_eq!(marker.pid, std::process::id());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&dir)?.permissions().mode() & 0o777, 0o700);
            assert_eq!(
                fs::metadata(dir.join(MARKER_FILE))?.permissions().mode() & 0o777,
                0o600
            );
        }
        Ok(())
    }

    #[test]
    fn two_sessions_get_different_directories() -> TestResult {
        let root = Root::new()?;
        let table = ProcessTable::system();
        let mut first = Session::new();
        let mut second = Session::new();
        let one = first.begin(&root.0, &table)?.to_path_buf();
        let two = second.begin(&root.0, &table)?.to_path_buf();
        assert_ne!(one, two);
        Ok(())
    }

    #[test]
    fn begin_in_a_missing_root_creates_nothing_and_says_where() -> TestResult {
        let root = Root::new()?;
        let missing = root.0.join("absent");
        let mut session = Session::new();
        let Err(message) = session.begin(&missing, &ProcessTable::system()) else {
            return Err("a missing root must be refused".into());
        };
        assert!(message.contains("absent"), "{message}");
        assert!(session.path().is_none());
        assert_eq!(root.entries()?, Vec::<String>::new());
        Ok(())
    }

    #[test]
    fn finish_removes_the_directory_with_its_contents_and_is_repeatable() -> TestResult {
        let root = Root::new()?;
        let mut session = Session::new();
        let dir = session
            .begin(&root.0, &ProcessTable::system())?
            .to_path_buf();
        fs::create_dir(dir.join("nested"))?;
        fs::write(dir.join("nested").join("data"), b"x")?;
        session.finish()?;
        assert_eq!(root.entries()?, Vec::<String>::new());
        session.finish()?;
        Ok(())
    }

    #[test]
    fn dropping_the_session_removes_the_directory() -> TestResult {
        let root = Root::new()?;
        {
            let mut session = Session::new();
            session.begin(&root.0, &ProcessTable::system())?;
            assert_eq!(root.entries()?.len(), 1);
        }
        assert_eq!(root.entries()?, Vec::<String>::new());
        Ok(())
    }

    #[test]
    fn a_panic_unwinding_through_the_session_removes_the_directory() -> TestResult {
        let root = Root::new()?;
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
            let mut session = Session::new();
            let began = session.begin(&root.0, &ProcessTable::system()).is_ok();
            assert!(began, "the session must start for this test");
            panic!("the server panics after the directory exists");
        }));
        assert!(outcome.is_err(), "the closure must have panicked");
        assert_eq!(root.entries()?, Vec::<String>::new());
        Ok(())
    }
}
