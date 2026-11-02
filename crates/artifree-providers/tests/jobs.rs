//! Behavioral contract of persistent jobs in the `SQLite` store (decision 0030):
//! enqueue, claim, lease, fencing, retry, cancel, and purge.
//!
//! Time is a number each call is given, so every test moves it by hand.

mod common;

use std::collections::BTreeSet;
use std::sync::{Arc, Barrier};
use std::thread;

use artifree_domain::SourceRevision;
use artifree_domain::identity::{ActorId, ArtifactId, UnixMillis};
use artifree_domain::job::{
    Finding, JobFailure, JobId, JobKind, JobState, MAX_ACTIVE_JOBS_PER_ACTOR, MAX_ATTEMPTS,
    Problem, VerifyReport, WorkerId,
};
use artifree_domain::{ArtifactKind, MetadataRevision};
use artifree_ports::artifact_store::{
    ArtifactStore, CleanupRequest, CommitReceipt, Delivery, NewArtifact, PurgeRequest,
    RequestFingerprint, RequestIdentity, StoreError,
};
use artifree_ports::job_store::{
    Checkpoint, Claim, ClaimedJob, JobCancel, JobClaim, JobListing, JobPosition, JobReceipt,
    JobRecord, JobStore, JobStoreError, NewJob, Retry,
};
use artifree_providers::storage::SqliteStore;
use common::{
    TempDir, TestResult, actor, create_demo, database_in, digest, key, package, parsed, revise,
    update_metadata,
};
use rusqlite::Connection;

const FIRST: &str = "<h1>first</h1>";
const SECOND: &str = "<h1>second</h1>";
const LEASE: u64 = 1_000;
const LEASE_MS: i64 = 1_000;

fn at(ms: i64) -> UnixMillis {
    UnixMillis(ms)
}

fn worker(name: &str) -> WorkerId {
    parsed(&format!("worker-{name}"))
}

struct Fixture {
    dir: TempDir,
    store: SqliteStore,
    owner: ActorId,
    created: CommitReceipt,
}

impl Fixture {
    /// An artifact with two revisions, the keys `create` and `save`.
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
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

    fn sql(&self) -> Result<Connection, rusqlite::Error> {
        Connection::open(database_in(self.dir.path()))
    }

    fn count(&self, query: &str) -> Result<i64, rusqlite::Error> {
        self.sql()?.query_row(query, [], |row| row.get(0))
    }

    fn another_store(&self) -> Result<SqliteStore, StoreError> {
        SqliteStore::open(&database_in(self.dir.path()))
    }

    fn enqueue_as(
        &self,
        who: &ActorId,
        name: &str,
        revision: u64,
        now: i64,
    ) -> Result<JobReceipt, JobStoreError> {
        let request_key = key(name);
        self.store.enqueue_job(NewJob {
            request: RequestIdentity {
                actor: who,
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

    fn enqueue(&self, name: &str, revision: u64, now: i64) -> Result<JobReceipt, JobStoreError> {
        self.enqueue_as(&self.owner, name, revision, now)
    }

    fn queued(&self, name: &str, now: i64) -> Result<JobRecord, Box<dyn std::error::Error>> {
        Ok(self.enqueue(name, 2, now)?.job)
    }

    fn cancel(&self, name: &str, job: &JobId, now: i64) -> Result<JobReceipt, JobStoreError> {
        self.cancel_as(&self.owner, name, job, now)
    }

    fn cancel_as(
        &self,
        who: &ActorId,
        name: &str,
        job: &JobId,
        now: i64,
    ) -> Result<JobReceipt, JobStoreError> {
        let request_key = key(name);
        self.store.cancel_job(JobCancel {
            request: RequestIdentity {
                actor: who,
                key: &request_key,
                fingerprint: RequestFingerprint(digest(&format!("cancel {job}"))),
            },
            job,
            now: at(now),
        })
    }

    fn try_claim(&self, who: &str, now: i64) -> Result<Option<ClaimedJob>, JobStoreError> {
        Ok(claim_with(&self.store, who, now)?.claimed)
    }

    fn claimed(&self, who: &str, now: i64) -> Result<ClaimedJob, Box<dyn std::error::Error>> {
        Ok(self.try_claim(who, now)?.ok_or("no job was due")?)
    }

    fn job(&self, id: &JobId) -> Result<JobRecord, Box<dyn std::error::Error>> {
        Ok(self.store.job(id)?.ok_or("the job is missing")?)
    }

    fn state(&self, id: &JobId) -> Result<JobState, Box<dyn std::error::Error>> {
        Ok(self.job(id)?.state)
    }

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

fn claim_with(store: &SqliteStore, who: &str, now: i64) -> Result<Claim, JobStoreError> {
    store.claim_job(JobClaim {
        worker: &worker(who),
        now: at(now),
        lease_ms: LEASE,
    })
}

fn report(findings: &[(&str, Problem)]) -> VerifyReport {
    VerifyReport {
        files_checked: 2,
        bytes_checked: 20,
        mismatch_count: findings.len() as u64,
        findings: findings
            .iter()
            .map(|(path, problem)| Finding {
                path: parsed(path),
                problem: *problem,
            })
            .collect(),
    }
}

// --- enqueue ---------------------------------------------------------------

#[test]
fn an_enqueued_job_pins_actor_artifact_and_revision_and_waits() -> TestResult {
    let fixture = Fixture::new()?;

    let receipt = fixture.enqueue("one", 1, 5_000)?;

    assert_eq!(receipt.delivery, Delivery::Committed);
    let job = receipt.job;
    assert_eq!(job.actor, fixture.owner);
    assert_eq!(&job.artifact, fixture.id());
    assert_eq!(job.revision, SourceRevision(1));
    assert_eq!(job.kind, JobKind::VerifyRevision);
    assert_eq!(job.state, JobState::Queued);
    assert_eq!((job.attempts, job.max_attempts), (0, MAX_ATTEMPTS));
    assert_eq!((job.run_after, job.created_at), (at(5_000), at(5_000)));
    assert!(job.worker.is_none() && job.lease_expires_at.is_none());
    assert!(job.report.is_none() && job.failure.is_none() && !job.cancel_requested);
    Ok(())
}

#[test]
fn a_retried_enqueue_returns_the_same_job_as_it_is_now() -> TestResult {
    let fixture = Fixture::new()?;
    let first = fixture.enqueue("one", 2, 1_000)?;
    fixture.claimed("a", 1_000)?;

    let replay = fixture.enqueue("one", 2, 9_000)?;

    assert_eq!(replay.delivery, Delivery::Replayed);
    assert_eq!(replay.job.id, first.job.id);
    assert_eq!(replay.job.state, JobState::Running, "the job moved on");
    assert_eq!(replay.job.created_at, at(1_000), "the first request's time");
    assert_eq!(fixture.count("SELECT count(*) FROM jobs")?, 1);
    Ok(())
}

#[test]
fn a_key_for_a_different_request_is_refused_and_creates_nothing() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.enqueue("one", 1, 1_000)?;

    let other_revision = fixture.enqueue("one", 2, 1_000);

    assert_eq!(other_revision.err(), Some(JobStoreError::RequestKeyReused));
    assert_eq!(fixture.count("SELECT count(*) FROM jobs")?, 1);
    Ok(())
}

#[test]
fn an_unknown_revision_artifact_or_zero_is_not_found() -> TestResult {
    let fixture = Fixture::new()?;
    for revision in [0, 3, 99] {
        assert_eq!(
            fixture.enqueue("r", revision, 1_000).err(),
            Some(JobStoreError::NotFound),
            "revision {revision}"
        );
    }
    let missing: ArtifactId = parsed("art_00000000000000000000000000000000");
    let name = key("missing");
    let result = fixture.store.enqueue_job(NewJob {
        request: RequestIdentity {
            actor: &fixture.owner,
            key: &name,
            fingerprint: RequestFingerprint(digest("x")),
        },
        artifact: &missing,
        revision: SourceRevision(1),
        kind: JobKind::VerifyRevision,
        now: at(1_000),
    });
    assert_eq!(result.err(), Some(JobStoreError::NotFound));
    assert_eq!(fixture.count("SELECT count(*) FROM jobs")?, 0);
    assert_eq!(
        fixture.count("SELECT count(*) FROM job_requests")?,
        0,
        "a refused request records no key"
    );
    Ok(())
}

#[test]
fn an_artifact_in_the_trash_or_purged_is_not_found_also_for_a_retry() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.enqueue("one", 2, 1_000)?;

    fixture.trash()?;
    assert_eq!(
        fixture.enqueue("two", 2, 2_000).err(),
        Some(JobStoreError::NotFound),
        "a new request while trashed"
    );
    assert_eq!(
        fixture.enqueue("one", 2, 2_000).err(),
        Some(JobStoreError::NotFound),
        "a retry while trashed"
    );

    fixture.purge()?;
    assert_eq!(
        fixture.enqueue("three", 2, 3_000).err(),
        Some(JobStoreError::NotFound),
        "a new request after the purge"
    );
    assert_eq!(
        fixture.enqueue("one", 2, 3_000).err(),
        Some(JobStoreError::NotFound),
        "a retry after the purge"
    );
    Ok(())
}

#[test]
fn an_actor_may_have_sixteen_jobs_waiting_or_running_and_no_more() -> TestResult {
    let fixture = Fixture::new()?;
    for number in 0..MAX_ACTIVE_JOBS_PER_ACTOR {
        fixture.enqueue(&format!("job{number}"), 2, 1_000)?;
    }

    let refused = fixture.enqueue("overflow", 2, 1_000);
    assert_eq!(refused.err(), Some(JobStoreError::LimitReached));
    assert_eq!(
        fixture.enqueue("job0", 2, 2_000)?.delivery,
        Delivery::Replayed,
        "a retry is not a new job and needs no room"
    );
    // The limit counts one actor's jobs. The other actor's job is not due
    // yet, so the claim below takes one of the first actor's jobs and never
    // depends on how jobs created at the same instant are ordered.
    fixture.enqueue_as(&actor("other"), "other", 2, 5_000)?;
    // Running jobs count; a job that ended makes room.
    let running = fixture.claimed("a", 1_000)?;
    assert_eq!(
        fixture.enqueue("overflow", 2, 1_000).err(),
        Some(JobStoreError::LimitReached)
    );
    fixture
        .store
        .complete_job(&running.lease, &report(&[]), at(1_100))?;
    assert_eq!(
        fixture.enqueue("overflow", 2, 1_200)?.delivery,
        Delivery::Committed
    );
    Ok(())
}

#[test]
fn the_key_space_is_shared_with_artifact_requests_in_both_directions() -> TestResult {
    let fixture = Fixture::new()?;
    // A key an artifact request used cannot enqueue a job.
    assert_eq!(
        fixture.enqueue("create", 2, 1_000).err(),
        Some(JobStoreError::RequestKeyReused)
    );
    // A key a job used cannot create an artifact or save a revision.
    fixture.enqueue("job-key", 2, 1_000)?;
    assert_eq!(
        create_demo(&fixture.store, &fixture.owner, &key("job-key"), "T", "<p/>").err(),
        Some(StoreError::RequestKeyReused)
    );
    assert_eq!(
        revise(
            &fixture.store,
            &fixture.created,
            &key("job-key"),
            2,
            "<p>x</p>"
        )
        .err(),
        Some(StoreError::RequestKeyReused)
    );
    assert_eq!(
        update_metadata(
            &fixture.store,
            &fixture.created,
            &key("job-key"),
            1,
            Some("Renamed"),
            None,
            None
        )
        .err(),
        Some(StoreError::RequestKeyReused)
    );
    // Another actor's use of the same text is a different key.
    fixture.enqueue_as(&actor("other"), "job-key", 2, 1_000)?;
    Ok(())
}

#[test]
fn the_key_space_is_shared_with_cleanups_in_both_directions() -> TestResult {
    let fixture = Fixture::new()?;
    let cleanup = |name: &str| {
        let base = fixture.store.unreferenced_content()?.digest;
        fixture.store.remove_unreferenced_content(CleanupRequest {
            request: RequestIdentity {
                actor: &fixture.owner,
                key: &key(name),
                fingerprint: RequestFingerprint(digest(&format!("cleanup {base}"))),
            },
            base,
        })
    };
    cleanup("swept")?;
    assert_eq!(
        fixture.enqueue("swept", 2, 1_000).err(),
        Some(JobStoreError::RequestKeyReused)
    );
    fixture.enqueue("queued", 2, 1_000)?;
    assert_eq!(
        cleanup("queued").err(),
        Some(StoreError::RequestKeyReused),
        "a cleanup under a job's key"
    );
    Ok(())
}

// --- claim -----------------------------------------------------------------

#[test]
fn a_claim_takes_the_oldest_due_job_and_leases_it() -> TestResult {
    let fixture = Fixture::new()?;
    let old = fixture.queued("old", 1_000)?;
    let new = fixture.queued("new", 2_000)?;

    let first = fixture.claimed("a", 3_000)?;

    assert_eq!(first.job.id, old.id);
    assert_eq!(first.job.state, JobState::Running);
    assert_eq!(first.job.attempts, 1);
    assert_eq!(first.job.worker, Some(worker("a")));
    assert_eq!(first.job.lease_expires_at, Some(at(3_000 + LEASE_MS)));
    assert_eq!(first.job.started_at, Some(at(3_000)));
    assert_eq!(first.lease.fence, 1);
    let second = fixture.claimed("b", 3_000)?;
    assert_eq!(second.job.id, new.id);
    assert!(fixture.try_claim("c", 3_000)?.is_none(), "nothing is due");
    Ok(())
}

#[test]
fn a_running_job_is_not_due_until_its_lease_expires() -> TestResult {
    let fixture = Fixture::new()?;
    let job = fixture.queued("one", 1_000)?;
    let first = fixture.claimed("a", 1_000)?;
    let expiry = first.job.lease_expires_at.ok_or("no lease")?.0;

    assert!(fixture.try_claim("b", expiry - 1)?.is_none());
    let second = fixture.claimed("b", expiry)?;

    assert_eq!(second.job.id, job.id);
    assert_eq!(second.job.attempts, 2);
    assert_eq!(second.lease.fence, 2, "the token rises with each claim");
    assert_eq!(second.job.worker, Some(worker("b")));
    assert_eq!(
        second.job.started_at,
        Some(at(1_000)),
        "started_at is the first claim"
    );
    Ok(())
}

#[test]
fn two_connections_never_claim_the_same_job() -> TestResult {
    let fixture = Fixture::new()?;
    let job = fixture.queued("one", 1_000)?;
    let other = fixture.another_store()?;

    let first = claim_with(&fixture.store, "a", 2_000)?;
    let second = claim_with(&other, "b", 2_000)?;

    assert_eq!(first.claimed.map(|claimed| claimed.job.id), Some(job.id));
    assert!(second.claimed.is_none());
    Ok(())
}

#[test]
fn concurrent_workers_claim_each_job_exactly_once() -> TestResult {
    let fixture = Fixture::new()?;
    for number in 0..6 {
        fixture.queued(&format!("job{number}"), 1_000)?;
    }
    let barrier = Arc::new(Barrier::new(4));
    let mut handles = Vec::new();
    for number in 0..4 {
        let store = fixture.another_store()?;
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(
            move || -> Result<Vec<JobId>, JobStoreError> {
                barrier.wait();
                let mut mine = Vec::new();
                while let Some(claimed) = claim_with(&store, &format!("t{number}"), 2_000)?.claimed
                {
                    mine.push(claimed.job.id);
                }
                Ok(mine)
            },
        ));
    }
    let mut all = Vec::new();
    for handle in handles {
        all.extend(handle.join().map_err(|_| "a worker thread panicked")??);
    }

    assert_eq!(all.len(), 6, "every job was claimed");
    assert_eq!(all.iter().collect::<BTreeSet<_>>().len(), 6, "none twice");
    assert_eq!(fixture.count("SELECT max(attempts) FROM jobs")?, 1);
    Ok(())
}

#[test]
fn a_heartbeat_renews_the_lease() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.queued("one", 0)?;
    let held = fixture.claimed("a", 0)?;

    let checkpoint = fixture.store.heartbeat_job(&held.lease, at(900), LEASE)?;

    assert_eq!(checkpoint, Checkpoint::Continue);
    assert!(
        fixture.try_claim("b", 1_500)?.is_none(),
        "the renewed lease holds past the first expiry"
    );
    assert_eq!(fixture.claimed("b", 1_900)?.lease.fence, 2);
    Ok(())
}

// --- fencing ---------------------------------------------------------------

#[test]
fn a_worker_that_lost_its_lease_can_write_nothing() -> TestResult {
    let fixture = Fixture::new()?;
    let job = fixture.queued("one", 0)?;
    let stale = fixture.claimed("a", 0)?;
    let current = fixture.claimed("b", LEASE_MS)?;
    assert_eq!(current.job.attempts, 2);

    let lost = Some(JobStoreError::LeaseLost);
    let found = report(&[("index.html", Problem::DigestMismatch)]);
    assert_eq!(
        fixture
            .store
            .heartbeat_job(&stale.lease, at(1_100), LEASE)
            .err(),
        lost
    );
    assert_eq!(
        fixture
            .store
            .complete_job(&stale.lease, &found, at(1_100))
            .err(),
        lost
    );
    assert_eq!(
        fixture
            .store
            .fail_job(&stale.lease, JobFailure::Internal, Retry::Never, at(1_100))
            .err(),
        lost
    );
    assert_eq!(
        fixture.store.confirm_cancel(&stale.lease, at(1_100)).err(),
        lost
    );
    assert_eq!(
        fixture.store.release_job(&stale.lease, at(1_100)).err(),
        lost
    );
    // None of it changed the job or left a result.
    let after = fixture.job(&job.id)?;
    assert_eq!(after.state, JobState::Running);
    assert_eq!(after.worker, Some(worker("b")));
    assert_eq!(after.attempts, 2);
    assert_eq!(fixture.count("SELECT count(*) FROM job_findings")?, 0);

    // The current worker finishes; the stale one cannot overwrite it later.
    fixture
        .store
        .complete_job(&current.lease, &report(&[]), at(1_200))?;
    assert_eq!(
        fixture
            .store
            .complete_job(&stale.lease, &found, at(1_300))
            .err(),
        lost
    );
    let ended = fixture.job(&job.id)?;
    assert_eq!(ended.state, JobState::Succeeded);
    assert_eq!(ended.report, Some(report(&[])));
    Ok(())
}

#[test]
fn a_result_is_written_once() -> TestResult {
    let fixture = Fixture::new()?;
    let job = fixture.queued("one", 0)?;
    let held = fixture.claimed("a", 0)?;
    let found = report(&[
        ("index.html", Problem::DigestMismatch),
        ("style.css", Problem::ContentMissing),
    ]);

    let ended = fixture.store.complete_job(&held.lease, &found, at(500))?;
    let again = fixture.store.complete_job(&held.lease, &found, at(600));

    assert_eq!(ended.state, JobState::Succeeded);
    assert_eq!(ended.report, Some(found.clone()));
    assert_eq!(ended.finished_at, Some(at(500)));
    assert!(ended.worker.is_none() && ended.lease_expires_at.is_none());
    assert_eq!(again.err(), Some(JobStoreError::LeaseLost));
    assert_eq!(fixture.count("SELECT count(*) FROM job_findings")?, 2);
    assert_eq!(fixture.job(&job.id)?.finished_at, Some(at(500)));
    Ok(())
}

#[test]
fn a_job_whose_lease_expired_on_its_last_attempt_fails() -> TestResult {
    let fixture = Fixture::new()?;
    let job = fixture.queued("one", 0)?;
    let mut now = 0;
    for _ in 0..MAX_ATTEMPTS {
        fixture.claimed("a", now)?;
        now += LEASE_MS;
    }

    let outcome = claim_with(&fixture.store, "b", now)?;
    assert!(outcome.claimed.is_none(), "no fourth attempt");
    assert_eq!(
        outcome
            .settled
            .iter()
            .map(|record| &record.id)
            .collect::<Vec<_>>(),
        [&job.id],
        "the claim reports the job it failed"
    );
    assert_eq!(outcome.settled[0].state, JobState::Failed);
    assert_eq!(outcome.settled[0].failure, Some(JobFailure::LeaseExpired));

    let ended = fixture.job(&job.id)?;
    assert_eq!(ended.state, JobState::Failed);
    assert_eq!(ended.failure, Some(JobFailure::LeaseExpired));
    assert_eq!(ended.attempts, MAX_ATTEMPTS);
    assert!(ended.worker.is_none() && ended.finished_at.is_some());
    Ok(())
}

#[test]
fn an_expired_job_with_a_cancel_requested_is_cancelled_not_rerun() -> TestResult {
    let fixture = Fixture::new()?;
    let job = fixture.queued("one", 0)?;
    fixture.claimed("a", 0)?;
    fixture.cancel("stop", &job.id, 100)?;

    let outcome = claim_with(&fixture.store, "b", LEASE_MS)?;
    assert!(outcome.claimed.is_none());
    assert_eq!(
        outcome
            .settled
            .iter()
            .map(|record| &record.id)
            .collect::<Vec<_>>(),
        [&job.id],
        "the claim reports the job it cancelled"
    );
    assert_eq!(outcome.settled[0].state, JobState::Cancelled);

    let ended = fixture.job(&job.id)?;
    assert_eq!(ended.state, JobState::Cancelled);
    assert_eq!(ended.attempts, 1, "it did not run again");
    Ok(())
}

#[test]
fn a_claim_reports_the_jobs_it_ended_and_still_claims_the_next() -> TestResult {
    let fixture = Fixture::new()?;
    let doomed = fixture.queued("doomed", 0)?;
    let mut now = 0;
    for _ in 0..MAX_ATTEMPTS {
        fixture.claimed("a", now)?;
        now += LEASE_MS;
    }
    let next = fixture.queued("next", now)?;

    let outcome = claim_with(&fixture.store, "b", now)?;

    assert_eq!(
        outcome
            .settled
            .iter()
            .map(|record| &record.id)
            .collect::<Vec<_>>(),
        [&doomed.id]
    );
    let claimed = outcome.claimed.ok_or("the next job is claimed")?;
    assert_eq!(claimed.job.id, next.id);
    Ok(())
}

// --- failure and retry -----------------------------------------------------

#[test]
fn a_retryable_failure_waits_for_its_time_then_runs_again() -> TestResult {
    let fixture = Fixture::new()?;
    let job = fixture.queued("one", 0)?;
    let held = fixture.claimed("a", 0)?;

    let scheduled = fixture.store.fail_job(
        &held.lease,
        JobFailure::StorageUnavailable,
        Retry::At(at(5_000)),
        at(100),
    )?;

    assert_eq!(scheduled.state, JobState::RetryScheduled);
    assert_eq!(scheduled.run_after, at(5_000));
    assert!(scheduled.worker.is_none() && scheduled.lease_expires_at.is_none());
    assert!(scheduled.failure.is_none() && scheduled.finished_at.is_none());
    assert!(fixture.try_claim("a", 4_999)?.is_none());
    let again = fixture.claimed("a", 5_000)?;
    assert_eq!(again.job.id, job.id);
    assert_eq!(again.job.attempts, 2);
    assert_eq!(again.lease.fence, 2);
    Ok(())
}

#[test]
fn the_last_attempt_ends_failed_even_if_a_retry_was_asked_for() -> TestResult {
    let fixture = Fixture::new()?;
    let job = fixture.queued("one", 0)?;
    for attempt in 1..=MAX_ATTEMPTS {
        let held = fixture.claimed("a", 0)?;
        let record = fixture.store.fail_job(
            &held.lease,
            JobFailure::StorageUnavailable,
            Retry::At(at(0)),
            at(10),
        )?;
        let last = attempt == MAX_ATTEMPTS;
        assert_eq!(
            record.state,
            if last {
                JobState::Failed
            } else {
                JobState::RetryScheduled
            },
            "attempt {attempt}"
        );
    }
    let ended = fixture.job(&job.id)?;
    assert_eq!(ended.failure, Some(JobFailure::StorageUnavailable));
    assert_eq!(ended.finished_at, Some(at(10)));
    assert!(fixture.try_claim("a", 1_000_000)?.is_none());
    Ok(())
}

#[test]
fn a_failure_that_is_not_retried_ends_the_job_at_once() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.queued("one", 0)?;
    let held = fixture.claimed("a", 0)?;

    let ended = fixture.store.fail_job(
        &held.lease,
        JobFailure::RevisionUnavailable,
        Retry::Never,
        at(10),
    )?;

    assert_eq!(ended.state, JobState::Failed);
    assert_eq!(ended.failure, Some(JobFailure::RevisionUnavailable));
    assert_eq!(ended.attempts, 1);
    Ok(())
}

// --- cancel ----------------------------------------------------------------

#[test]
fn a_job_that_has_not_started_is_cancelled_at_once() -> TestResult {
    let fixture = Fixture::new()?;
    let scheduled = fixture.queued("scheduled", 0)?;
    let claimed = fixture.claimed("a", 0)?;
    assert_eq!(claimed.job.id, scheduled.id);
    fixture.store.fail_job(
        &claimed.lease,
        JobFailure::StorageUnavailable,
        Retry::At(at(9_000)),
        at(1),
    )?;
    assert_eq!(fixture.state(&scheduled.id)?, JobState::RetryScheduled);
    let waiting = fixture.queued("waiting", 2)?;

    let cancelled = fixture.cancel("c1", &waiting.id, 10)?;
    let cancelled_scheduled = fixture.cancel("c2", &scheduled.id, 10)?;

    assert_eq!(cancelled.delivery, Delivery::Committed);
    assert_eq!(cancelled.job.state, JobState::Cancelled);
    assert_eq!(cancelled.job.finished_at, Some(at(10)));
    assert_eq!(cancelled_scheduled.job.state, JobState::Cancelled);
    assert!(fixture.try_claim("a", 100_000)?.is_none());
    Ok(())
}

#[test]
fn cancelling_a_running_job_asks_its_worker_to_stop_and_it_confirms() -> TestResult {
    let fixture = Fixture::new()?;
    let job = fixture.queued("one", 0)?;
    let held = fixture.claimed("a", 0)?;

    let asked = fixture.cancel("stop", &job.id, 10)?;

    assert_eq!(asked.job.state, JobState::Running, "it is not stopped yet");
    assert!(asked.job.cancel_requested);
    // A second request with another key changes nothing and is not an error.
    let again = fixture.cancel("stop-again", &job.id, 20)?;
    assert!(again.job.cancel_requested && again.job.state == JobState::Running);
    // The worker sees it at its next checkpoint, then confirms.
    assert_eq!(
        fixture.store.heartbeat_job(&held.lease, at(30), LEASE)?,
        Checkpoint::CancelRequested
    );
    let ended = fixture.store.confirm_cancel(&held.lease, at(40))?;
    assert_eq!(ended.state, JobState::Cancelled);
    assert_eq!(ended.finished_at, Some(at(40)));
    assert!(ended.worker.is_none() && ended.lease_expires_at.is_none());
    Ok(())
}

#[test]
fn a_result_that_arrives_after_a_cancel_request_is_dropped() -> TestResult {
    let fixture = Fixture::new()?;
    let job = fixture.queued("one", 0)?;
    let held = fixture.claimed("a", 0)?;
    fixture.cancel("stop", &job.id, 10)?;

    let ended = fixture.store.complete_job(
        &held.lease,
        &report(&[("index.html", Problem::SizeMismatch)]),
        at(20),
    )?;

    assert_eq!(ended.state, JobState::Cancelled);
    assert!(ended.report.is_none());
    assert_eq!(fixture.count("SELECT count(*) FROM job_findings")?, 0);
    assert_eq!(
        fixture.count("SELECT count(*) FROM jobs WHERE files_checked IS NOT NULL")?,
        0
    );
    Ok(())
}

#[test]
fn a_failure_after_a_cancel_request_ends_cancelled_without_a_retry() -> TestResult {
    let fixture = Fixture::new()?;
    let job = fixture.queued("one", 0)?;
    let held = fixture.claimed("a", 0)?;
    fixture.cancel("stop", &job.id, 10)?;

    let ended = fixture.store.fail_job(
        &held.lease,
        JobFailure::StorageUnavailable,
        Retry::At(at(100)),
        at(20),
    )?;

    assert_eq!(ended.state, JobState::Cancelled);
    assert!(ended.failure.is_none());
    Ok(())
}

#[test]
fn confirming_a_cancel_nobody_asked_for_is_refused() -> TestResult {
    let fixture = Fixture::new()?;
    let job = fixture.queued("one", 0)?;
    let held = fixture.claimed("a", 0)?;

    let result = fixture.store.confirm_cancel(&held.lease, at(10));

    assert_eq!(
        result.err(),
        Some(JobStoreError::Conflict(JobState::Running))
    );
    assert_eq!(fixture.state(&job.id)?, JobState::Running);
    Ok(())
}

#[test]
fn a_job_that_ended_cannot_be_cancelled_and_records_no_key() -> TestResult {
    let fixture = Fixture::new()?;
    let job = fixture.queued("one", 0)?;
    let held = fixture.claimed("a", 0)?;
    fixture
        .store
        .complete_job(&held.lease, &report(&[]), at(10))?;
    let keys_before = fixture.count("SELECT count(*) FROM job_requests")?;

    let refused = fixture.cancel("late", &job.id, 20);

    assert_eq!(
        refused.err(),
        Some(JobStoreError::Conflict(JobState::Succeeded))
    );
    assert_eq!(
        fixture.count("SELECT count(*) FROM job_requests")?,
        keys_before,
        "the refused request left no key behind"
    );
    // The key is still free for a request that works.
    let other = fixture.queued("two", 30)?;
    assert_eq!(
        fixture.cancel("late", &other.id, 40)?.job.state,
        JobState::Cancelled
    );
    Ok(())
}

#[test]
fn a_retried_cancel_replays_and_the_key_cannot_serve_another_job() -> TestResult {
    let fixture = Fixture::new()?;
    let one = fixture.queued("one", 0)?;
    let two = fixture.queued("two", 0)?;

    let first = fixture.cancel("stop", &one.id, 10)?;
    let replay = fixture.cancel("stop", &one.id, 20)?;
    let other = fixture.cancel("stop", &two.id, 30);

    assert_eq!(first.delivery, Delivery::Committed);
    assert_eq!(replay.delivery, Delivery::Replayed);
    assert_eq!(replay.job.id, one.id);
    assert_eq!(other.err(), Some(JobStoreError::RequestKeyReused));
    assert_eq!(fixture.state(&two.id)?, JobState::Queued);
    Ok(())
}

#[test]
fn only_the_job_owner_can_cancel_and_other_requests_are_not_found() -> TestResult {
    let fixture = Fixture::new()?;
    let job = fixture.queued("one", 0)?;
    let stranger = actor("stranger");
    let unknown: JobId = parsed(&format!("job_{}", "0".repeat(32)));

    assert_eq!(
        fixture.cancel_as(&stranger, "x", &job.id, 10).err(),
        Some(JobStoreError::NotFound)
    );
    assert_eq!(
        fixture.cancel("y", &unknown, 10).err(),
        Some(JobStoreError::NotFound)
    );
    assert_eq!(fixture.state(&job.id)?, JobState::Queued);
    fixture.trash()?;
    assert_eq!(
        fixture.cancel("z", &job.id, 10).err(),
        Some(JobStoreError::NotFound),
        "a job of an artifact in the trash"
    );
    Ok(())
}

// --- release ---------------------------------------------------------------

#[test]
fn a_released_job_goes_back_to_the_queue_without_using_an_attempt() -> TestResult {
    let fixture = Fixture::new()?;
    let job = fixture.queued("one", 0)?;
    let held = fixture.claimed("a", 0)?;

    let released = fixture.store.release_job(&held.lease, at(50))?;

    assert_eq!(released.state, JobState::Queued);
    assert_eq!(released.attempts, 0, "the attempt is given back");
    assert!(released.worker.is_none() && released.lease_expires_at.is_none());
    // The run that was released cannot write anything any more.
    assert_eq!(
        fixture
            .store
            .complete_job(&held.lease, &report(&[]), at(60))
            .err(),
        Some(JobStoreError::LeaseLost)
    );
    let next = fixture.claimed("b", 60)?;
    assert_eq!(next.job.id, job.id);
    assert_eq!((next.job.attempts, next.lease.fence), (1, 2));
    Ok(())
}

#[test]
fn releasing_a_job_with_a_cancel_requested_cancels_it() -> TestResult {
    let fixture = Fixture::new()?;
    let job = fixture.queued("one", 0)?;
    let held = fixture.claimed("a", 0)?;
    fixture.cancel("stop", &job.id, 10)?;

    let ended = fixture.store.release_job(&held.lease, at(20))?;

    assert_eq!(ended.state, JobState::Cancelled);
    Ok(())
}

// --- reading ---------------------------------------------------------------

#[test]
fn jobs_list_newest_first_by_page_for_one_actor() -> TestResult {
    let fixture = Fixture::new()?;
    let a = fixture.queued("a", 1_000)?;
    let b = fixture.queued("b", 2_000)?;
    let c = fixture.queued("c", 2_000)?;
    fixture.enqueue_as(&actor("other"), "o", 2, 3_000)?;
    let list = |after: Option<&JobPosition>, limit: usize| {
        fixture.store.list_jobs(JobListing {
            actor: &fixture.owner,
            artifact: None,
            after,
            limit,
        })
    };
    let (earlier, later) = if b.id < c.id { (&b, &c) } else { (&c, &b) };

    let first = list(None, 2)?;
    assert_eq!(
        first.iter().map(|job| &job.id).collect::<Vec<_>>(),
        [&earlier.id, &later.id],
        "newest first; the same time by identifier"
    );
    let position = JobPosition {
        created_at: first[1].created_at,
        id: first[1].id.clone(),
    };
    let rest = list(Some(&position), 2)?;
    assert_eq!(rest.iter().map(|job| &job.id).collect::<Vec<_>>(), [&a.id]);
    assert_eq!(list(None, 100)?.len(), 3, "the other actor's job is absent");
    Ok(())
}

#[test]
fn a_list_of_one_artifact_leaves_out_the_trash_and_purged_artifacts() -> TestResult {
    let fixture = Fixture::new()?;
    let kept = create_demo(
        &fixture.store,
        &fixture.owner,
        &key("kept"),
        "Kept",
        "<p>k</p>",
    )?;
    let name = key("kept-job");
    fixture.store.enqueue_job(NewJob {
        request: RequestIdentity {
            actor: &fixture.owner,
            key: &name,
            fingerprint: RequestFingerprint(digest("kept")),
        },
        artifact: &kept.artifact.id,
        revision: SourceRevision(1),
        kind: JobKind::VerifyRevision,
        now: at(1_000),
    })?;
    let doomed = fixture.queued("doomed", 2_000)?;
    let list = |artifact: Option<&ArtifactId>| {
        fixture.store.list_jobs(JobListing {
            actor: &fixture.owner,
            artifact,
            after: None,
            limit: 100,
        })
    };
    assert_eq!(list(None)?.len(), 2);
    assert_eq!(list(Some(fixture.id()))?.len(), 1);

    fixture.trash()?;
    assert_eq!(list(None)?.len(), 1, "the trashed artifact's job is hidden");
    assert!(list(Some(fixture.id()))?.is_empty());
    // The job itself is kept; it is the application that hides it.
    assert_eq!(fixture.state(&doomed.id)?, JobState::Queued);

    update_metadata(
        &fixture.store,
        &fixture.created,
        &key("recover"),
        2,
        None,
        None,
        Some(false),
    )?;
    assert_eq!(list(None)?.len(), 2, "recovery brings the job back");
    Ok(())
}

// --- trash and purge -------------------------------------------------------

impl Fixture {
    fn recover(&self) -> Result<(), StoreError> {
        update_metadata(
            &self.store,
            &self.created,
            &key("recover-all"),
            2,
            None,
            None,
            Some(false),
        )
        .map(|_| ())
    }
}

/// The reproduction of a review finding: sixteen queued jobs whose artifact
/// is then trashed. They cannot be seen, cancelled, or run, so they must not
/// also block the actor from queueing work on another artifact.
#[test]
fn jobs_of_an_artifact_in_the_trash_do_not_count_toward_the_cap_and_are_not_claimed() -> TestResult
{
    let fixture = Fixture::new()?;
    let mut ids = Vec::new();
    for number in 0..MAX_ACTIVE_JOBS_PER_ACTOR {
        ids.push(
            fixture
                .enqueue(&format!("job{number}"), 2, 1_000 + i64::try_from(number)?)?
                .job
                .id,
        );
    }
    assert_eq!(
        fixture.enqueue("overflow", 2, 2_000).err(),
        Some(JobStoreError::LimitReached)
    );

    fixture.trash()?;

    let other = create_demo(&fixture.store, &fixture.owner, &key("create-b"), "B", FIRST)?;
    let request_key = key("on-b");
    let on_other = fixture.store.enqueue_job(NewJob {
        request: RequestIdentity {
            actor: &fixture.owner,
            key: &request_key,
            fingerprint: RequestFingerprint(digest("on-b")),
        },
        artifact: &other.artifact.id,
        revision: SourceRevision(1),
        kind: JobKind::VerifyRevision,
        now: at(3_000),
    })?;
    assert_eq!(
        on_other.delivery,
        Delivery::Committed,
        "the cap counts visible jobs"
    );

    let outcome = claim_with(&fixture.store, "a", 4_000)?;
    assert_eq!(
        outcome.claimed.map(|claimed| claimed.job.id),
        Some(on_other.job.id),
        "only the job of the artifact in place is taken"
    );
    assert!(outcome.settled.is_empty());
    for id in &ids {
        assert_eq!(fixture.state(id)?, JobState::Queued, "left untouched");
    }
    assert!(fixture.try_claim("a", 4_500)?.is_none());

    // Recovery makes them claimable again, oldest first; nothing was lost.
    fixture.recover()?;
    let after = fixture.claimed("b", 6_000)?;
    assert_eq!(after.job.id, ids[0]);
    Ok(())
}

#[test]
fn a_running_job_of_a_trashed_artifact_is_left_alone_when_its_lease_expires() -> TestResult {
    let fixture = Fixture::new()?;
    let job = fixture.queued("one", 0)?;
    let first = fixture.claimed("a", 0)?;
    fixture.trash()?;

    let outcome = claim_with(&fixture.store, "b", LEASE_MS)?;
    assert!(outcome.claimed.is_none() && outcome.settled.is_empty());
    let waiting = fixture.job(&job.id)?;
    assert_eq!(
        waiting.state,
        JobState::Running,
        "not taken over while hidden"
    );
    assert_eq!(waiting.attempts, 1);

    fixture.recover()?;
    let second = fixture.claimed("b", LEASE_MS + 1)?;
    assert_eq!(second.job.id, job.id);
    assert_eq!(second.job.attempts, 2);
    assert!(second.lease.fence > first.lease.fence);
    Ok(())
}

/// An idle poll reads; it must not queue for the write lock behind a writer.
#[test]
fn an_idle_claim_does_not_take_the_write_lock() -> TestResult {
    let fixture = Fixture::new()?;
    let writer = fixture.sql()?;
    writer.execute_batch("BEGIN IMMEDIATE")?;

    let started = std::time::Instant::now();
    let outcome = claim_with(&fixture.store, "a", 1_000)?;

    assert!(outcome.claimed.is_none() && outcome.settled.is_empty());
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "it did not wait for the lock"
    );
    writer.execute_batch("ROLLBACK")?;
    Ok(())
}

#[test]
fn trashing_an_artifact_does_not_stop_a_running_job() -> TestResult {
    let fixture = Fixture::new()?;
    let job = fixture.queued("one", 0)?;
    let held = fixture.claimed("a", 0)?;

    fixture.trash()?;
    let ended = fixture
        .store
        .complete_job(&held.lease, &report(&[]), at(10))?;

    assert_eq!(ended.state, JobState::Succeeded);
    assert_eq!(fixture.state(&job.id)?, JobState::Succeeded);
    Ok(())
}

#[test]
fn a_purge_ends_open_jobs_revokes_leases_and_erases_results() -> TestResult {
    let fixture = Fixture::new()?;
    let finished = fixture.queued("finished", 0)?;
    let running = fixture.queued("running", 1)?;
    let waiting = fixture.queued("waiting", 2)?;
    let scheduled = fixture.queued("scheduled", 3)?;
    let done = fixture.claimed("a", 0)?;
    assert_eq!(done.job.id, finished.id);
    fixture.store.complete_job(
        &done.lease,
        &report(&[("index.html", Problem::DigestMismatch)]),
        at(5),
    )?;
    let held = fixture.claimed("a", 10)?;
    assert_eq!(held.job.id, running.id);
    let retry = fixture.claimed("b", 10)?;
    assert_eq!(retry.job.id, waiting.id);
    fixture.store.fail_job(
        &retry.lease,
        JobFailure::StorageUnavailable,
        Retry::At(at(99_000)),
        at(11),
    )?;
    let _ = scheduled;
    assert_eq!(fixture.count("SELECT count(*) FROM job_findings")?, 1);
    fixture.trash()?;

    fixture.purge()?;

    // The purge also raised the fencing token of the job a worker held.
    let fence: i64 = fixture.sql()?.query_row(
        "SELECT fence FROM jobs WHERE id = ?1",
        [running.id.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(fence, i64::try_from(held.lease.fence)? + 1);

    for (id, name) in [
        (&running.id, "running"),
        (&waiting.id, "retry-scheduled"),
        (&scheduled.id, "queued"),
    ] {
        let ended = fixture.job(id)?;
        assert_eq!(ended.state, JobState::Cancelled, "{name}");
        assert!(
            ended.worker.is_none() && ended.lease_expires_at.is_none(),
            "{name}"
        );
        assert!(ended.finished_at.is_some(), "{name}");
    }
    // The finished job keeps its header and loses what it found.
    let kept = fixture.job(&finished.id)?;
    assert_eq!(kept.state, JobState::Succeeded);
    assert!(kept.report.is_none());
    assert_eq!(fixture.count("SELECT count(*) FROM job_findings")?, 0);
    assert_eq!(
        fixture.count(
            "SELECT count(*) FROM jobs
             WHERE files_checked IS NOT NULL OR bytes_checked IS NOT NULL
                OR mismatch_count IS NOT NULL"
        )?,
        0
    );
    // The worker that held a lease learns of it and writes nothing.
    assert_eq!(
        fixture
            .store
            .heartbeat_job(&held.lease, at(30), LEASE)
            .err(),
        Some(JobStoreError::LeaseLost)
    );
    assert_eq!(
        fixture
            .store
            .complete_job(&held.lease, &report(&[]), at(30))
            .err(),
        Some(JobStoreError::LeaseLost)
    );
    assert!(
        fixture.try_claim("c", 1_000_000)?.is_none(),
        "nothing left to run"
    );
    // Retries and new requests are not found; the old keys stay reserved.
    assert_eq!(
        fixture.enqueue("running", 2, 40).err(),
        Some(JobStoreError::NotFound)
    );
    assert_eq!(fixture.count("SELECT count(*) FROM job_requests")?, 4);
    Ok(())
}

/// What a purge erases is overwritten in the file, as for content (0029): a
/// finding names a file of the purged artifact, so after the log is
/// checkpointed neither the file nor the log may hold the name.
#[test]
fn the_findings_of_a_purged_artifact_are_erased_from_the_database_file() -> TestResult {
    let marker = "finding-marker-3d5e8a21.html";
    let fixture = Fixture::new()?;
    let created = fixture.store.create_artifact(NewArtifact {
        request: RequestIdentity {
            actor: &fixture.owner,
            key: &key("marked"),
            fingerprint: RequestFingerprint(digest("marked")),
        },
        kind: ArtifactKind::App,
        title: &parsed("Marked"),
        package: &package(&[("index.html", "<p>x</p>"), (marker, "<p>y</p>")]),
        source: None,
    })?;
    let name = key("verify-marked");
    let job = fixture.store.enqueue_job(NewJob {
        request: RequestIdentity {
            actor: &fixture.owner,
            key: &name,
            fingerprint: RequestFingerprint(digest("verify-marked")),
        },
        artifact: &created.artifact.id,
        revision: SourceRevision(1),
        kind: JobKind::VerifyRevision,
        now: at(0),
    })?;
    let held = fixture.claimed("a", 0)?;
    assert_eq!(held.job.id, job.job.id);
    fixture.store.complete_job(
        &held.lease,
        &report(&[(marker, Problem::DigestMismatch)]),
        at(1),
    )?;
    let checkpoint = || -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let connection = fixture.sql()?;
        connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        let mut bytes = std::fs::read(database_in(fixture.dir.path()))?;
        let log = fixture.dir.join("artifree.sqlite3-wal");
        if log.exists() {
            bytes.extend(std::fs::read(log)?);
        }
        Ok(bytes)
    };
    let contains = |bytes: &[u8]| {
        bytes
            .windows(marker.len())
            .any(|window| window == marker.as_bytes())
    };
    assert!(
        contains(&checkpoint()?),
        "the test premise: the name is stored"
    );
    update_metadata(
        &fixture.store,
        &created,
        &key("delete-marked"),
        1,
        None,
        None,
        Some(true),
    )?;

    fixture.store.purge_artifact(PurgeRequest {
        request: RequestIdentity {
            actor: &fixture.owner,
            key: &key("purge-marked"),
            fingerprint: RequestFingerprint(digest("purge-marked")),
        },
        artifact: &created.artifact.id,
        base: MetadataRevision(2),
    })?;

    assert!(
        !contains(&checkpoint()?),
        "a finding survives in the database file after the purge"
    );
    Ok(())
}

// --- the stored shape ------------------------------------------------------

#[test]
fn the_database_refuses_job_rows_the_state_machine_cannot_produce() -> TestResult {
    let fixture = Fixture::new()?;
    let job = fixture.queued("one", 0)?;
    let sql = fixture.sql()?;
    let id = job.id.as_str();
    for (what, statement) in [
        (
            "running without a lease",
            "UPDATE jobs SET state = 'running' WHERE id = ?1",
        ),
        (
            "an unknown state",
            "UPDATE jobs SET state = 'paused' WHERE id = ?1",
        ),
        (
            "failed without a failure",
            "UPDATE jobs SET state = 'failed', finished_at = 1 WHERE id = ?1",
        ),
        (
            "more attempts than allowed",
            "UPDATE jobs SET attempts = 4 WHERE id = ?1",
        ),
        (
            "a result on a job that did not succeed",
            "UPDATE jobs SET files_checked = 1 WHERE id = ?1",
        ),
        (
            "a finish time on a waiting job",
            "UPDATE jobs SET finished_at = 1 WHERE id = ?1",
        ),
    ] {
        assert!(sql.execute(statement, [id]).is_err(), "accepted {what}");
    }
    Ok(())
}

/// Content digests identify file content, and the unreferenced-content rule
/// (decision 0029) decides what may be deleted by looking at the tables that
/// hold them. A new binary column would be a new place where a digest may
/// hide, so it must be added to the allowance below on purpose, with that
/// rule in mind.
#[test]
fn no_unreviewed_binary_column_could_hide_a_content_digest() -> TestResult {
    let fixture = Fixture::new()?;
    let connection = fixture.sql()?;
    let tables: Vec<String> = connection
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite!_%' ESCAPE '!'")?
        .query_map([], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    let mut binary = BTreeSet::new();
    for table in &tables {
        let mut columns = connection.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
        let rows = columns.query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, String>(2)?))
        })?;
        for row in rows {
            let (column, declared) = row?;
            if declared.eq_ignore_ascii_case("BLOB") || declared.is_empty() {
                binary.insert(format!("{table}.{column}"));
            }
        }
    }
    let reviewed: BTreeSet<String> = [
        // Content and its digest, and the files that refer to it.
        "blobs.digest",
        "blobs.content",
        "revision_files.digest",
        // Request fingerprints hash a request, never file content.
        "request_keys.fingerprint",
        "cleanup_runs.fingerprint",
        "job_requests.fingerprint",
        // The feed's identity is random.
        "feed.id",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let unreviewed: Vec<_> = binary.difference(&reviewed).collect();
    assert!(
        unreviewed.is_empty(),
        "binary columns the unreferenced-content rule has not been reviewed for: {unreviewed:?}"
    );
    Ok(())
}
