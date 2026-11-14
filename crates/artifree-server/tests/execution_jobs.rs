//! The enforcement cases of the worker that runs `run_profile` jobs (decision
//! 0057, row R02bb of rule 20), against the real server binary built with the
//! `enforcement-fixture` feature and a real `SQLite` database. They cover a
//! success with its counts, cancel, lease loss, a trash that is never
//! published, the renewals of R02ah against the store, a store that stops
//! answering, a stalled worker, a busy lock that spends no attempt, and the
//! delegated-group refusal at start. Its `recovery_*` modules hold the
//! recovery matrix of row R02bc.
//!
//! Every case is ignored. The enforcement job runs them, one per process, from
//! `scripts/enforcement/run.sh` (decision 0056 rule 12). A case that starts a
//! worker first moves its own process into a child group of its unit, and
//! starts each worker as the only process of a new child group of the unit
//! (decision 0057 rules 18 and 22). Every wait has a deadline and observes a condition, never a fixed delay
//! standing in for one. A worker is killed and reaped when its guard is dropped.
#![cfg(all(target_os = "linux", feature = "enforcement-fixture"))]

use std::error::Error;
use std::fmt::Display;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, BufRead, BufReader, Read};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::str::FromStr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::{Duration, Instant};

use artifree_core::artifacts::{ArtifactService, MetadataChange};
use artifree_core::jobs::{Clock, SystemClock};
use artifree_domain::SourceRevision;
use artifree_domain::identity::{ActorId, ArtifactId, RequestKey, Title, UserLabel};
use artifree_domain::job::{
    AttemptIdentity, AttemptState, JobFailure, JobId, JobKind, JobState, ProfileName, Revocation,
};
use artifree_domain::package::{FilePackage, PackagePath};
use artifree_ports::artifact_store::{RequestFingerprint, RequestIdentity};
use artifree_ports::job_store::{AttemptRow, JobStore, NewJob};
use artifree_ports::user_store::{NewUser, UserStore};
use artifree_providers::storage::{DATABASE_FILE, SqliteStore};
use artifree_providers::tokens::SqliteTokenStore;
use rusqlite::Connection;
use serde_json::Value;

type TestResult = Result<(), Box<dyn Error>>;

/// The server binary built with the feature. The unit runs it as the worker.
const BIN: &str = env!("CARGO_BIN_EXE_artifree-server");
/// The unified cgroup hierarchy. A unit's group lies below it.
const CGROUP_ROOT: &str = "/sys/fs/cgroup";
/// Moves the shell's own process into the group named by `$1`, then replaces the shell with the
/// remaining arguments, so the process keeps its identity.
const ATTACH: &str = r#"echo $$ > "$1" && shift && exec "$@""#;
/// The environment variable that names the worker's data directory in the unit's own checks.
const WORKER_DATA: &str = "ARTIFREE_WORKER_DATA";
/// The environment variable that names the renewal part of one renewal case.
const RENEWAL_PART: &str = "ARTIFREE_RENEWAL_PART";
/// The admission lock's file name in the execution directory.
const LOCK_FILE: &str = "admission.lock";
/// The program name that every payload process carries on its command line.
const PAYLOAD: &str = "artifree-execution-payload";
/// The owner of each case's artifact: a generated user id, registered in the users table.
const OWNER: &str = "u-0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a";
/// The lease of the cases whose renewals must reach the store.
const LEASE_3: [&str; 2] = ["--lease-secs", "3"];
/// How long a state, a line or a group may take to appear.
const DEADLINE: Duration = Duration::from_secs(30);
/// How long a renewal's stop may take after its change. The lease is three seconds, so the
/// guardian ends the attempt by its own window if the store's answer is not acted on in time.
const PROMPT: Duration = Duration::from_secs(3);
/// How long the store is held: past its ten-second busy timeout.
const STORE_HOLD: Duration = Duration::from_secs(12);
/// How long the worker is stopped: past the three-second lease.
const STALL: Duration = Duration::from_secs(4);
/// The pause between two looks at a condition.
const STEP: Duration = Duration::from_millis(50);
/// The busy timeout of the test's own connections, which matches the store's.
const BUSY: Duration = Duration::from_secs(10);

// --- fixtures ----------------------------------------------------------------

/// Parses a fixture value, naming it when it is not valid.
fn parsed<T>(text: &str) -> Result<T, Box<dyn Error>>
where
    T: FromStr,
    T::Err: Display,
{
    text.parse::<T>()
        .map_err(|error| format!("invalid fixture {text:?}: {error}").into())
}

/// The owner of the cases' artifact.
fn owner() -> Result<ActorId, Box<dyn Error>> {
    parsed(OWNER)
}

/// The instant `span` from now.
fn within(span: Duration) -> Instant {
    Instant::now() + span
}

/// Creates `path` readable and writable by its owner only.
fn private_dir(path: &Path) -> io::Result<()> {
    DirBuilder::new().mode(0o700).create(path)
}

/// The directories of one case: a data directory and an execution directory, both owner-only.
/// Both are removed when the case ends.
#[derive(Debug)]
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    /// A fresh pair of directories named after `label`.
    fn new(label: &str) -> Result<Self, Box<dyn Error>> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "artifree-execution-jobs-{}-{}-{label}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        // A directory left by an earlier case with the same name is cleared first.
        if root.exists() {
            fs::remove_dir_all(&root)?;
        }
        private_dir(&root)?;
        private_dir(&root.join("data"))?;
        private_dir(&root.join("execution"))?;
        Ok(Self { root })
    }

    fn data(&self) -> PathBuf {
        self.root.join("data")
    }

    fn execution(&self) -> PathBuf {
        self.root.join("execution")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Drop cannot report a failure; a directory left behind is cleared by the next case
        // that uses the same name.
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// The data directory that `job_prepare_a_worker_unit` and `job_check_the_worker_unit_result`
/// share, as `run.sh` names it.
fn worker_data() -> Result<PathBuf, Box<dyn Error>> {
    std::env::var_os(WORKER_DATA)
        .map(PathBuf::from)
        .ok_or_else(|| format!("{WORKER_DATA} must name the worker's data directory").into())
}

/// Calls `probe` until it yields a value. It pauses between calls and gives up at `limit`.
fn poll<T>(
    limit: Instant,
    what: &str,
    mut probe: impl FnMut() -> Result<Option<T>, Box<dyn Error>>,
) -> Result<T, Box<dyn Error>> {
    loop {
        if let Some(value) = probe()? {
            return Ok(value);
        }
        if Instant::now() >= limit {
            return Err(format!("timed out waiting for {what}").into());
        }
        thread::sleep(STEP);
    }
}

/// The attempt group's name, `attempt-<a>-<b>`: the first and the last eight bytes of the
/// identity as unsigned decimal numbers, which is the form the provider's registry uses.
fn group_name(attempt: &AttemptIdentity) -> String {
    let bytes = attempt.bytes();
    let [high @ .., _, _, _, _, _, _, _, _] = bytes;
    let [_, _, _, _, _, _, _, _, low @ ..] = bytes;
    format!(
        "attempt-{}-{}",
        u64::from_be_bytes(high),
        u64::from_be_bytes(low)
    )
}

/// The processes whose command line names the payload, found through `/proc`.
fn payload_processes() -> io::Result<Vec<u32>> {
    let mut found = Vec::new();
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        // A process that exits while it is read has no command line any more.
        let Ok(command) = fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        if String::from_utf8_lossy(&command).contains(PAYLOAD) {
            found.push(pid);
        }
    }
    Ok(found)
}

/// The child group of the unit that holds this test process.
const HARNESS_GROUP: &str = "harness";

/// Moves this test process into a child group of its own unit, enables the controllers of the
/// unit's children, and returns the unit. A second call finds the process already moved and
/// returns the same unit. The unit then holds no process of its own, so a worker started in a
/// child group of it (`worker_group`) can enable controllers below its own group.
fn enter_harness() -> Result<PathBuf, Box<dyn Error>> {
    let cgroup = fs::read_to_string("/proc/self/cgroup")?;
    let path = cgroup
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .ok_or("this process has no cgroup v2 path")?;
    let own = Path::new(CGROUP_ROOT).join(path.trim_start_matches('/'));
    if own.file_name().is_some_and(|name| name == HARNESS_GROUP) {
        let unit = own.parent().ok_or("the harness group has no parent")?;
        return Ok(unit.to_path_buf());
    }
    let harness = own.join(HARNESS_GROUP);
    fs::create_dir_all(&harness)?;
    fs::write(harness.join("cgroup.procs"), std::process::id().to_string())?;
    fs::write(own.join("cgroup.subtree_control"), "+memory +pids +cpu")?;
    Ok(own)
}

/// Makes a new, empty child group of `unit` for one worker, named `worker-<n>` with the lowest
/// free `n`. Each worker of a case gets its own: a worker's admission enables controllers in its
/// group, after which the kernel refuses any process directly in it, so a restarted worker
/// cannot join the group of the worker it replaces.
fn worker_group(unit: &Path) -> Result<PathBuf, Box<dyn Error>> {
    for number in 1..=64_u32 {
        let group = unit.join(format!("worker-{number}"));
        match fs::create_dir(&group) {
            Ok(()) => return Ok(group),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(format!("no free worker group in {}", unit.display()).into())
}

/// Reads `stream` line by line on a thread. The channel closes when the stream ends.
fn read_lines(stream: impl Read + Send + 'static) -> Receiver<String> {
    let (sender, receiver) = channel();
    thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    receiver
}

// --- the worker process -------------------------------------------------------

/// A worker process of this case's unit. Its output is read by threads and collected on demand.
/// It is killed and reaped when it is dropped.
#[derive(Debug)]
struct Worker {
    child: Child,
    /// The worker's own child group of the unit, in which its attempt groups are made.
    group: PathBuf,
    stdout_lines: Receiver<String>,
    stderr_lines: Receiver<String>,
    /// Every stdout line so far, one per line.
    stdout: String,
    /// Every stderr line so far, one per line.
    stderr: String,
}

impl Worker {
    /// Starts `worker run` on `scratch`, with `extra` added, as the only process of a new child
    /// group of the unit.
    fn spawn(scratch: &Scratch, extra: &[&str]) -> Result<Self, Box<dyn Error>> {
        Self::spawn_in(&scratch.data(), &scratch.execution(), extra)
    }

    /// Moves every line the threads have read so far into `stdout` and `stderr`.
    fn pump(&mut self) {
        for line in self.stdout_lines.try_iter() {
            self.stdout.push_str(&line);
            self.stdout.push('\n');
        }
        for line in self.stderr_lines.try_iter() {
            self.stderr.push_str(&line);
            self.stderr.push('\n');
        }
    }

    /// Every stdout line so far, parsed as JSON. A line that is not JSON is an error.
    fn events(&self) -> Result<Vec<Value>, serde_json::Error> {
        self.stdout
            .lines()
            .map(serde_json::from_str::<Value>)
            .collect()
    }

    /// Waits until stdout has an event that `matches` accepts.
    fn wait_event(&mut self, what: &str, matches: impl Fn(&Value) -> bool) -> TestResult {
        poll(within(DEADLINE), what, || {
            self.pump();
            Ok(self.events()?.iter().any(&matches).then_some(()))
        })
    }

    /// Waits until stderr contains `text`, by `limit`.
    fn wait_stderr(&mut self, text: &str, limit: Instant) -> TestResult {
        poll(limit, &format!("{text:?} on stderr"), || {
            self.pump();
            Ok(self.stderr.contains(text).then_some(()))
        })
    }

    /// Waits for the worker to exit.
    fn wait_exit(&mut self) -> Result<ExitStatus, Box<dyn Error>> {
        let child = &mut self.child;
        poll(within(DEADLINE), "the worker to exit", || {
            Ok(child.try_wait()?)
        })
    }

    /// Sends `signal` (for example `STOP` or `CONT`) to the worker with the shell's builtin.
    fn signal(&self, signal: &str) -> TestResult {
        let status = Command::new("sh")
            .arg("-c")
            .arg("kill \"$1\" \"$2\"")
            .arg("sh")
            .arg(format!("-{signal}"))
            .arg(self.child.id().to_string())
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("kill -{signal} {} failed: {status}", self.child.id()).into())
        }
    }

    /// Waits until the worker has started the attempt of its job: an attempt row is unsettled
    /// and the attempt's group is in the worker's group. Returns the attempt and its group.
    fn started_attempt(&self, store: &Store) -> Result<(AttemptIdentity, PathBuf), Box<dyn Error>> {
        let attempt = poll(within(DEADLINE), "an unsettled attempt row", || {
            Ok(store
                .port
                .unsettled_attempts()?
                .first()
                .map(|row| row.attempt))
        })?;
        let group = self.group.join(group_name(&attempt));
        poll(within(DEADLINE), "the attempt group", || {
            Ok(group.is_dir().then_some(()))
        })?;
        Ok((attempt, group))
    }

    /// Waits until the attempt of its job runs its payload: the attempt is started and a process
    /// of its group is the payload, which exists only after the release. Returns the group.
    fn released_attempt(&self, store: &Store) -> Result<PathBuf, Box<dyn Error>> {
        let (_, group) = self.started_attempt(store)?;
        poll(within(DEADLINE), "the released payload", || {
            Ok(runs_the_payload(&group).then_some(()))
        })?;
        Ok(group)
    }
}

/// Whether a process of `group` is the payload, by its command name (the kernel keeps the first
/// 15 bytes of the program's file name). A group or a process that vanished counts as no.
fn runs_the_payload(group: &Path) -> bool {
    let Ok(procs) = fs::read_to_string(group.join("cgroup.procs")) else {
        return false;
    };
    procs.lines().any(|pid| {
        fs::read_to_string(format!("/proc/{pid}/comm"))
            .is_ok_and(|comm| PAYLOAD.starts_with(comm.trim_end()) && !comm.trim_end().is_empty())
    })
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Drop cannot report a failure; the kill and the wait are best effort.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// --- the store ----------------------------------------------------------------

/// The `run_profile` job's columns that the store's job read does not return.
#[derive(Debug)]
struct JobRow {
    state: String,
    attempts: i64,
    failure: Option<String>,
    files: Option<i64>,
    bytes: Option<i64>,
    finished_at: Option<i64>,
}

/// The durable state of one case: a database in a data directory, read through the store and
/// through SQL where the store hides the `run_profile` kind.
struct Store {
    path: PathBuf,
    port: SqliteStore,
}

impl Store {
    /// Opens the database of `data`, creating it when it is absent.
    fn new(data: &Path) -> Result<Self, Box<dyn Error>> {
        let path = data.join(DATABASE_FILE);
        let port = SqliteStore::open(&path)?;
        Ok(Self { path, port })
    }

    /// A connection of the test's own, which waits for the worker's writes as the store does.
    fn sql(&self) -> Result<Connection, rusqlite::Error> {
        let connection = Connection::open(&self.path)?;
        connection.busy_timeout(BUSY)?;
        Ok(connection)
    }

    fn service(&self) -> Result<ArtifactService<SqliteStore>, Box<dyn Error>> {
        Ok(ArtifactService::new(SqliteStore::open(&self.path)?))
    }

    /// Registers the owner and creates its artifact at revision 1. Returns the artifact's id.
    fn create_artifact(&self) -> Result<ArtifactId, Box<dyn Error>> {
        SqliteTokenStore::open(&self.path)?.register(&NewUser {
            id: owner()?,
            label: parsed::<UserLabel>("Owner")?,
            created_at: "2026-11-03T00:00:00Z".to_owned(),
        })?;
        let package = FilePackage::new(vec![(
            parsed::<PackagePath>("index.html")?,
            b"<h1>index</h1>".to_vec(),
        )])?;
        let created = self.service()?.create(
            &owner()?,
            &parsed::<RequestKey>("create-artifact")?,
            &parsed::<Title>("Enforcement")?,
            &package,
        )?;
        Ok(created.artifact.id)
    }

    /// The artifact's id, read from its one row.
    fn artifact(&self) -> Result<ArtifactId, Box<dyn Error>> {
        let ids = self.texts("SELECT id FROM artifacts")?;
        let [id] = ids.as_slice() else {
            return Err(format!("expected one artifact, found {}", ids.len()).into());
        };
        parsed(id)
    }

    /// The first column of every row that `query` returns.
    fn texts(&self, query: &str) -> Result<Vec<String>, Box<dyn Error>> {
        let connection = self.sql()?;
        let mut statement = connection.prepare(query)?;
        let rows = statement.query_map([], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<String>>>()?)
    }

    /// The count that `query` returns in its one row.
    fn count(&self, query: &str) -> Result<i64, rusqlite::Error> {
        self.sql()?.query_row(query, [], |row| row.get(0))
    }

    /// Enqueues one fixture job of the artifact at revision 1, through the core's test-only
    /// enqueue, under the request key `name`. Returns the job's id.
    fn enqueue_fixture(&self, name: &str) -> Result<JobId, Box<dyn Error>> {
        let receipt = self.service()?.enqueue_profile_job(
            &owner()?,
            &parsed::<RequestKey>(&format!("fixture-{name}"))?,
            &self.artifact()?,
            SourceRevision(1),
            &parsed::<ProfileName>("fixture")?,
            SystemClock.now(),
        )?;
        Ok(receipt.job.id)
    }

    /// Enqueues one `verify_revision` job of the artifact, under the request key `name`.
    fn enqueue_verify(&self, name: &str) -> Result<JobId, Box<dyn Error>> {
        let owner = owner()?;
        let key = parsed::<RequestKey>(&format!("verify-{name}"))?;
        let artifact = self.artifact()?;
        let receipt = self.port.enqueue_job(NewJob {
            request: RequestIdentity {
                actor: &owner,
                key: &key,
                fingerprint: RequestFingerprint([3; 32]),
            },
            artifact: &artifact,
            revision: SourceRevision(1),
            kind: JobKind::VerifyRevision,
            now: SystemClock.now(),
        })?;
        Ok(receipt.job.id)
    }

    /// The id of the one `run_profile` job.
    fn only_run_profile_job(&self) -> Result<JobId, Box<dyn Error>> {
        let ids = self.texts("SELECT id FROM jobs WHERE kind = 'run_profile'")?;
        let [id] = ids.as_slice() else {
            return Err(format!("expected one run_profile job, found {}", ids.len()).into());
        };
        parsed(id)
    }

    /// The `run_profile` job's row. The store's job read does not return that kind.
    fn job_row(&self, job: &JobId) -> Result<JobRow, rusqlite::Error> {
        self.sql()?.query_row(
            "SELECT state, attempts, failure, files_checked, bytes_checked, finished_at
             FROM jobs WHERE id = ?1",
            [job.as_str()],
            |row| {
                Ok(JobRow {
                    state: row.get(0)?,
                    attempts: row.get(1)?,
                    failure: row.get(2)?,
                    files: row.get(3)?,
                    bytes: row.get(4)?,
                    finished_at: row.get(5)?,
                })
            },
        )
    }

    /// Waits until the job is in `state`.
    fn wait_for_state(&self, job: &JobId, state: JobState) -> TestResult {
        poll(
            within(DEADLINE),
            &format!("the job to reach {}", state.as_str()),
            || Ok((self.job_row(job)?.state == state.as_str()).then_some(())),
        )
    }

    /// The attempt identities of `job`, in their hexadecimal form.
    fn attempt_names(&self, job: &JobId) -> Result<Vec<String>, Box<dyn Error>> {
        let connection = self.sql()?;
        let mut statement =
            connection.prepare("SELECT attempt FROM execution_attempts WHERE job_id = ?1")?;
        let rows = statement.query_map([job.as_str()], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<String>>>()?)
    }

    /// The one attempt row of `job`, read through the store's attempt read.
    fn attempt_of(&self, job: &JobId) -> Result<AttemptRow, Box<dyn Error>> {
        let names = self.attempt_names(job)?;
        let [name] = names.as_slice() else {
            return Err(format!(
                "expected one attempt of {}, found {}",
                job.as_str(),
                names.len()
            )
            .into());
        };
        let attempt: AttemptIdentity = name.parse()?;
        self.port
            .attempt_row(&attempt)?
            .ok_or_else(|| format!("no attempt row is named {name}").into())
    }
}

// --- the renewals of R02ah ------------------------------------------------------

/// What one renewal part must show: the stop the attempt reports, the revocation the store
/// records, and whether the job ends cancelled. A fence raise is not a revocation: the job
/// stays running and its lease runs out.
struct RenewalCase {
    sql: &'static str,
    stop: &'static str,
    revoked: Option<Revocation>,
    cancelled: bool,
}

/// The case of one renewal part, named by `ARTIFREE_RENEWAL_PART`.
fn renewal_case(part: &str) -> Result<RenewalCase, Box<dyn Error>> {
    Ok(match part {
        "fence" => RenewalCase {
            sql: "UPDATE jobs SET fence = fence + 1 WHERE kind = 'run_profile'",
            stop: "stopped-fence",
            revoked: None,
            cancelled: false,
        },
        "cancel" => RenewalCase {
            sql: "UPDATE jobs SET cancel_requested = 1 WHERE kind = 'run_profile'",
            stop: "stopped-cancel",
            revoked: Some(Revocation::Cancel),
            cancelled: true,
        },
        "generation" => RenewalCase {
            sql: "UPDATE users SET state_generation = state_generation + 1
                  WHERE id = (SELECT owner FROM artifacts)",
            stop: "stopped-generation",
            revoked: Some(Revocation::Generation),
            cancelled: true,
        },
        "authority" => RenewalCase {
            sql: "UPDATE artifacts SET owner = 'u-07070707070707070707070707070707'",
            stop: "stopped-authority",
            revoked: Some(Revocation::Authority),
            cancelled: true,
        },
        other => {
            return Err(format!("{RENEWAL_PART} names no renewal part: {other}").into());
        }
    })
}

// --- shared checks ------------------------------------------------------------

/// Requests the cancel of `job`, then checks the end that cancel must reach: the job ends
/// `cancelled`, its attempt is settled and revoked `cancel`, its group is gone, and the worker
/// reports the stop (decision 0057 rules 7 and 10).
fn cancel_and_settle(store: &Store, worker: &mut Worker, job: &JobId, group: &Path) -> TestResult {
    store.sql()?.execute(
        "UPDATE jobs SET cancel_requested = 1 WHERE id = ?1",
        [job.as_str()],
    )?;
    store.wait_for_state(job, JobState::Cancelled)?;
    let attempt = store.attempt_of(job)?;
    assert_eq!(
        attempt.state,
        AttemptState::Settled,
        "a cancelled job's attempt must be settled"
    );
    assert_eq!(
        attempt.revoked,
        Some(Revocation::Cancel),
        "the cancel must be stored as the attempt's revocation"
    );
    assert!(
        !group.exists(),
        "the attempt group {} must be gone once the job is cancelled",
        group.display()
    );
    worker.wait_stderr(
        &format!("attempt of job {}: stopped-cancel", job.as_str()),
        within(DEADLINE),
    )
}

/// Checks the end of an attempt that the guardian ended because its renewals stopped (R02ah):
/// the guardian's report and the worker's line on stderr, the job failed as `internal`, the
/// attempt settled, and the group gone.
fn expect_guardian_end(
    store: &Store,
    worker: &mut Worker,
    job: &JobId,
    group: &Path,
) -> TestResult {
    worker.wait_stderr("guardian renewal-expired emptied=1", within(DEADLINE))?;
    worker.wait_stderr(
        &format!("attempt of job {}: guardian-renewal-expired", job.as_str()),
        within(DEADLINE),
    )?;
    store.wait_for_state(job, JobState::Failed)?;
    let row = store.job_row(job)?;
    assert_eq!(
        row.failure.as_deref(),
        Some(JobFailure::Internal.as_str()),
        "a job the guardian ended must fail as internal"
    );
    let attempt = store.attempt_of(job)?;
    assert_eq!(
        attempt.state,
        AttemptState::Settled,
        "the guardian's end must settle the attempt"
    );
    assert!(
        !group.exists(),
        "the attempt group {} must be gone after the guardian's end",
        group.display()
    );
    Ok(())
}

// --- the cases ------------------------------------------------------------------

/// Creates the artifact and one fixture job in the worker's data directory, before the unit's
/// own worker runs it (decision 0057 rule 22).
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_prepare_a_worker_unit() -> TestResult {
    let store = Store::new(&worker_data()?)?;
    store.create_artifact()?;
    let job = store.enqueue_fixture("unit")?;
    let row = store.job_row(&job)?;
    assert_eq!(
        row.state,
        JobState::Queued.as_str(),
        "the fixture job waits for the unit's worker"
    );
    Ok(())
}

/// The unit's own worker has run the fixture job to its success: two files and 38 bytes were
/// admitted, the attempt is settled and not revoked, and no attempt is left unsettled.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_check_the_worker_unit_result() -> TestResult {
    let store = Store::new(&worker_data()?)?;
    let job = store.only_run_profile_job()?;
    let row = store.job_row(&job)?;
    assert_eq!(
        row.state,
        JobState::Succeeded.as_str(),
        "the unit's worker must succeed the fixture job"
    );
    assert_eq!(row.attempts, 1, "the job takes one claim");
    assert_eq!(
        (row.files, row.bytes),
        (Some(2), Some(38)),
        "the copy scenario admits the staged 14-byte page with its 15-byte marker (29 bytes) and assets/result.txt (9 bytes)"
    );
    let attempt = store.attempt_of(&job)?;
    assert_eq!(attempt.state, AttemptState::Settled);
    assert_eq!(attempt.revoked, None, "a successful attempt is not revoked");
    let unsettled = store.port.unsettled_attempts()?;
    assert!(
        unsettled.is_empty(),
        "no attempt may be unsettled after the unit's run, found {}",
        unsettled.len()
    );
    Ok(())
}

/// A worker outside a delegated group refuses to start: exit status 2, nothing on stdout, a
/// refusal on stderr that names the cgroup facility, and no database created.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_a_worker_outside_a_delegated_group_refuses_to_start() -> TestResult {
    let scratch = Scratch::new("undelegated")?;
    let output = Command::new(BIN)
        .args(["worker", "run", "--data-dir"])
        .arg(scratch.data())
        .arg("--execution-dir")
        .arg(scratch.execution())
        .output()?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(2),
        "a refused worker exits with status 2; stderr: {stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "a refused worker prints nothing on stdout"
    );
    assert!(
        stderr.starts_with("Execution unavailable."),
        "the refusal must begin with its name; stderr: {stderr}"
    );
    assert!(
        stderr.contains("delegated") || stderr.contains("occupies"),
        "the refusal must name the cgroup facility; stderr: {stderr}"
    );
    assert!(
        !scratch.data().join(DATABASE_FILE).exists(),
        "a refused worker must create no database"
    );
    Ok(())
}

/// Cancel stops a running fixture job: the job ends cancelled only after its attempt is settled
/// and its group is gone.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_cancel_ends_the_job_cancelled_after_cleanup() -> TestResult {
    let scratch = Scratch::new("cancel")?;
    let store = Store::new(&scratch.data())?;
    store.create_artifact()?;
    let job = store.enqueue_fixture("cancel")?;
    let mut worker = Worker::spawn(&scratch, &LEASE_3)?;
    let (_, group) = worker.started_attempt(&store)?;
    cancel_and_settle(&store, &mut worker, &job, &group)
}

/// Cancel also ends a payload whose descendant ignores SIGTERM: no payload process survives,
/// and the job ends cancelled only after its attempt was settled.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_a_sigterm_ignoring_descendant_is_gone_at_cancel() -> TestResult {
    let scratch = Scratch::new("descendant")?;
    let store = Store::new(&scratch.data())?;
    store.create_artifact()?;
    let job = store.enqueue_fixture("descendant")?;
    let mut worker = Worker::spawn(&scratch, &LEASE_3)?;
    let (_, group) = worker.started_attempt(&store)?;
    cancel_and_settle(&store, &mut worker, &job, &group)?;
    let survivors = payload_processes()?;
    assert!(
        survivors.is_empty(),
        "a payload process outlived its cancelled job: {survivors:?}"
    );
    let attempt = store.attempt_of(&job)?;
    let settled = attempt
        .settled_at
        .ok_or("the cancelled attempt has no settlement time")?;
    let finished = store
        .job_row(&job)?
        .finished_at
        .ok_or("the cancelled job has no end time")?;
    assert!(
        finished >= settled.0,
        "the job ended at {finished}, before its attempt settled at {}",
        settled.0
    );
    Ok(())
}

/// A lease revoked while the attempt runs schedules a retry: the attempt is settled revoked
/// `lease`, the job is `retry_scheduled` with one claim spent, and no counts are written.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_lease_loss_schedules_a_retry() -> TestResult {
    let scratch = Scratch::new("lease")?;
    let store = Store::new(&scratch.data())?;
    store.create_artifact()?;
    let job = store.enqueue_fixture("lease")?;
    let worker = Worker::spawn(&scratch, &LEASE_3)?;
    let (attempt, group) = worker.started_attempt(&store)?;
    store.sql()?.execute(
        "UPDATE execution_attempts SET revoked = 'lease' WHERE attempt = ?1",
        [attempt.hex()],
    )?;
    store.wait_for_state(&job, JobState::RetryScheduled)?;
    let row = store.job_row(&job)?;
    assert_eq!(row.attempts, 1, "the lease loss spends one claim");
    assert_eq!(
        (row.files, row.bytes),
        (None, None),
        "a lease loss publishes no counts"
    );
    let recorded = store.attempt_of(&job)?;
    assert_eq!(recorded.state, AttemptState::Settled);
    assert_eq!(recorded.revoked, Some(Revocation::Lease));
    assert!(
        !group.exists(),
        "the attempt group must be gone after the lease loss"
    );
    Ok(())
}

/// A trash during the run is never published: the job ends cancelled with no counts, the
/// attempt is settled revoked `trash`, and stdout has no `succeeded` line for the job.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_a_trash_during_the_run_is_never_published() -> TestResult {
    let scratch = Scratch::new("trash")?;
    let store = Store::new(&scratch.data())?;
    let artifact = store.create_artifact()?;
    let job = store.enqueue_fixture("trash")?;
    let mut worker = Worker::spawn(&scratch, &["--lease-secs", "3600"])?;
    let (_, group) = worker.started_attempt(&store)?;
    let service = store.service()?;
    let owner = owner()?;
    let base = service.artifact(&owner, &artifact)?.metadata_revision;
    let change =
        MetadataChange::new(None, None, Some(true)).ok_or("a trash is a metadata change")?;
    service.update(
        &owner,
        &parsed::<RequestKey>("trash-artifact")?,
        &artifact,
        base,
        &change,
    )?;
    store.wait_for_state(&job, JobState::Cancelled)?;
    let row = store.job_row(&job)?;
    assert_eq!(
        (row.files, row.bytes),
        (None, None),
        "a trashed run publishes no counts"
    );
    let attempt = store.attempt_of(&job)?;
    assert_eq!(attempt.state, AttemptState::Settled);
    assert_eq!(attempt.revoked, Some(Revocation::Trash));
    assert!(
        !group.exists(),
        "the attempt group must be gone once the trashed job ends"
    );
    worker.wait_event("the cancelled line of the job", |event| {
        event["event"] == "job"
            && event["job"] == job.as_str()
            && event["state"] == JobState::Cancelled.as_str()
    })?;
    worker.pump();
    let succeeded = worker
        .events()?
        .iter()
        .any(|event| event["state"] == JobState::Succeeded.as_str());
    assert!(
        !succeeded,
        "a trashed run must not print a succeeded job line"
    );
    Ok(())
}

/// The store's answer at a renewal stops the attempt within three seconds of the change, and the
/// guardian never ends it by its own window. Each part (fence, cancel, generation, authority)
/// is run in its own unit by `run.sh`, named by `ARTIFREE_RENEWAL_PART`.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_a_renewal_stops_at_the_store_answer() -> TestResult {
    let part = std::env::var(RENEWAL_PART)?;
    let case = renewal_case(&part)?;
    let scratch = Scratch::new(&format!("renewal-{part}"))?;
    let store = Store::new(&scratch.data())?;
    store.create_artifact()?;
    let job = store.enqueue_fixture(&format!("renewal-{part}"))?;
    let mut worker = Worker::spawn(&scratch, &LEASE_3)?;
    let (_, group) = worker.started_attempt(&store)?;
    let changed = Instant::now();
    store.sql()?.execute(case.sql, [])?;
    worker.wait_stderr(
        &format!("attempt of job {}: {}", job.as_str(), case.stop),
        changed + PROMPT,
    )?;
    assert!(
        !worker.stderr.contains("renewal-expired") && !worker.stderr.contains("deadline"),
        "the guardian must not end a stopped attempt by its own window; stderr: {}",
        worker.stderr
    );
    if case.cancelled {
        store.wait_for_state(&job, JobState::Cancelled)?;
        let attempt = store.attempt_of(&job)?;
        assert_eq!(attempt.state, AttemptState::Settled);
        assert_eq!(
            attempt.revoked, case.revoked,
            "the store's answer must be stored as the attempt's revocation"
        );
    } else {
        worker.wait_event("the lease_lost line of the job", |event| {
            event["event"] == "lease_lost" && event["job"] == job.as_str()
        })?;
        let row = store.job_row(&job)?;
        assert_eq!(
            row.state,
            JobState::Running.as_str(),
            "a fence raise leaves the job running until its lease runs out"
        );
        let attempt = store.attempt_of(&job)?;
        assert_eq!(attempt.state, AttemptState::Settled);
        assert_eq!(attempt.revoked, None, "a fence raise is not a revocation");
    }
    assert!(
        !group.exists(),
        "the attempt group {} must be gone after the stop",
        group.display()
    );
    Ok(())
}

/// A store that does not answer for longer than its busy timeout ends the attempt through the
/// guardian: the guardian's report, the job failed as `internal`, and the attempt settled.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_an_unavailable_store_ends_the_attempt_by_the_guardian() -> TestResult {
    let scratch = Scratch::new("store-unavailable")?;
    let store = Store::new(&scratch.data())?;
    store.create_artifact()?;
    let job = store.enqueue_fixture("store-unavailable")?;
    let mut worker = Worker::spawn(&scratch, &LEASE_3)?;
    // Blocked only after the release: the release asks the store for a checkpoint, so a block
    // before it would end the attempt at readiness instead of at the guardian's window.
    let group = worker.released_attempt(&store)?;
    let blocker = Connection::open(&store.path)?;
    blocker.execute_batch("BEGIN EXCLUSIVE")?;
    thread::sleep(STORE_HOLD);
    blocker.execute_batch("COMMIT")?;
    expect_guardian_end(&store, &mut worker, &job, &group)
}

/// A worker that stops renewing for longer than its lease is ended by the guardian, the same
/// way as an unavailable store: the worker is stopped, then continued after the guardian's end.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_a_stalled_worker_ends_the_attempt_by_the_guardian() -> TestResult {
    let scratch = Scratch::new("stalled-worker")?;
    let store = Store::new(&scratch.data())?;
    store.create_artifact()?;
    let job = store.enqueue_fixture("stalled-worker")?;
    let mut worker = Worker::spawn(&scratch, &LEASE_3)?;
    // Stopped only after the release: a stop before it would end the attempt at readiness, with
    // another reason than the guardian's renewal window.
    let group = worker.released_attempt(&store)?;
    worker.signal("STOP")?;
    thread::sleep(STALL);
    worker.signal("CONT")?;
    expect_guardian_end(&store, &mut worker, &job, &group)
}

/// A busy admission lock spends no attempt: the verify job runs, the fixture job stays queued
/// with no attempt, and the worker ends its single run with status 0 (decision 0057 rule 19).
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_a_busy_lock_spends_no_attempt() -> TestResult {
    let scratch = Scratch::new("busy")?;
    let store = Store::new(&scratch.data())?;
    store.create_artifact()?;
    let fixture = store.enqueue_fixture("busy-fixture")?;
    let verify = store.enqueue_verify("busy-verify")?;
    let lock = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(scratch.execution().join(LOCK_FILE))?;
    lock.try_lock()
        .map_err(|error| format!("cannot hold the admission lock: {error:?}"))?;
    let mut worker = Worker::spawn(&scratch, &["--once"])?;
    let status = worker.wait_exit()?;
    assert_eq!(
        status.code(),
        Some(0),
        "a worker that finds the lock busy ends its run with status 0"
    );
    assert_eq!(
        store.job_row(&verify)?.state,
        JobState::Succeeded.as_str(),
        "the verify job runs while the lock is busy"
    );
    let held = store.job_row(&fixture)?;
    assert_eq!(
        (held.state.as_str(), held.attempts),
        (JobState::Queued.as_str(), 0),
        "the fixture job stays queued and spends no claim"
    );
    assert_eq!(
        store.count("SELECT count(*) FROM execution_attempts")?,
        0,
        "a busy lock writes no attempt row"
    );
    lock.unlock()?;
    Ok(())
}

/// The environment variable that names the payload scenario of one verdict case.
const VERDICT_CASE: &str = "ARTIFREE_VERDICT_CASE";
/// The one line that the `fail` scenario writes to stderr before it exits with status 3.
const FAIL_STDERR: &str = "artifree-execution-payload: fail scenario\n";
/// The bound that the payload's stdout and stderr share (decision 0042 rule 5).
const LOG_BOUND: usize = 1_048_576;
/// How long a verdict case may take to end: past the provider's 120-second wall time.
const VERDICT_DEADLINE: Duration = Duration::from_secs(180);

/// The provider's word for the end of the scenario `case` names, and the bounds of the stderr
/// that it hands to the worker (decision 0058 rule 18, slice R03bb). The `fail` scenario writes
/// one known line, and `flood` writes past the log bound, so its stderr reaches the worker on a
/// refused path; the others write no stderr, and neither does the launcher.
fn verdict_case(case: &str) -> Result<(&'static str, usize, usize), Box<dyn Error>> {
    match case {
        "fail" => Ok(("exit-nonzero", FAIL_STDERR.len(), FAIL_STDERR.len())),
        "thread-refusal" => Ok(("filter-killed", 0, 0)),
        "hang" => Ok(("deadline-exceeded", 0, 0)),
        "memory" => Ok(("out-of-memory", 0, 0)),
        "flood" => Ok(("output-limit-exceeded", 1, LOG_BOUND)),
        other => Err(format!("unknown verdict case {other:?}").into()),
    }
}

/// A run that fails ends with its own verdict, and the provider hands the payload's stderr to the
/// worker within the log bound: a non-zero exit, a filter kill, the wall time, the memory and the
/// log bound each have their own word on the worker's stderr (decision 0058 rule 18, slice
/// R03bb). The worker still fails each job as `internal` until R03c.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_a_failed_run_reports_its_own_verdict() -> TestResult {
    let case = std::env::var(VERDICT_CASE)?;
    let (word, least, most) = verdict_case(&case)?;
    let scratch = Scratch::new(&format!("verdict-{case}"))?;
    let store = Store::new(&scratch.data())?;
    store.create_artifact()?;
    let job = store.enqueue_fixture(&format!("verdict-{case}"))?;
    let mut worker = Worker::spawn(&scratch, &[])?;
    let prefix = format!("attempt of job {}: {word} (stderr_bytes=", job.as_str());
    worker.wait_stderr(&prefix, within(VERDICT_DEADLINE))?;
    let bytes: usize = worker
        .stderr
        .lines()
        .find_map(|line| line.split_once(&prefix))
        .map(|(_, count)| count.trim_end_matches(')').parse())
        .ok_or("the attempt line went missing")??;
    assert!(
        (least..=most).contains(&bytes),
        "the {case} run must hand over {least} to {most} bytes of stderr, got {bytes}"
    );
    store.wait_for_state(&job, JobState::Failed)?;
    assert_eq!(
        store.job_row(&job)?.failure.as_deref(),
        Some(JobFailure::Internal.as_str()),
        "until R03c a failed {case} run fails its job as internal"
    );
    assert_eq!(
        store.attempt_of(&job)?.state,
        AttemptState::Settled,
        "the {case} run's attempt must be settled"
    );
    Ok(())
}

// --- the recovery matrix of R02bc (decision 0057 rule 20) -------------------------

#[path = "execution_jobs/recovery_disable.rs"]
mod recovery_disable;
#[path = "execution_jobs/recovery_kill.rs"]
mod recovery_kill;
#[path = "execution_jobs/recovery_races.rs"]
mod recovery_races;
#[path = "execution_jobs/recovery_restore.rs"]
mod recovery_restore;
#[path = "execution_jobs/recovery_support.rs"]
mod recovery_support;
#[path = "execution_jobs/recovery_workers.rs"]
mod recovery_workers;
