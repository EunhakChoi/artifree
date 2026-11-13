//! The worker's behavior against a real `SQLite` store (decision 0030): what
//! it records for a verified revision, how it retries, stops, and loses a
//! lease. Time is a fake clock and every race is a hook at a known point, so
//! no test sleeps or depends on scheduling.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};

use artifree_core::jobs::Clock;
use artifree_core::worker::{Pace, Step, WorkerConfig, run_next};
use artifree_domain::SourceRevision;
use artifree_domain::change::ChangeSequence;
use artifree_domain::identity::{ActorId, ArtifactId, UnixMillis};
use artifree_domain::job::VerifyReport;
use artifree_domain::job::{
    AttemptIdentity, CheckpointAnswer, JobFailure, JobId, JobKind, JobState, MAX_ATTEMPTS,
    MAX_FINDINGS, Problem, WorkerId,
};
use artifree_domain::package::{FilePackage, PackagePath};
use artifree_domain::{ArtifactKind, MetadataRevision};
use artifree_ports::artifact_store::{
    ArtifactListing, ArtifactRecord, ArtifactStore, ChangePage, CleanupReceipt, CleanupRequest,
    CommitReceipt, ConditionalCommit, MetadataReceipt, MetadataUpdate, NewArtifact, PurgeReceipt,
    PurgeRequest, RequestFingerprint, RequestIdentity, RevisionCommit, RevisionRecord, StoreError,
    UnreferencedContent,
};
use artifree_ports::job_store::{
    AttemptRow, Checkpoint, Claim, JobCancel, JobClaim, JobListing, JobReceipt, JobRecord,
    JobStore, JobStoreError, Lease, NewJob, NewProfileJob, ProfileBinding, ProfileCounts, Retry,
    SettleOutcome,
};
use artifree_providers::storage::{DATABASE_FILE, SqliteStore};
use rusqlite::Connection;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const LEASE: u64 = 1_000;

// --- fixtures --------------------------------------------------------------

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "artifree-r01-worker-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path)
            .unwrap_or_else(|error| panic!("cannot create {}: {error}", path.display()));
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug, Default)]
struct FakeClock(AtomicI64);

impl FakeClock {
    fn advance(&self, ms: i64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }

    fn set(&self, ms: i64) {
        self.0.store(ms, Ordering::SeqCst);
    }
}

impl Clock for FakeClock {
    fn now(&self) -> UnixMillis {
        UnixMillis(self.0.load(Ordering::SeqCst))
    }
}

/// Failures to inject, each counted down as it fires.
#[derive(Default)]
struct Faults {
    file_reads_unavailable: Cell<u32>,
    file_reads_internal: Cell<u32>,
    revision_missing: Cell<bool>,
    claims_unavailable: Cell<u32>,
    completes_unavailable: Cell<u32>,
    /// Run once right after a successful claim.
    after_claim: RefCell<Option<Box<dyn FnOnce()>>>,
}

fn take(counter: &Cell<u32>) -> bool {
    let left = counter.get();
    counter.set(left.saturating_sub(1));
    left > 0
}

/// A real store that fails on request.
struct Flaky {
    inner: SqliteStore,
    faults: Faults,
}

impl ArtifactStore for Flaky {
    fn create_artifact(&self, request: NewArtifact<'_>) -> Result<CommitReceipt, StoreError> {
        self.inner.create_artifact(request)
    }
    fn commit_revision(&self, request: RevisionCommit<'_>) -> Result<CommitReceipt, StoreError> {
        self.inner.commit_revision(request)
    }
    fn commit_edits(&self, request: ConditionalCommit<'_>) -> Result<CommitReceipt, StoreError> {
        self.inner.commit_edits(request)
    }
    fn update_metadata(&self, request: MetadataUpdate<'_>) -> Result<MetadataReceipt, StoreError> {
        self.inner.update_metadata(request)
    }
    fn replay_create(
        &self,
        request: RequestIdentity<'_>,
    ) -> Result<Option<CommitReceipt>, StoreError> {
        self.inner.replay_create(request)
    }
    fn purge_artifact(&self, request: PurgeRequest<'_>) -> Result<PurgeReceipt, StoreError> {
        self.inner.purge_artifact(request)
    }
    fn unreferenced_content(&self) -> Result<UnreferencedContent, StoreError> {
        self.inner.unreferenced_content()
    }
    fn remove_unreferenced_content(
        &self,
        request: CleanupRequest<'_>,
    ) -> Result<CleanupReceipt, StoreError> {
        self.inner.remove_unreferenced_content(request)
    }
    fn artifact(&self, id: &ArtifactId) -> Result<Option<ArtifactRecord>, StoreError> {
        self.inner.artifact(id)
    }
    fn list_artifacts(
        &self,
        listing: ArtifactListing<'_>,
    ) -> Result<Vec<ArtifactRecord>, StoreError> {
        self.inner.list_artifacts(listing)
    }
    fn revisions(&self, id: &ArtifactId) -> Result<Vec<RevisionRecord>, StoreError> {
        self.inner.revisions(id)
    }
    fn revision(
        &self,
        id: &ArtifactId,
        revision: SourceRevision,
    ) -> Result<Option<RevisionRecord>, StoreError> {
        if self.faults.revision_missing.get() {
            return Ok(None);
        }
        self.inner.revision(id, revision)
    }
    fn revision_package(
        &self,
        id: &ArtifactId,
        revision: SourceRevision,
    ) -> Result<Option<FilePackage>, StoreError> {
        self.inner.revision_package(id, revision)
    }
    fn revision_file(
        &self,
        id: &ArtifactId,
        revision: SourceRevision,
        path: &PackagePath,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        if take(&self.faults.file_reads_unavailable) {
            return Err(StoreError::Unavailable("injected".to_owned()));
        }
        if take(&self.faults.file_reads_internal) {
            return Err(StoreError::NotFound);
        }
        self.inner.revision_file(id, revision, path)
    }
    fn changes(
        &self,
        actor: &ActorId,
        after: ChangeSequence,
        limit: usize,
    ) -> Result<ChangePage, StoreError> {
        self.inner.changes(actor, after, limit)
    }
}

impl JobStore for Flaky {
    fn enqueue_job(&self, request: NewJob<'_>) -> Result<JobReceipt, JobStoreError> {
        self.inner.enqueue_job(request)
    }
    fn cancel_job(&self, request: JobCancel<'_>) -> Result<JobReceipt, JobStoreError> {
        self.inner.cancel_job(request)
    }
    fn job(&self, id: &JobId) -> Result<Option<JobRecord>, JobStoreError> {
        self.inner.job(id)
    }
    fn list_jobs(&self, listing: JobListing<'_>) -> Result<Vec<JobRecord>, JobStoreError> {
        self.inner.list_jobs(listing)
    }
    fn claim_job(&self, claim: JobClaim<'_>) -> Result<Claim, JobStoreError> {
        if take(&self.faults.claims_unavailable) {
            return Err(JobStoreError::Unavailable("injected".to_owned()));
        }
        let claimed = self.inner.claim_job(claim)?;
        if claimed.claimed.is_some()
            && let Some(hook) = self.faults.after_claim.borrow_mut().take()
        {
            hook();
        }
        Ok(claimed)
    }
    fn heartbeat_job(
        &self,
        lease: &Lease,
        now: UnixMillis,
        lease_ms: u64,
    ) -> Result<Checkpoint, JobStoreError> {
        self.inner.heartbeat_job(lease, now, lease_ms)
    }
    fn complete_job(
        &self,
        lease: &Lease,
        report: &VerifyReport,
        now: UnixMillis,
    ) -> Result<JobRecord, JobStoreError> {
        if take(&self.faults.completes_unavailable) {
            return Err(JobStoreError::Unavailable("injected".to_owned()));
        }
        self.inner.complete_job(lease, report, now)
    }
    fn fail_job(
        &self,
        lease: &Lease,
        failure: JobFailure,
        retry: Retry,
        now: UnixMillis,
    ) -> Result<JobRecord, JobStoreError> {
        self.inner.fail_job(lease, failure, retry, now)
    }
    fn confirm_cancel(&self, lease: &Lease, now: UnixMillis) -> Result<JobRecord, JobStoreError> {
        self.inner.confirm_cancel(lease, now)
    }
    fn release_job(&self, lease: &Lease, now: UnixMillis) -> Result<JobRecord, JobStoreError> {
        self.inner.release_job(lease, now)
    }
    fn enqueue_profile_job(&self, request: NewProfileJob<'_>) -> Result<JobReceipt, JobStoreError> {
        self.inner.enqueue_profile_job(request)
    }
    fn checkpoint_attempt(
        &self,
        lease: &Lease,
        attempt: &AttemptIdentity,
        now: UnixMillis,
        lease_ms: u64,
    ) -> Result<CheckpointAnswer, JobStoreError> {
        self.inner.checkpoint_attempt(lease, attempt, now, lease_ms)
    }
    fn settle_attempt(
        &self,
        attempt: &AttemptIdentity,
        now: UnixMillis,
    ) -> Result<SettleOutcome, JobStoreError> {
        self.inner.settle_attempt(attempt, now)
    }
    fn complete_profile_job(
        &self,
        lease: &Lease,
        attempt: &AttemptIdentity,
        binding: ProfileBinding<'_>,
        counts: ProfileCounts,
        now: UnixMillis,
    ) -> Result<JobRecord, JobStoreError> {
        self.inner
            .complete_profile_job(lease, attempt, binding, counts, now)
    }
    fn attempt_row(&self, attempt: &AttemptIdentity) -> Result<Option<AttemptRow>, JobStoreError> {
        self.inner.attempt_row(attempt)
    }
    fn unsettled_attempts(&self) -> Result<Vec<AttemptRow>, JobStoreError> {
        self.inner.unsettled_attempts()
    }
}

/// Hooks at the points between files.
struct Hooks<'a> {
    files_started: Cell<usize>,
    shutdown_after: Option<usize>,
    on_file: &'a dyn Fn(usize),
}

impl Pace for Hooks<'_> {
    fn shutdown_requested(&self) -> bool {
        self.shutdown_after
            .is_some_and(|after| self.files_started.get() >= after)
    }

    fn before_file(&self) {
        let number = self.files_started.get() + 1;
        self.files_started.set(number);
        (self.on_file)(number);
    }
}

fn nothing(_: usize) {}

fn finished(step: Step) -> Result<JobRecord, Box<dyn std::error::Error>> {
    match step {
        Step::Finished(record) => Ok(record),
        other => Err(format!("expected a finished job, got {other:?}").into()),
    }
}

struct Fixture {
    dir: TempDir,
    store: Flaky,
    clock: FakeClock,
    owner: ActorId,
    artifact: ArtifactId,
    config: WorkerConfig,
}

fn parsed<T: std::str::FromStr>(text: &str) -> T
where
    T::Err: std::fmt::Display,
{
    text.parse()
        .unwrap_or_else(|error| panic!("invalid fixture {text:?}: {error}"))
}

fn worker(name: &str) -> WorkerId {
    parsed(&format!("worker-{name}"))
}

impl Fixture {
    /// An artifact whose revision 1 holds the given files.
    fn with_files(files: &[(String, String)]) -> Result<Self, Box<dyn std::error::Error>> {
        let dir = TempDir::new();
        let inner = SqliteStore::open(&dir.0.join(DATABASE_FILE))?;
        let owner: ActorId = parsed("owner");
        let package = FilePackage::new(
            files
                .iter()
                .map(|(path, body)| (parsed::<PackagePath>(path), body.as_bytes().to_vec()))
                .collect(),
        )?;
        let key = parsed("test-key-create");
        let created = inner.create_artifact(NewArtifact {
            request: RequestIdentity {
                actor: &owner,
                key: &key,
                fingerprint: RequestFingerprint([1; 32]),
            },
            kind: ArtifactKind::App,
            title: &parsed("Title"),
            package: &package,
            source: None,
        })?;
        let clock = FakeClock::default();
        clock.set(1_000_000);
        Ok(Self {
            dir,
            store: Flaky {
                inner,
                faults: Faults::default(),
            },
            clock,
            owner,
            artifact: created.artifact.id,
            config: WorkerConfig::with_lease_ms(LEASE),
        })
    }

    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        Self::with_files(&[
            ("index.html".to_owned(), "<h1>index</h1>".to_owned()),
            ("style.css".to_owned(), "h1{color:red}".to_owned()),
            ("app.js".to_owned(), "let a=1;".to_owned()),
        ])
    }

    fn sql(&self) -> Result<Connection, rusqlite::Error> {
        Connection::open(self.dir.0.join(DATABASE_FILE))
    }

    fn enqueue(&self, name: &str) -> Result<JobId, Box<dyn std::error::Error>> {
        let key = parsed(&format!("test-key-{name}"));
        let receipt = self.store.inner.enqueue_job(NewJob {
            request: RequestIdentity {
                actor: &self.owner,
                key: &key,
                fingerprint: RequestFingerprint([2; 32]),
            },
            artifact: &self.artifact,
            revision: SourceRevision(1),
            kind: JobKind::VerifyRevision,
            now: self.clock.now(),
        })?;
        Ok(receipt.job.id)
    }

    fn run(&self, who: &str) -> Result<Step, JobStoreError> {
        self.run_with(
            who,
            &Hooks {
                files_started: Cell::new(0),
                shutdown_after: None,
                on_file: &nothing,
            },
        )
    }

    fn run_with(&self, who: &str, pace: &Hooks<'_>) -> Result<Step, JobStoreError> {
        run_next(
            &self.store,
            &self.clock,
            &worker(who),
            &self.config,
            pace,
            &mut |_| {},
        )
    }

    /// One round that also returns the jobs the claim ended on the way.
    fn run_settling(&self, who: &str) -> Result<(Step, Vec<JobRecord>), JobStoreError> {
        let settled = RefCell::new(Vec::new());
        let step = run_next(
            &self.store,
            &self.clock,
            &worker(who),
            &self.config,
            &Hooks {
                files_started: Cell::new(0),
                shutdown_after: None,
                on_file: &nothing,
            },
            &mut |record| settled.borrow_mut().push(record.clone()),
        )?;
        Ok((step, settled.into_inner()))
    }

    /// Claims the job and lets its worker die: the lease is left to expire.
    fn abandon(&self, who: &str) -> Result<(), Box<dyn std::error::Error>> {
        let claim = self.store.inner.claim_job(JobClaim {
            worker: &worker(who),
            now: self.clock.now(),
            lease_ms: LEASE,
            known_profiles: &[],
            execution: None,
        })?;
        claim.claimed.ok_or("no job was due")?;
        self.clock.advance(i64::try_from(LEASE)? + 1);
        Ok(())
    }

    fn job(&self, id: &JobId) -> Result<JobRecord, Box<dyn std::error::Error>> {
        Ok(self.store.inner.job(id)?.ok_or("the job is missing")?)
    }

    /// Replaces the stored bytes of the file at `path`.
    fn overwrite(&self, path: &str, bytes: &[u8]) -> Result<(), rusqlite::Error> {
        self.sql()?.execute(
            "UPDATE blobs SET content = ?3
             WHERE digest = (SELECT digest FROM revision_files
                             WHERE artifact_id = ?1 AND revision = 1 AND path = ?2)",
            rusqlite::params![self.artifact.as_str(), path, bytes],
        )?;
        Ok(())
    }

    /// Removes the stored bytes of the file at `path`, as a damaged file would.
    fn lose(&self, path: &str) -> Result<(), rusqlite::Error> {
        let connection = self.sql()?;
        connection.pragma_update(None, "foreign_keys", false)?;
        connection.execute(
            "DELETE FROM blobs
             WHERE digest = (SELECT digest FROM revision_files
                             WHERE artifact_id = ?1 AND revision = 1 AND path = ?2)",
            rusqlite::params![self.artifact.as_str(), path],
        )?;
        Ok(())
    }
}

// --- the verification ------------------------------------------------------

#[test]
fn an_intact_revision_succeeds_with_counts_and_no_findings() -> TestResult {
    let fixture = Fixture::new()?;
    let id = fixture.enqueue("one")?;

    let ended = finished(fixture.run("a")?)?;

    assert_eq!(ended.id, id);
    assert_eq!(ended.state, JobState::Succeeded);
    let report = ended.report.ok_or("no result")?;
    assert!(report.is_ok() && !report.is_truncated());
    assert_eq!(report.files_checked, 3);
    assert_eq!(
        report.bytes_checked,
        u64::try_from("<h1>index</h1>".len() + "h1{color:red}".len() + "let a=1;".len())?
    );
    assert_eq!(report.mismatch_count, 0);
    assert!(report.findings.is_empty());
    assert_eq!(ended.attempts, 1);
    assert!(matches!(fixture.run("a")?, Step::Idle), "nothing is left");
    Ok(())
}

#[test]
fn each_kind_of_damage_is_found_and_the_job_still_succeeds() -> TestResult {
    let fixture = Fixture::new()?;
    // Same length, other bytes; another length; no bytes at all.
    fixture.overwrite("index.html", b"<h1>INDEX</h1>")?;
    fixture.overwrite("style.css", b"x")?;
    fixture.lose("app.js")?;
    fixture.enqueue("one")?;

    let ended = finished(fixture.run("a")?)?;

    assert_eq!(
        ended.state,
        JobState::Succeeded,
        "the job worked; the verdict is the finding"
    );
    let report = ended.report.ok_or("no result")?;
    assert!(!report.is_ok());
    assert_eq!(report.mismatch_count, 3);
    let found: Vec<_> = report
        .findings
        .iter()
        .map(|finding| (finding.path.as_str(), finding.problem))
        .collect();
    assert_eq!(
        found,
        [
            ("app.js", Problem::ContentMissing),
            ("index.html", Problem::DigestMismatch),
            ("style.css", Problem::SizeMismatch),
        ]
    );
    assert_eq!(report.files_checked, 3, "a missing file was still examined");
    Ok(())
}

#[test]
fn findings_are_bounded_but_the_count_is_not() -> TestResult {
    let mut files: Vec<(String, String)> = (1..60)
        .map(|number| (format!("f{number:02}.html"), format!("<p>{number:02}</p>")))
        .collect();
    files.push(("index.html".to_owned(), "<p>index</p>".to_owned()));
    let fixture = Fixture::with_files(&files)?;
    fixture
        .sql()?
        .execute("UPDATE blobs SET content = x'5a'", [])?;
    fixture.enqueue("one")?;

    let ended = finished(fixture.run("a")?)?;

    let report = ended.report.ok_or("no result")?;
    assert_eq!(report.mismatch_count, 60);
    assert_eq!(report.findings.len(), MAX_FINDINGS);
    assert!(report.is_truncated());
    let paths: Vec<_> = report
        .findings
        .iter()
        .map(|finding| finding.path.as_str().to_owned())
        .collect();
    let expected: Vec<_> = files
        .iter()
        .take(MAX_FINDINGS)
        .map(|(p, _)| p.clone())
        .collect();
    assert_eq!(paths, expected, "the first in path order");
    assert_eq!(
        fixture
            .sql()?
            .query_row("SELECT count(*) FROM job_findings", [], |row| row
                .get::<_, i64>(0))?,
        i64::try_from(MAX_FINDINGS)?
    );
    Ok(())
}

// --- failure and retry -----------------------------------------------------

#[test]
fn storage_trouble_retries_with_a_doubling_backoff_until_the_attempts_run_out() -> TestResult {
    let fixture = Fixture::new()?;
    let id = fixture.enqueue("one")?;
    fixture.store.faults.file_reads_unavailable.set(3);

    // First attempt: retry in 1 s.
    let first = finished(fixture.run("a")?)?;
    assert_eq!(first.state, JobState::RetryScheduled);
    assert_eq!(first.run_after, UnixMillis(1_001_000));
    assert!(matches!(fixture.run("a")?, Step::Idle), "not due yet");
    fixture.clock.advance(999);
    assert!(
        matches!(fixture.run("a")?, Step::Idle),
        "one millisecond early"
    );
    fixture.clock.advance(1);

    // Second attempt: retry in 2 s from the failure.
    let second = finished(fixture.run("a")?)?;
    assert_eq!(second.state, JobState::RetryScheduled);
    assert_eq!(second.attempts, 2);
    assert_eq!(second.run_after, UnixMillis(1_001_000 + 2_000));
    fixture.clock.advance(2_000);

    // Third and last attempt: no retry is left.
    let third = finished(fixture.run("a")?)?;
    assert_eq!(third.state, JobState::Failed);
    assert_eq!(third.failure, Some(JobFailure::StorageUnavailable));
    assert_eq!(third.attempts, 3);
    fixture.clock.advance(1_000_000);
    assert!(matches!(fixture.run("a")?, Step::Idle));
    assert_eq!(fixture.job(&id)?.state, JobState::Failed);
    Ok(())
}

#[test]
fn a_retry_that_works_leaves_one_result() -> TestResult {
    let fixture = Fixture::new()?;
    let id = fixture.enqueue("one")?;
    fixture.overwrite("style.css", b"x")?;
    fixture.store.faults.file_reads_unavailable.set(1);
    finished(fixture.run("a")?)?;
    fixture.clock.advance(1_000);

    let ended = finished(fixture.run("b")?)?;

    assert_eq!(ended.id, id);
    assert_eq!((ended.state, ended.attempts), (JobState::Succeeded, 2));
    assert_eq!(ended.report.ok_or("no result")?.mismatch_count, 1);
    assert_eq!(
        fixture
            .sql()?
            .query_row("SELECT count(*) FROM job_findings", [], |row| row
                .get::<_, i64>(0))?,
        1,
        "findings are not duplicated by the second attempt"
    );
    Ok(())
}

#[test]
fn an_unexpected_store_error_fails_the_job_without_a_retry() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.enqueue("one")?;
    fixture.store.faults.file_reads_internal.set(1);

    let ended = finished(fixture.run("a")?)?;

    assert_eq!(ended.state, JobState::Failed);
    assert_eq!(ended.failure, Some(JobFailure::Internal));
    assert_eq!(ended.attempts, 1);
    Ok(())
}

#[test]
fn a_revision_that_is_gone_fails_the_job_without_a_retry() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.enqueue("one")?;
    fixture.store.faults.revision_missing.set(true);

    let ended = finished(fixture.run("a")?)?;

    assert_eq!(ended.state, JobState::Failed);
    assert_eq!(ended.failure, Some(JobFailure::RevisionUnavailable));
    assert_eq!(ended.attempts, 1);
    Ok(())
}

#[test]
fn a_store_error_around_the_claim_leaves_the_job_for_the_next_poll() -> TestResult {
    let fixture = Fixture::new()?;
    let id = fixture.enqueue("one")?;
    fixture.store.faults.claims_unavailable.set(1);

    assert!(matches!(
        fixture.run("a"),
        Err(JobStoreError::Unavailable(_))
    ));
    assert_eq!(fixture.job(&id)?.state, JobState::Queued);
    assert_eq!(fixture.job(&id)?.attempts, 0);
    assert_eq!(finished(fixture.run("a")?)?.state, JobState::Succeeded);
    Ok(())
}

#[test]
fn a_result_that_could_not_be_written_is_written_once_by_the_next_worker() -> TestResult {
    let fixture = Fixture::new()?;
    let id = fixture.enqueue("one")?;
    fixture.overwrite("app.js", b"y")?;
    fixture.store.faults.completes_unavailable.set(1);

    assert!(matches!(
        fixture.run("a"),
        Err(JobStoreError::Unavailable(_))
    ));
    let stuck = fixture.job(&id)?;
    assert_eq!(stuck.state, JobState::Running, "the lease is kept");
    assert!(matches!(fixture.run("b")?, Step::Idle), "and respected");

    fixture.clock.advance(i64::try_from(LEASE)?);
    let ended = finished(fixture.run("b")?)?;

    assert_eq!((ended.state, ended.attempts), (JobState::Succeeded, 2));
    assert_eq!(
        fixture
            .sql()?
            .query_row("SELECT count(*) FROM job_findings", [], |row| row
                .get::<_, i64>(0))?,
        1
    );
    Ok(())
}

// --- leases ----------------------------------------------------------------

#[test]
fn heartbeats_keep_a_slow_job_from_being_taken_over() -> TestResult {
    let fixture = Fixture::new()?;
    let id = fixture.enqueue("one")?;
    let stolen = Cell::new(false);
    // Each file takes 400 ms; the whole run takes longer than one lease.
    let on_file = |_: usize| {
        fixture.clock.advance(400);
        if matches!(
            fixture.store.inner.claim_job(JobClaim {
                worker: &worker("thief"),
                now: fixture.clock.now(),
                lease_ms: LEASE,
                known_profiles: &[],
                execution: None,
            }),
            Ok(Claim {
                claimed: Some(_),
                ..
            })
        ) {
            stolen.set(true);
        }
    };

    let step = fixture.run_with(
        "a",
        &Hooks {
            files_started: Cell::new(0),
            shutdown_after: None,
            on_file: &on_file,
        },
    )?;

    assert!(!stolen.get(), "a renewed lease was taken");
    let ended = finished(step)?;
    assert_eq!(
        (ended.id, ended.state, ended.attempts),
        (id, JobState::Succeeded, 1)
    );
    Ok(())
}

#[test]
fn a_worker_that_stalled_past_its_lease_writes_nothing_and_the_job_runs_once_more() -> TestResult {
    let fixture = Fixture::new()?;
    let id = fixture.enqueue("one")?;
    fixture.overwrite("style.css", b"x")?;
    // While worker a is between files, the lease runs out and b takes the job.
    let on_file = |number: usize| {
        if number == 2 {
            fixture.clock.advance(5_000);
            let stolen = fixture.run("b");
            assert!(
                matches!(stolen, Ok(Step::Finished(_))),
                "b finishes the job: {stolen:?}"
            );
        }
    };

    let step = fixture.run_with(
        "a",
        &Hooks {
            files_started: Cell::new(0),
            shutdown_after: None,
            on_file: &on_file,
        },
    )?;

    assert!(
        matches!(step, Step::LeaseLost(ref lost) if *lost == id),
        "{step:?}"
    );
    let ended = fixture.job(&id)?;
    assert_eq!(ended.state, JobState::Succeeded);
    assert_eq!(ended.attempts, 2, "a's attempt and b's");
    assert_eq!(ended.report.ok_or("no result")?.mismatch_count, 1);
    assert_eq!(
        fixture
            .sql()?
            .query_row("SELECT count(*) FROM job_findings", [], |row| row
                .get::<_, i64>(0))?,
        1,
        "only the worker that held the lease wrote"
    );
    Ok(())
}

// --- cancel and shutdown ---------------------------------------------------

#[test]
fn a_cancel_is_seen_at_the_next_checkpoint_and_stops_the_reading() -> TestResult {
    let fixture = Fixture::new()?;
    let id = fixture.enqueue("one")?;
    let read = Cell::new(0);
    let on_file = |number: usize| {
        read.set(number);
        if number == 1 {
            let key = parsed("test-key-stop");
            let cancelled = fixture.store.inner.cancel_job(JobCancel {
                request: RequestIdentity {
                    actor: &fixture.owner,
                    key: &key,
                    fingerprint: RequestFingerprint([4; 32]),
                },
                job: &id,
                now: fixture.clock.now(),
            });
            assert!(cancelled.is_ok(), "{cancelled:?}");
            // Past the heartbeat interval, so the next checkpoint looks.
            fixture.clock.advance(500);
        }
    };

    let step = fixture.run_with(
        "a",
        &Hooks {
            files_started: Cell::new(0),
            shutdown_after: None,
            on_file: &on_file,
        },
    )?;

    let ended = finished(step)?;
    assert_eq!(ended.state, JobState::Cancelled);
    assert!(ended.report.is_none());
    assert_eq!(read.get(), 1, "it stopped before the second file");
    Ok(())
}

#[test]
fn a_cancel_that_arrives_during_the_last_file_still_wins_over_the_result() -> TestResult {
    let fixture = Fixture::new()?;
    let id = fixture.enqueue("one")?;
    let on_file = |number: usize| {
        if number == 3 {
            let key = parsed("test-key-stop");
            let cancelled = fixture.store.inner.cancel_job(JobCancel {
                request: RequestIdentity {
                    actor: &fixture.owner,
                    key: &key,
                    fingerprint: RequestFingerprint([4; 32]),
                },
                job: &id,
                now: fixture.clock.now(),
            });
            assert!(cancelled.is_ok(), "{cancelled:?}");
        }
    };

    let step = fixture.run_with(
        "a",
        &Hooks {
            files_started: Cell::new(0),
            shutdown_after: None,
            on_file: &on_file,
        },
    )?;

    let ended = finished(step)?;
    assert_eq!(ended.state, JobState::Cancelled);
    assert!(ended.report.is_none(), "the result was dropped");
    Ok(())
}

#[test]
fn a_shutdown_gives_the_job_back_without_using_an_attempt() -> TestResult {
    let fixture = Fixture::new()?;
    let id = fixture.enqueue("one")?;

    let step = fixture.run_with(
        "a",
        &Hooks {
            files_started: Cell::new(0),
            shutdown_after: Some(1),
            on_file: &nothing,
        },
    )?;

    let Step::Released(record) = step else {
        return Err(format!("expected a release, got {step:?}").into());
    };
    assert_eq!(
        (record.id.clone(), record.state, record.attempts),
        (id.clone(), JobState::Queued, 0)
    );
    // Another worker can take it at once, and the run finishes normally.
    let ended = finished(fixture.run("b")?)?;
    assert_eq!(
        (ended.id, ended.state, ended.attempts),
        (id, JobState::Succeeded, 1)
    );
    Ok(())
}

#[test]
fn a_purge_during_a_run_revokes_the_lease_and_nothing_is_written() -> TestResult {
    let fixture = Fixture::new()?;
    let id = fixture.enqueue("one")?;
    fixture.overwrite("style.css", b"x")?;
    let on_file = |number: usize| {
        if number == 2 {
            let moved = fixture.store.inner.update_metadata(MetadataUpdate {
                request: RequestIdentity {
                    actor: &fixture.owner,
                    key: &parsed("test-key-trash"),
                    fingerprint: RequestFingerprint([5; 32]),
                },
                artifact: &fixture.artifact,
                base: MetadataRevision(1),
                title: None,
                pinned: None,
                deleted: Some(true),
            });
            assert!(moved.is_ok(), "{moved:?}");
            let purged = fixture.store.inner.purge_artifact(PurgeRequest {
                request: RequestIdentity {
                    actor: &fixture.owner,
                    key: &parsed("test-key-purge"),
                    fingerprint: RequestFingerprint([6; 32]),
                },
                artifact: &fixture.artifact,
                base: MetadataRevision(2),
            });
            assert!(purged.is_ok(), "{purged:?}");
        }
    };

    let step = fixture.run_with(
        "a",
        &Hooks {
            files_started: Cell::new(0),
            shutdown_after: None,
            on_file: &on_file,
        },
    )?;

    assert!(matches!(step, Step::LeaseLost(_)), "{step:?}");
    let ended = fixture.job(&id)?;
    assert_eq!(ended.state, JobState::Cancelled);
    assert!(ended.report.is_none());
    assert_eq!(
        fixture
            .sql()?
            .query_row("SELECT count(*) FROM job_findings", [], |row| row
                .get::<_, i64>(0))?,
        0
    );
    Ok(())
}

#[test]
fn a_job_claimed_just_before_a_purge_does_not_run_on_what_is_left() -> TestResult {
    let fixture = Fixture::new()?;
    let id = fixture.enqueue("one")?;
    let other = SqliteStore::open(&fixture.dir.0.join(DATABASE_FILE))?;
    let (owner, artifact) = (fixture.owner.clone(), fixture.artifact.clone());
    // Between the claim and the first read, the artifact is purged.
    *fixture.store.faults.after_claim.borrow_mut() = Some(Box::new(move || {
        let moved = other.update_metadata(MetadataUpdate {
            request: RequestIdentity {
                actor: &owner,
                key: &parsed("test-key-trash"),
                fingerprint: RequestFingerprint([5; 32]),
            },
            artifact: &artifact,
            base: MetadataRevision(1),
            title: None,
            pinned: None,
            deleted: Some(true),
        });
        assert!(moved.is_ok(), "{moved:?}");
        let purged = other.purge_artifact(PurgeRequest {
            request: RequestIdentity {
                actor: &owner,
                key: &parsed("test-key-purge"),
                fingerprint: RequestFingerprint([6; 32]),
            },
            artifact: &artifact,
            base: MetadataRevision(2),
        });
        assert!(purged.is_ok(), "{purged:?}");
    }));

    let step = fixture.run("a")?;

    assert!(matches!(step, Step::LeaseLost(_)), "{step:?}");
    assert_eq!(fixture.job(&id)?.state, JobState::Cancelled);
    Ok(())
}

// --- jobs a claim ends, shutdown at the end, trash during a run -------------

#[test]
fn a_job_ended_by_the_claim_is_reported_once_through_the_loop() -> TestResult {
    let fixture = Fixture::new()?;
    let exhausted = fixture.enqueue("exhausted")?;
    for number in 0..MAX_ATTEMPTS {
        fixture.abandon(&format!("dead{number}"))?;
    }
    let (step, settled) = fixture.run_settling("a")?;

    assert!(matches!(step, Step::Idle), "{step:?}");
    assert_eq!(settled.len(), 1, "{settled:?}");
    assert_eq!(settled[0].id, exhausted);
    assert_eq!(settled[0].state, JobState::Failed);
    assert_eq!(settled[0].failure, Some(JobFailure::LeaseExpired));
    // Nothing is reported twice.
    let (_, again) = fixture.run_settling("a")?;
    assert!(again.is_empty());

    let cancelled = fixture.enqueue("cancelled")?;
    fixture.abandon("dead")?;
    let key = parsed("test-key-stop");
    fixture.store.inner.cancel_job(JobCancel {
        request: RequestIdentity {
            actor: &fixture.owner,
            key: &key,
            fingerprint: RequestFingerprint([7; 32]),
        },
        job: &cancelled,
        now: fixture.clock.now(),
    })?;
    let (step, settled) = fixture.run_settling("a")?;
    assert!(matches!(step, Step::Idle), "{step:?}");
    assert_eq!(settled.len(), 1, "{settled:?}");
    assert_eq!(
        (settled[0].id.clone(), settled[0].state),
        (cancelled, JobState::Cancelled)
    );
    Ok(())
}

#[test]
fn a_job_ended_by_the_claim_is_reported_before_the_next_job_runs() -> TestResult {
    let fixture = Fixture::new()?;
    let exhausted = fixture.enqueue("exhausted")?;
    for number in 0..MAX_ATTEMPTS {
        fixture.abandon(&format!("dead{number}"))?;
    }
    let next = {
        let key = parsed("test-key-next");
        fixture
            .store
            .inner
            .enqueue_job(NewJob {
                request: RequestIdentity {
                    actor: &fixture.owner,
                    key: &key,
                    fingerprint: RequestFingerprint([8; 32]),
                },
                artifact: &fixture.artifact,
                revision: SourceRevision(1),
                kind: JobKind::VerifyRevision,
                now: fixture.clock.now(),
            })?
            .job
            .id
    };

    let (step, settled) = fixture.run_settling("a")?;

    assert_eq!(
        settled.iter().map(|job| &job.id).collect::<Vec<_>>(),
        [&exhausted]
    );
    assert_eq!(finished(step)?.id, next, "and the next job ran");
    Ok(())
}

#[test]
fn a_stop_request_after_the_last_file_still_records_the_finished_result() -> TestResult {
    let fixture = Fixture::new()?;
    let id = fixture.enqueue("one")?;

    // The flag turns on while the third and last file is read.
    let step = fixture.run_with(
        "a",
        &Hooks {
            files_started: Cell::new(0),
            shutdown_after: Some(3),
            on_file: &nothing,
        },
    )?;

    let ended = finished(step)?;
    assert_eq!(
        (ended.id, ended.state, ended.attempts),
        (id, JobState::Succeeded, 1),
        "the work was done, so it is recorded instead of rerun"
    );
    // A job that is not finished is still given back (see the test above).
    Ok(())
}

#[test]
fn trashing_the_artifact_during_a_run_does_not_stop_it() -> TestResult {
    let fixture = Fixture::new()?;
    let id = fixture.enqueue("one")?;
    let on_file = |number: usize| {
        if number == 2 {
            let moved = fixture.store.inner.update_metadata(MetadataUpdate {
                request: RequestIdentity {
                    actor: &fixture.owner,
                    key: &parsed("test-key-trash"),
                    fingerprint: RequestFingerprint([5; 32]),
                },
                artifact: &fixture.artifact,
                base: MetadataRevision(1),
                title: None,
                pinned: None,
                deleted: Some(true),
            });
            assert!(moved.is_ok(), "{moved:?}");
        }
    };

    let step = fixture.run_with(
        "a",
        &Hooks {
            files_started: Cell::new(0),
            shutdown_after: None,
            on_file: &on_file,
        },
    )?;

    let ended = finished(step)?;
    assert_eq!((ended.id, ended.state), (id, JobState::Succeeded));
    assert_eq!(ended.report.map(|report| report.files_checked), Some(3));
    Ok(())
}
