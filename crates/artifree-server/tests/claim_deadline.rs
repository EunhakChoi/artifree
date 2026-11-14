//! The claim deadline of decision 0058 rule 20 (slice R03cdb), against a real
//! `SQLite` store: a worker's claim loop, here the trusted worker's (no
//! execution port), ends a build profile's job that no execution worker
//! claimed within ten minutes of its enqueue as `execution_unavailable`, and
//! leaves alone a job with an attempt row, a running job, a job whose
//! artifact is in the trash, a disabled user's job, a job of a profile that is
//! not a build profile, and every job while an execution worker is ready. An
//! execution worker's loop ends jobs as the trusted worker's does. Time is a
//! fixed clock, so nothing sleeps.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use artifree_core::artifacts::{ArtifactService, MetadataChange};
use artifree_core::jobs::Clock;
use artifree_core::worker::{ClaimEvent, Pace, Profiles, Step, WorkerConfig, run_next_with};
use artifree_domain::identity::{ActorId, ArtifactId, RequestKey, Title, UnixMillis};
use artifree_domain::job::{AttemptIdentity, JobFailure, JobId, JobState, ProfileName, WorkerId};
use artifree_domain::package::{FilePackage, PackagePath};
use artifree_domain::{MetadataRevision, SourceRevision};
use artifree_ports::build_store::CLAIM_DEADLINE_MS;
use artifree_ports::execution::{Capacity, Execution};
use artifree_ports::job_store::{
    AttemptRow, ExecutionClaim, ExecutionPresence, JobClaim, JobStore, READY_WINDOW_MS,
};
use artifree_providers::storage::{DATABASE_FILE, SqliteStore};
use rusqlite::Connection;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const ENQUEUED: i64 = 1_000_000;
/// The owner, a registered user.
const OWNER: &str = "u-00000000000000000000000000000001";
const LEASE_MS: u64 = 30_000;

/// A directory for one test's database, removed when the test ends.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> std::io::Result<Self> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "artifree-claim-deadline-{}-{}",
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

/// A store with one artifact at revision 1. The service and the worker use two
/// connections to the same database file.
struct Fixture {
    store: SqliteStore,
    service: ArtifactService<SqliteStore>,
    owner: ActorId,
    artifact: ArtifactId,
    build: ProfileName,
    other: ProfileName,
    dir: TempDir,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let path = dir.0.join(DATABASE_FILE);
        let store = SqliteStore::open(&path)?;
        let service = ArtifactService::new(SqliteStore::open(&path)?);
        let owner: ActorId = parsed(OWNER)?;
        // The owner is a registered user, so that the test can disable it.
        Connection::open(&path)?.execute(
            "INSERT INTO users (id, label, created_at) VALUES (?1, 'Owner', '2026-11-15T00:00:00Z')",
            [OWNER],
        )?;
        let package = FilePackage::new(vec![(PackagePath::entry(), b"<h1>1</h1>".to_vec())])?;
        let created = service.create(
            &owner,
            &parsed::<RequestKey>("create-key")?,
            &parsed::<Title>("Deadline")?,
            &package,
        )?;
        Ok(Self {
            store,
            service,
            owner,
            artifact: created.artifact.id,
            build: parsed("react-v1")?,
            other: parsed("fixture")?,
            dir,
        })
    }

    fn enqueue(
        &self,
        profile: &ProfileName,
        name: &str,
        at: i64,
    ) -> Result<JobId, Box<dyn std::error::Error>> {
        Ok(self
            .service
            .enqueue_profile_job(
                &self.owner,
                &parsed(&format!("enqueue-{name}"))?,
                &self.artifact,
                SourceRevision(1),
                profile,
                UnixMillis(at),
            )?
            .job
            .id)
    }

    /// One turn of the trusted worker's claim loop at `now`: it knows both
    /// profiles, `react-v1` is its build profile, and it has no execution
    /// port. Returns the jobs the turn reported as ended.
    fn turn(&self, now: i64) -> Result<Vec<JobId>, Box<dyn std::error::Error>> {
        self.turn_with(now, None)
    }

    /// One turn of a worker's claim loop at `now`, with `execution` as its
    /// port (`None` for the trusted worker).
    fn turn_with(
        &self,
        now: i64,
        execution: Option<&dyn Execution>,
    ) -> Result<Vec<JobId>, Box<dyn std::error::Error>> {
        let known = [self.build.clone(), self.other.clone()];
        let builds = [self.build.clone()];
        let mut ended = Vec::new();
        let step = run_next_with(
            &self.store,
            &FixedClock(UnixMillis(now)),
            &parsed::<WorkerId>("trusted-worker")?,
            &WorkerConfig::with_lease_ms(LEASE_MS),
            &Calm,
            Profiles {
                known: &known,
                builds: &builds,
                execution,
            },
            &mut |event| {
                if let ClaimEvent::Settled(record) = event {
                    ended.push(record.id.clone());
                }
            },
        )?;
        if !matches!(step, Step::Idle) {
            return Err(format!("the worker ran a job: {step:?}").into());
        }
        Ok(ended)
    }

    /// The state and failure of a job, read from the database.
    fn row(&self, job: &JobId) -> Result<(String, Option<String>), Box<dyn std::error::Error>> {
        Ok(Connection::open(self.dir.0.join(DATABASE_FILE))?.query_row(
            "SELECT state, failure FROM jobs WHERE id = ?1",
            [job.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?)
    }

    fn trash(&self, base: u64, deleted: bool) -> TestResult {
        let change = MetadataChange::new(None, None, Some(deleted)).ok_or("an empty change")?;
        self.service.update(
            &self.owner,
            &parsed(&format!("trash-key-{base}"))?,
            &self.artifact,
            MetadataRevision(base),
            &change,
        )?;
        Ok(())
    }
}

fn queued() -> (String, Option<String>) {
    (JobState::Queued.as_str().to_owned(), None)
}

fn unavailable() -> (String, Option<String>) {
    (
        JobState::Failed.as_str().to_owned(),
        Some(JobFailure::ExecutionUnavailable.as_str().to_owned()),
    )
}

/// Rule 20: a build profile's job that nobody claimed ends as
/// `execution_unavailable` once ten minutes have passed since its enqueue, at
/// the trusted worker's claim loop, and not a millisecond before.
#[test]
fn an_unclaimed_build_ends_ten_minutes_after_its_enqueue() -> TestResult {
    let fixture = Fixture::new()?;
    let job = fixture.enqueue(&fixture.build.clone(), "build", ENQUEUED)?;

    assert_eq!(fixture.turn(ENQUEUED + CLAIM_DEADLINE_MS - 1)?, Vec::new());
    assert_eq!(
        fixture.row(&job)?,
        queued(),
        "a millisecond before the deadline"
    );

    assert_eq!(
        fixture.turn(ENQUEUED + CLAIM_DEADLINE_MS)?,
        vec![job.clone()]
    );
    assert_eq!(fixture.row(&job)?, unavailable(), "at the deadline");
    assert_eq!(
        fixture.turn(ENQUEUED + CLAIM_DEADLINE_MS + 1)?,
        Vec::new(),
        "an ended job is not ended again"
    );
    Ok(())
}

/// Rule 20: a job with an attempt row is governed by 0057, even when a
/// graceful release put it back in the queue.
#[test]
fn a_job_with_an_attempt_is_left_alone() -> TestResult {
    let fixture = Fixture::new()?;
    let job = fixture.enqueue(&fixture.build.clone(), "attempted", ENQUEUED)?;
    let attempt = AttemptIdentity::from_random([5; 16]);
    let known = [fixture.build.clone()];
    let claimed = fixture
        .store
        .claim_job(JobClaim {
            worker: &parsed::<WorkerId>("execution-worker")?,
            now: UnixMillis(ENQUEUED + 1),
            lease_ms: LEASE_MS,
            known_profiles: &known,
            execution: Some(ExecutionClaim {
                attempt,
                tool_digest: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                machine: "machine-a",
                boot: "boot-a",
                cgroupns: "cgroupns-a",
                directory: "0123456789abcdef0123456789abcdef",
            }),
        })?
        .claimed
        .ok_or("no job was due")?;
    fixture
        .store
        .settle_attempt(&attempt, UnixMillis(ENQUEUED + 2))?;
    fixture
        .store
        .release_job(&claimed.lease, UnixMillis(ENQUEUED + 3))?;
    assert_eq!(fixture.row(&job)?, queued(), "the release put it back");

    assert_eq!(fixture.turn(ENQUEUED + 2 * CLAIM_DEADLINE_MS)?, Vec::new());
    assert_eq!(fixture.row(&job)?, queued());
    Ok(())
}

/// Rule 20 ends only jobs of a build profile of the build, of an artifact
/// outside the trash; a job in the trash is ended once it is recovered.
#[test]
fn only_a_build_profile_outside_the_trash_ends() -> TestResult {
    let fixture = Fixture::new()?;
    let other = fixture.enqueue(&fixture.other.clone(), "other", ENQUEUED)?;
    let build = fixture.enqueue(&fixture.build.clone(), "trashed", ENQUEUED)?;
    let late = ENQUEUED + CLAIM_DEADLINE_MS;

    fixture.trash(1, true)?;
    assert_eq!(fixture.turn(late)?, Vec::new());
    assert_eq!(fixture.row(&build)?, queued(), "the trash keeps it");

    fixture.trash(2, false)?;
    assert_eq!(fixture.turn(late)?, vec![build.clone()]);
    assert_eq!(fixture.row(&build)?, unavailable());
    assert_eq!(
        fixture.row(&other)?,
        queued(),
        "a profile that is not a build profile waits for its worker"
    );
    Ok(())
}

/// An execution port whose admission always refuses, so its worker holds no
/// capacity and keeps no readiness row.
struct Refusing;

impl Execution for Refusing {
    fn capacity(&self, _unsettled: &[AttemptRow]) -> Capacity {
        Capacity::Refused("refused")
    }
}

/// Rule 20 applies only while no execution worker is ready: a worker seen
/// within five minutes keeps an old job queued, and once its row is older the
/// job ends.
#[test]
fn a_ready_execution_worker_keeps_an_old_job_queued() -> TestResult {
    let fixture = Fixture::new()?;
    let job = fixture.enqueue(&fixture.build.clone(), "ready", ENQUEUED)?;
    let seen = ENQUEUED + CLAIM_DEADLINE_MS;
    fixture
        .store
        .mark_seen(&parsed::<WorkerId>("execution-worker")?, UnixMillis(seen))?;

    assert_eq!(fixture.turn(seen + READY_WINDOW_MS)?, Vec::new());
    assert_eq!(fixture.row(&job)?, queued(), "a worker is still ready");

    assert_eq!(fixture.turn(seen + READY_WINDOW_MS + 1)?, vec![job.clone()]);
    assert_eq!(
        fixture.row(&job)?,
        unavailable(),
        "no worker is ready any more"
    );
    Ok(())
}

/// A disabled user's job stays queued, as the claim leaves it (0036), and ends
/// once the user is enabled again.
#[test]
fn a_disabled_users_job_is_left_queued() -> TestResult {
    let fixture = Fixture::new()?;
    let job = fixture.enqueue(&fixture.build.clone(), "disabled", ENQUEUED)?;
    let late = ENQUEUED + CLAIM_DEADLINE_MS;
    let disable = |at: Option<&str>| {
        Connection::open(fixture.dir.0.join(DATABASE_FILE))?.execute(
            "UPDATE users SET disabled_at = ?2 WHERE id = ?1",
            rusqlite::params![OWNER, at],
        )?;
        Ok::<(), Box<dyn std::error::Error>>(())
    };

    disable(Some("2026-11-15T00:00:00Z"))?;
    assert_eq!(fixture.turn(late)?, Vec::new());
    assert_eq!(fixture.row(&job)?, queued(), "a disabled user's job waits");

    disable(None)?;
    assert_eq!(fixture.turn(late)?, vec![job.clone()]);
    assert_eq!(fixture.row(&job)?, unavailable());
    Ok(())
}

/// Every worker's claim loop applies the deadline: an execution worker that is
/// refused capacity, and so is not ready, ends the job too.
#[test]
fn an_execution_workers_loop_ends_an_old_job() -> TestResult {
    let fixture = Fixture::new()?;
    let job = fixture.enqueue(&fixture.build.clone(), "execution", ENQUEUED)?;
    let late = ENQUEUED + CLAIM_DEADLINE_MS;
    assert_eq!(fixture.turn_with(late, Some(&Refusing))?, vec![job.clone()]);
    assert_eq!(fixture.row(&job)?, unavailable());
    Ok(())
}

/// A job that an execution worker claimed is running, not queued, and the
/// deadline leaves it to its lease.
#[test]
fn a_claimed_job_is_not_ended() -> TestResult {
    let fixture = Fixture::new()?;
    let job = fixture.enqueue(&fixture.build.clone(), "claimed", ENQUEUED)?;
    let known = [fixture.build.clone()];
    fixture
        .store
        .claim_job(JobClaim {
            worker: &parsed::<WorkerId>("execution-worker")?,
            now: UnixMillis(ENQUEUED + 1),
            lease_ms: LEASE_MS,
            known_profiles: &known,
            execution: Some(ExecutionClaim {
                attempt: AttemptIdentity::from_random([6; 16]),
                tool_digest: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                machine: "machine-a",
                boot: "boot-a",
                cgroupns: "cgroupns-a",
                directory: "0123456789abcdef0123456789abcdef",
            }),
        })?
        .claimed
        .ok_or("no job was due")?;
    assert_eq!(fixture.turn(ENQUEUED + 2 * CLAIM_DEADLINE_MS)?, Vec::new());
    assert_eq!(fixture.row(&job)?.0, "running");
    Ok(())
}
