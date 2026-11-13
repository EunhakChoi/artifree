//! Behavioral contract of `run_profile` jobs in the `SQLite` store (decision 0057,
//! rule 3): they are hidden from every read, write no `job_changed` row, do not
//! count toward the open-job bound of 16, and cannot be cancelled. The
//! `verify_revision` jobs that share the store keep their behavior beside them
//! (decision 0042, rule 6).
//!
//! Time is a number each call is given, so every test moves it by hand.

mod common;

use std::error::Error;

use artifree_domain::change::ChangeSequence;
use artifree_domain::identity::{ActorId, ArtifactId, UnixMillis};
use artifree_domain::job::{
    AttemptIdentity, CheckpointAnswer, Finding, JobFailure, JobId, JobKind, JobState,
    MAX_ACTIVE_JOBS_PER_ACTOR, Problem, ProfileName, Revocation, VerifyReport, WorkerId,
};
use artifree_domain::{MetadataRevision, SourceRevision};
use artifree_ports::artifact_store::{
    ArtifactStore, ChangeKind, ChangeRecord, CommitReceipt, PurgeRequest, RequestFingerprint,
    RequestIdentity, StoreError,
};
use artifree_ports::job_store::{
    Checkpoint, Claim, ClaimedJob, ExecutionClaim, JobCancel, JobClaim, JobListing, JobReceipt,
    JobRecord, JobStore, JobStoreError, NewJob, NewProfileJob, ProfileBinding, ProfileCounts,
    SettleOutcome,
};
use artifree_providers::storage::SqliteStore;
use common::{
    TempDir, TestResult, actor, create_demo, database_in, digest, key, parsed, revise,
    update_metadata,
};
use rusqlite::Connection;

const FIRST: &str = "<h1>first</h1>";
const SECOND: &str = "<h1>second</h1>";

/// The lease that a claim takes, in milliseconds.
const LEASE_MS: u64 = 1_000;

/// A tool root digest in the form the store checks: 64 lowercase hexadecimal
/// digits.
const TOOL_DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// An execution directory identity in the form the store checks: 32 lowercase
/// hexadecimal digits.
const DIRECTORY: &str = "0123456789abcdef0123456789abcdef";

/// Refuses every write of a `job_changed` row, as a change table that cannot
/// record a job would.
const REFUSE_JOB_CHANGES: &str = "CREATE TRIGGER refuse_job_changes BEFORE INSERT ON changes
    WHEN new.kind = 'job_changed' BEGIN SELECT RAISE(ABORT, 'injected'); END;";

fn at(ms: i64) -> UnixMillis {
    UnixMillis(ms)
}

fn worker(name: &str) -> WorkerId {
    parsed(&format!("worker-{name}"))
}

/// The profile that the `run_profile` jobs of these tests run.
fn fixture_profile() -> Result<ProfileName, Box<dyn Error>> {
    let profile: ProfileName = "fixture".parse()?;
    Ok(profile)
}

/// An attempt identity whose bytes are `seed`, so that each test names its
/// attempts apart.
fn attempt_identity(seed: u8) -> AttemptIdentity {
    AttemptIdentity::from_random([seed; 16])
}

/// The execution identities that a claim records for `attempt`.
fn execution(attempt: AttemptIdentity) -> ExecutionClaim<'static> {
    ExecutionClaim {
        attempt,
        tool_digest: TOOL_DIGEST,
        machine: "machine-a",
        boot: "boot-a",
        cgroupns: "cgroup-a",
        directory: DIRECTORY,
    }
}

/// One claim by the worker `name` at `now`. It offers the profiles `known`;
/// with `attempt`, it also carries the worker's execution identities.
fn claim(
    store: &SqliteStore,
    name: &str,
    now: i64,
    known: &[ProfileName],
    attempt: Option<AttemptIdentity>,
) -> Result<Claim, JobStoreError> {
    store.claim_job(JobClaim {
        worker: &worker(name),
        now: at(now),
        lease_ms: LEASE_MS,
        known_profiles: known,
        execution: attempt.map(execution),
    })
}

/// The job that one claim takes, with its lease. A claim that takes nothing
/// fails.
fn claimed_job(
    store: &SqliteStore,
    name: &str,
    now: i64,
    known: &[ProfileName],
    attempt: Option<AttemptIdentity>,
) -> Result<ClaimedJob, Box<dyn Error>> {
    Ok(claim(store, name, now, known, attempt)?
        .claimed
        .ok_or("no job was due")?)
}

/// The jobs of the owner that `list_jobs` returns, newest first, for `artifact`
/// or for every artifact.
fn listing_of(
    fixture: &Fixture,
    artifact: Option<&ArtifactId>,
    limit: usize,
) -> Result<Vec<JobRecord>, JobStoreError> {
    fixture.store.list_jobs(JobListing {
        actor: &fixture.owner,
        artifact,
        after: None,
        limit,
    })
}

fn job_ids(jobs: &[JobRecord]) -> Vec<JobId> {
    jobs.iter().map(|job| job.id.clone()).collect()
}

/// The position of the owner's feed: its head, read from a feed of one row.
fn head(store: &SqliteStore, who: &ActorId) -> Result<ChangeSequence, StoreError> {
    Ok(store.changes(who, ChangeSequence(0), 1)?.head)
}

/// The rows of `who`'s feed after `mark`.
fn rows_after(
    store: &SqliteStore,
    who: &ActorId,
    mark: ChangeSequence,
) -> Result<Vec<ChangeRecord>, StoreError> {
    Ok(store.changes(who, mark, 100)?.changes)
}

/// The one item of `found`; `what` names the step in the failure message.
fn only<T>(found: Vec<T>, what: &str) -> Result<T, Box<dyn Error>> {
    let [item] = <[T; 1]>::try_from(found)
        .map_err(|unexpected| format!("{what}: expected one item, got {}", unexpected.len()))?;
    Ok(item)
}

/// The error of a request that must be refused; `what` names the request.
fn refusal<T>(
    result: Result<T, JobStoreError>,
    what: &str,
) -> Result<JobStoreError, Box<dyn Error>> {
    match result {
        Err(error) => Ok(error),
        Ok(_) => Err(format!("{what} was accepted, but it must be refused").into()),
    }
}

/// Checks that `row` is a `job_changed` row that names `job` and `artifact`.
fn assert_job_row(row: &ChangeRecord, job: &JobId, artifact: &ArtifactId) {
    assert_eq!(
        row.kind,
        ChangeKind::JobChanged,
        "a job transition appends a job_changed row"
    );
    assert_eq!(
        row.job_id.as_ref(),
        Some(job),
        "the row must name the job that changed"
    );
    assert_eq!(
        &row.artifact, artifact,
        "the row must name the job's artifact"
    );
}

/// A report of two files with one digest mismatch.
fn report_with_a_finding() -> VerifyReport {
    VerifyReport {
        files_checked: 2,
        bytes_checked: 20,
        mismatch_count: 1,
        findings: vec![Finding {
            path: parsed("index.html"),
            problem: Problem::DigestMismatch,
        }],
    }
}

struct Fixture {
    dir: TempDir,
    store: SqliteStore,
    owner: ActorId,
    created: CommitReceipt,
}

impl Fixture {
    /// An artifact of the owner with two revisions, the keys `create` and `save`.
    fn new() -> Result<Self, Box<dyn Error>> {
        let dir = TempDir::new();
        let store = SqliteStore::open(&database_in(dir.path()))?;
        let owner = actor("owner");
        let created = create_demo(&store, &owner, &key("create"), "Secret", FIRST)?;
        revise(&store, &created, &key("save"), 1, SECOND)?;
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

    /// A connection that enforces foreign keys, as the store's own connections do.
    fn sql(&self) -> Result<Connection, rusqlite::Error> {
        let connection = Connection::open(database_in(self.dir.path()))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        Ok(connection)
    }

    /// The integer that `query` returns, for a query that takes no parameter.
    fn count(&self, query: &str) -> Result<i64, Box<dyn Error>> {
        Ok(self.sql()?.query_row(query, [], |row| row.get(0))?)
    }

    /// The first column of the one row that `query` returns; `query` takes
    /// `param` as `?1`.
    fn read<T: rusqlite::types::FromSql>(
        &self,
        query: &str,
        param: &str,
    ) -> Result<T, Box<dyn Error>> {
        Ok(self.sql()?.query_row(query, [param], |row| row.get(0))?)
    }

    /// Enqueues a verification of the owner's artifact at `revision`, under the
    /// key `name`.
    fn enqueue(&self, name: &str, revision: u64, now: i64) -> Result<JobReceipt, JobStoreError> {
        let request_key = key(name);
        self.store.enqueue_job(NewJob {
            request: RequestIdentity {
                actor: &self.owner,
                key: &request_key,
                fingerprint: RequestFingerprint(digest(&format!(
                    "enqueue {} {revision}",
                    self.id()
                ))),
            },
            artifact: self.id(),
            revision: SourceRevision(revision),
            kind: JobKind::VerifyRevision,
            now: at(now),
        })
    }

    /// Enqueues a verification at revision 2, the owner's latest revision.
    fn queued(&self, name: &str, now: i64) -> Result<JobRecord, Box<dyn Error>> {
        Ok(self.enqueue(name, 2, now)?.job)
    }

    /// Enqueues a `run_profile` job of the owner's artifact at `revision`, which
    /// runs `profile`, under the key `name`.
    fn enqueue_profile(
        &self,
        name: &str,
        revision: u64,
        profile: &ProfileName,
        now: i64,
    ) -> Result<JobReceipt, JobStoreError> {
        let request_key = key(name);
        self.store.enqueue_profile_job(NewProfileJob {
            request: RequestIdentity {
                actor: &self.owner,
                key: &request_key,
                fingerprint: RequestFingerprint(digest(&format!(
                    "profile {} {revision} {profile}",
                    self.id()
                ))),
            },
            artifact: self.id(),
            revision: SourceRevision(revision),
            profile,
            now: at(now),
        })
    }

    /// Asks to cancel `job` for the owner, under the key `name`.
    fn cancel(&self, name: &str, job: &JobId, now: i64) -> Result<JobReceipt, JobStoreError> {
        let request_key = key(name);
        self.store.cancel_job(JobCancel {
            request: RequestIdentity {
                actor: &self.owner,
                key: &request_key,
                fingerprint: RequestFingerprint(digest(&format!("cancel {job}"))),
            },
            job,
            now: at(now),
        })
    }

    /// Moves the artifact to the trash, which makes its metadata revision 2.
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

    /// Purges the trashed artifact, from its metadata revision 2.
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
}

#[test]
fn run_profile_rows_are_hidden_from_job_and_list() -> TestResult {
    let fixture = Fixture::new()?;
    let profile = fixture_profile()?;
    let run = fixture.enqueue_profile("run", 2, &profile, 1_000)?.job;
    let verify = fixture.queued("verify", 1_001)?;
    assert_eq!(
        run.kind,
        JobKind::RunProfile,
        "the enqueue stores a run_profile job"
    );
    assert_eq!(
        verify.kind,
        JobKind::VerifyRevision,
        "the enqueue stores a verify job"
    );

    assert_eq!(
        fixture.store.job(&run.id)?,
        None,
        "a run_profile job is not readable by its identifier"
    );
    assert_eq!(
        fixture.store.job(&verify.id)?,
        Some(verify.clone()),
        "a verify job is readable by its identifier"
    );

    let everything = listing_of(&fixture, None, 10)?;
    assert_eq!(
        job_ids(&everything),
        vec![verify.id.clone()],
        "the listing of the owner leaves out the run_profile job"
    );
    let of_artifact = listing_of(&fixture, Some(fixture.id()), 10)?;
    assert_eq!(
        job_ids(&of_artifact),
        vec![verify.id.clone()],
        "the listing of the artifact leaves out the run_profile job"
    );
    Ok(())
}

#[test]
fn run_profile_transitions_append_no_change_row() -> TestResult {
    let fixture = Fixture::new()?;
    // Every job_changed insert fails from here on, so a row written for a
    // run_profile transition would fail that transition.
    fixture.sql()?.execute_batch(REFUSE_JOB_CHANGES)?;
    let store = &fixture.store;
    let profile = fixture_profile()?;
    let other: ProfileName = "other".parse()?;
    let attempt = attempt_identity(0x31);
    let mark = head(store, &fixture.owner)?;
    let sequence = fixture.count("SELECT max(sequence) FROM changes")?;

    let run = fixture.enqueue_profile("run", 2, &profile, 1_000)?.job;
    let claimed = claimed_job(
        store,
        "a",
        1_000,
        std::slice::from_ref(&profile),
        Some(attempt),
    )?;
    assert_eq!(claimed.job.id, run.id, "the run_profile job is claimed");
    let lease = claimed.lease;
    assert_eq!(
        store.checkpoint_attempt(&lease, &attempt, at(1_100), LEASE_MS)?,
        CheckpointAnswer::Continue,
        "a checkpoint of a live attempt continues"
    );
    assert!(
        matches!(
            store.settle_attempt(&attempt, at(1_200))?,
            SettleOutcome::Settled(_)
        ),
        "the first settlement of a live attempt settles it"
    );
    let done = store.complete_profile_job(
        &lease,
        &attempt,
        ProfileBinding {
            profile: &fixture_profile()?,
            tool_digest: TOOL_DIGEST,
        },
        ProfileCounts {
            files: 2,
            bytes: 20,
        },
        at(1_300),
    )?;
    assert_eq!(done.state, JobState::Succeeded, "the attempt completes");

    let unknown = fixture.enqueue_profile("unknown", 2, &other, 1_400)?.job;
    let ended = claim(store, "b", 1_500, &[], None)?;
    assert!(
        ended.claimed.is_none(),
        "a claim that ends a job takes none"
    );
    let failed = only(ended.settled, "the claim with no known profile")?;
    assert_eq!(failed.id, unknown.id, "the unknown job is the one ended");
    assert_eq!(failed.state, JobState::Failed);
    assert_eq!(failed.failure, Some(JobFailure::ExecutionUnavailable));

    assert_eq!(
        fixture.count("SELECT count(*) FROM changes WHERE kind = 'job_changed'")?,
        0,
        "no run_profile transition appends a job_changed row"
    );
    assert_eq!(
        fixture.count("SELECT max(sequence) FROM changes")?,
        sequence,
        "the change table's head does not move"
    );
    assert_eq!(
        head(store, &fixture.owner)?,
        mark,
        "the owner's feed head does not move"
    );

    let error = refusal(
        fixture.enqueue("probe", 2, 2_000),
        "a verify job, whose change row the trigger refuses",
    )?;
    assert!(
        matches!(error, JobStoreError::Unavailable(_)),
        "the trigger is active, so a verify job is refused: got {error}"
    );
    Ok(())
}

#[test]
fn sixteen_open_run_profile_jobs_do_not_count_toward_the_bound() -> TestResult {
    let fixture = Fixture::new()?;
    let profile = fixture_profile()?;
    for index in 0..MAX_ACTIVE_JOBS_PER_ACTOR {
        let now = 1_000 + i64::try_from(index)?;
        fixture.enqueue_profile(&format!("run-{index}"), 2, &profile, now)?;
    }
    for index in 0..MAX_ACTIVE_JOBS_PER_ACTOR {
        let now = 2_000 + i64::try_from(index)?;
        fixture.enqueue(&format!("verify-{index}"), 2, now)?;
    }

    let open = listing_of(&fixture, None, 100)?;
    assert_eq!(
        u64::try_from(open.len())?,
        MAX_ACTIVE_JOBS_PER_ACTOR,
        "the sixteen verify jobs are listed, and no run_profile job"
    );
    assert!(
        open.iter().all(|job| job.kind == JobKind::VerifyRevision),
        "the listing holds verify jobs only"
    );

    let error = refusal(
        fixture.enqueue("verify-over", 2, 3_000),
        "a seventeenth verify job",
    )?;
    assert_eq!(
        error,
        JobStoreError::LimitReached,
        "the bound of verify jobs is still sixteen"
    );
    fixture.enqueue_profile("run-over", 2, &profile, 3_001)?;
    Ok(())
}

#[test]
fn nobody_can_cancel_a_run_profile_job() -> TestResult {
    let fixture = Fixture::new()?;
    let profile = fixture_profile()?;
    let run = fixture.enqueue_profile("run", 2, &profile, 1_000)?.job;

    let error = refusal(
        fixture.cancel("cancel-r", &run.id, 1_100),
        "a cancel of a run_profile job",
    )?;
    assert_eq!(
        error,
        JobStoreError::NotFound,
        "a run_profile job cancels as not found, as an unknown job does"
    );
    let recorded_run: i64 = fixture.read(
        "SELECT count(*) FROM job_requests WHERE request_key = ?1",
        "test-key-cancel-r",
    )?;
    assert_eq!(recorded_run, 0, "the refused cancel records no request key");
    let state: String = fixture.read("SELECT state FROM jobs WHERE id = ?1", run.id.as_str())?;
    assert_eq!(state, "queued", "the run_profile job is still queued");

    // The same query finds the key of a cancel that succeeds, so the zero above
    // is not a query that finds nothing.
    let verify = fixture.queued("verify", 1_200)?;
    fixture.cancel("cancel-v", &verify.id, 1_300)?;
    let recorded_verify: i64 = fixture.read(
        "SELECT count(*) FROM job_requests WHERE request_key = ?1",
        "test-key-cancel-v",
    )?;
    assert_eq!(recorded_verify, 1, "an accepted cancel records its key");
    Ok(())
}

/// Enqueues, claims, heartbeats and completes one verify job while a
/// `run_profile` attempt is live. Each transition appends one `job_changed` row
/// that names the job. Returns the completed job.
fn complete_verify_job(fixture: &Fixture) -> Result<JobRecord, Box<dyn Error>> {
    let store = &fixture.store;
    // The enqueue, the claim and the completion of a verify job each append one
    // job_changed row that names it.
    let mark = head(store, &fixture.owner)?;
    let verify = fixture.queued("verify-v", 1_100)?;
    assert_job_row(
        &only(
            rows_after(store, &fixture.owner, mark)?,
            "the enqueue of the verify job",
        )?,
        &verify.id,
        fixture.id(),
    );

    let mark = head(store, &fixture.owner)?;
    let claimed_verify = claimed_job(store, "b", 1_200, &[], None)?;
    assert_eq!(
        claimed_verify.job.id, verify.id,
        "a waiting verify job is claimed while a run_profile attempt is live"
    );
    assert_job_row(
        &only(
            rows_after(store, &fixture.owner, mark)?,
            "the claim of the verify job",
        )?,
        &verify.id,
        fixture.id(),
    );

    assert_eq!(
        store.heartbeat_job(&claimed_verify.lease, at(1_300), LEASE_MS)?,
        Checkpoint::Continue,
        "a heartbeat of a verify job continues"
    );
    let mark = head(store, &fixture.owner)?;
    let done = store.complete_job(&claimed_verify.lease, &report_with_a_finding(), at(1_400))?;
    assert_eq!(done.state, JobState::Succeeded, "the verify job succeeds");
    assert_eq!(
        done.report,
        Some(report_with_a_finding()),
        "the completed verify job keeps its finding"
    );
    assert_job_row(
        &only(
            rows_after(store, &fixture.owner, mark)?,
            "the completion of the verify job",
        )?,
        &verify.id,
        fixture.id(),
    );
    Ok(done)
}

/// Trashes a verify job while it runs, then purges the artifact. The job keeps
/// running through the trash, and the trash revokes the `run_profile` attempt
/// and nothing of the job. The purge cancels the job and erases the finding of
/// `done`, the verify job that `complete_verify_job` completed.
fn trash_then_purge(fixture: &Fixture, attempt: AttemptIdentity, done: &JobRecord) -> TestResult {
    let store = &fixture.store;
    let waiting = fixture.queued("verify-w", 1_500)?;
    let claimed_waiting = claimed_job(store, "c", 1_600, &[], None)?;
    assert_eq!(
        claimed_waiting.job.id, waiting.id,
        "the second verify job is claimed"
    );
    assert_eq!(
        claimed_waiting.lease.fence, 1,
        "the first claim of a job has fence 1"
    );
    fixture.trash()?;
    assert_eq!(
        store.heartbeat_job(&claimed_waiting.lease, at(1_700), LEASE_MS)?,
        Checkpoint::Continue,
        "a trashed verify job keeps running (decision 0030, continue-and-hide)"
    );
    let trashed = store
        .job(&waiting.id)?
        .ok_or("the trashed verify job is missing")?;
    assert_eq!(
        trashed.state,
        JobState::Running,
        "the trash leaves it running"
    );
    assert!(
        !trashed.cancel_requested,
        "the trash does not request a cancel of a verify job"
    );
    let revoked = store
        .attempt_row(&attempt)?
        .ok_or("the run_profile attempt is missing")?;
    assert_eq!(
        revoked.revoked,
        Some(Revocation::Trash),
        "the trash revokes the run_profile attempt"
    );

    // The purge cancels the running verify job and raises its fence, and erases
    // the finding of the completed one.
    assert_eq!(
        fixture.read::<i64>(
            "SELECT count(*) FROM job_findings WHERE job_id = ?1",
            done.id.as_str()
        )?,
        1,
        "the completed job has its finding before the purge"
    );
    fixture.purge()?;
    let purged_waiting = store
        .job(&waiting.id)?
        .ok_or("the purged verify job is missing")?;
    assert_eq!(
        purged_waiting.state,
        JobState::Cancelled,
        "a purge cancels a running verify job"
    );
    assert_eq!(
        fixture.read::<i64>("SELECT fence FROM jobs WHERE id = ?1", waiting.id.as_str())?,
        2,
        "a purge raises the fence of the job it cancels"
    );
    let error = refusal(
        store.heartbeat_job(&claimed_waiting.lease, at(1_800), LEASE_MS),
        "a heartbeat of the purged verify job",
    )?;
    assert_eq!(
        error,
        JobStoreError::LeaseLost,
        "the lease of a purged verify job is lost"
    );
    let purged_done = store
        .job(&done.id)?
        .ok_or("the purged completed job is missing")?;
    assert_eq!(
        purged_done.state,
        JobState::Succeeded,
        "a purge leaves a succeeded job succeeded"
    );
    assert_eq!(
        purged_done.report, None,
        "a purge erases the counts of a succeeded job"
    );
    assert_eq!(
        fixture.read::<i64>(
            "SELECT count(*) FROM job_findings WHERE job_id = ?1",
            done.id.as_str()
        )?,
        0,
        "a purge erases the findings of a succeeded job"
    );
    Ok(())
}

#[test]
fn verify_revision_is_unchanged_with_run_profile_rows_present() -> TestResult {
    let fixture = Fixture::new()?;
    let profile = fixture_profile()?;
    let attempt = attempt_identity(0x41);

    // A run_profile attempt stays live through the whole test.
    let run = fixture.enqueue_profile("run", 2, &profile, 1_000)?.job;
    let claimed_run = claimed_job(
        &fixture.store,
        "a",
        1_000,
        std::slice::from_ref(&profile),
        Some(attempt),
    )?;
    assert_eq!(claimed_run.job.id, run.id, "the run_profile job is claimed");

    let done = complete_verify_job(&fixture)?;
    trash_then_purge(&fixture, attempt, &done)
}
