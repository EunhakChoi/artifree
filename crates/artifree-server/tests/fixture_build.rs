//! The worker of the enforcement-fixture build without an execution directory (decision 0057
//! rules 1 and 18): a known-profile job stays queued with no attempt while trusted jobs still run,
//! and an unusable execution directory is refused with status 2 before the database is opened.
//!
//! The file is compiled only for a Linux build with the `enforcement-fixture` feature. No case
//! passes a usable execution directory, so no profile runs here (0056 rule 12). The cases are not
//! ignored: they run in the ordinary test pass of that build.

#![cfg(all(target_os = "linux", feature = "enforcement-fixture"))]

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use artifree_core::jobs::{Clock, SystemClock};
use artifree_domain::ArtifactKind;
use artifree_domain::SourceRevision;
use artifree_domain::identity::ActorId;
use artifree_domain::job::{JobId, JobKind, ProfileName};
use artifree_domain::package::{FilePackage, PackagePath};
use artifree_ports::artifact_store::{
    ArtifactStore, NewArtifact, RequestFingerprint, RequestIdentity,
};
use artifree_ports::job_store::{JobStore, NewJob, NewProfileJob};
use artifree_providers::storage::{DATABASE_FILE, SqliteStore};
use rusqlite::Connection;
use serde_json::Value;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const BIN: &str = env!("CARGO_BIN_EXE_artifree-server");
/// The longest a worker may run before the test kills it.
const PATIENCE: Duration = Duration::from_secs(60);
/// The pause between looks at a worker that has not exited yet.
const STEP: Duration = Duration::from_millis(20);
/// The start of the stderr line for an execution directory that is not a private directory of
/// this account (decision 0057 rule 18).
const DIRECTORY_REFUSAL: &str =
    "Execution unavailable. the execution directory is not a private directory of this account";
/// The content of the regular file that the file case passes as its execution directory.
const UNTOUCHED: &[u8] = b"not a directory";

// --- fixtures --------------------------------------------------------------

/// A directory of one test, removed when the test ends. It holds the data directory of the worker
/// and the logs of its runs.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> io::Result<Self> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "artifree-fixture-build-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        // A directory left by an earlier run with the same name is cleared first.
        if path.exists() {
            fs::remove_dir_all(&path)?;
        }
        fs::create_dir_all(&path)?;
        Ok(Self(path))
    }

    fn data_dir(&self) -> PathBuf {
        self.0.join("data")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Drop cannot report a failure; a directory left behind is cleared by the next run that
        // uses the same name.
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A data directory with one artifact at revision 1, a queued `verify_revision` job and a queued
/// `run_profile` job of the `fixture` profile, both for that revision.
struct Data {
    scratch: Scratch,
    verify: JobId,
    fixture: JobId,
}

impl Data {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let scratch = Scratch::new()?;
        fs::create_dir(scratch.data_dir())?;
        let store = SqliteStore::open(&scratch.data_dir().join(DATABASE_FILE))?;
        let owner: ActorId = "owner".parse()?;
        let package = FilePackage::new(vec![
            (PackagePath::entry(), b"<h1>index</h1>".to_vec()),
            ("style.css".parse::<PackagePath>()?, b"h1{}".to_vec()),
        ])?;
        let created = store.create_artifact(NewArtifact {
            request: RequestIdentity {
                actor: &owner,
                key: &"test-key-create".parse()?,
                fingerprint: RequestFingerprint([1; 32]),
            },
            kind: ArtifactKind::App,
            title: &"Title".parse()?,
            package: &package,
            source: None,
        })?;
        let artifact = &created.artifact.id;
        let verify = store.enqueue_job(NewJob {
            request: RequestIdentity {
                actor: &owner,
                key: &"test-key-verify".parse()?,
                fingerprint: RequestFingerprint([2; 32]),
            },
            artifact,
            revision: SourceRevision(1),
            kind: JobKind::VerifyRevision,
            now: SystemClock.now(),
        })?;
        let profile: ProfileName = "fixture".parse()?;
        let fixture = store.enqueue_profile_job(NewProfileJob {
            request: RequestIdentity {
                actor: &owner,
                key: &"test-key-profile".parse()?,
                fingerprint: RequestFingerprint([3; 32]),
            },
            artifact,
            revision: SourceRevision(1),
            profile: &profile,
            now: SystemClock.now(),
        })?;
        Ok(Self {
            scratch,
            verify: verify.job.id,
            fixture: fixture.job.id,
        })
    }

    /// The state and the attempt count of a job. The store does not return a `run_profile` job,
    /// so the row is read from the database.
    fn job_row(&self, id: &JobId) -> Result<(String, i64), rusqlite::Error> {
        Connection::open(self.scratch.data_dir().join(DATABASE_FILE))?.query_row(
            "SELECT state, attempts FROM jobs WHERE id = ?1",
            [id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
    }

    /// The number of attempt rows of any job.
    fn attempt_count(&self) -> Result<i64, rusqlite::Error> {
        Connection::open(self.scratch.data_dir().join(DATABASE_FILE))?.query_row(
            "SELECT count(*) FROM execution_attempts",
            [],
            |row| row.get(0),
        )
    }
}

/// What a finished worker process left: its exit status and its two output streams.
struct Run {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

/// Runs `worker run` over the scratch's data directory with `extra`, and waits for it to exit. Its
/// output goes to log files in the scratch, and a worker that does not exit in time is killed.
fn run_worker(scratch: &Scratch, extra: &[&OsStr]) -> Result<Run, Box<dyn std::error::Error>> {
    let stdout = scratch.0.join("stdout");
    let stderr = scratch.0.join("stderr");
    let mut child = Command::new(BIN)
        .args(["worker", "run", "--data-dir"])
        .arg(scratch.data_dir())
        .args(extra)
        .stdin(Stdio::null())
        .stdout(Stdio::from(fs::File::create(&stdout)?))
        .stderr(Stdio::from(fs::File::create(&stderr)?))
        .spawn()?;
    let status = wait_for_exit(&mut child)?;
    Ok(Run {
        status,
        stdout: fs::read_to_string(&stdout)?,
        stderr: fs::read_to_string(&stderr)?,
    })
}

/// Waits for `child` to exit. A child still running at the deadline is killed and reaped, and the
/// wait is an error.
fn wait_for_exit(child: &mut Child) -> Result<ExitStatus, Box<dyn std::error::Error>> {
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            // Killing and reaping is best effort here; the error below is the failure reported.
            let _ = child.kill();
            let _ = child.wait();
            return Err("the worker did not exit within the time limit".into());
        }
        thread::sleep(STEP);
    }
}

/// The JSON lines a worker printed on stdout.
fn events(stdout: &str) -> Result<Vec<Value>, serde_json::Error> {
    stdout.lines().map(serde_json::from_str).collect()
}

/// The `event` of each line, in order.
fn kinds(events: &[Value]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| event["event"].as_str())
        .collect()
}

/// The execution directory argument of one refusal case.
#[derive(Clone, Copy, Debug)]
enum Unusable {
    /// A path that does not exist.
    Missing,
    /// A directory that other accounts may open (mode 0755).
    OpenToOthers,
    /// A regular file.
    RegularFile,
}

impl Unusable {
    /// Makes the argument of this case under `scratch` and returns its path.
    fn make(self, scratch: &Scratch) -> io::Result<PathBuf> {
        let path = scratch.0.join("execution");
        match self {
            Self::Missing => {}
            Self::OpenToOthers => {
                fs::create_dir(&path)?;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
            }
            Self::RegularFile => fs::write(&path, UNTOUCHED)?,
        }
        Ok(path)
    }

    /// Whether the argument is as `make` left it: nothing was created in a directory, and a file
    /// still has its content.
    fn left_alone(self, path: &Path) -> io::Result<bool> {
        Ok(match self {
            Self::Missing => !path.exists(),
            Self::OpenToOthers => fs::read_dir(path)?.next().is_none(),
            Self::RegularFile => fs::read(path)? == UNTOUCHED,
        })
    }
}

// --- tests -----------------------------------------------------------------

#[test]
fn a_worker_without_an_execution_directory_leaves_a_fixture_job_queued() -> TestResult {
    let data = Data::new()?;
    let run = run_worker(&data.scratch, &["--once".as_ref()])?;
    assert!(run.status.success(), "{}", run.stderr);
    assert!(run.stderr.is_empty(), "{}", run.stderr);

    // The fixture job is known to this build but has no execution: it is left queued, with no
    // attempt spent and no attempt row written (decision 0057 rule 1).
    assert_eq!(
        data.job_row(&data.fixture)?,
        ("queued".to_owned(), 0),
        "the fixture job was claimed without an execution directory"
    );
    assert_eq!(data.attempt_count()?, 0, "an attempt row was written");

    // The trusted job of the same worker still runs (decision 0057 rule 18).
    assert_eq!(data.job_row(&data.verify)?, ("succeeded".to_owned(), 1));
    let events = events(&run.stdout)?;
    assert_eq!(
        kinds(&events),
        ["ready", "claimed", "job", "stopped"],
        "{events:?}"
    );
    let ended = events
        .iter()
        .find(|event| event["event"] == "job")
        .ok_or("the trusted job was not reported")?;
    assert_eq!(ended["job"], data.verify.as_str());
    assert_eq!(ended["state"], "succeeded");
    assert_eq!(ended["verified"], true);
    let stopped = events.last().ok_or("no events")?;
    assert_eq!(stopped["reason"], "drained");
    Ok(())
}

#[test]
fn an_unusable_execution_directory_is_refused_before_the_database() -> TestResult {
    for case in [
        Unusable::Missing,
        Unusable::OpenToOthers,
        Unusable::RegularFile,
    ] {
        let scratch = Scratch::new()?;
        fs::create_dir(scratch.data_dir())?;
        let execution = case.make(&scratch)?;
        let run = run_worker(
            &scratch,
            &[
                "--once".as_ref(),
                "--execution-dir".as_ref(),
                execution.as_os_str(),
            ],
        )?;

        assert_eq!(run.status.code(), Some(2), "{case:?}: {}", run.stderr);
        assert!(run.stdout.is_empty(), "{case:?}: {}", run.stdout);
        assert_eq!(
            run.stderr.lines().count(),
            1,
            "{case:?}: one diagnostic line: {}",
            run.stderr
        );
        assert!(
            run.stderr.starts_with(DIRECTORY_REFUSAL),
            "{case:?}: {}",
            run.stderr
        );
        assert!(
            case.left_alone(&execution)?,
            "{case:?}: the execution argument was written"
        );
        assert_eq!(
            fs::read_dir(scratch.data_dir())?.count(),
            0,
            "{case:?}: the data directory was written, so the database was opened"
        );
    }
    Ok(())
}
