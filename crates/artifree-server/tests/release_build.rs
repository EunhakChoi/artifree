//! The release build of the server (decision 0057 rules 1, 2, 4 and 18). A build
//! without the `enforcement-fixture` feature knows no execution profile: its
//! worker ends a `run_profile` job as `execution_unavailable` without an attempt,
//! refuses `--execution-dir` before it opens the database, and refuses every
//! `execution` entry point. Each refusal exits with status 2 and writes one line
//! to stderr. The file compiles to nothing with the feature, which the enforcement
//! job builds; the server-binary job of CI runs this file on default features.

#![cfg(not(feature = "enforcement-fixture"))]

use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::str::FromStr;
use std::sync::atomic::{AtomicU32, Ordering};

use artifree_core::artifacts::ArtifactService;
use artifree_domain::SourceRevision;
use artifree_domain::identity::{ActorId, RequestKey, Title, UnixMillis};
use artifree_domain::job::ProfileName;
use artifree_domain::package::{FilePackage, PackagePath};
use artifree_ports::job_store::JobStore;
use artifree_providers::storage::{DATABASE_FILE, SqliteStore};
use rusqlite::Connection;
use serde_json::Value;

type TestResult = Result<(), Box<dyn Error>>;

const BIN: &str = env!("CARGO_BIN_EXE_artifree-server");
const NOW: UnixMillis = UnixMillis(1_000_000);
/// The words of the one stderr line that refuses an execution request.
const REFUSAL: &str = "knows no execution profile";
/// The entry points of `execution` (decision 0057 rule 4), each with the
/// arguments it is given.
const ENTRY_POINTS: [&[&str]; 4] = [
    &["execution"],
    &["execution", "bootstrap", "artifree-fixture-inspection-v1"],
    &[
        "execution",
        "stub",
        "artifree-fixture-inspection-v1",
        "/nonexistent-input",
    ],
    &["execution", "guardian", "attempt-1-2", "1000", "100"],
];

// --- fixtures ----------------------------------------------------------------

/// A directory for one test, removed when the test ends.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> io::Result<Self> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "artifree-release-build-{}-{}",
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

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // Drop cannot report a failure; a directory left behind is cleared by the
        // next run that uses the same name.
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Parses a fixture value, naming the text when it does not parse.
fn parsed<T>(text: &str) -> Result<T, Box<dyn Error>>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    text.parse::<T>()
        .map_err(|error| format!("invalid fixture {text:?}: {error}").into())
}

/// Creates the database in `dir` with one artifact at revision 1 and one queued
/// `fixture` `run_profile` job for it, and returns the job's identifier.
fn data_with_fixture_job(dir: &Path) -> Result<String, Box<dyn Error>> {
    let service = ArtifactService::new(SqliteStore::open(&dir.join(DATABASE_FILE))?);
    let owner: ActorId = parsed("owner")?;
    let create_key: RequestKey = parsed("create-key")?;
    let title: Title = parsed("Title")?;
    let package = FilePackage::new(vec![(
        parsed::<PackagePath>("index.html")?,
        b"<h1>index</h1>".to_vec(),
    )])?;
    let created = service.create(&owner, &create_key, &title, &package)?;
    let job_key: RequestKey = parsed("fixture-key")?;
    let profile: ProfileName = parsed("fixture")?;
    let receipt = service.enqueue_profile_job(
        &owner,
        &job_key,
        &created.artifact.id,
        SourceRevision(1),
        &profile,
        NOW,
    )?;
    Ok(receipt.job.id.as_str().to_owned())
}

/// Creates `path` as an empty directory that only its owner can enter.
fn private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// The names of the entries of `dir`.
fn entries(dir: &Path) -> io::Result<Vec<OsString>> {
    fs::read_dir(dir)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect()
}

/// Asserts that `dir` holds no entry, which a refused command must leave as it was.
fn assert_empty(dir: &Path) -> TestResult {
    let names = entries(dir)?;
    assert!(
        names.is_empty(),
        "{} must stay empty, got {names:?}",
        dir.display()
    );
    Ok(())
}

/// The stderr of `output` as text, for messages.
fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The JSON values of a worker's stdout, one per line.
fn json_lines(stdout: &[u8]) -> Result<Vec<Value>, Box<dyn Error>> {
    let text = String::from_utf8_lossy(stdout);
    let mut values = Vec::new();
    for line in text.lines() {
        let value: Value = serde_json::from_str(line)?;
        values.push(value);
    }
    Ok(values)
}

/// Asserts that `output` wrote exactly one line to stderr and that the line names
/// the missing execution profile support.
fn assert_refusal_line(output: &Output, what: &str) {
    let stderr = stderr_of(output);
    let lines: Vec<&str> = stderr.lines().collect();
    assert!(
        matches!(lines.as_slice(), [line] if line.contains(REFUSAL)),
        "{what}: expected one stderr line naming {REFUSAL:?}, got {stderr:?}"
    );
}

// --- the release worker -----------------------------------------------------

#[test]
fn a_release_worker_ends_a_fixture_job_as_execution_unavailable() -> TestResult {
    let dir = TempDir::new()?;
    let job_id = data_with_fixture_job(dir.path())?;

    let output = Command::new(BIN)
        .args(["worker", "run", "--once", "--data-dir"])
        .arg(dir.path())
        .stdin(Stdio::null())
        .output()?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "the worker drains the queue and exits 0, stderr: {}",
        stderr_of(&output)
    );

    let events = json_lines(&output.stdout)?;
    let kinds: Vec<&str> = events
        .iter()
        .filter_map(|event| event["event"].as_str())
        .collect();
    assert_eq!(kinds, ["ready", "job", "stopped"], "{events:?}");
    let [_ready, job, stopped] = events.as_slice() else {
        return Err(format!("expected three stdout lines, got {events:?}").into());
    };
    assert_eq!(
        job["job"].as_str(),
        Some(job_id.as_str()),
        "the job line names the enqueued job: {job}"
    );
    assert_eq!(job["state"], "failed", "the job line: {job}");
    assert_eq!(
        job["failure"], "execution_unavailable",
        "the job line: {job}"
    );
    assert_eq!(
        job["attempts"].as_i64(),
        Some(0),
        "ending the job counts no attempt: {job}"
    );
    assert_eq!(stopped["reason"], "drained", "{stopped}");

    let connection = Connection::open(dir.path().join(DATABASE_FILE))?;
    let (state, failure, attempts): (String, Option<String>, i64) = connection.query_row(
        "SELECT state, failure, attempts FROM jobs WHERE id = ?1",
        [job_id.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(state, "failed", "the stored job is failed");
    assert_eq!(
        failure.as_deref(),
        Some("execution_unavailable"),
        "the stored failure says the execution is unavailable"
    );
    assert_eq!(attempts, 0, "the stored job counts no attempt");

    let attempt_rows: i64 =
        connection.query_row("SELECT COUNT(*) FROM execution_attempts", [], |row| {
            row.get(0)
        })?;
    assert_eq!(
        attempt_rows, 0,
        "the release worker records no execution attempt"
    );
    let store = SqliteStore::open(&dir.path().join(DATABASE_FILE))?;
    assert!(
        store.unsettled_attempts()?.is_empty(),
        "no attempt is left unsettled"
    );
    Ok(())
}

#[test]
fn a_release_worker_refuses_an_execution_directory_before_the_database() -> TestResult {
    let dir = TempDir::new()?;
    let data = dir.path().join("data");
    let execution = dir.path().join("execution");
    fs::create_dir(&data)?;
    private_dir(&execution)?;

    // `--once` bounds the run should the refusal ever be lost; the refusal itself
    // does not depend on it.
    let output = Command::new(BIN)
        .args(["worker", "run", "--once", "--data-dir"])
        .arg(&data)
        .arg("--execution-dir")
        .arg(&execution)
        .stdin(Stdio::null())
        .output()?;

    assert_eq!(
        output.status.code(),
        Some(2),
        "the refusal exits with status 2, stderr: {}",
        stderr_of(&output)
    );
    assert!(
        output.stdout.is_empty(),
        "no line is printed before the refusal, got {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_refusal_line(&output, "--execution-dir");
    assert!(
        !data.join(DATABASE_FILE).exists(),
        "the refused worker creates no database"
    );
    assert_empty(&data)?;
    assert_empty(&execution)?;
    Ok(())
}

// --- the execution entry points ---------------------------------------------

#[test]
fn a_release_build_refuses_every_execution_entry_point() -> TestResult {
    // The command runs in an empty directory, which a refused command must leave empty.
    let cwd = TempDir::new()?;
    for arguments in ENTRY_POINTS {
        let output = Command::new(BIN)
            .args(arguments)
            .current_dir(cwd.path())
            .stdin(Stdio::null())
            .output()?;
        assert_eq!(
            output.status.code(),
            Some(2),
            "{arguments:?} exits with status 2, stderr: {}",
            stderr_of(&output)
        );
        assert!(
            output.stdout.is_empty(),
            "{arguments:?} prints nothing on stdout, got {}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert_refusal_line(&output, &format!("{arguments:?}"));
        assert_empty(cwd.path())?;
    }
    Ok(())
}
