//! Settlement, the reclaim and terminal gates, publication, and restore of the
//! execution attempts of `run_profile` jobs in the `SQLite` store (decision 0057
//! rules 10 to 13). Time is a number each call is given, so every test moves it
//! by hand.
//!
//! The store hides `run_profile` jobs from `job` (decision 0057 rule 3). These
//! tests therefore read a job's stored columns through a raw connection, and
//! check the records that each transition returns.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use artifree_domain::SourceRevision;
use artifree_domain::identity::{ActorId, ArtifactId, UnixMillis};
use artifree_domain::job::{
    AttemptIdentity, AttemptState, JobFailure, JobId, JobState, ProfileName, Revocation,
    StopReason, WorkerId,
};
use artifree_ports::artifact_store::{RequestFingerprint, RequestIdentity};
use artifree_ports::job_store::{
    AttemptRow, Claim, ClaimedJob, ExecutionClaim, JobClaim, JobRecord, JobStore, JobStoreError,
    Lease, NewProfileJob, ProfileBinding, ProfileCounts, Retry, SettleOutcome,
};
use artifree_providers::backup::{backup, restore};
use artifree_providers::storage::SqliteStore;
use common::{TempDir, TestResult, actor, create_demo, database_in, digest, key, parsed};
use rusqlite::types::{FromSql, Value};
use rusqlite::{Connection, params};

/// The lease of one claim, in milliseconds from the claim.
const LEASE_MS: u64 = 1_000;
/// A tool root manifest digest: 64 lowercase hexadecimal digits.
const TOOL_DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
/// An execution directory identity: 32 lowercase hexadecimal digits.
const DIRECTORY: &str = "0123456789abcdef0123456789abcdef";
/// The one profile this build knows in these tests.
const PROFILE: &str = "review-files";
/// The counts a successful run publishes.
const COUNTS: ProfileCounts = ProfileCounts {
    files: 3,
    bytes: 42,
};

fn at(ms: i64) -> UnixMillis {
    UnixMillis(ms)
}

fn worker() -> WorkerId {
    parsed("worker-a")
}

fn profile() -> ProfileName {
    parsed(PROFILE)
}

/// The identity of the attempt that a claim records when it is given `seed`.
fn attempt_id(seed: u8) -> AttemptIdentity {
    AttemptIdentity::from_random([seed; 16])
}

/// The execution identities of a claim whose attempt is `seed`.
fn execution(seed: u8) -> ExecutionClaim<'static> {
    ExecutionClaim {
        attempt: attempt_id(seed),
        tool_digest: TOOL_DIGEST,
        machine: "machine-a",
        boot: "boot-a",
        cgroupns: "cgroupns-a",
        directory: DIRECTORY,
    }
}

/// One claim at `now` by the test worker, which knows the test profile.
fn claim_on(
    store: &SqliteStore,
    now: i64,
    identities: Option<ExecutionClaim<'_>>,
) -> Result<Claim, JobStoreError> {
    let claimant = worker();
    let known = [profile()];
    store.claim_job(JobClaim {
        worker: &claimant,
        now: at(now),
        lease_ms: LEASE_MS,
        known_profiles: &known,
        execution: identities,
    })
}

/// The stored attempt with identity `seed`, or an error when there is none.
fn stored_attempt(store: &SqliteStore, seed: u8) -> Result<AttemptRow, Box<dyn std::error::Error>> {
    Ok(store
        .attempt_row(&attempt_id(seed))?
        .ok_or("the attempt row is missing")?)
}

/// Settles the attempt `seed` and returns its job. A settlement that changed
/// nothing is an error here.
fn settled_job_in(
    store: &SqliteStore,
    seed: u8,
    now: i64,
) -> Result<JobRecord, Box<dyn std::error::Error>> {
    match store.settle_attempt(&attempt_id(seed), at(now))? {
        SettleOutcome::Settled(job) => Ok(job),
        SettleOutcome::AlreadySettled(_) => {
            Err(format!("attempt {seed} was settled already").into())
        }
    }
}

/// One column of a job row, read from the database file itself.
fn read_column<T: FromSql>(
    database: &Path,
    id: &JobId,
    column: &str,
) -> Result<T, rusqlite::Error> {
    Connection::open(database)?.query_row(
        &format!("SELECT {column} FROM jobs WHERE id = ?1"),
        [id.as_str()],
        |row| row.get(0),
    )
}

/// Every column of a job row, in the table's column order, as `SQLite` stores it.
fn raw_row(database: &Path, id: &JobId) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    let connection = Connection::open(database)?;
    let mut statement = connection.prepare("SELECT * FROM jobs WHERE id = ?1")?;
    let columns = statement.column_count();
    let mut rows = statement.query([id.as_str()])?;
    let row = rows.next()?.ok_or("the job row is missing")?;
    let mut values = Vec::with_capacity(columns);
    for column in 0..columns {
        values.push(row.get::<_, Value>(column)?);
    }
    Ok(values)
}

/// A database with one artifact, whose revision 1 the jobs pin.
struct Fixture {
    root: TempDir,
    store: SqliteStore,
    owner: ActorId,
    artifact: ArtifactId,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let root = TempDir::new();
        fs::create_dir(root.join("data"))?;
        let store = SqliteStore::open(&database_in(&root.join("data")))?;
        let owner = actor("owner");
        let created = create_demo(&store, &owner, &key("create"), "Secret", "<h1>first</h1>")?;
        Ok(Self {
            root,
            store,
            owner,
            artifact: created.artifact.id,
        })
    }

    fn data(&self) -> PathBuf {
        self.root.join("data")
    }

    fn database(&self) -> PathBuf {
        database_in(&self.data())
    }

    fn sql(&self) -> Result<Connection, rusqlite::Error> {
        Connection::open(self.database())
    }

    fn count(&self, query: &str) -> Result<i64, rusqlite::Error> {
        self.sql()?.query_row(query, [], |row| row.get(0))
    }

    fn column<T: FromSql>(&self, id: &JobId, column: &str) -> Result<T, rusqlite::Error> {
        read_column(&self.database(), id, column)
    }

    fn state(&self, id: &JobId) -> Result<JobState, Box<dyn std::error::Error>> {
        let text: String = self.column(id, "state")?;
        Ok(text.parse::<JobState>()?)
    }

    /// Changes columns of a job by a raw write that the store did not make.
    /// `assignment` is SQL, such as `fence = fence + 1`.
    fn update_job(&self, id: &JobId, assignment: &str) -> Result<(), rusqlite::Error> {
        self.sql()?.execute(
            &format!("UPDATE jobs SET {assignment} WHERE id = ?1"),
            [id.as_str()],
        )?;
        Ok(())
    }

    /// Stores `revocation` on an attempt by a raw write.
    fn revoke_raw(&self, seed: u8, revocation: Revocation) -> Result<(), rusqlite::Error> {
        self.sql()?.execute(
            "UPDATE execution_attempts SET revoked = ?2 WHERE attempt = ?1",
            params![attempt_id(seed).hex(), revocation.as_str()],
        )?;
        Ok(())
    }

    /// Enqueues a `run_profile` job for revision 1 of the artifact.
    fn enqueue(&self, name: &str, now: i64) -> Result<JobRecord, JobStoreError> {
        let request_key = key(name);
        let profile = profile();
        self.store
            .enqueue_profile_job(NewProfileJob {
                request: RequestIdentity {
                    actor: &self.owner,
                    key: &request_key,
                    fingerprint: RequestFingerprint(digest(name)),
                },
                artifact: &self.artifact,
                revision: SourceRevision(1),
                profile: &profile,
                now: at(now),
            })
            .map(|receipt| receipt.job)
    }

    /// A claim at `now` whose attempt is `seed`.
    fn claim(&self, now: i64, seed: u8) -> Result<Claim, JobStoreError> {
        claim_on(&self.store, now, Some(execution(seed)))
    }

    /// A claim at `now` whose attempt is `seed`, which must find a job.
    fn claimed(&self, now: i64, seed: u8) -> Result<ClaimedJob, Box<dyn std::error::Error>> {
        Ok(self.claim(now, seed)?.claimed.ok_or("no job was due")?)
    }

    fn settle(&self, seed: u8, now: i64) -> Result<SettleOutcome, JobStoreError> {
        self.store.settle_attempt(&attempt_id(seed), at(now))
    }

    fn settled_job(&self, seed: u8, now: i64) -> Result<JobRecord, Box<dyn std::error::Error>> {
        settled_job_in(&self.store, seed, now)
    }

    fn attempt_row(&self, seed: u8) -> Result<AttemptRow, Box<dyn std::error::Error>> {
        stored_attempt(&self.store, seed)
    }
}

/// The operations that end a `run_profile` job, or give it back, on the lease
/// of its claim and the attempt of that claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    Complete,
    FailWithoutRetry,
    FailWithRetry,
    ConfirmCancel,
    Release,
}

impl Ending {
    const ALL: [Self; 5] = [
        Self::Complete,
        Self::FailWithoutRetry,
        Self::FailWithRetry,
        Self::ConfirmCancel,
        Self::Release,
    ];

    fn run(
        self,
        store: &SqliteStore,
        lease: &Lease,
        attempt: &AttemptIdentity,
    ) -> Result<JobRecord, JobStoreError> {
        match self {
            Self::Complete => store.complete_profile_job(
                lease,
                attempt,
                ProfileBinding {
                    profile: &profile(),
                    tool_digest: TOOL_DIGEST,
                },
                COUNTS,
                at(300),
            ),
            Self::FailWithoutRetry => {
                store.fail_job(lease, JobFailure::Internal, Retry::Never, at(300))
            }
            Self::FailWithRetry => store.fail_job(
                lease,
                JobFailure::StorageUnavailable,
                Retry::At(at(5_000)),
                at(300),
            ),
            Self::ConfirmCancel => store.confirm_cancel(lease, at(300)),
            Self::Release => store.release_job(lease, at(300)),
        }
    }

    /// The state the job is in after this operation, once its attempt is settled.
    fn ended_as(self) -> JobState {
        match self {
            Self::Complete => JobState::Succeeded,
            Self::FailWithoutRetry => JobState::Failed,
            Self::FailWithRetry => JobState::RetryScheduled,
            Self::ConfirmCancel => JobState::Cancelled,
            Self::Release => JobState::Queued,
        }
    }
}

#[test]
fn settle_of_an_unrevoked_attempt_leaves_the_job_running_then_complete_stores_counts() -> TestResult
{
    let fixture = Fixture::new()?;
    fixture.enqueue("a", 0)?;
    let claimed = fixture.claimed(100, 1)?;

    let outcome = fixture.settle(1, 200)?;
    assert_eq!(
        outcome,
        SettleOutcome::Settled(claimed.job.clone()),
        "an unrevoked settlement leaves the claimed job as it was"
    );
    let row = fixture.attempt_row(1)?;
    assert_eq!(row.state, AttemptState::Settled);
    assert_eq!(row.settled_at, Some(at(200)));

    let done = fixture.store.complete_profile_job(
        &claimed.lease,
        &attempt_id(1),
        ProfileBinding {
            profile: &profile(),
            tool_digest: TOOL_DIGEST,
        },
        COUNTS,
        at(300),
    )?;
    assert_eq!(done.state, JobState::Succeeded);
    assert_eq!(done.report, None, "a run_profile result is no verification");
    assert_eq!(done.counts, Some(COUNTS), "the two counts are the result");
    assert_eq!(done.finished_at, Some(at(300)));
    assert_eq!(
        fixture.count("SELECT count(*) FROM job_findings")?,
        0,
        "a run_profile result stores no findings"
    );
    Ok(())
}

#[test]
fn settle_is_idempotent() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.enqueue("a", 0)?;
    fixture.claimed(100, 1)?;

    let first = fixture.settled_job(1, 200)?;
    let second = fixture.settle(1, 300)?;
    assert_eq!(
        second,
        SettleOutcome::AlreadySettled(first),
        "a second settlement returns the job and changes nothing"
    );
    assert_eq!(
        fixture.attempt_row(1)?.settled_at,
        Some(at(200)),
        "the first settlement time is kept"
    );
    Ok(())
}

#[test]
fn a_revoked_attempt_ends_its_job_as_cancelled_once_settled() -> TestResult {
    for revocation in [
        Revocation::Cancel,
        Revocation::Trash,
        Revocation::Purge,
        Revocation::Generation,
        Revocation::Authority,
    ] {
        let fixture = Fixture::new()?;
        let queued = fixture.enqueue("a", 0)?;
        fixture.claimed(100, 1)?;
        fixture.revoke_raw(1, revocation)?;

        let job = fixture.settled_job(1, 200)?;
        assert_eq!(job.id, queued.id);
        assert_eq!(
            job.state,
            JobState::Cancelled,
            "{revocation:?} ends the job as cancelled"
        );
        assert_eq!(job.worker, None, "{revocation:?}: the job has no worker");
        assert_eq!(job.lease_expires_at, None, "{revocation:?}: no lease");
        assert_eq!(job.finished_at, Some(at(200)));
        let row = fixture.attempt_row(1)?;
        assert_eq!(row.state, AttemptState::Settled);
        assert_eq!(row.revoked, Some(revocation));
    }
    Ok(())
}

#[test]
fn a_lease_or_shutdown_revocation_schedules_the_job_again_while_attempts_are_left() -> TestResult {
    for revocation in [Revocation::Lease, Revocation::Shutdown] {
        let fixture = Fixture::new()?;
        fixture.enqueue("a", 0)?;
        fixture.claimed(100, 1)?;
        fixture.revoke_raw(1, revocation)?;

        let job = fixture.settled_job(1, 200)?;
        assert_eq!(
            job.state,
            JobState::RetryScheduled,
            "{revocation:?} with attempts left"
        );
        assert_eq!(job.run_after, at(200), "the retry is due at the settlement");
        assert_eq!(job.worker, None);
        assert_eq!(job.lease_expires_at, None);

        let next = fixture.claimed(300, 2)?;
        assert_eq!(
            (next.job.attempts, next.lease.fence),
            (2, 2),
            "{revocation:?}: the next claim is the second attempt, with a higher fence"
        );
    }
    Ok(())
}

#[test]
fn a_lease_or_shutdown_revocation_fails_the_job_on_its_last_attempt() -> TestResult {
    for revocation in [Revocation::Lease, Revocation::Shutdown] {
        let fixture = Fixture::new()?;
        let queued = fixture.enqueue("a", 0)?;
        fixture.claimed(100, 1)?;
        fixture.update_job(&queued.id, "attempts = max_attempts")?;
        fixture.revoke_raw(1, revocation)?;

        let job = fixture.settled_job(1, 200)?;
        assert_eq!(
            job.state,
            JobState::Failed,
            "{revocation:?} on the last attempt"
        );
        assert_eq!(job.failure, Some(JobFailure::LeaseExpired));
        assert_eq!(job.finished_at, Some(at(200)));
    }
    Ok(())
}

#[test]
fn settle_of_an_unknown_attempt_is_not_found() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.enqueue("a", 0)?;
    fixture.claimed(100, 1)?;

    assert_eq!(
        fixture.store.settle_attempt(&attempt_id(7), at(200)),
        Err(JobStoreError::NotFound)
    );
    assert_eq!(
        fixture.attempt_row(1)?.state,
        AttemptState::Live,
        "the known attempt is untouched"
    );
    Ok(())
}

#[test]
fn an_expired_lease_with_an_unsettled_attempt_is_not_claimed_until_settled() -> TestResult {
    let fixture = Fixture::new()?;
    let queued = fixture.enqueue("a", 0)?;
    let first = fixture.claimed(0, 1)?;
    assert_eq!(first.lease.fence, 1);

    // The lease ended at 1_000, but attempt 1 is still live: the job is not due.
    assert_eq!(
        fixture.claim(1_001, 2)?,
        Claim::default(),
        "an expired lease with an unsettled attempt is not claimed"
    );
    assert_eq!(fixture.state(&queued.id)?, JobState::Running);
    assert_eq!(fixture.column::<i64>(&queued.id, "attempts")?, 1);

    let settled = fixture.settled_job(1, 1_002)?;
    assert_eq!(
        settled.state,
        JobState::Running,
        "an unrevoked settlement leaves the job for its worker"
    );

    let reclaimed = fixture.claimed(1_003, 2)?;
    assert_eq!(reclaimed.job.id, queued.id);
    assert_eq!(reclaimed.lease.fence, 2);
    assert!(
        reclaimed.lease.fence > first.lease.fence,
        "the new claim has a higher fence than {}",
        first.lease.fence
    );
    assert_eq!(reclaimed.job.attempts, 2);
    assert_eq!(fixture.attempt_row(2)?.fence, 2);
    assert_eq!(fixture.attempt_row(1)?.state, AttemptState::Settled);
    Ok(())
}

#[test]
fn no_terminal_transition_while_the_attempt_is_unsettled() -> TestResult {
    for ending in Ending::ALL {
        let fixture = Fixture::new()?;
        fixture.enqueue("a", 0)?;
        let claimed = fixture.claimed(100, 1)?;
        let id = &claimed.job.id;
        if ending == Ending::ConfirmCancel {
            fixture.update_job(id, "cancel_requested = 1")?;
        }
        let before = raw_row(&fixture.database(), id)?;

        let refused = ending.run(&fixture.store, &claimed.lease, &attempt_id(1));
        assert_eq!(
            refused,
            Err(JobStoreError::AttemptUnsettled),
            "{ending:?} while the attempt is unsettled"
        );
        assert_eq!(
            raw_row(&fixture.database(), id)?,
            before,
            "{ending:?} changed the job columns while it was refused"
        );

        let settled = fixture.settled_job(1, 200)?;
        assert_eq!(settled.state, JobState::Running);
        let ended = ending.run(&fixture.store, &claimed.lease, &attempt_id(1))?;
        assert_eq!(
            ended.state,
            ending.ended_as(),
            "{ending:?} once the attempt is settled"
        );
        match ending {
            Ending::Complete => {
                assert_eq!(ended.report, None);
                assert_eq!(ended.counts, Some(COUNTS));
            }
            Ending::FailWithoutRetry => {
                assert_eq!(ended.failure, Some(JobFailure::Internal));
            }
            Ending::Release => assert_eq!(ended.attempts, 0, "a release gives its attempt back"),
            Ending::FailWithRetry | Ending::ConfirmCancel => {}
        }
    }
    Ok(())
}

#[test]
fn complete_with_a_stale_fence_is_lease_lost() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.enqueue("a", 0)?;
    let claimed = fixture.claimed(100, 1)?;
    fixture.update_job(&claimed.job.id, "fence = fence + 1")?;
    fixture.settled_job(1, 200)?;

    assert_eq!(
        fixture.store.complete_profile_job(
            &claimed.lease,
            &attempt_id(1),
            ProfileBinding {
                profile: &profile(),
                tool_digest: TOOL_DIGEST
            },
            COUNTS,
            at(300)
        ),
        Err(JobStoreError::LeaseLost)
    );
    assert_eq!(fixture.state(&claimed.job.id)?, JobState::Running);
    assert_eq!(
        fixture.attempt_row(1)?.revoked,
        None,
        "a stale fence stores no revocation"
    );
    Ok(())
}

#[test]
fn complete_after_a_cancel_request_is_refused_and_the_settled_job_ends_cancelled() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.enqueue("a", 0)?;
    let claimed = fixture.claimed(100, 1)?;
    fixture.settled_job(1, 200)?;
    fixture.update_job(&claimed.job.id, "cancel_requested = 1")?;

    assert_eq!(
        fixture.store.complete_profile_job(
            &claimed.lease,
            &attempt_id(1),
            ProfileBinding {
                profile: &profile(),
                tool_digest: TOOL_DIGEST
            },
            COUNTS,
            at(300)
        ),
        Err(JobStoreError::Revoked(StopReason::Cancel))
    );
    assert_eq!(fixture.state(&claimed.job.id)?, JobState::Cancelled);
    assert_eq!(fixture.attempt_row(1)?.revoked, Some(Revocation::Cancel));
    Ok(())
}

#[test]
fn a_restore_marks_live_attempts_cleanup_pending_and_keeps_a_stored_reason() -> TestResult {
    let fixture = Fixture::new()?;
    let first_job = fixture.enqueue("a", 0)?;
    let second_job = fixture.enqueue("b", 1)?;
    let first = fixture.claimed(100, 1)?;
    assert_eq!(first.job.id, first_job.id);
    let second = fixture.claimed(101, 2)?;
    assert_eq!(second.job.id, second_job.id);
    fixture.revoke_raw(2, Revocation::Cancel)?;

    let backup_file = fixture.root.join("backup.sqlite3");
    backup(&fixture.data(), &backup_file)?;
    let target = fixture.root.join("restored");
    restore(&backup_file, &target)?;
    let target_database = database_in(&target);
    let restored = SqliteStore::open(&target_database)?;

    let first_row = stored_attempt(&restored, 1)?;
    assert_eq!(
        first_row.state,
        AttemptState::CleanupPending,
        "a restore marks a live attempt cleanup pending"
    );
    assert_eq!(first_row.revoked, Some(Revocation::Lease));
    let second_row = stored_attempt(&restored, 2)?;
    assert_eq!(second_row.state, AttemptState::CleanupPending);
    assert_eq!(
        second_row.revoked,
        Some(Revocation::Cancel),
        "a reason stored before the restore is kept"
    );
    assert_eq!(
        read_column::<Option<i64>>(&target_database, &first.job.id, "lease_expires_at")?,
        Some(0),
        "the restored lease is expired"
    );
    assert_eq!(
        read_column::<Option<i64>>(&target_database, &second.job.id, "lease_expires_at")?,
        Some(0)
    );
    assert_eq!(
        claim_on(&restored, 200, Some(execution(3)))?,
        Claim::default(),
        "no job of an unsettled attempt is claimable after the restore"
    );

    let retry = settled_job_in(&restored, 1, 200)?;
    assert_eq!(
        retry.state,
        JobState::RetryScheduled,
        "the lease revocation schedules the job again"
    );
    let next = claim_on(&restored, 300, Some(execution(4)))?
        .claimed
        .ok_or("the settled job was not claimed")?;
    assert_eq!(next.job.id, first.job.id);
    assert_eq!(
        next.lease.fence, 2,
        "the claim after the settlement has a higher fence"
    );
    let cancelled = settled_job_in(&restored, 2, 300)?;
    assert_eq!(
        cancelled.state,
        JobState::Cancelled,
        "the stored cancel ends its job once it is settled"
    );

    assert_eq!(fixture.attempt_row(1)?.state, AttemptState::Live);
    assert_eq!(fixture.attempt_row(1)?.revoked, None);
    assert_eq!(fixture.attempt_row(2)?.state, AttemptState::Live);
    assert_eq!(
        fixture.column::<Option<i64>>(&first.job.id, "lease_expires_at")?,
        Some(1_100),
        "the original keeps its lease"
    );
    Ok(())
}

/// A late call of a replaced attempt cannot end the job of the attempt that
/// holds it now (decision 0057 rules 7 and 10).
#[test]
fn a_replaced_attempt_answers_fence_and_moves_nothing() -> TestResult {
    let fixture = Fixture::new()?;
    let queued = fixture.enqueue("a", 0)?;
    let first = fixture.claimed(0, 1)?;
    let _ = fixture.settled_job(1, 1_002)?;
    let second = fixture.claimed(1_003, 2)?;
    assert_eq!(second.job.id, queued.id);
    fixture.update_job(&queued.id, "cancel_requested = 1")?;

    assert_eq!(
        fixture
            .store
            .checkpoint_attempt(&first.lease, &attempt_id(1), at(1_100), LEASE_MS)?,
        artifree_domain::job::CheckpointAnswer::Stop(StopReason::Fence),
        "the replaced attempt answers fence, not the current cancel"
    );
    assert_eq!(
        fixture.store.complete_profile_job(
            &first.lease,
            &attempt_id(1),
            ProfileBinding {
                profile: &profile(),
                tool_digest: TOOL_DIGEST,
            },
            COUNTS,
            at(1_101),
        ),
        Err(JobStoreError::LeaseLost),
        "the replaced attempt cannot publish"
    );
    assert_eq!(
        fixture.attempt_row(1)?.revoked,
        None,
        "nothing is stored on it"
    );
    assert_eq!(
        fixture.state(&queued.id)?,
        JobState::Running,
        "the job of the live attempt is not ended"
    );
    assert_eq!(fixture.attempt_row(2)?.state, AttemptState::Live);
    Ok(())
}

/// Publication refuses a binding of other tools than the attempt's (decision
/// 0057 rule 12) and stores nothing.
#[test]
fn publication_refuses_a_binding_of_other_tools() -> TestResult {
    let fixture = Fixture::new()?;
    let queued = fixture.enqueue("a", 0)?;
    let claimed = fixture.claimed(0, 1)?;
    let _ = fixture.settled_job(1, 10)?;
    let other_profile: ProfileName = parsed("other");
    let other_digest = "f".repeat(64);
    for binding in [
        ProfileBinding {
            profile: &other_profile,
            tool_digest: TOOL_DIGEST,
        },
        ProfileBinding {
            profile: &profile(),
            tool_digest: &other_digest,
        },
    ] {
        assert!(
            matches!(
                fixture.store.complete_profile_job(
                    &claimed.lease,
                    &attempt_id(1),
                    binding,
                    COUNTS,
                    at(20)
                ),
                Err(JobStoreError::InvalidRequest(_))
            ),
            "a binding of other tools is refused: {binding:?}"
        );
        assert_eq!(fixture.state(&queued.id)?, JobState::Running);
    }
    let done = fixture.store.complete_profile_job(
        &claimed.lease,
        &attempt_id(1),
        ProfileBinding {
            profile: &profile(),
            tool_digest: TOOL_DIGEST,
        },
        COUNTS,
        at(30),
    )?;
    assert_eq!(
        done.state,
        JobState::Succeeded,
        "the attempt's own binding publishes"
    );
    Ok(())
}
