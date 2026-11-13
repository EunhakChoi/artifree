//! The core's `run_profile` surface against a real `SQLite` store (decision
//! 0057 rules 1 and 3): the test-only enqueue pins the profile in the request
//! fingerprint, the reads never show the kind, and a worker of this build ends
//! such a job as `execution_unavailable` without an attempt. Time is a fixed
//! clock, so nothing sleeps.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use artifree_core::artifacts::{ArtifactError, ArtifactService, Missing};
use artifree_core::jobs::{Clock, JobPageSize};
use artifree_core::worker::{Pace, Step, WorkerConfig, run_next};
use artifree_domain::SourceRevision;
use artifree_domain::identity::{ActorId, ArtifactId, RequestKey, Title, UnixMillis};
use artifree_domain::job::{AttemptIdentity, JobFailure, JobKind, JobState, ProfileName, WorkerId};
use artifree_domain::package::{FilePackage, PackagePath};
use artifree_ports::artifact_store::Delivery;
use artifree_ports::job_store::{JobReceipt, JobRecord, JobStore};
use artifree_providers::storage::{DATABASE_FILE, SqliteStore};
use rusqlite::Connection;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const LEASE_MS: u64 = 1_000;
const NOW: UnixMillis = UnixMillis(1_000_000);

// --- fixtures --------------------------------------------------------------

/// A directory for one test's database, removed when the test ends.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> std::io::Result<Self> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "artifree-run-profile-core-{}-{}",
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

fn parsed<T>(text: &str) -> Result<T, Box<dyn std::error::Error>>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    text.parse::<T>()
        .map_err(|error| format!("invalid fixture {text:?}: {error}").into())
}

/// A store with one app artifact at revision 1, owned by `owner`. The service
/// and the worker use two connections to the same database file.
struct Fixture {
    store: SqliteStore,
    service: ArtifactService<SqliteStore>,
    owner: ActorId,
    artifact: ArtifactId,
    dir: TempDir,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let path = dir.0.join(DATABASE_FILE);
        let store = SqliteStore::open(&path)?;
        let service = ArtifactService::new(SqliteStore::open(&path)?);
        let owner: ActorId = parsed("owner")?;
        let key: RequestKey = parsed("create-key")?;
        let title: Title = parsed("Title")?;
        let package = FilePackage::new(vec![(
            parsed::<PackagePath>("index.html")?,
            b"<h1>index</h1>".to_vec(),
        )])?;
        let created = service.create(&owner, &key, &title, &package)?;
        Ok(Self {
            store,
            service,
            owner,
            artifact: created.artifact.id,
            dir,
        })
    }

    /// The core's test-only enqueue of `profile` at revision 1, under `key`.
    fn profile_job(
        &self,
        key: &RequestKey,
        profile: &ProfileName,
    ) -> Result<JobReceipt, ArtifactError> {
        self.service.enqueue_profile_job(
            &self.owner,
            key,
            &self.artifact,
            SourceRevision(1),
            profile,
            NOW,
        )
    }
}

// --- the enqueue ------------------------------------------------------------

#[test]
fn the_core_enqueue_pins_the_profile_in_the_fingerprint() -> TestResult {
    let fixture = Fixture::new()?;
    let key: RequestKey = parsed("profile-key")?;
    let profile: ProfileName = parsed("fixture")?;
    let other: ProfileName = parsed("other")?;

    let first = fixture.profile_job(&key, &profile)?;
    assert_eq!(
        first.delivery,
        Delivery::Committed,
        "the first request is committed"
    );
    assert_eq!(
        first.job.kind,
        JobKind::RunProfile,
        "the enqueued job is a run_profile job"
    );
    assert_eq!(
        first.job.profile,
        Some(profile.clone()),
        "the job carries the profile it was enqueued with"
    );

    let replay = fixture.profile_job(&key, &profile)?;
    assert_eq!(
        replay.delivery,
        Delivery::Replayed,
        "the same key and profile is a replay"
    );
    assert_eq!(replay.job.id, first.job.id, "a replay returns the same job");

    assert_eq!(
        fixture.profile_job(&key, &other),
        Err(ArtifactError::RequestKeyReused),
        "the same key with another profile is another request"
    );
    assert_eq!(
        fixture.service.enqueue_job(
            &fixture.owner,
            &key,
            &fixture.artifact,
            SourceRevision(1),
            JobKind::VerifyRevision,
            NOW,
        ),
        Err(ArtifactError::RequestKeyReused),
        "the same key as a verify request is another request"
    );
    Ok(())
}

// --- the reads --------------------------------------------------------------

#[test]
fn the_core_reads_do_not_show_a_run_profile_job() -> TestResult {
    let fixture = Fixture::new()?;
    let key: RequestKey = parsed("hidden-key")?;
    let profile: ProfileName = parsed("fixture")?;
    let receipt = fixture.profile_job(&key, &profile)?;

    // The row is stored; only the reads leave it out.
    let connection = Connection::open(fixture.dir.0.join(DATABASE_FILE))?;
    let stored: i64 = connection.query_row(
        "SELECT COUNT(*) FROM jobs WHERE id = ?1",
        [receipt.job.id.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(stored, 1, "the run_profile job is stored");

    assert_eq!(
        fixture.service.job(&fixture.owner, &receipt.job.id),
        Err(ArtifactError::NotFound(Missing::Job)),
        "job() does not return a run_profile job"
    );
    let everything = fixture
        .service
        .jobs(&fixture.owner, None, None, JobPageSize::default())?;
    assert_eq!(
        everything.jobs,
        Vec::<JobRecord>::new(),
        "jobs() lists no run_profile job"
    );
    let of_artifact = fixture.service.jobs(
        &fixture.owner,
        Some(&fixture.artifact),
        None,
        JobPageSize::default(),
    )?;
    assert_eq!(
        of_artifact.jobs,
        Vec::<JobRecord>::new(),
        "jobs() of the artifact lists no run_profile job"
    );
    Ok(())
}

// --- the worker of this build -----------------------------------------------

#[test]
fn a_worker_of_this_build_ends_a_run_profile_job_as_execution_unavailable() -> TestResult {
    let fixture = Fixture::new()?;
    let key: RequestKey = parsed("unknown-profile-key")?;
    let profile: ProfileName = parsed("fixture")?;
    let receipt = fixture.profile_job(&key, &profile)?;
    let worker: WorkerId = parsed("worker-build")?;
    let config = WorkerConfig::with_lease_ms(LEASE_MS);
    let clock = FixedClock(NOW);

    let mut settled: Vec<JobRecord> = Vec::new();
    let step = run_next(
        &fixture.store,
        &clock,
        &worker,
        &config,
        &Calm,
        &mut |record| settled.push(record.clone()),
    )?;
    assert_eq!(
        step,
        Step::Idle,
        "a build without profiles has no job to run"
    );
    let [ended] = settled.as_slice() else {
        return Err(format!(
            "expected exactly one job ended by the claim, got {}",
            settled.len()
        )
        .into());
    };
    assert_eq!(
        ended.id, receipt.job.id,
        "the claim ends the run_profile job"
    );
    assert_eq!(ended.state, JobState::Failed, "the ended job is failed");
    assert_eq!(
        ended.failure,
        Some(JobFailure::ExecutionUnavailable),
        "the failure says the execution is unavailable"
    );
    assert_eq!(ended.attempts, 0, "ending the job counts no attempt");

    assert!(
        fixture.store.unsettled_attempts()?.is_empty(),
        "the claim recorded no attempt that is still unsettled"
    );
    let any_identity = AttemptIdentity::from_random([7; 16]);
    assert!(
        fixture.store.attempt_row(&any_identity)?.is_none(),
        "the claim recorded no attempt row"
    );

    let mut again: Vec<JobRecord> = Vec::new();
    let step = run_next(
        &fixture.store,
        &clock,
        &worker,
        &config,
        &Calm,
        &mut |record| again.push(record.clone()),
    )?;
    assert_eq!(step, Step::Idle, "the next round has nothing to run");
    assert!(
        again.is_empty(),
        "the ended job is not reported a second time, got {again:?}"
    );
    Ok(())
}
