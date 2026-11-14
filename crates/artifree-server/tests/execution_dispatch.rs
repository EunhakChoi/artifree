//! Behavioral contract of the worker's dispatch of `run_profile` jobs through the
//! execution port (decision 0057 rules 6, 7, 10, 15, 16, 17 and 19), against a
//! real `SQLite` store and a scripted port. The launch is scripted and runs no
//! payload, so every host runs these tests.
//!
//! The service and the worker's store are two connections to one database file.
//! Time is fixed, so nothing sleeps.

use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use artifree_core::artifacts::ArtifactService;
use artifree_core::jobs::Clock;
use artifree_core::worker::{self, ClaimEvent, Pace, Profiles, Step, WorkerConfig};
use artifree_domain::SourceRevision;
use artifree_domain::identity::{ActorId, ArtifactId, RequestKey, Title, UnixMillis};
use artifree_domain::job::{
    AttemptIdentity, AttemptState, CheckpointAnswer, JobFailure, JobId, JobKind, JobState,
    ProfileName, Revocation, StopReason, WorkerId,
};
use artifree_domain::package::{FileChange, FilePackage, PackagePath};
use artifree_ports::artifact_store::{ArtifactStore, FileEntry};
use artifree_ports::execution::{
    BoundRun, Capacity, Checkpoint, CheckpointUnavailable, Execution, ExecutionHost, HeldCapacity,
    LaunchFailure, LaunchOutcome, PinnedInput, ProfileOutput, ProfileRun,
};
use artifree_ports::job_store::{
    AttemptRow, ExecutionClaim, JobClaim, JobRecord, JobStore, JobStoreError, ProfileCounts,
};
use artifree_providers::storage::{DATABASE_FILE, SqliteStore};
use rusqlite::Connection;

type TestResult = Result<(), Box<dyn Error>>;

const LEASE_MS: u64 = 1_000;
const NOW: UnixMillis = UnixMillis(1_000_000);
const INDEX_V1: &str = "<h1>pinned revision one</h1>";
const INDEX_V2: &str = "<h1>head revision two</h1>";
const STYLE: &str = "h1{color:red}";
/// The counts the scripted launch reports for its output.
const OUTPUT_COUNTS: ProfileCounts = ProfileCounts {
    files: 2,
    bytes: 38,
};
/// The word the scripted capacity gives for a refusal.
const REFUSAL: &str = "quarantined";
const TOOL_DIGEST: &str = concat!(
    "abababab", "abababab", "abababab", "abababab", "abababab", "abababab", "abababab", "abababab"
);
const DIRECTORY: &str = concat!("0123456789abcdef", "0123456789abcdef");
const OTHER_DIRECTORY: &str = concat!("fedcba9876543210", "fedcba9876543210");
const MACHINE: &str = "00112233445566778899aabbccddeeff";
const BOOT: &str = "6f1c2a3e-9b4d-4e5f-8a7b-0c1d2e3f4a5b";
const CGROUPNS: &str = "cgroup:[1]";

// --- fixtures --------------------------------------------------------------

/// A directory for one test's database, removed when the test ends.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> std::io::Result<Self> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "artifree-execution-dispatch-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        // A directory left by an earlier run with the same name is cleared first.
        if path.exists() {
            std::fs::remove_dir_all(&path)?;
        }
        std::fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // Drop cannot report a failure; a directory left behind is cleared by the
        // next run that uses the same name.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A clock that always reads the same time.
#[derive(Debug)]
struct FixedClock(UnixMillis);

impl Clock for FixedClock {
    fn now(&self) -> UnixMillis {
        self.0
    }
}

/// A process that is never asked to stop.
#[derive(Debug)]
struct Calm;

impl Pace for Calm {
    fn shutdown_requested(&self) -> bool {
        false
    }
}

fn parsed<T>(text: &str) -> Result<T, Box<dyn Error>>
where
    T: std::str::FromStr,
    T::Err: fmt::Display,
{
    text.parse::<T>()
        .map_err(|error| format!("invalid fixture {text:?}: {error}").into())
}

fn package(index: &str) -> Result<FilePackage, Box<dyn Error>> {
    let files = vec![
        (
            parsed::<PackagePath>("index.html")?,
            index.as_bytes().to_vec(),
        ),
        (
            parsed::<PackagePath>("style.css")?,
            STYLE.as_bytes().to_vec(),
        ),
    ];
    Ok(FilePackage::new(files)?)
}

fn text(job: &JobId) -> String {
    job.as_str().to_owned()
}

fn attempt_of(seed: u8) -> AttemptIdentity {
    AttemptIdentity::from_random([seed; 16])
}

/// The attempt the scripted held capacity draws.
fn held_attempt() -> AttemptIdentity {
    attempt_of(3)
}

/// One database with an app artifact at revision 1, owned by `owner`. The
/// service and the worker's store are two connections to the same file.
struct World {
    store: SqliteStore,
    service: ArtifactService<SqliteStore>,
    database: PathBuf,
    owner: ActorId,
    artifact: ArtifactId,
    profile: ProfileName,
    worker: WorkerId,
    _dir: TempDir,
}

impl World {
    fn new() -> Result<Self, Box<dyn Error>> {
        let dir = TempDir::new()?;
        let database = dir.0.join(DATABASE_FILE);
        let store = SqliteStore::open(&database)?;
        let service = ArtifactService::new(SqliteStore::open(&database)?);
        let owner: ActorId = parsed("owner")?;
        let key: RequestKey = parsed("create-key")?;
        let title: Title = parsed("Title")?;
        let created = service.create(&owner, &key, &title, &package(INDEX_V1)?)?;
        Ok(Self {
            store,
            service,
            database,
            owner,
            artifact: created.artifact.id,
            profile: parsed("fixture")?,
            worker: parsed("worker-execution")?,
            _dir: dir,
        })
    }

    /// The handles a scripted port and its held capacity share with the test.
    fn shared(&self, log: &Arc<Log>) -> Shared {
        Shared {
            store: self.store.clone(),
            database: self.database.clone(),
            log: Arc::clone(log),
        }
    }

    /// Enqueues the test's `run_profile` job at revision 1 under `key`.
    fn enqueue_fixture(&self, key: &str) -> Result<JobId, Box<dyn Error>> {
        let key: RequestKey = parsed(key)?;
        let receipt = self.service.enqueue_profile_job(
            &self.owner,
            &key,
            &self.artifact,
            SourceRevision(1),
            &self.profile,
            NOW,
        )?;
        Ok(receipt.job.id)
    }

    /// Enqueues a `verify_revision` job at revision 1 under `key`.
    fn enqueue_verify(&self, key: &str) -> Result<JobId, Box<dyn Error>> {
        let key: RequestKey = parsed(key)?;
        let receipt = self.service.enqueue_job(
            &self.owner,
            &key,
            &self.artifact,
            SourceRevision(1),
            JobKind::VerifyRevision,
            NOW,
        )?;
        Ok(receipt.job.id)
    }

    /// The attempt row, which the test expects to exist.
    fn attempt(&self, attempt: &AttemptIdentity) -> Result<AttemptRow, Box<dyn Error>> {
        let row = self
            .store
            .attempt_row(attempt)?
            .ok_or("the attempt row is missing")?;
        Ok(row)
    }
}

/// The shared record of one test: the worker's claim events and the scripted
/// port's observations, in the order they happened.
#[derive(Debug, Default)]
struct Log(Mutex<Vec<Seen>>);

impl Log {
    fn push(&self, seen: Seen) {
        // A poisoned lock follows only a panic while it was held, and nothing here
        // panics there; `snapshot` fails the test in that case.
        if let Ok(mut events) = self.0.lock() {
            events.push(seen);
        }
    }

    fn snapshot(&self) -> Result<Vec<Seen>, &'static str> {
        let events = self.0.lock().map_err(|_| "the event log is poisoned")?;
        Ok(events.clone())
    }
}

/// One thing the worker or the scripted port did.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Seen {
    /// The worker asked the port for capacity, with the unsettled attempts it read.
    CapacityAsked(Vec<AttemptIdentity>),
    /// A claim ended a job without running it.
    Settled(String),
    /// A claim took a job.
    Claimed(String),
    /// A launch started, with what it received.
    Started(Box<Started>),
    /// A checkpoint renewal answered, or could not.
    Renewal(Result<CheckpointAnswer, CheckpointUnavailable>),
    /// A launch ended, as the worker reports it.
    Launched {
        /// The job whose launch ended.
        job: String,
        /// The port's word for the end.
        detail: &'static str,
    },
    /// The worker reported that the port refused capacity, with its text.
    Refused(&'static str),
    /// The scripted port could not do what its script needs.
    Problem(String),
}

/// What a launch received: the run's identities, the pinned input as the listing
/// and the bytes the worker checked it against, and each reconciled attempt as the
/// store showed it when the launch began.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Started {
    job: String,
    fence: u64,
    attempt: AttemptIdentity,
    profile: String,
    tool_digest: String,
    listing: Vec<FileEntry>,
    input: Vec<(String, Vec<u8>)>,
    reconciled: Vec<(AttemptIdentity, Result<Option<AttemptState>, JobStoreError>)>,
}

/// How the scripted capacity answers a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    /// The admission lock is held by another process.
    Busy,
    /// Admission or the quarantine refused.
    Refused,
    /// Capacity is held.
    Held,
}

/// How a held launch ends.
#[derive(Debug, Clone, Copy)]
enum Launch {
    /// Cleanup observed; success whose binding names this run.
    Succeeds,
    /// Cleanup observed; success whose binding names another fence.
    NamesAnotherRun,
    /// Cleanup observed; the payload never started.
    Unavailable,
    /// Cleanup observed; the payload ran and failed.
    Fails,
    /// Cleanup not observed; a success is returned, and nothing may be written.
    NoCleanup,
    /// Renews, has the job's cancel requested in the store, renews again and stops.
    StopsAtCancel,
}

/// The script of one scripted execution port.
#[derive(Debug)]
struct Scene {
    answer: Answer,
    attempt: AttemptIdentity,
    reconciled: Vec<AttemptIdentity>,
    launch: Launch,
}

fn scene(answer: Answer, launch: Launch) -> Scene {
    Scene {
        answer,
        attempt: held_attempt(),
        reconciled: Vec::new(),
        launch,
    }
}

/// The store, the database file and the record that the scripted port shares
/// with its held capacity, which must own its handles.
#[derive(Clone)]
struct Shared {
    store: SqliteStore,
    database: PathBuf,
    log: Arc<Log>,
}

/// The scripted execution port. It answers every capacity request the same way.
struct FakeExecution {
    shared: Shared,
    scene: Scene,
}

impl Execution for FakeExecution {
    fn capacity(&self, unsettled: &[AttemptRow]) -> Capacity {
        self.shared.log.push(Seen::CapacityAsked(
            unsettled.iter().map(|row| row.attempt).collect(),
        ));
        match self.scene.answer {
            Answer::Busy => Capacity::Busy,
            Answer::Refused => Capacity::Refused(REFUSAL),
            Answer::Held => Capacity::Held(Box::new(HeldFixture {
                shared: self.shared.clone(),
                host: ExecutionHost {
                    machine: MACHINE.to_owned(),
                    boot: BOOT.to_owned(),
                    cgroupns: CGROUPNS.to_owned(),
                },
                directory: DIRECTORY.to_owned(),
                tool_digest: TOOL_DIGEST.to_owned(),
                attempt: self.scene.attempt,
                reconciled: self.scene.reconciled.clone(),
                launch: self.scene.launch,
            })),
        }
    }
}

/// Capacity the scripted port holds. Its launch follows the script and runs no
/// payload.
struct HeldFixture {
    shared: Shared,
    host: ExecutionHost,
    directory: String,
    tool_digest: String,
    attempt: AttemptIdentity,
    reconciled: Vec<AttemptIdentity>,
    launch: Launch,
}

impl fmt::Debug for HeldFixture {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HeldFixture")
            .field("attempt", &self.attempt)
            .finish_non_exhaustive()
    }
}

impl HeldFixture {
    /// What the launch received, with each reconciled attempt as the store shows
    /// it now.
    fn started(&self, run: &ProfileRun<'_>, input: PinnedInput) -> Started {
        let PinnedInput { package, listing } = input;
        let reconciled: Vec<_> = self
            .reconciled
            .iter()
            .map(|attempt| {
                let state = self
                    .shared
                    .store
                    .attempt_row(attempt)
                    .map(|row| row.map(|row| row.state));
                (*attempt, state)
            })
            .collect();
        Started {
            job: text(run.job),
            fence: run.fence,
            attempt: run.attempt,
            profile: run.profile.as_str().to_owned(),
            tool_digest: self.tool_digest.clone(),
            listing,
            input: package
                .into_files()
                .map(|(path, content)| (path.as_str().to_owned(), content))
                .collect(),
            reconciled,
        }
    }

    fn output(&self, run: &ProfileRun<'_>, bound: BoundRun) -> ProfileOutput {
        ProfileOutput {
            counts: OUTPUT_COUNTS,
            profile: run.profile.clone(),
            tool_digest: self.tool_digest.clone(),
            bound,
        }
    }

    /// Renews, has the job's cancel requested in the store, renews again, and
    /// reports the stop.
    fn stop_at_cancel(&self, run: &ProfileRun<'_>, checkpoint: &dyn Checkpoint) -> LaunchOutcome {
        self.renew(checkpoint);
        if let Err(error) = self.request_cancel(run.job) {
            self.shared.log.push(Seen::Problem(error.to_string()));
        }
        self.renew(checkpoint);
        LaunchOutcome {
            cleanup_observed: true,
            result: Err(LaunchFailure::Stopped(StopReason::Cancel)),
            detail: "cancelled",
        }
    }

    fn renew(&self, checkpoint: &dyn Checkpoint) {
        self.shared.log.push(Seen::Renewal(checkpoint.renew()));
    }

    fn request_cancel(&self, job: &JobId) -> Result<(), rusqlite::Error> {
        Connection::open(&self.shared.database)?.execute(
            "UPDATE jobs SET cancel_requested = 1 WHERE id = ?1",
            [job.as_str()],
        )?;
        Ok(())
    }
}

impl HeldCapacity for HeldFixture {
    fn host(&self) -> &ExecutionHost {
        &self.host
    }

    fn directory(&self) -> &str {
        &self.directory
    }

    fn tool_digest(&self) -> &str {
        &self.tool_digest
    }

    fn attempt(&self) -> AttemptIdentity {
        self.attempt
    }

    fn reconciled(&self) -> &[AttemptIdentity] {
        &self.reconciled
    }

    fn launch(
        &mut self,
        run: &ProfileRun<'_>,
        input: PinnedInput,
        checkpoint: &dyn Checkpoint,
    ) -> LaunchOutcome {
        let started = self.started(run, input);
        self.shared.log.push(Seen::Started(Box::new(started)));
        match self.launch {
            Launch::Succeeds => LaunchOutcome {
                cleanup_observed: true,
                result: Ok(self.output(run, BoundRun::of(run))),
                detail: "succeeded",
            },
            Launch::NamesAnotherRun => LaunchOutcome {
                cleanup_observed: true,
                result: Ok(self.output(
                    run,
                    BoundRun {
                        fence: run.fence + 1,
                        ..BoundRun::of(run)
                    },
                )),
                detail: "succeeded",
            },
            Launch::Unavailable => LaunchOutcome {
                cleanup_observed: true,
                result: Err(LaunchFailure::Unavailable),
                detail: "unavailable",
            },
            Launch::Fails => LaunchOutcome {
                cleanup_observed: true,
                result: Err(LaunchFailure::Failed),
                detail: "failed",
            },
            Launch::NoCleanup => LaunchOutcome {
                cleanup_observed: false,
                result: Ok(self.output(run, BoundRun::of(run))),
                detail: "unobserved",
            },
            Launch::StopsAtCancel => self.stop_at_cancel(run, checkpoint),
        }
    }
}

/// Converts a claim event of the worker into the record of this test.
fn seen_of(event: ClaimEvent<'_>) -> Seen {
    match event {
        ClaimEvent::Settled(record) => Seen::Settled(text(&record.id)),
        ClaimEvent::Claimed(job) => Seen::Claimed(text(job)),
        ClaimEvent::Launched { job, detail } => Seen::Launched {
            job: text(job),
            detail,
        },
        ClaimEvent::Refused(detail) => Seen::Refused(detail),
    }
}

/// One call of the worker, with `execution` as its port (`None` for a worker
/// without one).
fn run_once(
    world: &World,
    execution: Option<&dyn Execution>,
    log: &Log,
) -> Result<Step, JobStoreError> {
    worker::run_next_with(
        &world.store,
        &FixedClock(NOW),
        &world.worker,
        &WorkerConfig::with_lease_ms(LEASE_MS),
        &Calm,
        Profiles {
            known: std::slice::from_ref(&world.profile),
            execution,
        },
        &mut |event| log.push(seen_of(event)),
    )
}

/// Claims the oldest due `run_profile` job directly, as a worker of `directory`
/// holding `attempt` would, and returns its id.
fn claim_directly(
    store: &SqliteStore,
    worker: &WorkerId,
    known: &[ProfileName],
    attempt: AttemptIdentity,
    directory: &str,
) -> Result<JobId, Box<dyn Error>> {
    let claim = store.claim_job(JobClaim {
        worker,
        now: NOW,
        lease_ms: LEASE_MS,
        known_profiles: known,
        execution: Some(ExecutionClaim {
            attempt,
            tool_digest: TOOL_DIGEST,
            machine: MACHINE,
            boot: BOOT,
            cgroupns: CGROUPNS,
            directory,
        }),
    })?;
    let claimed = claim.claimed.ok_or("no job was due for the direct claim")?;
    Ok(claimed.job.id)
}

fn finished(step: Step) -> Result<JobRecord, Box<dyn Error>> {
    match step {
        Step::Finished(record) => Ok(record),
        other => Err(format!("expected the job to finish, got {other:?}").into()),
    }
}

/// The state and the attempt count of a `run_profile` job, read from its row
/// because the reads of the core do not return that kind.
fn job_row(database: &Path, job: &JobId) -> Result<(String, i64), Box<dyn Error>> {
    let connection = Connection::open(database)?;
    let row = connection.query_row(
        "SELECT state, attempts FROM jobs WHERE id = ?1",
        [job.as_str()],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
    )?;
    Ok(row)
}

/// The files count of a `run_profile` job's end, which is `None` until it
/// succeeds.
fn job_files(database: &Path, job: &JobId) -> Result<Option<i64>, Box<dyn Error>> {
    let connection = Connection::open(database)?;
    let files = connection.query_row(
        "SELECT files_checked FROM jobs WHERE id = ?1",
        [job.as_str()],
        |row| row.get::<_, Option<i64>>(0),
    )?;
    Ok(files)
}

fn attempt_row_count(database: &Path) -> Result<i64, Box<dyn Error>> {
    let connection = Connection::open(database)?;
    let count = connection.query_row("SELECT COUNT(*) FROM execution_attempts", [], |row| {
        row.get::<_, i64>(0)
    })?;
    Ok(count)
}

fn unsettled_attempts(world: &World) -> Result<Vec<AttemptIdentity>, JobStoreError> {
    Ok(world
        .store
        .unsettled_attempts()?
        .into_iter()
        .map(|row| row.attempt)
        .collect())
}

/// The claim events and the launch ends of the worker, in order.
fn worker_events(seen: &[Seen]) -> Vec<Seen> {
    seen.iter()
        .filter(|event| {
            matches!(
                event,
                Seen::Settled(_) | Seen::Claimed(_) | Seen::Launched { .. } | Seen::Refused(_)
            )
        })
        .cloned()
        .collect()
}

fn started_runs(seen: &[Seen]) -> Vec<&Started> {
    seen.iter()
        .filter_map(|event| {
            if let Seen::Started(started) = event {
                Some(started.as_ref())
            } else {
                None
            }
        })
        .collect()
}

fn renewals(seen: &[Seen]) -> Vec<Result<CheckpointAnswer, CheckpointUnavailable>> {
    seen.iter()
        .filter_map(|event| {
            if let Seen::Renewal(answer) = event {
                Some(*answer)
            } else {
                None
            }
        })
        .collect()
}

fn problems(seen: &[Seen]) -> Vec<String> {
    seen.iter()
        .filter_map(|event| {
            if let Seen::Problem(problem) = event {
                Some(problem.clone())
            } else {
                None
            }
        })
        .collect()
}

fn capacity_asks(seen: &[Seen]) -> usize {
    seen.iter()
        .filter(|event| matches!(event, Seen::CapacityAsked(_)))
        .count()
}

/// The single launch the test expects, or an error that names how many there were.
fn one_launch<'a>(runs: &[&'a Started]) -> Result<&'a Started, Box<dyn Error>> {
    match runs {
        [run] => Ok(*run),
        other => Err(format!("expected one launch, got {}", other.len()).into()),
    }
}

// --- the capacity ------------------------------------------------------------

/// A port that is busy or refused spends no attempt. The worker runs the trusted
/// job and leaves the `run_profile` job queued.
fn capacity_refused_or_busy_spends_no_attempt(answer: Answer) -> TestResult {
    let world = World::new()?;
    let log = Arc::new(Log::default());
    let fixture = world.enqueue_fixture("busy-fixture-key")?;
    let verify = world.enqueue_verify("busy-verify-key")?;
    let execution = FakeExecution {
        shared: world.shared(&log),
        scene: scene(answer, Launch::Succeeds),
    };

    let first = finished(run_once(&world, Some(&execution), &log)?)?;
    assert_eq!(
        first.id, verify,
        "the trusted job is claimed while capacity is not held"
    );
    assert_eq!(
        first.state,
        JobState::Succeeded,
        "the trusted job runs to success"
    );
    let second = run_once(&world, Some(&execution), &log)?;
    assert_eq!(
        second,
        Step::Idle,
        "the run_profile job waits, so nothing else is due"
    );

    assert_eq!(
        job_row(&world.database, &fixture)?,
        ("queued".to_owned(), 0),
        "the fixture job stays queued with no attempt counted"
    );
    assert_eq!(
        attempt_row_count(&world.database)?,
        0,
        "no execution attempt was recorded"
    );
    let events = log.snapshot()?;
    assert_eq!(
        capacity_asks(&events),
        2,
        "the port was asked before each claim"
    );
    assert!(
        started_runs(&events).is_empty(),
        "no launch was started without capacity"
    );
    // A refusal reaches the worker's caller at every ask, so that the operator
    // sees its text (decision 0057 rules 13 and 19); a busy lock is silent.
    let expected = match answer {
        Answer::Refused => vec![
            Seen::Refused(REFUSAL),
            Seen::Claimed(text(&verify)),
            Seen::Refused(REFUSAL),
        ],
        _ => vec![Seen::Claimed(text(&verify))],
    };
    assert_eq!(
        worker_events(&events),
        expected,
        "the only claim is the trusted job, and only a refusal is reported"
    );
    Ok(())
}

#[test]
fn busy_capacity_spends_no_attempt_and_still_runs_trusted_jobs() -> TestResult {
    capacity_refused_or_busy_spends_no_attempt(Answer::Busy)
}

#[test]
fn refused_capacity_spends_no_attempt_and_still_runs_trusted_jobs() -> TestResult {
    capacity_refused_or_busy_spends_no_attempt(Answer::Refused)
}

// --- the reconciliation ----------------------------------------------------

#[test]
fn reconciliation_settles_every_reported_attempt_before_the_claim() -> TestResult {
    let world = World::new()?;
    let log = Arc::new(Log::default());
    let first = world.enqueue_fixture("reconcile-first-key")?;
    let second = world.enqueue_fixture("reconcile-second-key")?;
    let third = world.enqueue_fixture("reconcile-third-key")?;
    let known = std::slice::from_ref(&world.profile);
    // Two earlier claims, made directly, leave two running jobs with attempts
    // that the worker must meet. The first attempt is revoked by the store.
    let reconciled = attempt_of(1);
    let foreign = attempt_of(2);
    let reconciled_job = claim_directly(&world.store, &world.worker, known, reconciled, DIRECTORY)?;
    let foreign_job = claim_directly(&world.store, &world.worker, known, foreign, OTHER_DIRECTORY)?;
    let remaining = [first, second, third]
        .into_iter()
        .find(|job| *job != reconciled_job && *job != foreign_job)
        .ok_or("no queued job is left for the worker")?;
    Connection::open(&world.database)?.execute(
        "UPDATE execution_attempts SET revoked = 'cancel' WHERE attempt = ?1",
        [reconciled.hex()],
    )?;

    let execution = FakeExecution {
        shared: world.shared(&log),
        scene: Scene {
            reconciled: vec![reconciled],
            ..scene(Answer::Held, Launch::Succeeds)
        },
    };
    let record = finished(run_once(&world, Some(&execution), &log)?)?;

    assert_eq!(record.id, remaining, "the worker claims the one queued job");
    assert_eq!(
        record.state,
        JobState::Succeeded,
        "the claimed job runs to success"
    );
    let events = log.snapshot()?;
    assert_eq!(
        worker_events(&events),
        vec![
            Seen::Settled(text(&reconciled_job)),
            Seen::Claimed(text(&remaining)),
            Seen::Launched {
                job: text(&remaining),
                detail: "succeeded",
            },
        ],
        "the reconciled job is settled before the claim, and the claim precedes the launch"
    );
    let runs = started_runs(&events);
    let run = one_launch(&runs)?;
    let settled: Result<Option<AttemptState>, JobStoreError> = Ok(Some(AttemptState::Settled));
    assert_eq!(
        run.reconciled,
        vec![(reconciled, settled)],
        "the reconciled attempt was settled when the launch began"
    );
    assert_eq!(
        job_row(&world.database, &reconciled_job)?.0,
        "cancelled",
        "settling the revoked attempt cancelled its job"
    );
    assert_eq!(
        unsettled_attempts(&world)?,
        vec![foreign],
        "only the foreign attempt is still unsettled"
    );
    Ok(())
}

// --- the outcome ----------------------------------------------------------

#[test]
fn a_success_settles_then_publishes_the_counts() -> TestResult {
    let world = World::new()?;
    let log = Arc::new(Log::default());
    let job = world.enqueue_fixture("success-key")?;
    let execution = FakeExecution {
        shared: world.shared(&log),
        scene: scene(Answer::Held, Launch::Succeeds),
    };

    let record = finished(run_once(&world, Some(&execution), &log)?)?;

    assert_eq!(record.id, job, "the claimed job is the one that ran");
    assert_eq!(
        record.state,
        JobState::Succeeded,
        "a verified launch succeeds the job"
    );
    assert_eq!(
        record.counts,
        Some(OUTPUT_COUNTS),
        "the job keeps the launch's two counts"
    );
    let row = world.attempt(&held_attempt())?;
    assert_eq!(
        row.state,
        AttemptState::Settled,
        "the attempt is settled when the job ends"
    );
    assert_eq!(
        row.revoked, None,
        "a settled attempt that was not revoked keeps no revocation"
    );
    assert_eq!(
        worker_events(&log.snapshot()?),
        vec![
            Seen::Claimed(text(&job)),
            Seen::Launched {
                job: text(&job),
                detail: "succeeded",
            },
        ],
        "the claim and the end of the launch are reported in order"
    );
    Ok(())
}

/// A launch that does not succeed settles its attempt and then fails the job with
/// the given failure, publishing no counts.
fn a_failed_launch_settles_then_fails_the_job(
    launch: Launch,
    failure: JobFailure,
    detail: &'static str,
) -> TestResult {
    let world = World::new()?;
    let log = Arc::new(Log::default());
    let job = world.enqueue_fixture("failure-key")?;
    let execution = FakeExecution {
        shared: world.shared(&log),
        scene: scene(Answer::Held, launch),
    };

    let record = finished(run_once(&world, Some(&execution), &log)?)?;

    assert_eq!(record.state, JobState::Failed, "the job ends as failed");
    assert_eq!(
        record.failure,
        Some(failure),
        "the failure names the launch's end"
    );
    assert_eq!(record.counts, None, "a failed job publishes no counts");
    assert_eq!(
        world.attempt(&held_attempt())?.state,
        AttemptState::Settled,
        "the attempt is settled before the job is failed"
    );
    assert_eq!(
        worker_events(&log.snapshot()?),
        vec![
            Seen::Claimed(text(&job)),
            Seen::Launched {
                job: text(&job),
                detail,
            },
        ],
        "the claim and the end of the launch are reported in order"
    );
    Ok(())
}

#[test]
fn a_launch_that_never_started_fails_the_job_as_execution_unavailable() -> TestResult {
    a_failed_launch_settles_then_fails_the_job(
        Launch::Unavailable,
        JobFailure::ExecutionUnavailable,
        "unavailable",
    )
}

#[test]
fn a_launch_that_failed_fails_the_job_as_internal() -> TestResult {
    a_failed_launch_settles_then_fails_the_job(Launch::Fails, JobFailure::Internal, "failed")
}

#[test]
fn an_unobserved_cleanup_marks_the_attempt_cleanup_pending_and_nothing_else() -> TestResult {
    let world = World::new()?;
    let log = Arc::new(Log::default());
    let job = world.enqueue_fixture("unobserved-key")?;
    let execution = FakeExecution {
        shared: world.shared(&log),
        scene: scene(Answer::Held, Launch::NoCleanup),
    };

    let step = run_once(&world, Some(&execution), &log)?;

    assert_eq!(
        step,
        Step::Unsettled(job.clone()),
        "the run reports that its attempt is unsettled"
    );
    assert_eq!(
        job_row(&world.database, &job)?,
        ("running".to_owned(), 1),
        "the job stays running with its claim counted"
    );
    assert_eq!(
        job_files(&world.database, &job)?,
        None,
        "the job has no counts"
    );
    assert_eq!(
        world.attempt(&held_attempt())?.state,
        AttemptState::CleanupPending,
        "an attempt whose cleanup was not observed becomes cleanup_pending (0057 rules 10 and 19)"
    );
    assert_eq!(
        unsettled_attempts(&world)?,
        vec![held_attempt()],
        "the attempt is still unsettled"
    );
    assert_eq!(
        worker_events(&log.snapshot()?),
        vec![
            Seen::Claimed(text(&job)),
            Seen::Launched {
                job: text(&job),
                detail: "unobserved",
            },
        ],
        "no settlement is reported for the job"
    );
    Ok(())
}

#[test]
fn a_binding_that_names_another_run_is_not_published() -> TestResult {
    let world = World::new()?;
    let log = Arc::new(Log::default());
    let job = world.enqueue_fixture("binding-key")?;
    let execution = FakeExecution {
        shared: world.shared(&log),
        scene: scene(Answer::Held, Launch::NamesAnotherRun),
    };

    let record = finished(run_once(&world, Some(&execution), &log)?)?;

    assert_eq!(
        record.state,
        JobState::Failed,
        "a binding of another run does not succeed the job"
    );
    assert_eq!(
        record.failure,
        Some(JobFailure::Internal),
        "the refused binding fails the job as internal"
    );
    assert_eq!(record.counts, None, "no counts are published");
    assert_eq!(
        world.attempt(&held_attempt())?.state,
        AttemptState::Settled,
        "the attempt is settled before the job is failed"
    );
    assert_eq!(
        worker_events(&log.snapshot()?),
        vec![
            Seen::Claimed(text(&job)),
            Seen::Launched {
                job: text(&job),
                detail: "succeeded",
            },
        ],
        "the launch reported its own word, and the worker refused its binding"
    );
    Ok(())
}

// --- the checkpoint --------------------------------------------------------

#[test]
fn a_checkpoint_stop_settles_and_the_store_moves_the_job() -> TestResult {
    let world = World::new()?;
    let log = Arc::new(Log::default());
    let job = world.enqueue_fixture("stop-key")?;
    let execution = FakeExecution {
        shared: world.shared(&log),
        scene: scene(Answer::Held, Launch::StopsAtCancel),
    };

    let record = finished(run_once(&world, Some(&execution), &log)?)?;

    assert_eq!(
        record.state,
        JobState::Cancelled,
        "the store moves the job once its revoked attempt is settled"
    );
    let row = world.attempt(&held_attempt())?;
    assert_eq!(
        row.state,
        AttemptState::Settled,
        "the stopped attempt is settled"
    );
    assert_eq!(
        row.revoked,
        Some(Revocation::Cancel),
        "the stop is stored as the attempt's revocation"
    );
    let events = log.snapshot()?;
    let expected: Vec<Result<CheckpointAnswer, CheckpointUnavailable>> = vec![
        Ok(CheckpointAnswer::Continue),
        Ok(CheckpointAnswer::Stop(StopReason::Cancel)),
    ];
    assert_eq!(
        renewals(&events),
        expected,
        "the first renewal goes on, and the one after the cancel request stops"
    );
    assert_eq!(
        problems(&events),
        Vec::<String>::new(),
        "the cancel request reached the store"
    );
    assert_eq!(
        worker_events(&events),
        vec![
            Seen::Claimed(text(&job)),
            Seen::Launched {
                job: text(&job),
                detail: "cancelled",
            },
        ],
        "the claim and the stopped launch are reported in order"
    );
    Ok(())
}

// --- the input -----------------------------------------------------------

#[test]
fn a_damaged_input_is_never_launched() -> TestResult {
    let world = World::new()?;
    let log = Arc::new(Log::default());
    // The stored bytes of `index.html` no longer match its listing.
    Connection::open(&world.database)?.execute(
        "UPDATE blobs SET content = CAST('xxxx' AS BLOB)
         WHERE digest = (SELECT digest FROM revision_files WHERE path = 'index.html')",
        [],
    )?;
    let job = world.enqueue_fixture("damaged-key")?;
    let execution = FakeExecution {
        shared: world.shared(&log),
        scene: scene(Answer::Held, Launch::Succeeds),
    };

    let record = finished(run_once(&world, Some(&execution), &log)?)?;

    assert_eq!(record.state, JobState::Failed, "the damaged job fails");
    assert_eq!(
        record.failure,
        Some(JobFailure::RevisionUnavailable),
        "the failure says the revision is unavailable"
    );
    assert_eq!(
        world.attempt(&held_attempt())?.state,
        AttemptState::Settled,
        "the attempt is settled before the job is failed"
    );
    let events = log.snapshot()?;
    assert!(
        started_runs(&events).is_empty(),
        "the damaged file was never handed to a launch"
    );
    assert_eq!(
        worker_events(&events),
        vec![Seen::Claimed(text(&job))],
        "the claim ends without a launch"
    );
    Ok(())
}

#[test]
fn the_input_is_the_pinned_revision_not_the_head() -> TestResult {
    let world = World::new()?;
    let log = Arc::new(Log::default());
    let job = world.enqueue_fixture("pinned-key")?;
    let index: PackagePath = parsed("index.html")?;
    let key: RequestKey = parsed("revise-key")?;
    world.service.save(
        &world.owner,
        &key,
        &world.artifact,
        SourceRevision(1),
        vec![FileChange::Put {
            path: index.clone(),
            content: INDEX_V2.as_bytes().to_vec(),
        }],
    )?;
    // The head is revision 2 now. The run must still read revision 1.
    assert_eq!(
        world
            .store
            .revision_file(&world.artifact, SourceRevision(2), &index)?,
        Some(INDEX_V2.as_bytes().to_vec()),
        "the head holds the second revision's bytes"
    );
    let execution = FakeExecution {
        shared: world.shared(&log),
        scene: scene(Answer::Held, Launch::Succeeds),
    };

    let record = finished(run_once(&world, Some(&execution), &log)?)?;

    assert_eq!(record.id, job, "the claimed job is the one that ran");
    assert_eq!(record.state, JobState::Succeeded, "the pinned run succeeds");
    let events = log.snapshot()?;
    let runs = started_runs(&events);
    let run = one_launch(&runs)?;
    let pinned = world
        .store
        .revision(&world.artifact, SourceRevision(1))?
        .ok_or("revision 1 is missing")?;
    assert_eq!(
        run.listing, pinned.files,
        "the listing the launch received is revision 1's"
    );
    assert_eq!(
        run.input,
        vec![
            ("index.html".to_owned(), INDEX_V1.as_bytes().to_vec()),
            ("style.css".to_owned(), STYLE.as_bytes().to_vec()),
        ],
        "the bytes the launch received are revision 1's, not the head's"
    );
    Ok(())
}
