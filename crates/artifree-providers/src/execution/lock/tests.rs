//! Behavioral tests of the admission lock: a second acquire is `Busy` in the same process and from
//! another process, the kernel releases the lock when its holder is killed, and an unusable
//! execution directory is refused before any lock file is opened (decisions 0042 rule 5 and 0056
//! rule 13). Every test works in its own private directory under the system temporary directory.

use super::*;

use std::env;
use std::error::Error;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt, symlink};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{self, Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

/// The environment variable that tells the helper process which directory to lock.
const HELPER_DIR_VARIABLE: &str = "ARTIFREE_LOCK_HELPER_DIR";
/// The helper test, named the way libtest names it, that a child process runs by itself.
const HELPER_TEST: &str = "execution::lock::tests::lock_holder_helper";
/// The text the helper prints once it holds the lock.
const HELD_MARKER: &str = "artifree-lock-held";
/// How long the test waits for the helper to report that it holds the lock.
const HELPER_DEADLINE: Duration = Duration::from_secs(10);
/// The number of the SIGKILL signal on Linux.
const SIGKILL: i32 = 9;
/// Numbers the scratch directories of this process, so that no two tests share a name.
static NEXT_SCRATCH: AtomicU32 = AtomicU32::new(0);
/// Serializes the tests that take lock files. A child process spawned while another test holds a
/// lock file keeps a copy of that descriptor until its exec, which would keep the lock taken for a
/// moment. The guard protects no data.
static SERIAL: Mutex<()> = Mutex::new(());

type TestResult = Result<(), Box<dyn Error>>;

/// A private directory under the system temporary directory, removed when dropped.
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    /// Creates an empty directory, readable only by its owner, whose name is unique to this
    /// process and call.
    fn new() -> io::Result<Self> {
        let path = env::temp_dir().join(format!(
            "artifree-lock-test-{}-{}",
            process::id(),
            NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed)
        ));
        // A leftover with the same name, from an earlier run that had the same process id, is
        // cleared first. Its absence is expected.
        match fs::remove_dir_all(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        private_directory(&path)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let removed = fs::remove_dir_all(&self.path);
        // A failed removal fails the test, unless the test is already unwinding.
        assert!(
            removed.is_ok() || thread::panicking(),
            "could not remove the scratch directory {}: {removed:?}",
            self.path.display()
        );
    }
}

/// Creates `path` for its owner alone, as an execution directory is created.
fn private_directory(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new().mode(0o700).create(path)
}

/// Takes the serialization guard. A poisoned guard is still usable, because it protects no data.
fn serialized() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Turns a refusal into a test error that names it.
fn refused(refusal: LockRefusal) -> String {
    format!("the lock was refused: {refusal:?}")
}

/// Helper process of the crash-release test. It does nothing unless the test sets the directory
/// variable, so the ordinary test run skips it.
#[test]
#[ignore = "helper process of the crash-release test; does nothing unless ARTIFREE_LOCK_HELPER_DIR is set"]
fn lock_holder_helper() -> TestResult {
    let Some(directory) = env::var_os(HELPER_DIR_VARIABLE) else {
        return Ok(());
    };
    let _lock = AdmissionLock::acquire(Path::new(&directory)).map_err(refused)?;
    let mut stdout = io::stdout();
    writeln!(stdout, "{HELD_MARKER}")?;
    stdout.flush()?;
    // Blocks until the parent closes the helper's standard input or kills the helper.
    io::stdin().read_to_end(&mut Vec::new())?;
    Ok(())
}

/// A helper process that holds the admission lock. It is killed when dropped, so that a failed
/// test does not leave it behind.
struct LockHolder {
    child: Child,
    /// Kept open: the helper blocks until its standard input reaches end of file.
    _stdin: ChildStdin,
    /// The helper's output lines. A thread sends them and ends when the helper's output closes.
    lines: Receiver<io::Result<String>>,
}

impl LockHolder {
    /// Starts this test binary again, running only the helper, which locks `directory`.
    fn spawn(directory: &Path) -> io::Result<Self> {
        let mut child = Command::new(env::current_exe()?)
            .args([
                "--ignored",
                "--exact",
                HELPER_TEST,
                "--nocapture",
                "--test-threads=1",
            ])
            .env(HELPER_DIR_VARIABLE, directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("the helper has no standard input"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("the helper has no standard output"))?;
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            _stdin: stdin,
            lines,
        })
    }

    /// Waits until the helper reports that it holds the lock. libtest may print the test name on
    /// the same line before the helper's own text, so the line only has to contain the marker.
    fn await_marker(&self) -> TestResult {
        let deadline = Instant::now() + HELPER_DEADLINE;
        loop {
            match self
                .lines
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(Ok(line)) if line.contains(HELD_MARKER) => return Ok(()),
                Ok(Ok(_other_output)) => {}
                Ok(Err(error)) => return Err(error.into()),
                Err(RecvTimeoutError::Timeout) => {
                    return Err(format!(
                        "the helper did not report the lock within {HELPER_DEADLINE:?}"
                    )
                    .into());
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err("the helper closed its output before it reported the lock".into());
                }
            }
        }
    }

    /// Kills the helper with SIGKILL and waits for it to exit, returning its exit status.
    fn kill_and_wait(&mut self) -> io::Result<ExitStatus> {
        self.child.kill()?;
        self.child.wait()
    }
}

impl Drop for LockHolder {
    fn drop(&mut self) {
        // Both results are ignored: the helper may have exited already, and nothing can report
        // a failure from here. A helper that was already reaped is not signalled again.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The ways an execution directory can be unusable. Each is built inside its own case directory.
#[derive(Clone, Copy, Debug)]
enum Unusable {
    /// A path with nothing at it.
    Missing,
    /// A regular file where the directory should be.
    RegularFile,
    /// A symbolic link to a private directory, which the check must not follow.
    SymlinkToPrivateDirectory,
    /// A directory whose permission bits are the given mode.
    Mode(u32),
}

/// The unusable execution directories that the refusal test covers.
const UNUSABLE: [Unusable; 5] = [
    Unusable::Missing,
    Unusable::RegularFile,
    Unusable::SymlinkToPrivateDirectory,
    Unusable::Mode(0o750),
    Unusable::Mode(0o707),
];

impl Unusable {
    /// Builds this case inside `case`, an empty private directory, and returns the path to pass to
    /// `acquire`.
    fn prepare(self, case: &Path) -> io::Result<PathBuf> {
        match self {
            Self::Missing => Ok(case.join("missing")),
            Self::RegularFile => {
                let path = case.join("file");
                fs::write(&path, b"not a directory")?;
                Ok(path)
            }
            Self::SymlinkToPrivateDirectory => {
                let target = case.join("target");
                private_directory(&target)?;
                let link = case.join("link");
                symlink(&target, &link)?;
                Ok(link)
            }
            Self::Mode(mode) => {
                let path = case.join("directory");
                private_directory(&path)?;
                // Set explicitly, so that the umask cannot change the mode under test.
                fs::set_permissions(&path, fs::Permissions::from_mode(mode))?;
                Ok(path)
            }
        }
    }
}

#[test]
fn second_acquire_in_one_process_is_busy() -> TestResult {
    let _serial = serialized();
    let scratch = Scratch::new()?;
    let first = AdmissionLock::acquire(scratch.path()).map_err(refused)?;
    assert_eq!(
        AdmissionLock::acquire(scratch.path()).err(),
        Some(LockRefusal::Busy),
        "a second acquire while the lock is held must be Busy"
    );
    let mode = fs::metadata(scratch.path().join(LOCK_FILE))?.mode() & 0o777;
    assert_eq!(
        mode, 0o600,
        "the lock file must be owner-only, got mode {mode:o}"
    );
    drop(first);
    AdmissionLock::acquire(scratch.path()).map_err(refused)?;
    Ok(())
}

#[test]
fn lock_held_by_another_process_is_busy_until_it_is_killed() -> TestResult {
    let _serial = serialized();
    let scratch = Scratch::new()?;
    let mut holder = LockHolder::spawn(scratch.path())?;
    holder.await_marker()?;
    assert_eq!(
        AdmissionLock::acquire(scratch.path()).err(),
        Some(LockRefusal::Busy),
        "a lock held by another process must be Busy"
    );
    let status = holder.kill_and_wait()?;
    assert_eq!(
        status.signal(),
        Some(SIGKILL),
        "the helper must die by SIGKILL, got {status}"
    );
    AdmissionLock::acquire(scratch.path()).map_err(refused)?;
    Ok(())
}

#[test]
fn unusable_directories_are_refused() -> TestResult {
    let _serial = serialized();
    let scratch = Scratch::new()?;
    let mut failures = Vec::new();
    for (index, kind) in UNUSABLE.iter().enumerate() {
        let case = scratch.path().join(format!("case-{index}"));
        private_directory(&case)?;
        let path = kind.prepare(&case)?;
        let outcome = AdmissionLock::acquire(&path).err();
        if outcome != Some(LockRefusal::Unusable) {
            failures.push(format!("{kind:?}: expected Unusable, got {outcome:?}"));
        }
        // The refusal comes before the lock file is opened, so no lock file may exist at the path.
        // A symbolic link is followed here, so a lock file in its target would also be found.
        if path.join(LOCK_FILE).exists() {
            failures.push(format!(
                "{kind:?}: a lock file was created under the refused path"
            ));
        }
    }
    // The owner check cannot be exercised here, because it needs a directory owned by another
    // account. The owner-only directory of the first test shows that an accepted case exists.
    assert!(
        failures.is_empty(),
        "unusable execution directories must be refused before any lock is taken: {failures:?}"
    );
    Ok(())
}
