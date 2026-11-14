//! The generation gate of `run_profile` claims (decision 0042 rule 6; decision
//! 0057 rules 5 and 9). A job whose newest attempt pinned an actor generation
//! other than the actor's current one ends cancelled at its next claim, with no
//! new attempt. A job whose generation did not change, and a job that never
//! started, are claimed under the current generation as usual.
//!
//! The fixtures are copies of those in `run_profile_lifecycle.rs`, so this file
//! stands alone. Time is a number each call is given, so every test moves it by
//! hand.

mod common;

use std::error::Error;

use artifree_domain::SourceRevision;
use artifree_domain::identity::{ActorId, ArtifactId, StateGeneration, UnixMillis, UserLabel};
use artifree_domain::job::{AttemptIdentity, AttemptState, JobId, JobState, ProfileName, WorkerId};
use artifree_ports::artifact_store::{CommitReceipt, RequestFingerprint, RequestIdentity};
use artifree_ports::job_store::{
    AttemptRow, Claim, ClaimedJob, ExecutionClaim, JobClaim, JobRecord, JobStore, JobStoreError,
    NewProfileJob, SettleOutcome,
};
use artifree_ports::user_store::{NewUser, UserStore};
use artifree_providers::storage::SqliteStore;
use artifree_providers::tokens::SqliteTokenStore;
use common::{TempDir, TestResult, create_demo, database_in, digest, key, parsed};
use rusqlite::Connection;

const FIRST: &str = "<h1>first</h1>";
const LEASE: u64 = 1_000;
/// 64 lowercase hexadecimal digits, as the tool root digest must be.
const TOOL_DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
/// 32 lowercase hexadecimal digits, as the execution directory identity must be.
const DIRECTORY: &str = "fedcba9876543210fedcba9876543210";

fn at(ms: i64) -> UnixMillis {
    UnixMillis(ms)
}

fn worker() -> WorkerId {
    parsed("worker-a")
}

fn review() -> ProfileName {
    parsed("review")
}

fn attempt_id(byte: u8) -> AttemptIdentity {
    AttemptIdentity::from_random([byte; 16])
}

/// The execution identities of a claim that runs as attempt `byte`.
fn execution(byte: u8) -> ExecutionClaim<'static> {
    ExecutionClaim {
        attempt: attempt_id(byte),
        tool_digest: TOOL_DIGEST,
        machine: "machine-a",
        boot: "boot-a",
        cgroupns: "cgroup-a",
        directory: DIRECTORY,
    }
}

/// Claims the oldest due job as attempt `byte`, in a build that knows the one
/// profile of these tests and has an execution directory.
fn claim_as(store: &SqliteStore, now: i64, byte: u8) -> Result<Claim, JobStoreError> {
    let profiles = [review()];
    store.claim_job(JobClaim {
        worker: &worker(),
        now: at(now),
        lease_ms: LEASE,
        known_profiles: &profiles,
        execution: Some(execution(byte)),
    })
}

/// Like [`claim_as`], but the claim must have taken a job.
fn claim_one(store: &SqliteStore, now: i64, byte: u8) -> Result<ClaimedJob, Box<dyn Error>> {
    let claim = claim_as(store, now, byte)?;
    let claimed = claim.claimed.ok_or("no job was due")?;
    Ok(claimed)
}

/// One attempt row, which must exist.
fn row_of(store: &SqliteStore, attempt: AttemptIdentity) -> Result<AttemptRow, Box<dyn Error>> {
    let row = store
        .attempt_row(&attempt)?
        .ok_or("the attempt row is missing")?;
    Ok(row)
}

/// The job of a settlement that settled the attempt now. A call that changed
/// nothing is an error here, because these tests expect the settlement.
fn settled_job(outcome: SettleOutcome) -> Result<JobRecord, Box<dyn Error>> {
    match outcome {
        SettleOutcome::Settled(job) => Ok(job),
        SettleOutcome::AlreadySettled(_) => Err("the attempt was settled already".into()),
    }
}

fn enqueue_profile(
    store: &SqliteStore,
    actor: &ActorId,
    artifact: &ArtifactId,
    name: &str,
    now: i64,
) -> Result<JobRecord, JobStoreError> {
    let request_key = key(name);
    let profile = review();
    let receipt = store.enqueue_profile_job(NewProfileJob {
        request: RequestIdentity {
            actor,
            key: &request_key,
            fingerprint: RequestFingerprint(digest(name)),
        },
        artifact,
        revision: SourceRevision(1),
        profile: &profile,
        now: at(now),
    })?;
    Ok(receipt.job)
}

/// How the first attempt of a job ended before the generation moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    /// Settled with no revocation, so the job stays running with its lease
    /// expired.
    Unrevoked,
    /// Revoked with `lease`, the restore marking of decision 0057 rule 13, so
    /// the settlement schedules a retry.
    LeaseRevoked,
}

impl Ending {
    /// The state the job is in once the first attempt has settled.
    fn state_after_settlement(self) -> &'static str {
        match self {
            Self::Unrevoked => "running",
            Self::LeaseRevoked => "retry_scheduled",
        }
    }
}

/// A store in a fresh database directory.
struct Fixture {
    dir: TempDir,
    store: SqliteStore,
}

/// A registered user and an artifact of their own.
struct Tenant {
    user: ActorId,
    created: CommitReceipt,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn Error>> {
        let dir = TempDir::new();
        let store = SqliteStore::open(&database_in(dir.path()))?;
        Ok(Self { dir, store })
    }

    fn sql(&self) -> Result<Connection, rusqlite::Error> {
        Connection::open(database_in(self.dir.path()))
    }

    fn identity(&self) -> Result<SqliteTokenStore, Box<dyn Error>> {
        let store = SqliteTokenStore::open(&database_in(self.dir.path()))?;
        Ok(store)
    }

    /// Registers user number `seed` and creates an artifact for them.
    fn tenant(&self, seed: u8) -> Result<Tenant, Box<dyn Error>> {
        let user = ActorId::user([seed; 16]);
        self.identity()?.register(&NewUser {
            id: user.clone(),
            label: parsed::<UserLabel>(&format!("User {seed}")),
            created_at: "2026-11-03T00:00:00Z".to_owned(),
        })?;
        let created = create_demo(
            &self.store,
            &user,
            &key(&format!("tenant-{seed}")),
            "Mine",
            FIRST,
        )?;
        Ok(Tenant { user, created })
    }

    /// Enqueues a `run_profile` job of the tenant's artifact at revision 1.
    fn enqueue_for(
        &self,
        tenant: &Tenant,
        name: &str,
        now: i64,
    ) -> Result<JobRecord, JobStoreError> {
        enqueue_profile(
            &self.store,
            &tenant.user,
            &tenant.created.artifact.id,
            name,
            now,
        )
    }

    fn disable(&self, tenant: &Tenant) -> Result<(), Box<dyn Error>> {
        self.identity()?
            .set_disabled(&tenant.user, Some("2026-11-04T00:00:00Z"))?;
        Ok(())
    }

    fn enable(&self, tenant: &Tenant) -> Result<(), Box<dyn Error>> {
        self.identity()?.set_disabled(&tenant.user, None)?;
        Ok(())
    }

    /// The tenant's state generation as the users row holds it now.
    fn generation(&self, tenant: &Tenant) -> Result<StateGeneration, Box<dyn Error>> {
        let record = self
            .identity()?
            .user(&tenant.user)?
            .ok_or("the tenant has no users row")?;
        Ok(record.state_generation)
    }

    /// The stored state of a job, read from SQL.
    fn state_of(&self, job: &JobId) -> Result<String, Box<dyn Error>> {
        let state: String = self.sql()?.query_row(
            "SELECT state FROM jobs WHERE id = ?1",
            [job.as_str()],
            |raw| raw.get(0),
        )?;
        Ok(state)
    }

    /// How many attempt rows the job has.
    fn attempt_count(&self, job: &JobId) -> Result<i64, Box<dyn Error>> {
        let count: i64 = self.sql()?.query_row(
            "SELECT COUNT(*) FROM execution_attempts WHERE job_id = ?1",
            [job.as_str()],
            |raw| raw.get(0),
        )?;
        Ok(count)
    }
}

/// Enqueues the tenant's job, claims it as attempt 1 at time 0, and ends that
/// attempt as `ending` names. Once this returns the job waits for its next
/// claim: a running job whose lease expired at time 1000, or a retry due at
/// time 1.
fn first_attempt_ends(
    fixture: &Fixture,
    tenant: &Tenant,
    ending: Ending,
) -> Result<JobRecord, Box<dyn Error>> {
    let job = fixture.enqueue_for(tenant, "job", 0)?;
    let claimed = claim_one(&fixture.store, 0, 1)?;
    assert_eq!(
        claimed.job.id, job.id,
        "the first claim takes the tenant's job"
    );
    if ending == Ending::LeaseRevoked {
        // Decision 0057 rule 13: a restore marks the attempt's cleanup pending
        // with a `lease` revocation.
        fixture.sql()?.execute(
            "UPDATE execution_attempts SET revoked = 'lease' WHERE attempt = ?1",
            [attempt_id(1).hex()],
        )?;
    }
    settled_job(fixture.store.settle_attempt(&attempt_id(1), at(1))?)?;
    assert_eq!(
        fixture.state_of(&job.id)?,
        ending.state_after_settlement(),
        "the settled attempt leaves the job {ending:?}"
    );
    Ok(job)
}

/// A job whose first attempt ended as `ending` is not claimed again once its
/// actor's generation has moved on. It ends cancelled at the claim, and no
/// second attempt is written (decision 0042 rule 6; decision 0057 rule 9).
fn cancelled_at_reclaim(ending: Ending) -> TestResult {
    let fixture = Fixture::new()?;
    let tenant = fixture.tenant(1)?;
    let job = first_attempt_ends(&fixture, &tenant, ending)?;
    let pinned = row_of(&fixture.store, attempt_id(1))?.actor_generation;
    assert_eq!(
        pinned,
        Some(StateGeneration::FIRST),
        "the first claim pins the registration generation"
    );
    fixture.disable(&tenant)?;
    fixture.enable(&tenant)?;
    let current = fixture.generation(&tenant)?;
    assert_ne!(
        Some(current),
        pinned,
        "disable and enable moved the generation on"
    );

    let reclaim = claim_as(&fixture.store, 5_000, 2)?;
    assert!(
        reclaim.claimed.is_none(),
        "a job whose generation changed takes no new attempt"
    );
    assert_eq!(reclaim.settled.len(), 1, "the claim ends the one due job");
    let ended = reclaim.settled.first().ok_or("the claim ended no job")?;
    assert_eq!(ended.id, job.id, "the claim ends the tenant's job");
    assert_eq!(
        ended.state,
        JobState::Cancelled,
        "a job whose generation changed ends cancelled"
    );
    assert_eq!(
        fixture.state_of(&job.id)?,
        "cancelled",
        "the stored job is cancelled"
    );
    assert_eq!(
        fixture.attempt_count(&job.id)?,
        1,
        "no attempt is started under the new generation"
    );
    Ok(())
}

/// A job whose generation did not change is claimed again after its first
/// attempt ended as `ending`. The second claim is fence 2, and its attempt pins
/// the unchanged generation.
fn reclaimed_without_generation_change(ending: Ending) -> TestResult {
    let fixture = Fixture::new()?;
    let tenant = fixture.tenant(2)?;
    let job = first_attempt_ends(&fixture, &tenant, ending)?;

    let reclaim = claim_one(&fixture.store, 5_000, 2)?;
    assert_eq!(reclaim.job.id, job.id, "the job is claimed again");
    assert_eq!(
        reclaim.lease.fence, 2,
        "the second claim is the job's second fence"
    );
    assert_eq!(
        fixture.attempt_count(&job.id)?,
        2,
        "the second claim writes its own attempt row"
    );
    let current = fixture.generation(&tenant)?;
    assert_eq!(
        row_of(&fixture.store, attempt_id(2))?.actor_generation,
        Some(current),
        "the new attempt pins the unchanged generation"
    );
    Ok(())
}

#[test]
fn a_reclaim_after_a_disable_and_enable_before_any_checkpoint_ends_cancelled() -> TestResult {
    cancelled_at_reclaim(Ending::Unrevoked)
}

#[test]
fn a_retry_after_a_lease_revocation_and_a_generation_change_ends_cancelled() -> TestResult {
    cancelled_at_reclaim(Ending::LeaseRevoked)
}

#[test]
fn an_unchanged_generation_is_reclaimed() -> TestResult {
    reclaimed_without_generation_change(Ending::Unrevoked)
}

#[test]
fn an_unchanged_generation_after_a_lease_revocation_is_reclaimed() -> TestResult {
    reclaimed_without_generation_change(Ending::LeaseRevoked)
}

#[test]
fn a_never_started_job_claims_under_the_new_generation() -> TestResult {
    let fixture = Fixture::new()?;
    let tenant = fixture.tenant(3)?;
    let job = fixture.enqueue_for(&tenant, "fresh", 0)?;
    fixture.disable(&tenant)?;
    fixture.enable(&tenant)?;
    let current = fixture.generation(&tenant)?;
    assert_ne!(
        current,
        StateGeneration::FIRST,
        "disable and enable moved the generation on"
    );

    let claimed = claim_one(&fixture.store, 1, 1)?;
    assert_eq!(claimed.job.id, job.id, "the never-started job is claimed");
    assert_eq!(
        row_of(&fixture.store, attempt_id(1))?.actor_generation,
        Some(current),
        "its attempt pins the current generation, not the registration one"
    );
    Ok(())
}

/// An attempt whose cleanup its worker did not observe becomes `cleanup_pending` and keeps its
/// job from a claim after the lease expired; only a later settlement frees it. A settled attempt
/// stays settled, and an unknown identity is not found (decision 0057 rules 10 and 19).
#[test]
fn an_unobserved_cleanup_marks_only_a_live_attempt_cleanup_pending() -> TestResult {
    let fixture = Fixture::new()?;
    let tenant = fixture.tenant(4)?;
    let job = fixture.enqueue_for(&tenant, "stuck", 0)?;
    let claimed = claim_one(&fixture.store, 1, 1)?;
    assert_eq!(claimed.job.id, job.id, "the job is claimed");

    fixture.store.mark_cleanup_pending(&attempt_id(1))?;
    assert_eq!(
        row_of(&fixture.store, attempt_id(1))?.state,
        AttemptState::CleanupPending,
        "a live attempt whose cleanup was not observed becomes cleanup_pending"
    );
    let later = claim_as(&fixture.store, 10_000, 2)?;
    assert!(
        later.claimed.is_none(),
        "a job whose attempt is cleanup_pending is not claimed after its lease expired"
    );

    settled_job(fixture.store.settle_attempt(&attempt_id(1), at(10_001))?)?;
    fixture.store.mark_cleanup_pending(&attempt_id(1))?;
    assert_eq!(
        row_of(&fixture.store, attempt_id(1))?.state,
        AttemptState::Settled,
        "a settled attempt is left settled"
    );
    assert!(
        matches!(
            fixture.store.mark_cleanup_pending(&attempt_id(9)),
            Err(JobStoreError::NotFound)
        ),
        "an attempt that does not exist is not found"
    );
    Ok(())
}
