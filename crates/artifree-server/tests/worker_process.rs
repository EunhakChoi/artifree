//! The worker as a real process (decision 0030): a killed worker's job is
//! taken over once its lease expires and finishes exactly once; a terminated
//! worker gives its job back without using an attempt.
//!
//! Every wait is bounded and observes a condition (a job state in the
//! database, a line on stdout, an exit status), never a fixed delay standing
//! in for one. A child is killed and reaped when its guard is dropped, so a
//! failing test leaves no process behind.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::thread;
use std::time::{Duration, Instant};

use artifree_core::jobs::{Clock, SystemClock};
use artifree_domain::ArtifactKind;
use artifree_domain::SourceRevision;
use artifree_domain::identity::ActorId;
use artifree_domain::job::JobKind;
use artifree_domain::package::{FilePackage, PackagePath};
use artifree_ports::artifact_store::{
    ArtifactStore, NewArtifact, RequestFingerprint, RequestIdentity,
};
use artifree_ports::job_store::{JobStore, NewJob};
use artifree_providers::storage::{DATABASE_FILE, SqliteStore};
use rusqlite::Connection;
use serde_json::Value;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const BIN: &str = env!("CARGO_BIN_EXE_artifree-server");
const PATIENCE: Duration = Duration::from_secs(60);
const STEP: Duration = Duration::from_millis(20);

struct Data {
    dir: PathBuf,
}

impl Data {
    /// A data directory holding an artifact of three files, one of them
    /// damaged, and one queued job for its revision.
    fn with_job() -> Result<Self, Box<dyn std::error::Error>> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "artifree-worker-process-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        let data = Self { dir };
        let store = SqliteStore::open(&data.database())?;
        let owner: ActorId = "owner".parse()?;
        let package = FilePackage::new(vec![
            (PackagePath::entry(), b"<h1>index</h1>".to_vec()),
            ("style.css".parse::<PackagePath>()?, b"h1{}".to_vec()),
            ("app.js".parse::<PackagePath>()?, b"let a=1;".to_vec()),
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
        data.sql()?.execute(
            "UPDATE blobs SET content = CAST('xxxx' AS BLOB)
             WHERE digest = (SELECT digest FROM revision_files WHERE path = 'style.css')",
            [],
        )?;
        store.enqueue_job(NewJob {
            request: RequestIdentity {
                actor: &owner,
                key: &"test-key-job".parse()?,
                fingerprint: RequestFingerprint([2; 32]),
            },
            artifact: &created.artifact.id,
            revision: SourceRevision(1),
            kind: JobKind::VerifyRevision,
            now: SystemClock.now(),
        })?;
        Ok(data)
    }

    fn database(&self) -> PathBuf {
        self.dir.join(DATABASE_FILE)
    }

    fn sql(&self) -> Result<Connection, rusqlite::Error> {
        Connection::open(self.database())
    }

    /// `(state, attempts, fence, worker)` of the one job.
    fn job(&self) -> Result<(String, i64, i64, Option<String>), rusqlite::Error> {
        self.sql()?.query_row(
            "SELECT state, attempts, fence, worker FROM jobs",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
    }

    fn findings(&self) -> Result<i64, rusqlite::Error> {
        self.sql()?
            .query_row("SELECT count(*) FROM job_findings", [], |row| row.get(0))
    }

    /// Waits until the job is in `state`.
    fn wait_for_state(&self, state: &str) -> TestResult {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let now = self.job().map(|job| job.0);
            if now.as_deref() == Ok(state) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!("the job never became {state}; it is {now:?}").into());
            }
            thread::sleep(STEP);
        }
    }

    fn stderr_file(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

impl Drop for Data {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A worker process that is killed and reaped when dropped.
struct Worker {
    child: Child,
    lines: Receiver<String>,
    stderr: PathBuf,
}

impl Worker {
    fn spawn(data: &Data, name: &str, extra: &[&std::ffi::OsStr]) -> Result<Self, std::io::Error> {
        let stderr = data.stderr_file(&format!("{name}.stderr"));
        let mut child = Command::new(BIN)
            .args(["worker", "run", "--poll-ms", "10", "--data-dir"])
            .arg(&data.dir)
            .args(extra)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(File::create(&stderr)?))
            .spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("no stdout"))?;
        let (sender, lines) = channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            lines,
            stderr,
        })
    }

    /// The next line of stdout, as JSON.
    fn next_event(&self) -> Result<Value, Box<dyn std::error::Error>> {
        match self.lines.recv_timeout(PATIENCE) {
            Ok(line) => Ok(serde_json::from_str(&line)?),
            Err(RecvTimeoutError::Timeout) => Err("no output within the time limit".into()),
            Err(RecvTimeoutError::Disconnected) => Err("the worker closed stdout".into()),
        }
    }

    /// Waits for the process to exit.
    fn wait_exit(&mut self) -> Result<ExitStatus, Box<dyn std::error::Error>> {
        let deadline = Instant::now() + PATIENCE;
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err("the worker did not exit within the time limit".into());
            }
            thread::sleep(STEP);
        }
    }

    /// Every remaining event, after the process exited.
    fn rest(&self) -> Vec<Value> {
        let mut events = Vec::new();
        // The reader thread ends at the end of the output, which closes the
        // channel; the wait is bounded for a worker that has not exited.
        while let Ok(line) = self.lines.recv_timeout(PATIENCE) {
            if let Ok(event) = serde_json::from_str(&line) {
                events.push(event);
            }
        }
        events
    }

    fn diagnostics(&self) -> String {
        std::fs::read_to_string(&self.stderr).unwrap_or_default()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn path_arg(path: &Path) -> &std::ffi::OsStr {
    path.as_os_str()
}

// --- tests -----------------------------------------------------------------

#[test]
fn a_job_of_a_killed_worker_is_finished_once_by_another_after_its_lease_expires() -> TestResult {
    let data = Data::with_job()?;
    let hold = data.dir.join("hold");
    let mut first = Worker::spawn(
        &data,
        "first",
        &[
            "--hold-file".as_ref(),
            path_arg(&hold),
            "--lease-secs".as_ref(),
            "1".as_ref(),
        ],
    )?;
    let ready = first.next_event()?;
    assert_eq!(ready["event"], "ready");
    data.wait_for_state("running")?;
    let (_, attempts, fence, holder) = data.job()?;
    assert_eq!((attempts, fence), (1, 1));
    assert_eq!(holder.as_deref(), ready["worker"].as_str());

    // The worker dies holding the job, with no chance to say so.
    first.child.kill()?;
    first.wait_exit()?;
    let (state, ..) = data.job()?;
    assert_eq!(state, "running", "a killed worker leaves its lease behind");
    assert_eq!(data.findings()?, 0, "and no result");
    // Its lease runs out (here made so at once, instead of waiting for it).
    data.sql()?
        .execute("UPDATE jobs SET lease_expires_at = 0", [])?;

    let mut second = Worker::spawn(&data, "second", &["--once".as_ref()])?;
    assert!(second.wait_exit()?.success(), "{}", second.diagnostics());

    let events = second.rest();
    let ended = events
        .iter()
        .find(|event| event["event"] == "job")
        .ok_or("the second worker reported no job")?;
    assert_eq!(ended["state"], "succeeded");
    assert_eq!(ended["verified"], false, "the damaged file was found");
    let stopped = events.last().ok_or("no events")?;
    assert_eq!(stopped["event"], "stopped");
    assert_eq!(stopped["reason"], "drained");
    let (state, attempts, fence, holder) = data.job()?;
    assert_eq!((state.as_str(), attempts, fence), ("succeeded", 2, 2));
    assert_eq!(holder, None);
    assert_eq!(data.findings()?, 1, "one result, written once");
    assert_ne!(
        events.first().map(|event| event["worker"].clone()),
        Some(ready["worker"].clone()),
        "a different worker finished it"
    );
    Ok(())
}

#[test]
fn a_hold_file_gates_a_job_until_it_exists_and_the_worker_then_drains() -> TestResult {
    let data = Data::with_job()?;
    let hold = data.dir.join("hold");
    let mut worker = Worker::spawn(
        &data,
        "held",
        &["--once".as_ref(), "--hold-file".as_ref(), path_arg(&hold)],
    )?;
    data.wait_for_state("running")?;
    assert!(worker.child.try_wait()?.is_none(), "still holding the job");

    File::create(&hold)?;

    assert!(worker.wait_exit()?.success(), "{}", worker.diagnostics());
    let (state, attempts, ..) = data.job()?;
    assert_eq!((state.as_str(), attempts), ("succeeded", 1));
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_terminated_worker_gives_its_job_back_without_using_an_attempt() -> TestResult {
    let data = Data::with_job()?;
    let hold = data.dir.join("hold");
    let mut worker = Worker::spawn(&data, "term", &["--hold-file".as_ref(), path_arg(&hold)])?;
    assert_eq!(worker.next_event()?["event"], "ready");
    data.wait_for_state("running")?;

    let sent = Command::new("kill")
        .args(["-TERM", &worker.child.id().to_string()])
        .status()?;
    assert!(sent.success());

    assert!(
        worker.wait_exit()?.success(),
        "a handled signal is a clean exit: {}",
        worker.diagnostics()
    );
    let events = worker.rest();
    let given_back = events
        .iter()
        .find(|event| event["event"] == "job")
        .ok_or("the job was not reported")?;
    assert_eq!(
        (
            given_back["state"].as_str(),
            given_back["attempts"].as_i64()
        ),
        (Some("queued"), Some(0))
    );
    let stopped = events.last().ok_or("no events")?;
    assert_eq!(
        (stopped["event"].as_str(), stopped["reason"].as_str()),
        (Some("stopped"), Some("signal"))
    );
    let (state, attempts, fence, holder) = data.job()?;
    assert_eq!((state.as_str(), attempts, fence), ("queued", 0, 1));
    assert_eq!(holder, None);
    assert_eq!(data.findings()?, 0);

    // The next worker runs it, with a new token.
    let mut next = Worker::spawn(&data, "next", &["--once".as_ref()])?;
    assert!(next.wait_exit()?.success(), "{}", next.diagnostics());
    let (state, attempts, fence, _) = data.job()?;
    assert_eq!((state.as_str(), attempts, fence), ("succeeded", 1, 2));
    assert_eq!(data.findings()?, 1);
    Ok(())
}

#[test]
fn a_worker_without_a_database_fails_and_creates_none() -> TestResult {
    let data = Data::with_job()?;
    let empty = data.dir.join("empty");
    std::fs::create_dir(&empty)?;
    let mut worker = Command::new(BIN)
        .args(["worker", "run", "--once", "--data-dir"])
        .arg(&empty)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + PATIENCE;
    let status = loop {
        if let Some(status) = worker.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = worker.kill();
            let _ = worker.wait();
            return Err("the worker did not exit".into());
        }
        thread::sleep(STEP);
    };

    assert_eq!(status.code(), Some(1));
    assert!(
        !empty.join(DATABASE_FILE).exists(),
        "a worker creates no database"
    );
    Ok(())
}

#[test]
fn an_empty_queue_drains_at_once_and_a_bad_command_line_is_refused() -> TestResult {
    let data = Data::with_job()?;
    data.sql()?
        .execute("UPDATE jobs SET state = 'cancelled', finished_at = 1", [])?;
    let mut worker = Worker::spawn(&data, "idle", &["--once".as_ref()])?;
    assert!(worker.wait_exit()?.success(), "{}", worker.diagnostics());
    let events = worker.rest();
    let kinds: Vec<_> = events
        .iter()
        .filter_map(|event| event["event"].as_str())
        .collect();
    assert_eq!(kinds, ["ready", "stopped"], "{events:?}");

    let refused = Command::new(BIN)
        .args(["worker", "run", "--once", "--poll-ms", "5", "--data-dir"])
        .arg(&data.dir)
        .stdin(Stdio::null())
        .output()?;
    assert_eq!(refused.status.code(), Some(1));
    assert!(refused.stdout.is_empty());
    let message = String::from_utf8_lossy(&refused.stderr);
    assert!(message.starts_with("Unknown arguments."), "{message}");
    Ok(())
}

/// Runs `worker run --once` over a job whose worker died and whose lease has
/// expired (`update` is the SQL that puts the job there), and returns the
/// events it printed.
fn drain_an_abandoned_job(update: &str) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    let data = Data::with_job()?;
    data.sql()?.execute(update, [])?;
    let mut worker = Worker::spawn(&data, "settle", &["--once".as_ref()])?;
    assert!(worker.wait_exit()?.success(), "{}", worker.diagnostics());
    let events = worker.rest();
    assert_eq!(data.findings()?, 0, "the job was never run");
    Ok(events)
}

#[test]
fn a_job_the_claim_fails_for_its_last_expired_lease_is_reported() -> TestResult {
    let events = drain_an_abandoned_job(
        "UPDATE jobs SET state = 'running', attempts = 3, fence = 3, worker = 'worker-dead',
             lease_expires_at = 1, started_at = 1",
    )?;

    let kinds: Vec<_> = events
        .iter()
        .filter_map(|event| event["event"].as_str())
        .collect();
    assert_eq!(kinds, ["ready", "job", "stopped"], "{events:?}");
    assert_eq!(events[1]["state"], "failed", "{events:?}");
    assert_eq!(events[1]["failure"], "lease_expired", "{events:?}");
    assert_eq!(events[2]["reason"], "drained");
    Ok(())
}

#[test]
fn a_job_the_claim_cancels_for_an_expired_lease_with_a_cancel_pending_is_reported() -> TestResult {
    let events = drain_an_abandoned_job(
        "UPDATE jobs SET state = 'running', attempts = 1, fence = 1, worker = 'worker-dead',
             lease_expires_at = 1, started_at = 1, cancel_requested = 1",
    )?;

    let kinds: Vec<_> = events
        .iter()
        .filter_map(|event| event["event"].as_str())
        .collect();
    assert_eq!(kinds, ["ready", "job", "stopped"], "{events:?}");
    assert_eq!(events[1]["state"], "cancelled", "{events:?}");
    assert_eq!(events[2]["reason"], "drained");
    Ok(())
}

#[test]
fn an_empty_database_file_is_refused_and_left_as_it_was() -> TestResult {
    let data = Data::with_job()?;
    let empty = data.dir.join("empty-db");
    std::fs::create_dir(&empty)?;
    let file = empty.join(DATABASE_FILE);
    std::fs::File::create(&file)?;

    let refused = Command::new(BIN)
        .args(["worker", "run", "--once", "--data-dir"])
        .arg(&empty)
        .stdin(Stdio::null())
        .output()?;

    assert_eq!(refused.status.code(), Some(1));
    assert!(refused.stdout.is_empty());
    assert_eq!(
        std::fs::metadata(&file)?.len(),
        0,
        "the file was not written"
    );
    let names: Vec<_> = std::fs::read_dir(&empty)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<_, _>>()?;
    assert_eq!(
        names,
        [std::ffi::OsString::from(DATABASE_FILE)],
        "no sidecar file appeared"
    );
    Ok(())
}
