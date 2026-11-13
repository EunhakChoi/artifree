//! Behavioral contract of `run_profile` jobs in the `SQLite` store (decision
//! 0057, rules 5 and 7 to 12): trash and recovery, disable and enable, purge,
//! settlement, publication, and the tombstone that survives a restore.
//!
//! The store's job reads leave `run_profile` jobs out (rule 3), so these tests
//! read the job row with SQL where its state matters. Time is a number each
//! call is given, so every test moves it by hand.

mod common;

use std::error::Error;

use artifree_domain::identity::{ActorId, ArtifactId, StateGeneration, UnixMillis, UserLabel};
use artifree_domain::job::{
    AttemptIdentity, AttemptState, CheckpointAnswer, JobId, JobState, ProfileName, Revocation,
    StopReason, WorkerId,
};
use artifree_domain::{MetadataRevision, SourceRevision};
use artifree_ports::artifact_store::{
    ArtifactStore, CommitReceipt, PurgeRequest, RequestFingerprint, RequestIdentity, StoreError,
};
use artifree_ports::job_store::{
    AttemptRow, Claim, ClaimedJob, ExecutionClaim, JobClaim, JobRecord, JobStore, JobStoreError,
    NewProfileJob, ProfileBinding, ProfileCounts, SettleOutcome,
};
use artifree_ports::user_store::{NewUser, UserStore};
use artifree_providers::backup::{backup, restore};
use artifree_providers::storage::SqliteStore;
use artifree_providers::tokens::SqliteTokenStore;
use common::{
    TempDir, TestResult, actor, create_demo, database_in, digest, key, parsed, update_metadata,
};
use rusqlite::Connection;

const FIRST: &str = "<h1>first</h1>";
const LEASE: u64 = 1_000;
const LEASE_MS: i64 = 1_000;
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

fn counts() -> ProfileCounts {
    ProfileCounts {
        files: 3,
        bytes: 42,
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

/// Whether `execution_attempts` has a row that names a missing job.
fn has_foreign_key_violation(connection: &Connection) -> Result<bool, rusqlite::Error> {
    let mut statement = connection.prepare("PRAGMA foreign_key_check(execution_attempts)")?;
    let mut violations = statement.query([])?;
    let found = violations.next()?.is_some();
    Ok(found)
}

/// A `run_profile` job as its row stores it. The store's job reads do not
/// return these jobs (decision 0057 rule 3), so the tests read the row.
struct Stored {
    state: String,
    fence: i64,
    attempts: i64,
    cancel_requested: bool,
    files_checked: Option<i64>,
    bytes_checked: Option<i64>,
}

struct Fixture {
    dir: TempDir,
    store: SqliteStore,
    owner: ActorId,
    created: CommitReceipt,
}

/// A registered user and an artifact of their own.
struct Tenant {
    user: ActorId,
    created: CommitReceipt,
}

/// Whether a settled attempt or a live one is refused for the generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// The attempt is still running.
    Live,
    /// Its cleanup finished with no revocation, so the job still waits for
    /// its worker's publication.
    SettledUnrevoked,
}

impl Fixture {
    /// An artifact of the owner, created with the request key `create`.
    fn new() -> Result<Self, Box<dyn Error>> {
        let dir = TempDir::new();
        let store = SqliteStore::open(&database_in(dir.path()))?;
        let owner = actor("owner");
        let created = create_demo(&store, &owner, &key("create"), "Secret", FIRST)?;
        Ok(Self {
            dir,
            store,
            owner,
            created,
        })
    }

    fn id(&self) -> &ArtifactId {
        &self.created.artifact.id
    }

    fn sql(&self) -> Result<Connection, rusqlite::Error> {
        Connection::open(database_in(self.dir.path()))
    }

    fn count(&self, query: &str) -> Result<i64, rusqlite::Error> {
        self.sql()?.query_row(query, [], |raw| raw.get(0))
    }

    fn identity(&self) -> Result<SqliteTokenStore, Box<dyn Error>> {
        Ok(SqliteTokenStore::open(&database_in(self.dir.path()))?)
    }

    /// Enqueues a `run_profile` job of the owner's artifact at revision 1.
    fn enqueue(&self, name: &str, now: i64) -> Result<JobRecord, JobStoreError> {
        enqueue_profile(&self.store, &self.owner, self.id(), name, now)
    }

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

    /// The row of a `run_profile` job, read from SQL.
    fn stored(&self, job: &JobId) -> Result<Stored, Box<dyn Error>> {
        let row = self.sql()?.query_row(
            "SELECT state, fence, attempts, cancel_requested, files_checked, bytes_checked
             FROM jobs WHERE id = ?1",
            [job.as_str()],
            |raw| {
                Ok(Stored {
                    state: raw.get(0)?,
                    fence: raw.get(1)?,
                    attempts: raw.get(2)?,
                    cancel_requested: raw.get(3)?,
                    files_checked: raw.get(4)?,
                    bytes_checked: raw.get(5)?,
                })
            },
        )?;
        Ok(row)
    }

    /// Trashes the owner's artifact at metadata revision 1.
    fn trash(&self) -> Result<(), StoreError> {
        update_metadata(
            &self.store,
            &self.created,
            &key("trash"),
            1,
            None,
            None,
            Some(true),
        )
        .map(|_| ())
    }

    /// Recovers the owner's artifact at metadata revision 2.
    fn recover(&self) -> Result<(), StoreError> {
        update_metadata(
            &self.store,
            &self.created,
            &key("recover"),
            2,
            None,
            None,
            Some(false),
        )
        .map(|_| ())
    }

    /// Purges the owner's artifact, which is trashed at metadata revision 2.
    fn purge(&self) -> Result<(), StoreError> {
        self.store
            .purge_artifact(PurgeRequest {
                request: RequestIdentity {
                    actor: &self.owner,
                    key: &key("purge"),
                    fingerprint: RequestFingerprint(digest("purge")),
                },
                artifact: self.id(),
                base: MetadataRevision(2),
            })
            .map(|_| ())
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

    fn disable(&self, tenant: &Tenant) -> Result<(), Box<dyn Error>> {
        self.identity()?
            .set_disabled(&tenant.user, Some("2026-11-04T00:00:00Z"))?;
        Ok(())
    }

    fn enable(&self, tenant: &Tenant) -> Result<(), Box<dyn Error>> {
        self.identity()?.set_disabled(&tenant.user, None)?;
        Ok(())
    }

    /// Trashes a tenant's artifact at metadata revision 1.
    fn trash_tenant(&self, tenant: &Tenant) -> Result<(), StoreError> {
        update_metadata(
            &self.store,
            &tenant.created,
            &key("trash-tenant"),
            1,
            None,
            None,
            Some(true),
        )
        .map(|_| ())
    }

    /// Purges a tenant's artifact, which is trashed at metadata revision 2.
    fn purge_tenant(&self, tenant: &Tenant) -> Result<(), StoreError> {
        self.store
            .purge_artifact(PurgeRequest {
                request: RequestIdentity {
                    actor: &tenant.user,
                    key: &key("purge-tenant"),
                    fingerprint: RequestFingerprint(digest("purge-tenant")),
                },
                artifact: &tenant.created.artifact.id,
                base: MetadataRevision(2),
            })
            .map(|_| ())
    }
}

// --- trash (rule 8) and recovery ------------------------------------------

#[test]
fn trash_revokes_a_live_attempt_raises_the_fence_and_requests_cancel() -> TestResult {
    let fixture = Fixture::new()?;
    let running = fixture.enqueue("running", 0)?;
    let claimed = claim_one(&fixture.store, 1, 1)?;
    assert_eq!(claimed.job.id, running.id);
    let queued = fixture.enqueue("queued", 2)?;

    fixture.trash()?;

    let attempt = row_of(&fixture.store, attempt_id(1))?;
    assert_eq!(
        attempt.revoked,
        Some(Revocation::Trash),
        "trash revokes the live attempt"
    );
    assert_eq!(
        attempt.state,
        AttemptState::Live,
        "trash does not settle the attempt"
    );
    let held = fixture.stored(&running.id)?;
    assert_eq!(held.state, JobState::Running.as_str());
    assert!(held.cancel_requested, "trash requests a cancel");
    assert_eq!(held.fence, 2, "the claim gave fence 1 and trash raised it");
    let waiting = fixture.stored(&queued.id)?;
    assert_eq!(
        waiting.state,
        JobState::Queued.as_str(),
        "a queued job is untouched"
    );
    assert_eq!(waiting.fence, 0, "a queued job's fence is not raised");
    assert!(!waiting.cancel_requested, "a queued job is not cancelled");
    Ok(())
}

#[test]
fn trash_then_recover_at_claim_takes_a_queued_job() -> TestResult {
    let fixture = Fixture::new()?;
    let queued = fixture.enqueue("queued", 0)?;
    fixture.trash()?;
    let while_trashed = claim_as(&fixture.store, 1, 1)?;
    assert!(
        while_trashed.claimed.is_none(),
        "a job of a trashed artifact is not due"
    );
    fixture.recover()?;

    let claimed = claim_one(&fixture.store, 2, 1)?;
    assert_eq!(claimed.job.id, queued.id);
    assert_eq!(claimed.job.attempts, 1, "the first claim of the job");
    Ok(())
}

#[test]
fn trash_then_recover_at_claim_never_takes_an_unsettled_running_job() -> TestResult {
    let fixture = Fixture::new()?;
    let running = fixture.enqueue("running", 0)?;
    let claimed = claim_one(&fixture.store, 0, 1)?;
    assert_eq!(claimed.job.id, running.id);
    fixture.trash()?;
    fixture.recover()?;

    // The lease ended at LEASE_MS and a cancel is requested, but the attempt
    // is unsettled, so the job is not due and nothing is ended or reclaimed.
    let later = claim_as(&fixture.store, LEASE_MS + 1, 2)?;
    assert!(
        later.claimed.is_none(),
        "an unsettled attempt keeps its job out of the claim"
    );
    assert!(
        later.settled.is_empty(),
        "the job is not ended by the claim"
    );
    let held = fixture.stored(&running.id)?;
    assert_eq!(held.state, JobState::Running.as_str());
    assert_eq!(held.attempts, 1, "no second claim was made");
    assert_eq!(
        fixture.count("SELECT count(*) FROM execution_attempts")?,
        1,
        "exactly one attempt row"
    );
    assert_eq!(
        row_of(&fixture.store, attempt_id(1))?.state,
        AttemptState::Live,
        "the attempt is still unsettled"
    );
    Ok(())
}

#[test]
fn trash_then_recover_at_a_checkpoint_answers_trash() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.enqueue("running", 0)?;
    let claimed = claim_one(&fixture.store, 0, 1)?;
    fixture.trash()?;
    fixture.recover()?;

    let answer = fixture
        .store
        .checkpoint_attempt(&claimed.lease, &attempt_id(1), at(1), LEASE)?;
    assert_eq!(
        answer,
        CheckpointAnswer::Stop(StopReason::Trash),
        "the stored trash answers, not the fence and not the recovered artifact"
    );
    Ok(())
}

#[test]
fn trash_then_recover_at_publication_is_refused_and_settles_cancelled() -> TestResult {
    let fixture = Fixture::new()?;
    let running = fixture.enqueue("running", 0)?;
    let claimed = claim_one(&fixture.store, 0, 1)?;
    fixture.trash()?;
    fixture.recover()?;

    let refused = fixture.store.complete_profile_job(
        &claimed.lease,
        &attempt_id(1),
        ProfileBinding {
            profile: &review(),
            tool_digest: TOOL_DIGEST,
        },
        counts(),
        at(1),
    );
    assert_eq!(refused, Err(JobStoreError::Revoked(StopReason::Trash)));
    assert_eq!(
        fixture.stored(&running.id)?.state,
        JobState::Running.as_str(),
        "a refused publication leaves the job running until settlement"
    );

    let ended = settled_job(fixture.store.settle_attempt(&attempt_id(1), at(2))?)?;
    assert_eq!(ended.state, JobState::Cancelled);
    assert!(
        ended.finished_at.is_some(),
        "the settlement ends the job with a finish time"
    );
    assert_eq!(ended.worker, None, "a cancelled job has no worker");
    assert_eq!(ended.lease_expires_at, None, "a cancelled job has no lease");
    let again = fixture.store.complete_profile_job(
        &claimed.lease,
        &attempt_id(1),
        ProfileBinding {
            profile: &review(),
            tool_digest: TOOL_DIGEST,
        },
        counts(),
        at(3),
    );
    assert_eq!(again, Err(JobStoreError::Revoked(StopReason::Trash)));
    assert_eq!(
        fixture.stored(&running.id)?.state,
        JobState::Cancelled.as_str()
    );
    Ok(())
}

// --- disable and enable (rules 5 and 9) ------------------------------------

#[test]
fn disable_and_enable_before_a_checkpoint_answers_generation_then_settles_cancelled() -> TestResult
{
    let fixture = Fixture::new()?;
    let tenant = fixture.tenant(1)?;
    let first = fixture.enqueue_for(&tenant, "first", 0)?;
    let claimed = claim_one(&fixture.store, 1, 1)?;
    assert_eq!(claimed.job.id, first.id);
    assert_eq!(
        row_of(&fixture.store, attempt_id(1))?.actor_generation,
        StateGeneration::new(1),
        "the claim pins the registration generation"
    );
    fixture.disable(&tenant)?;
    fixture.enable(&tenant)?;

    let answer = fixture
        .store
        .checkpoint_attempt(&claimed.lease, &attempt_id(1), at(2), LEASE)?;
    assert_eq!(
        answer,
        CheckpointAnswer::Stop(StopReason::Generation),
        "disable and enable both moved the generation on"
    );
    assert_eq!(
        row_of(&fixture.store, attempt_id(1))?.revoked,
        Some(Revocation::Generation)
    );
    let ended = settled_job(fixture.store.settle_attempt(&attempt_id(1), at(3))?)?;
    assert_eq!(ended.state, JobState::Cancelled);

    let second = fixture.enqueue_for(&tenant, "second", 4)?;
    let next = claim_one(&fixture.store, 5, 2)?;
    assert_eq!(next.job.id, second.id, "a new keyed job is claimed");
    assert_eq!(
        row_of(&fixture.store, attempt_id(2))?.actor_generation,
        StateGeneration::new(3),
        "the new claim pins the generation after enable"
    );
    assert_eq!(
        fixture
            .store
            .checkpoint_attempt(&next.lease, &attempt_id(2), at(6), LEASE)?,
        CheckpointAnswer::Continue,
        "the new attempt runs under the current generation"
    );
    Ok(())
}

#[test]
fn a_disabled_users_live_attempt_answers_generation() -> TestResult {
    let fixture = Fixture::new()?;
    let tenant = fixture.tenant(2)?;
    fixture.enqueue_for(&tenant, "job", 0)?;
    let claimed = claim_one(&fixture.store, 1, 1)?;
    fixture.disable(&tenant)?;

    let answer = fixture
        .store
        .checkpoint_attempt(&claimed.lease, &attempt_id(1), at(2), LEASE)?;
    assert_eq!(
        answer,
        CheckpointAnswer::Stop(StopReason::Generation),
        "a disabled user counts as a new generation"
    );
    Ok(())
}

/// Publication is refused for the generation after the user was disabled and
/// enabled again. The job ends cancelled, and it never succeeds.
fn publication_refused_by_generation(stage: Stage) -> TestResult {
    let fixture = Fixture::new()?;
    let tenant = fixture.tenant(3)?;
    let job = fixture.enqueue_for(&tenant, "job", 0)?;
    let claimed = claim_one(&fixture.store, 1, 1)?;
    assert_eq!(claimed.job.id, job.id);
    if stage == Stage::SettledUnrevoked {
        let settled = settled_job(fixture.store.settle_attempt(&attempt_id(1), at(2))?)?;
        assert_eq!(
            settled.state,
            JobState::Running,
            "an unrevoked settlement leaves the job to its worker"
        );
    }
    fixture.disable(&tenant)?;
    fixture.enable(&tenant)?;

    let refused = fixture.store.complete_profile_job(
        &claimed.lease,
        &attempt_id(1),
        ProfileBinding {
            profile: &review(),
            tool_digest: TOOL_DIGEST,
        },
        counts(),
        at(3),
    );
    assert_eq!(refused, Err(JobStoreError::Revoked(StopReason::Generation)));
    assert_eq!(
        row_of(&fixture.store, attempt_id(1))?.revoked,
        Some(Revocation::Generation),
        "the refusal stores its revocation"
    );
    if stage == Stage::Live {
        assert_eq!(
            fixture.stored(&job.id)?.state,
            JobState::Running.as_str(),
            "a live attempt waits for its settlement"
        );
        settled_job(fixture.store.settle_attempt(&attempt_id(1), at(4))?)?;
    }
    assert_eq!(
        fixture.stored(&job.id)?.state,
        JobState::Cancelled.as_str(),
        "the job ends cancelled, never succeeded"
    );
    Ok(())
}

#[test]
fn disable_and_enable_before_publication_refuses_a_live_attempt() -> TestResult {
    publication_refused_by_generation(Stage::Live)
}

#[test]
fn disable_and_enable_before_publication_cancels_a_settled_attempt_at_once() -> TestResult {
    publication_refused_by_generation(Stage::SettledUnrevoked)
}

// --- purge (rule 11) and the tombstone -------------------------------------

#[test]
fn a_purge_leaves_a_cleanup_pending_tombstone_until_settlement() -> TestResult {
    let fixture = Fixture::new()?;
    let running = fixture.enqueue("running", 0)?;
    let claimed = claim_one(&fixture.store, 1, 1)?;
    assert_eq!(claimed.job.id, running.id);
    let queued = fixture.enqueue("queued", 2)?;
    fixture.trash()?;

    fixture.purge()?;

    let tombstone = row_of(&fixture.store, attempt_id(1))?;
    assert_eq!(
        tombstone.state,
        AttemptState::CleanupPending,
        "the purge leaves the attempt as the cleanup tombstone"
    );
    assert_eq!(
        tombstone.revoked,
        Some(Revocation::Trash),
        "the purge keeps the first revocation"
    );
    let held = fixture.stored(&running.id)?;
    assert_eq!(
        held.state,
        JobState::Running.as_str(),
        "the job waits for its attempt"
    );
    assert_eq!(
        held.fence, 3,
        "the claim gave 1, trash raised it, purge did"
    );
    assert_eq!(
        fixture.stored(&queued.id)?.state,
        JobState::Cancelled.as_str(),
        "a queued job of the purged artifact is cancelled"
    );
    assert_eq!(
        fixture
            .store
            .checkpoint_attempt(&claimed.lease, &attempt_id(1), at(3), LEASE)?,
        CheckpointAnswer::Stop(StopReason::Trash),
        "the stored revocation answers the checkpoint"
    );
    assert!(
        !has_foreign_key_violation(&fixture.sql()?)?,
        "the attempt's job reference is kept by the purge"
    );

    let ended = settled_job(fixture.store.settle_attempt(&attempt_id(1), at(4))?)?;
    assert_eq!(ended.state, JobState::Cancelled);
    assert_eq!(
        row_of(&fixture.store, attempt_id(1))?.state,
        AttemptState::Settled,
        "the attempt row is kept after its settlement"
    );
    Ok(())
}

#[test]
fn a_purge_records_purge_on_an_attempt_that_was_not_revoked() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.enqueue("running", 0)?;
    claim_one(&fixture.store, 0, 1)?;
    fixture.trash()?;
    // Trash revokes a live attempt, so this clears the revocation to reach the
    // purge's own revocation (decision 0057 rule 11).
    fixture
        .sql()?
        .execute("UPDATE execution_attempts SET revoked = NULL", [])?;

    fixture.purge()?;

    let row = row_of(&fixture.store, attempt_id(1))?;
    assert_eq!(row.revoked, Some(Revocation::Purge), "the purge revokes it");
    assert_eq!(row.state, AttemptState::CleanupPending);
    Ok(())
}

#[test]
fn a_purge_after_the_trash_ended_a_settled_attempt_changes_nothing() -> TestResult {
    let fixture = Fixture::new()?;
    let running = fixture.enqueue("running", 0)?;
    claim_one(&fixture.store, 0, 1)?;
    settled_job(fixture.store.settle_attempt(&attempt_id(1), at(1))?)?;
    assert_eq!(
        fixture.stored(&running.id)?.state,
        JobState::Running.as_str(),
        "an unrevoked settlement leaves the job running"
    );
    fixture.trash()?;
    let before = row_of(&fixture.store, attempt_id(1))?;
    assert_eq!(
        fixture.stored(&running.id)?.state,
        JobState::Cancelled.as_str(),
        "the trash ended the job of the settled attempt"
    );

    fixture.purge()?;

    let ended = fixture.stored(&running.id)?;
    assert_eq!(ended.state, JobState::Cancelled.as_str());
    assert_eq!(ended.fence, 2, "the claim gave 1 and the trash raised it");
    assert_eq!(
        row_of(&fixture.store, attempt_id(1))?,
        before,
        "the settled attempt is not touched by the purge"
    );
    assert_eq!(before.state, AttemptState::Settled);
    assert_eq!(before.revoked, Some(Revocation::Trash));
    Ok(())
}

#[test]
fn a_purge_erases_run_profile_counts_and_keeps_the_job_header() -> TestResult {
    let fixture = Fixture::new()?;
    let finished = fixture.enqueue("finished", 0)?;
    let claimed = claim_one(&fixture.store, 0, 1)?;
    settled_job(fixture.store.settle_attempt(&attempt_id(1), at(1))?)?;
    let done = fixture.store.complete_profile_job(
        &claimed.lease,
        &attempt_id(1),
        ProfileBinding {
            profile: &review(),
            tool_digest: TOOL_DIGEST,
        },
        counts(),
        at(2),
    )?;
    assert_eq!(done.state, JobState::Succeeded);
    let counted = fixture.stored(&finished.id)?;
    assert_eq!(
        (counted.files_checked, counted.bytes_checked),
        (Some(3), Some(42)),
        "publication stores the two counts"
    );

    fixture.trash()?;
    fixture.purge()?;

    let erased = fixture.stored(&finished.id)?;
    assert_eq!(
        erased.state,
        JobState::Succeeded.as_str(),
        "the purge keeps the job header"
    );
    assert_eq!(
        (erased.files_checked, erased.bytes_checked),
        (None, None),
        "the purge erases the counts"
    );
    assert_eq!(
        row_of(&fixture.store, attempt_id(1))?.state,
        AttemptState::Settled
    );
    Ok(())
}

#[test]
fn the_tombstone_survives_a_purge_a_disable_and_a_backup_restore() -> TestResult {
    let fixture = Fixture::new()?;
    let tenant = fixture.tenant(4)?;
    fixture.enqueue_for(&tenant, "job", 0)?;
    let claimed = claim_one(&fixture.store, 1, 1)?;
    assert_eq!(claimed.job.attempts, 1);
    fixture.trash_tenant(&tenant)?;
    fixture.purge_tenant(&tenant)?;
    let purged = row_of(&fixture.store, attempt_id(1))?;
    assert_eq!(purged.state, AttemptState::CleanupPending);
    assert_eq!(purged.revoked, Some(Revocation::Trash));

    fixture.disable(&tenant)?;
    assert_eq!(
        row_of(&fixture.store, attempt_id(1))?,
        purged,
        "disabling the user leaves the tombstone as it was"
    );

    let out = TempDir::new();
    let output = out.join("backup.sqlite3");
    let target = out.join("restored");
    backup(fixture.dir.path(), &output)?;
    restore(&output, &target)?;
    let restored = SqliteStore::open(&database_in(&target))?;

    let kept = row_of(&restored, attempt_id(1))?;
    assert_eq!(kept, purged, "the restored tombstone is the same row");
    assert_eq!(
        kept.actor_generation,
        StateGeneration::new(1),
        "the pinned generation is not adopted anew from the current user"
    );
    assert_eq!(
        restored.unsettled_attempts()?,
        std::slice::from_ref(&kept),
        "the restored tombstone is still unsettled"
    );
    assert!(
        claim_as(&restored, 10, 2)?.claimed.is_none(),
        "the restored tombstone keeps its job out of the claim"
    );
    assert_eq!(
        row_of(&fixture.store, attempt_id(1))?,
        purged,
        "the original database keeps the tombstone"
    );
    Ok(())
}

/// A `lease` revocation (a restore) on an unsettled attempt would retry its
/// job at settlement. Trash and purge replace it, so the job still ends
/// cancelled (decision 0057 rules 8 and 11).
#[test]
fn trash_and_purge_replace_a_lease_revocation_so_the_job_ends_cancelled() -> TestResult {
    let fixture = Fixture::new()?;
    let running = fixture.enqueue("running", 0)?;
    let claimed = claim_one(&fixture.store, 1, 1)?;
    assert_eq!(claimed.job.id, running.id);
    let restored = "UPDATE execution_attempts SET state = 'cleanup_pending', revoked = 'lease'";
    fixture.sql()?.execute(restored, [])?;

    fixture.trash()?;
    assert_eq!(
        row_of(&fixture.store, attempt_id(1))?.revoked,
        Some(Revocation::Trash),
        "trash replaces the lease revocation"
    );

    fixture.sql()?.execute(restored, [])?;
    fixture.purge()?;
    let tombstone = row_of(&fixture.store, attempt_id(1))?;
    assert_eq!(
        tombstone.revoked,
        Some(Revocation::Purge),
        "purge replaces the lease revocation"
    );
    assert_eq!(tombstone.state, AttemptState::CleanupPending);
    assert_eq!(
        fixture.stored(&running.id)?.state,
        JobState::Running.as_str(),
        "the purged job waits for its settlement"
    );

    let settled = settled_job(fixture.store.settle_attempt(&attempt_id(1), at(5))?)?;
    assert_eq!(
        settled.state,
        JobState::Cancelled,
        "the purged job ends cancelled, not scheduled again"
    );
    Ok(())
}

/// An attempt that settled but has not published yet is revoked by the trash,
/// and its job ends cancelled at once: neither its publication nor a reclaim
/// after the recovery runs it (decision 0057 rule 8).
#[test]
fn trash_ends_a_job_whose_attempt_settled_before_its_publication() -> TestResult {
    let fixture = Fixture::new()?;
    let running = fixture.enqueue("running", 0)?;
    let claimed = claim_one(&fixture.store, 1, 1)?;
    assert_eq!(claimed.job.id, running.id);
    let settled = settled_job(fixture.store.settle_attempt(&attempt_id(1), at(2))?)?;
    assert_eq!(settled.state, JobState::Running);

    fixture.trash()?;
    assert_eq!(
        row_of(&fixture.store, attempt_id(1))?.revoked,
        Some(Revocation::Trash),
        "trash revokes the settled attempt"
    );
    assert_eq!(
        fixture.stored(&running.id)?.state,
        JobState::Cancelled.as_str(),
        "the job ends cancelled at once"
    );
    let refused = fixture.store.complete_profile_job(
        &claimed.lease,
        &attempt_id(1),
        ProfileBinding {
            profile: &review(),
            tool_digest: TOOL_DIGEST,
        },
        counts(),
        at(3),
    );
    assert!(
        refused.is_err(),
        "the trashed attempt cannot publish: {refused:?}"
    );

    fixture.recover()?;
    assert!(
        claim_as(&fixture.store, 5_000, 2)?.claimed.is_none(),
        "the recovered artifact's cancelled job is not claimed again"
    );
    assert_eq!(
        fixture.stored(&running.id)?.state,
        JobState::Cancelled.as_str()
    );
    Ok(())
}
