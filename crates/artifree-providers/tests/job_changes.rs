//! Behavioral contract of the `job_changed` rows of the owner's feed (decision
//! 0055, rules 7 to 10): which job transitions append a row, what the row
//! names, what the trash and a purge leave out, and what the table refuses.
//!
//! Time is a number each call is given, so every test moves it by hand.

mod common;

use std::error::Error;

use artifree_domain::change::ChangeSequence;
use artifree_domain::identity::{ActorId, ArtifactId, UnixMillis};
use artifree_domain::job::{
    Finding, JobFailure, JobId, JobKind, JobState, Problem, VerifyReport, WorkerId,
};
use artifree_domain::{MetadataRevision, SourceRevision};
use artifree_ports::artifact_store::{
    ArtifactStore, ChangeKind, ChangeRecord, CommitReceipt, Delivery, PurgeRequest,
    RequestFingerprint, RequestIdentity, StoreError,
};
use artifree_ports::job_store::{
    Checkpoint, Claim, ClaimedJob, JobCancel, JobClaim, JobReceipt, JobRecord, JobStore,
    JobStoreError, Lease, NewJob, Retry,
};
use artifree_providers::storage::SqliteStore;
use common::{
    TempDir, TestResult, actor, create_demo, database_in, digest, key, parsed, revise,
    update_metadata,
};
use rusqlite::{Connection, params};

const FIRST: &str = "<h1>first</h1>";
const SECOND: &str = "<h1>second</h1>";
const THIRD: &str = "<h1>third</h1>";

/// The lease that a claim takes, in milliseconds.
const LEASE_MS: u64 = 1_000;

/// The moves that still happen in the trash. Their lease was taken before the
/// trash; claims and cancels do not reach a trashed artifact.
const TRASHED_MOVES: [Move; 5] = [
    Move::Complete,
    Move::RetryLater,
    Move::FailForGood,
    Move::Release,
    Move::ConfirmCancel,
];

/// Kinds that take no job, with the data version that a `data_changed` row needs.
const KINDS_WITHOUT_A_JOB: [(&str, Option<i64>); 7] = [
    ("created", None),
    ("revised", None),
    ("metadata_updated", None),
    ("deleted", None),
    ("recovered", None),
    ("purged", None),
    ("data_changed", Some(1)),
];

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

fn clean_report() -> VerifyReport {
    VerifyReport {
        files_checked: 2,
        bytes_checked: 20,
        mismatch_count: 0,
        findings: Vec::new(),
    }
}

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

/// Enqueues a verification of `artifact` at `revision` for `who`, under the key `name`.
fn enqueue_on(
    store: &SqliteStore,
    who: &ActorId,
    artifact: &ArtifactId,
    revision: u64,
    name: &str,
    now: i64,
) -> Result<JobReceipt, JobStoreError> {
    let request_key = key(name);
    store.enqueue_job(NewJob {
        request: RequestIdentity {
            actor: who,
            key: &request_key,
            fingerprint: RequestFingerprint(digest(&format!("enqueue {artifact} {revision}"))),
        },
        artifact,
        revision: SourceRevision(revision),
        kind: JobKind::VerifyRevision,
        now: at(now),
    })
}

/// Cancels `job` for `who`, under the key `name`.
fn cancel_as(
    store: &SqliteStore,
    who: &ActorId,
    name: &str,
    job: &JobId,
    now: i64,
) -> Result<JobReceipt, JobStoreError> {
    let request_key = key(name);
    store.cancel_job(JobCancel {
        request: RequestIdentity {
            actor: who,
            key: &request_key,
            fingerprint: RequestFingerprint(digest(&format!("cancel {job}"))),
        },
        job,
        now: at(now),
    })
}

/// One claim by the worker `name` at time `now`, with the lease of `LEASE_MS`.
fn claim_as(store: &SqliteStore, name: &str, now: i64) -> Result<Claim, JobStoreError> {
    store.claim_job(JobClaim {
        worker: &worker(name),
        now: at(now),
        lease_ms: LEASE_MS,
        known_profiles: &[],
        execution: None,
    })
}

/// The job that one claim takes, with its lease. A claim that takes nothing fails.
fn claimed(store: &SqliteStore, name: &str, now: i64) -> Result<ClaimedJob, Box<dyn Error>> {
    Ok(claim_as(store, name, now)?
        .claimed
        .ok_or("no job was due")?)
}

/// Queues a job of the owner's artifact and claims it at time `1_000`, so that
/// the job is running under the lease of the worker `a`.
fn running_job(fixture: &Fixture) -> Result<(JobId, Lease), Box<dyn Error>> {
    let queued = fixture.queued("job", 1_000)?;
    let claim = claimed(&fixture.store, "a", 1_000)?;
    if claim.job.id != queued.id {
        return Err("the claim took a job other than the one queued".into());
    }
    Ok((claim.job.id, claim.lease))
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

/// The one row of `found`; `what` names the operation in the failure message.
fn only(found: Vec<ChangeRecord>, what: &str) -> Result<ChangeRecord, Box<dyn Error>> {
    let [row] = <[ChangeRecord; 1]>::try_from(found)
        .map_err(|unexpected| format!("{what}: expected one row, got {}", unexpected.len()))?;
    Ok(row)
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

/// Checks that `row` is a `job_changed` row that names `job` and `artifact`
/// (rule 8).
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

/// Writes one change row of the owner's feed by hand, at revision 2 and
/// metadata revision 1 of `artifact`.
fn insert_change(
    connection: &Connection,
    artifact: &ArtifactId,
    actor_seq: i64,
    kind: &str,
    data_version: Option<i64>,
    job: Option<&str>,
) -> rusqlite::Result<usize> {
    connection.execute(
        "INSERT INTO changes (actor, actor_seq, artifact_id, kind, revision, metadata_revision,
             data_version, job_id, committed_at)
         VALUES ('owner', ?1, ?2, ?3, 2, 1, ?4, ?5, 1000)",
        params![actor_seq, artifact.as_str(), kind, data_version, job],
    )
}

/// Checks that `result` was refused by a CHECK constraint and by nothing else.
fn assert_check_violation(result: rusqlite::Result<usize>, what: &str) -> TestResult {
    match result {
        Err(rusqlite::Error::SqliteFailure(error, _))
            if error.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_CHECK =>
        {
            Ok(())
        }
        Err(error) => Err(format!(
            "{what}: refused by something other than a CHECK constraint: {error}"
        )
        .into()),
        Ok(_) => Err(format!("{what}: the insert was accepted").into()),
    }
}

struct Fixture {
    dir: TempDir,
    store: SqliteStore,
    owner: ActorId,
    created: CommitReceipt,
}

impl Fixture {
    /// An artifact of the owner at revision 2 and metadata revision 1.
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

    fn count(&self, query: &str) -> Result<i64, Box<dyn Error>> {
        let value: i64 = self.sql()?.query_row(query, [], |row| row.get(0))?;
        Ok(value)
    }

    /// Enqueues a job of the owner's artifact at `revision`, under the key `name`.
    fn enqueue(&self, name: &str, revision: u64, now: i64) -> Result<JobReceipt, JobStoreError> {
        enqueue_on(&self.store, &self.owner, self.id(), revision, name, now)
    }

    /// Enqueues a job at revision 2, the owner's latest revision.
    fn queued(&self, name: &str, now: i64) -> Result<JobRecord, Box<dyn Error>> {
        Ok(self.enqueue(name, 2, now)?.job)
    }

    fn job(&self, id: &JobId) -> Result<JobRecord, Box<dyn Error>> {
        Ok(self.store.job(id)?.ok_or("the job is missing")?)
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

/// A job transition that appends a row when it happens (decision 0055, rule 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Move {
    Claim,
    Complete,
    RetryLater,
    FailForGood,
    Release,
    ConfirmCancel,
    CancelQueued,
    CancelRunning,
}

impl Move {
    /// Every move, in the order the tests run them.
    const ALL: [Move; 8] = [
        Move::Claim,
        Move::Complete,
        Move::RetryLater,
        Move::FailForGood,
        Move::Release,
        Move::ConfirmCancel,
        Move::CancelQueued,
        Move::CancelRunning,
    ];

    /// The state that the job is in after the move.
    fn ends_in(self) -> JobState {
        match self {
            Self::Claim | Self::CancelRunning => JobState::Running,
            Self::Complete => JobState::Succeeded,
            Self::RetryLater => JobState::RetryScheduled,
            Self::FailForGood => JobState::Failed,
            Self::Release => JobState::Queued,
            Self::ConfirmCancel | Self::CancelQueued => JobState::Cancelled,
        }
    }

    /// Whether the cancel flag is set after the move.
    fn cancel_requested(self) -> bool {
        matches!(self, Self::ConfirmCancel | Self::CancelRunning)
    }

    /// Brings a fresh job of the owner's artifact to the state that the move
    /// starts from. Returns the job, and the lease of its claim for the moves
    /// that need one.
    fn arrange(self, fixture: &Fixture) -> Result<(JobId, Option<Lease>), Box<dyn Error>> {
        match self {
            Self::Claim | Self::CancelQueued => Ok((fixture.queued("job", 1_000)?.id, None)),
            Self::CancelRunning
            | Self::Complete
            | Self::RetryLater
            | Self::FailForGood
            | Self::Release
            | Self::ConfirmCancel => {
                let (job, lease) = running_job(fixture)?;
                if self == Self::ConfirmCancel {
                    cancel_as(
                        &fixture.store,
                        &fixture.owner,
                        "arrange-cancel",
                        &job,
                        1_500,
                    )?;
                }
                Ok((job, Some(lease)))
            }
        }
    }

    /// Makes the move on `job` at time `now`, holding `lease` when the move
    /// needs one.
    fn perform(
        self,
        fixture: &Fixture,
        job: &JobId,
        lease: Option<&Lease>,
        now: i64,
    ) -> Result<(), Box<dyn Error>> {
        let store = &fixture.store;
        let held = || lease.ok_or("this move needs the lease of its claim");
        match self {
            Self::Claim => {
                claimed(store, "b", now)?;
            }
            Self::CancelQueued | Self::CancelRunning => {
                cancel_as(store, &fixture.owner, "move-cancel", job, now)?;
            }
            Self::Complete => {
                store.complete_job(held()?, &clean_report(), at(now))?;
            }
            Self::RetryLater => {
                store.fail_job(
                    held()?,
                    JobFailure::StorageUnavailable,
                    Retry::At(at(now + 60_000)),
                    at(now),
                )?;
            }
            Self::FailForGood => {
                store.fail_job(held()?, JobFailure::Internal, Retry::Never, at(now))?;
            }
            Self::Release => {
                store.release_job(held()?, at(now))?;
            }
            Self::ConfirmCancel => {
                store.confirm_cancel(held()?, at(now))?;
            }
        }
        Ok(())
    }
}

#[test]
fn an_enqueue_appends_one_row_for_the_job_to_its_owners_feed() -> TestResult {
    let fixture = Fixture::new()?;
    let mark = head(&fixture.store, &fixture.owner)?;

    let receipt = fixture.enqueue("one", 2, 1_000)?;

    assert_eq!(receipt.delivery, Delivery::Committed);
    let row = only(
        rows_after(&fixture.store, &fixture.owner, mark)?,
        "the enqueue",
    )?;
    assert_job_row(&row, &receipt.job.id, fixture.id());
    assert_eq!(
        row.revision,
        SourceRevision(2),
        "the row names the artifact's current revision"
    );
    assert_eq!(
        row.metadata_revision,
        MetadataRevision(1),
        "the row names the artifact's current metadata revision"
    );

    let mark = head(&fixture.store, &fixture.owner)?;
    let replay = fixture.enqueue("one", 2, 9_000)?;
    assert_eq!(replay.delivery, Delivery::Replayed);
    assert!(
        rows_after(&fixture.store, &fixture.owner, mark)?.is_empty(),
        "a replayed enqueue must not append a row"
    );
    Ok(())
}

#[test]
fn every_transition_appends_one_row_that_names_its_job() -> TestResult {
    for step in Move::ALL {
        let fixture = Fixture::new()?;
        let (job, lease) = step.arrange(&fixture)?;
        let mark = head(&fixture.store, &fixture.owner)?;

        step.perform(&fixture, &job, lease.as_ref(), 1_500)?;

        let row = only(
            rows_after(&fixture.store, &fixture.owner, mark)?,
            &format!("{step:?}"),
        )?;
        assert_job_row(&row, &job, fixture.id());
        assert_eq!(
            row.revision,
            SourceRevision(2),
            "{step:?} names the artifact's current revision"
        );
        assert_eq!(
            row.metadata_revision,
            MetadataRevision(1),
            "{step:?} names the artifact's current metadata revision"
        );
        let record = fixture.job(&job)?;
        assert_eq!(
            record.state,
            step.ends_in(),
            "{step:?} ends the job in the wrong state"
        );
        assert_eq!(
            record.cancel_requested,
            step.cancel_requested(),
            "{step:?} sets the cancel flag wrongly"
        );
    }
    Ok(())
}

#[test]
fn a_heartbeat_appends_no_row() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.enqueue("one", 2, 1_000)?;
    let running = claimed(&fixture.store, "a", 1_000)?;
    let mark = head(&fixture.store, &fixture.owner)?;

    let checkpoint = fixture
        .store
        .heartbeat_job(&running.lease, at(1_500), LEASE_MS)?;

    assert_eq!(checkpoint, Checkpoint::Continue);
    assert_eq!(
        fixture.job(&running.job.id)?.lease_expires_at,
        Some(at(2_500)),
        "the heartbeat must renew the lease, so the test shows it ran"
    );
    assert!(
        rows_after(&fixture.store, &fixture.owner, mark)?.is_empty(),
        "a heartbeat must not append a row"
    );
    Ok(())
}

#[test]
fn a_second_cancel_and_a_replayed_cancel_of_a_running_job_append_no_row() -> TestResult {
    let fixture = Fixture::new()?;
    let job = fixture.queued("one", 1_000)?.id;
    claimed(&fixture.store, "a", 1_000)?;
    let mark = head(&fixture.store, &fixture.owner)?;

    let first = cancel_as(&fixture.store, &fixture.owner, "cancel-1", &job, 1_500)?;

    assert_eq!(first.delivery, Delivery::Committed);
    let row = only(
        rows_after(&fixture.store, &fixture.owner, mark)?,
        "the first cancel",
    )?;
    assert_job_row(&row, &job, fixture.id());
    let mark = head(&fixture.store, &fixture.owner)?;

    let second = cancel_as(&fixture.store, &fixture.owner, "cancel-2", &job, 1_600)?;
    assert_eq!(
        second.delivery,
        Delivery::Committed,
        "a new key is a new request"
    );
    let replay = cancel_as(&fixture.store, &fixture.owner, "cancel-1", &job, 1_700)?;
    assert_eq!(replay.delivery, Delivery::Replayed);
    assert!(
        rows_after(&fixture.store, &fixture.owner, mark)?.is_empty(),
        "a second cancel, or a replay, of a running job must not append a row"
    );
    Ok(())
}

#[test]
fn a_claim_that_ends_two_jobs_and_takes_a_third_appends_one_row_each_in_its_owners_feed()
-> TestResult {
    // The other actor's job is claimed at 0, 1_000 and 2_000; its last lease
    // ends at 3_000 with no attempt left. The owner's job is claimed at 1_500
    // and cancelled while running. The owner's third job is due from 2_600.
    let fixture = Fixture::new()?;
    let other = actor("other");
    let elsewhere = create_demo(
        &fixture.store,
        &other,
        &key("other-create"),
        "Elsewhere",
        FIRST,
    )?;
    let elsewhere_id = &elsewhere.artifact.id;
    let other_job = enqueue_on(&fixture.store, &other, elsewhere_id, 1, "other", 0)?
        .job
        .id;
    assert_eq!(
        claimed(&fixture.store, "w", 0)?.job.id,
        other_job,
        "the other actor's job is taken first"
    );
    assert_eq!(claimed(&fixture.store, "w", 1_000)?.job.id, other_job);
    let cancelled_job = fixture.enqueue("cancelled", 2, 1_500)?.job.id;
    assert_eq!(
        claimed(&fixture.store, "w", 1_500)?.job.id,
        cancelled_job,
        "only the owner's job is due at 1_500"
    );
    cancel_as(
        &fixture.store,
        &fixture.owner,
        "cancel",
        &cancelled_job,
        1_600,
    )?;
    assert_eq!(claimed(&fixture.store, "w", 2_000)?.job.id, other_job);
    let waiting_job = fixture.enqueue("waiting", 2, 2_600)?.job.id;
    let owner_mark = head(&fixture.store, &fixture.owner)?;
    let other_mark = head(&fixture.store, &other)?;

    let claim = claim_as(&fixture.store, "w", 3_000)?;

    let [failed, cancelled] = <[JobRecord; 2]>::try_from(claim.settled)
        .map_err(|settled| format!("two jobs must end, got {}", settled.len()))?;
    assert_eq!(
        failed.id, other_job,
        "the first settled job is the other actor's"
    );
    assert_eq!(failed.state, JobState::Failed);
    assert_eq!(
        failed.failure,
        Some(JobFailure::LeaseExpired),
        "no attempt is left for it"
    );
    assert_eq!(cancelled.id, cancelled_job);
    assert_eq!(
        cancelled.state,
        JobState::Cancelled,
        "a running job with a cancel requested ends as cancelled"
    );
    let taken = claim.claimed.ok_or("the waiting job must be claimed")?;
    assert_eq!(taken.job.id, waiting_job, "the third job is taken");

    let owner_changes = rows_after(&fixture.store, &fixture.owner, owner_mark)?;
    let [ended_row, taken_row] = <[ChangeRecord; 2]>::try_from(owner_changes)
        .map_err(|changes| format!("the owner must get one row per job, got {}", changes.len()))?;
    assert_job_row(&ended_row, &cancelled_job, fixture.id());
    assert_job_row(&taken_row, &waiting_job, fixture.id());
    let other_changes = rows_after(&fixture.store, &other, other_mark)?;
    let other_row = only(other_changes, "the other actor's feed")?;
    assert_job_row(&other_row, &other_job, elsewhere_id);
    Ok(())
}

#[test]
fn a_job_row_names_the_artifacts_current_revision_and_metadata_revision() -> TestResult {
    let fixture = Fixture::new()?;
    let job = fixture.queued("one", 1_000)?;
    revise(
        &fixture.store,
        &fixture.created,
        &key("save-again"),
        2,
        THIRD,
    )?;
    update_metadata(
        &fixture.store,
        &fixture.created,
        &key("rename"),
        1,
        Some("Renamed"),
        None,
        None,
    )?;
    let mark = head(&fixture.store, &fixture.owner)?;

    claimed(&fixture.store, "a", 2_000)?;

    let row = only(
        rows_after(&fixture.store, &fixture.owner, mark)?,
        "the claim",
    )?;
    assert_job_row(&row, &job.id, fixture.id());
    assert_eq!(
        row.revision,
        SourceRevision(3),
        "the row names the artifact's current revision, not the job's"
    );
    assert_eq!(
        row.metadata_revision,
        MetadataRevision(2),
        "the row names the artifact's current metadata revision"
    );
    assert_eq!(
        fixture.job(&job.id)?.revision,
        SourceRevision(2),
        "the job keeps the revision that it was pinned to"
    );
    Ok(())
}

#[test]
fn a_transition_of_a_job_in_the_trash_appends_no_row() -> TestResult {
    for step in TRASHED_MOVES {
        let fixture = Fixture::new()?;
        let (job, lease) = step.arrange(&fixture)?;
        fixture.trash()?;
        let mark = head(&fixture.store, &fixture.owner)?;

        step.perform(&fixture, &job, lease.as_ref(), 1_500)?;

        let record = fixture.job(&job)?;
        assert_eq!(
            record.state,
            step.ends_in(),
            "{step:?} must still apply in the trash"
        );
        let rows = rows_after(&fixture.store, &fixture.owner, mark)?;
        assert!(
            rows.is_empty(),
            "{step:?} in the trash appended {} row(s)",
            rows.len()
        );
    }
    Ok(())
}

#[test]
fn a_purge_appends_only_the_purged_row() -> TestResult {
    let fixture = Fixture::new()?;
    let (running, _) = running_job(&fixture)?;
    let waiting = fixture.queued("waiting", 1_100)?.id;
    fixture.trash()?;
    let mark = head(&fixture.store, &fixture.owner)?;

    fixture.purge()?;

    let row = only(
        rows_after(&fixture.store, &fixture.owner, mark)?,
        "the purge",
    )?;
    assert_eq!(
        row.kind,
        ChangeKind::Purged,
        "a purge records one purged row"
    );
    assert!(row.job_id.is_none(), "the purged row names no job");
    assert_eq!(
        fixture.job(&running)?.state,
        JobState::Cancelled,
        "a purge ends the running job"
    );
    assert_eq!(
        fixture.job(&waiting)?.state,
        JobState::Cancelled,
        "a purge ends the waiting job"
    );
    Ok(())
}

#[test]
fn the_table_refuses_a_job_change_that_names_no_job() -> TestResult {
    let fixture = Fixture::new()?;
    let job = fixture.queued("one", 1_000)?;
    let connection = fixture.sql()?;

    let accepted = insert_change(
        &connection,
        fixture.id(),
        1_000_001,
        "job_changed",
        None,
        Some(job.id.as_str()),
    )?;
    assert_eq!(
        accepted, 1,
        "a job_changed row that names its job must be accepted"
    );
    assert_check_violation(
        insert_change(
            &connection,
            fixture.id(),
            1_000_002,
            "job_changed",
            None,
            None,
        ),
        "a job_changed row that names no job",
    )
}

#[test]
fn other_kinds_are_refused_with_a_job_and_accepted_without_one() -> TestResult {
    let fixture = Fixture::new()?;
    let job = fixture.queued("one", 1_000)?;
    let connection = fixture.sql()?;
    let mut sequence = 1_000_000;
    for (kind, data_version) in KINDS_WITHOUT_A_JOB {
        sequence += 1;
        let accepted = insert_change(
            &connection,
            fixture.id(),
            sequence,
            kind,
            data_version,
            None,
        )?;
        assert_eq!(accepted, 1, "a {kind} row without a job must be accepted");
        sequence += 1;
        assert_check_violation(
            insert_change(
                &connection,
                fixture.id(),
                sequence,
                kind,
                data_version,
                Some(job.id.as_str()),
            ),
            &format!("a {kind} row that names a job"),
        )?;
    }
    Ok(())
}

#[test]
fn an_enqueue_whose_change_row_fails_creates_no_job_and_records_no_key() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.sql()?.execute_batch(REFUSE_JOB_CHANGES)?;

    let error = refusal(
        fixture.enqueue("one", 2, 1_000),
        "an enqueue whose change row cannot be written",
    )?;

    assert!(
        matches!(error, JobStoreError::Unavailable(_)),
        "the failed row must fail the enqueue, got {error:?}"
    );
    assert_eq!(
        fixture.count("SELECT count(*) FROM jobs")?,
        0,
        "the job must roll back with its row"
    );
    assert_eq!(
        fixture.count("SELECT count(*) FROM job_requests")?,
        0,
        "the key must roll back with its row"
    );

    fixture
        .sql()?
        .execute_batch("DROP TRIGGER refuse_job_changes;")?;
    let retried = fixture.enqueue("one", 2, 1_000)?;
    assert_eq!(
        retried.delivery,
        Delivery::Committed,
        "the same request is recorded once its row can be written"
    );
    Ok(())
}

#[test]
fn a_completion_whose_change_row_fails_leaves_the_job_running() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.queued("one", 1_000)?;
    let running = claimed(&fixture.store, "a", 1_000)?;
    fixture.sql()?.execute_batch(REFUSE_JOB_CHANGES)?;

    let error = refusal(
        fixture
            .store
            .complete_job(&running.lease, &report_with_a_finding(), at(1_500)),
        "a completion whose change row cannot be written",
    )?;

    assert!(
        matches!(error, JobStoreError::Unavailable(_)),
        "the failed row must fail the completion, got {error:?}"
    );
    let record = fixture.job(&running.job.id)?;
    assert_eq!(
        record.state,
        JobState::Running,
        "the completion must roll back with its row"
    );
    assert_eq!(
        record.lease_expires_at,
        Some(at(2_000)),
        "the lease must stay as it was"
    );
    assert!(
        record.report.is_none(),
        "the report must roll back with its row"
    );
    assert_eq!(
        fixture.count("SELECT count(*) FROM job_findings")?,
        0,
        "the findings must roll back with the completion"
    );

    fixture
        .sql()?
        .execute_batch("DROP TRIGGER refuse_job_changes;")?;
    let done = fixture
        .store
        .complete_job(&running.lease, &report_with_a_finding(), at(1_600))?;
    assert_eq!(
        done.state,
        JobState::Succeeded,
        "the same completion succeeds once its row can be written"
    );
    Ok(())
}
