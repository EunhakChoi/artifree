//! The worker side of persistent jobs (decision 0030): claim a job, run it, and
//! write its result once.
//!
//! [`run_next`] does one job and returns; the loop, the sleeping between
//! polls, and the signal handling belong to the process that runs it. Time
//! comes from a [`Clock`], never from the wall clock, so tests move it.
//!
//! A `verify_revision` job re-reads every file of a pinned revision through the
//! artifact store, recomputes its SHA-256 digest and size, and compares them
//! with the stored listing. It runs trusted in-repository code only.
//!
//! A `run_profile` job (decision 0057) runs only through the [`Execution`] port
//! that the process gives its worker in [`Profiles`]. The worker asks the port
//! for capacity before each claim, reads the pinned revision's files through the
//! store, checks them against the listing, and launches them through the port.
//! This module knows no profile, no Linux and no provider: a worker without a
//! port claims trusted kinds and ends unknown-profile jobs only. There is no
//! network use here. Building React sources (R03) and calling model providers
//! (P01) are not implemented.
//!
//! A worker that loses its lease (it stalled past the expiry and another
//! worker claimed the job, or a purge revoked the job) learns it at its next
//! store call, which is rejected with [`JobStoreError::LeaseLost`], and writes
//! nothing.

use std::fmt;

use artifree_domain::identity::UnixMillis;
use artifree_domain::job::{
    AttemptIdentity, CheckpointAnswer, Finding, JobFailure, JobId, JobKind, JobState, MAX_FINDINGS,
    Problem, ProfileName, StopReason, VerifyReport, WorkerId,
};
use artifree_domain::package::FilePackage;
use artifree_ports::artifact_store::{ArtifactStore, StoreError};
use artifree_ports::execution::{
    BoundRun, Capacity, Checkpoint as CheckpointPort, CheckpointUnavailable, Execution,
    HeldCapacity, LaunchFailure, PinnedInput, ProfileRun,
};
use artifree_ports::job_store::{
    Checkpoint, ExecutionClaim, JobClaim, JobRecord, JobStore, JobStoreError, Lease,
    ProfileBinding, Retry, SettleOutcome,
};
use sha2::{Digest, Sha256};

use crate::jobs::Clock;

/// When a failed attempt is tried again: `base_ms` after the first attempt,
/// doubling after each, up to `cap_ms`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// The wait after the first failed attempt, in milliseconds.
    pub base_ms: u64,
    /// The longest wait, in milliseconds.
    pub cap_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            base_ms: 1_000,
            cap_ms: 60_000,
        }
    }
}

impl RetryPolicy {
    /// The wait after `attempt` failed attempts (1 for the first).
    #[must_use]
    pub fn delay_ms(&self, attempt: u32) -> u64 {
        let doublings = attempt.saturating_sub(1).min(32);
        self.base_ms
            .saturating_mul(1_u64 << doublings)
            .min(self.cap_ms)
    }
}

/// How a worker leases and retries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerConfig {
    /// How long a lease lasts from its last renewal, in milliseconds.
    pub lease_ms: u64,
    /// How often the lease is renewed while a job runs, in milliseconds. At
    /// most a third of the lease, so two renewals can be missed.
    pub heartbeat_ms: u64,
    /// The backoff between attempts.
    pub retry: RetryPolicy,
}

impl WorkerConfig {
    /// A configuration with a lease of `lease_ms` renewed every third of it.
    #[must_use]
    pub fn with_lease_ms(lease_ms: u64) -> Self {
        Self {
            lease_ms,
            heartbeat_ms: (lease_ms / 3).max(1),
            retry: RetryPolicy::default(),
        }
    }
}

/// What the process around a worker can tell it between files.
pub trait Pace {
    /// Whether the process was asked to stop. The worker then gives a job it
    /// has not finished verifying back and returns; a verification that is
    /// complete is recorded first.
    fn shutdown_requested(&self) -> bool;

    /// Called before each file is read. A test uses it to hold a job at a
    /// known point; the worker has no other use for it.
    fn before_file(&self) {}
}

/// What a worker's build runs (decision 0057 rules 1 and 15).
#[derive(Clone, Copy)]
pub struct Profiles<'a> {
    /// The profiles this build knows. Every claim passes them as
    /// `known_profiles`, so a due `run_profile` job of another profile ends as
    /// `execution_unavailable`.
    pub known: &'a [ProfileName],
    /// The execution port of a worker that has an execution directory. `None`
    /// claims trusted kinds only and spends no attempt.
    pub execution: Option<&'a dyn Execution>,
}

impl fmt::Debug for Profiles<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Profiles")
            .field("known", &self.known)
            .field("execution", &self.execution.is_some())
            .finish()
    }
}

/// What one call of [`run_next`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// No job was due.
    Idle,
    /// A job was claimed and left in the state of the record: succeeded,
    /// failed, retry scheduled, queued again, or cancelled.
    Finished(JobRecord),
    /// The process was asked to stop: the job was given back, and no attempt
    /// was used.
    Released(JobRecord),
    /// The worker lost the lease and wrote nothing.
    LeaseLost(JobId),
    /// A `run_profile` attempt ended without an observed cleanup: nothing was
    /// written, the attempt stays unsettled, and the provider is quarantined
    /// (decision 0057 rules 10 and 19).
    Unsettled(JobId),
}

/// How the verification of a claimed job ended.
enum Verified {
    Done(VerifyReport),
    Cancelled,
    Shutdown,
    LeaseLost,
}

/// What a claim of [`run_next_observed`] or [`run_next_with`] reports, in this
/// order: the jobs it ended, then the job it took, and for a `run_profile` job
/// the end of its launch.
#[derive(Debug, Clone, Copy)]
pub enum ClaimEvent<'a> {
    /// A job the claim ended because nobody can run it any more.
    Settled(&'a JobRecord),
    /// The job the claim took. Its claim has committed and it has not run yet.
    Claimed(&'a JobId),
    /// A `run_profile` attempt ended. `detail` is the provider's short word for
    /// the operator's stderr only.
    Launched {
        /// The job whose attempt ended.
        job: &'a JobId,
        /// The provider's word for the end, such as `succeeded`.
        detail: &'static str,
    },
    /// The execution port refused capacity, so no known-profile job was
    /// claimed and no attempt was spent (decision 0057 rule 19). The text is
    /// the provider's fixed word or operator text, for stderr only; a
    /// quarantine's text says what the operator must do (decision 0057 rule
    /// 13).
    Refused(&'static str),
}

/// Claims the oldest due job and runs it to an end the store records, or
/// returns [`Step::Idle`].
///
/// A claim can also end jobs that nobody can run any more: an expired lease
/// with a cancel requested, or on its last attempt. Each such job is passed to
/// `on_claim` as [`ClaimEvent::Settled`] as the claim returns, before any job
/// is run, so the process around the worker can report every job that reached
/// an end (the result is not lost if the run that follows fails). The job
/// the claim takes is passed as [`ClaimEvent::Claimed`] once its claim has
/// committed and before it runs. This worker knows no profile and has no
/// execution port (decision 0057 rule 1): see [`run_next_with`].
///
/// # Errors
///
/// [`JobStoreError::Unavailable`] when the store fails around the claim or the
/// result. The job then keeps its lease until it expires, and a later claim
/// runs it again: a result is never written twice, but a verification can run
/// more than once.
pub fn run_next_observed<S: ArtifactStore + JobStore>(
    store: &S,
    clock: &dyn Clock,
    worker: &WorkerId,
    config: &WorkerConfig,
    pace: &dyn Pace,
    on_claim: &mut dyn FnMut(ClaimEvent<'_>),
) -> Result<Step, JobStoreError> {
    run_next_with(
        store,
        clock,
        worker,
        config,
        pace,
        Profiles {
            known: &[],
            execution: None,
        },
        on_claim,
    )
}

/// Claims the oldest due job and runs it to an end the store records, or
/// returns [`Step::Idle`].
///
/// Each job that a claim ends without running is passed to `on_settled` as the
/// claim returns, before any job is run. [`run_next_observed`] reports the same
/// jobs and also the job it claims.
///
/// # Errors
///
/// The same as [`run_next_observed`].
pub fn run_next<S: ArtifactStore + JobStore>(
    store: &S,
    clock: &dyn Clock,
    worker: &WorkerId,
    config: &WorkerConfig,
    pace: &dyn Pace,
    on_settled: &mut dyn FnMut(&JobRecord),
) -> Result<Step, JobStoreError> {
    run_next_observed(store, clock, worker, config, pace, &mut |event| {
        if let ClaimEvent::Settled(record) = event {
            on_settled(record);
        }
    })
}

/// Claims the oldest due job and runs it to an end the store records, or
/// returns [`Step::Idle`], as [`run_next_observed`] does, for the build that
/// `profiles` describes.
///
/// With an execution port, the port is asked for capacity before the claim
/// (decision 0057 rule 19). When it holds capacity, the attempts its
/// reconciliation found removed or record-less are settled first, and the claim
/// records a new attempt with the capacity's identities. When the lock is busy
/// or admission refuses, the claim takes trusted kinds only and spends no
/// attempt. A held capacity is released once its job's attempt is settled, or
/// at the end of the call.
///
/// # Errors
///
/// [`JobStoreError::Unavailable`] when the store fails around the capacity, the
/// settlement, the claim or the result. The job then keeps its lease until it
/// expires, and a later claim runs it again, as [`run_next_observed`] documents.
pub fn run_next_with<S: ArtifactStore + JobStore>(
    store: &S,
    clock: &dyn Clock,
    worker: &WorkerId,
    config: &WorkerConfig,
    pace: &dyn Pace,
    profiles: Profiles<'_>,
    on_claim: &mut dyn FnMut(ClaimEvent<'_>),
) -> Result<Step, JobStoreError> {
    let mut held = match profiles.execution {
        Some(execution) => take_capacity(store, clock, execution, on_claim)?,
        None => None,
    };
    let claim = store.claim_job(JobClaim {
        worker,
        now: clock.now(),
        lease_ms: config.lease_ms,
        known_profiles: profiles.known,
        execution: held.as_deref().map(|capacity| ExecutionClaim {
            attempt: capacity.attempt(),
            tool_digest: capacity.tool_digest(),
            machine: capacity.host().machine.as_str(),
            boot: capacity.host().boot.as_str(),
            cgroupns: capacity.host().cgroupns.as_str(),
            directory: capacity.directory(),
        }),
    })?;
    for record in &claim.settled {
        on_claim(ClaimEvent::Settled(record));
    }
    let Some(claimed) = claim.claimed else {
        return Ok(Step::Idle);
    };
    let (job, lease) = (claimed.job, claimed.lease);
    on_claim(ClaimEvent::Claimed(&job.id));
    if job.kind != JobKind::RunProfile {
        // A trusted job runs without the lock.
        drop(held);
        return run_verify(store, clock, lease, &job, config, pace);
    }
    // The store claims a run_profile job only with execution, so a held
    // capacity is present here. Without one the job is given back, as the
    // other unexpected states are; it spends no attempt.
    let Some(capacity) = held.take() else {
        return match store.release_job(&lease, clock.now()) {
            Ok(record) => Ok(Step::Finished(record)),
            Err(JobStoreError::LeaseLost) => Ok(Step::LeaseLost(lease.job)),
            Err(error) => Err(error),
        };
    };
    let profile_claim = ProfileClaim {
        store,
        clock,
        config,
        pace,
        lease: &lease,
        job: &job,
    };
    run_profile(&profile_claim, capacity, on_claim)
}

/// Asks the execution port for capacity before a claim. When it holds capacity,
/// settles the attempts its reconciliation found removed or record-less and
/// returns the capacity. When it is busy or refuses, returns `None` and spends
/// nothing.
fn take_capacity<S: JobStore>(
    store: &S,
    clock: &dyn Clock,
    execution: &dyn Execution,
    on_claim: &mut dyn FnMut(ClaimEvent<'_>),
) -> Result<Option<Box<dyn HeldCapacity>>, JobStoreError> {
    let unsettled = store.unsettled_attempts()?;
    let capacity = match execution.capacity(&unsettled) {
        Capacity::Held(capacity) => capacity,
        Capacity::Busy => return Ok(None),
        Capacity::Refused(detail) => {
            on_claim(ClaimEvent::Refused(detail));
            return Ok(None);
        }
    };
    for attempt in capacity.reconciled() {
        match store.settle_attempt(attempt, clock.now()) {
            Ok(SettleOutcome::Settled(record)) if record.state != JobState::Running => {
                on_claim(ClaimEvent::Settled(&record));
            }
            Ok(_) | Err(JobStoreError::NotFound) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(Some(capacity))
}

/// Runs a claimed `verify_revision` job and records its end (decision 0030).
fn run_verify<S: ArtifactStore + JobStore>(
    store: &S,
    clock: &dyn Clock,
    lease: Lease,
    job: &JobRecord,
    config: &WorkerConfig,
    pace: &dyn Pace,
) -> Result<Step, JobStoreError> {
    let ended = match verify(store, clock, &lease, job, config, pace) {
        Ok(Verified::Done(report)) => store.complete_job(&lease, &report, clock.now()),
        Ok(Verified::Cancelled) => store.confirm_cancel(&lease, clock.now()),
        Ok(Verified::Shutdown) => {
            return match store.release_job(&lease, clock.now()) {
                Ok(record) => Ok(Step::Released(record)),
                Err(JobStoreError::LeaseLost) => Ok(Step::LeaseLost(lease.job)),
                Err(error) => Err(error),
            };
        }
        Ok(Verified::LeaseLost) => return Ok(Step::LeaseLost(lease.job)),
        Err(failure) => {
            let now = clock.now();
            let retry = if failure.is_retryable() {
                Retry::At(add_ms(now, config.retry.delay_ms(job.attempts)))
            } else {
                Retry::Never
            };
            store.fail_job(&lease, failure, retry, now)
        }
    };
    match ended {
        Ok(record) => Ok(Step::Finished(record)),
        Err(JobStoreError::LeaseLost) => Ok(Step::LeaseLost(lease.job)),
        Err(error) => Err(error),
    }
}

fn add_ms(time: UnixMillis, ms: u64) -> UnixMillis {
    UnixMillis(time.0.saturating_add(i64::try_from(ms).unwrap_or(i64::MAX)))
}

/// The parts of one claimed `run_profile` job that its run uses.
struct ProfileClaim<'a, S> {
    store: &'a S,
    clock: &'a dyn Clock,
    config: &'a WorkerConfig,
    pace: &'a dyn Pace,
    lease: &'a Lease,
    job: &'a JobRecord,
}

/// The store's checkpoint of one attempt, which the provider's guardian asks
/// before each renewal (decision 0057 rule 7).
struct StoreCheckpoint<'a, S> {
    store: &'a S,
    clock: &'a dyn Clock,
    lease: &'a Lease,
    attempt: AttemptIdentity,
    lease_ms: u64,
}

impl<S: JobStore> CheckpointPort for StoreCheckpoint<'_, S> {
    fn renew(&self) -> Result<CheckpointAnswer, CheckpointUnavailable> {
        // An attempt that is no longer this job's is a replaced claim, so the
        // guardian must stop the run.
        match self.store.checkpoint_attempt(
            self.lease,
            &self.attempt,
            self.clock.now(),
            self.lease_ms,
        ) {
            Ok(answer) => Ok(answer),
            Err(JobStoreError::NotFound) => Ok(CheckpointAnswer::Stop(StopReason::Fence)),
            Err(_) => Err(CheckpointUnavailable),
        }
    }
}

/// Runs a claimed `run_profile` job on the capacity it was claimed with (decision
/// 0057 rules 7, 10, 12, 16 and 17). The capacity is released when the attempt is
/// settled, or when it stays unsettled.
fn run_profile<S: ArtifactStore + JobStore>(
    claim: &ProfileClaim<'_, S>,
    mut capacity: Box<dyn HeldCapacity>,
    on_claim: &mut dyn FnMut(ClaimEvent<'_>),
) -> Result<Step, JobStoreError> {
    let attempt = capacity.attempt();
    let tool_digest = capacity.tool_digest().to_owned();
    let row = claim.store.attempt_row(&attempt)?;
    // The attempt row and the profile name what the run binds to. Without them
    // nothing can be bound, so the job is failed.
    let (Some(row), Some(profile)) = (row, claim.job.profile.as_ref()) else {
        return settle_and_fail(claim, capacity, JobFailure::Internal);
    };
    let input = match read_input(claim.store, claim.job, claim.pace) {
        Ok(input) => input,
        Err(failure) => return settle_and_fail(claim, capacity, failure),
    };
    let run = ProfileRun {
        job: &claim.job.id,
        fence: claim.lease.fence,
        attempt,
        actor: &claim.job.actor,
        generation: row.actor_generation,
        artifact: &claim.job.artifact,
        revision: claim.job.revision,
        profile,
        lease_ms: claim.config.lease_ms,
    };
    let checkpoint = StoreCheckpoint {
        store: claim.store,
        clock: claim.clock,
        lease: claim.lease,
        attempt,
        lease_ms: claim.config.lease_ms,
    };
    let outcome = capacity.launch(&run, input, &checkpoint);
    on_claim(ClaimEvent::Launched {
        job: &claim.job.id,
        detail: outcome.detail,
    });
    if !outcome.cleanup_observed {
        // Nothing is written: the attempt stays unsettled and the capacity, with
        // its lock, is dropped here.
        return Ok(Step::Unsettled(claim.lease.job.clone()));
    }
    let settled = settle_record(claim.store, claim.clock, &attempt)?;
    drop(capacity);
    if settled.state != JobState::Running {
        return Ok(Step::Finished(settled));
    }
    let now = claim.clock.now();
    match outcome.result {
        Ok(output)
            if output.bound == BoundRun::of(&run)
                && output.profile == *profile
                && output.tool_digest == tool_digest =>
        {
            let binding = ProfileBinding {
                profile: &output.profile,
                tool_digest: output.tool_digest.as_str(),
            };
            record_end(
                claim.store.complete_profile_job(
                    claim.lease,
                    &attempt,
                    binding,
                    output.counts,
                    now,
                ),
                &claim.job.id,
            )
        }
        Ok(_) | Err(LaunchFailure::Failed) => fail_settled(claim, JobFailure::Internal),
        Err(LaunchFailure::Stopped(_)) => Ok(Step::LeaseLost(claim.lease.job.clone())),
        Err(LaunchFailure::Unavailable) => fail_settled(claim, JobFailure::ExecutionUnavailable),
    }
}

/// Settles the attempt of a claim whose launch never started, then fails the
/// job. No record was written: the launch did not start, and the capacity still
/// holds the lock.
fn settle_and_fail<S: JobStore>(
    claim: &ProfileClaim<'_, S>,
    capacity: Box<dyn HeldCapacity>,
    failure: JobFailure,
) -> Result<Step, JobStoreError> {
    let settled = settle_record(claim.store, claim.clock, &capacity.attempt())?;
    drop(capacity);
    if settled.state != JobState::Running {
        return Ok(Step::Finished(settled));
    }
    fail_settled(claim, failure)
}

/// Ends a claimed job as failed once its attempt is settled. A failure that
/// another attempt can fix is retried after the backoff.
fn fail_settled<S: JobStore>(
    claim: &ProfileClaim<'_, S>,
    failure: JobFailure,
) -> Result<Step, JobStoreError> {
    let now = claim.clock.now();
    let retry = if failure.is_retryable() {
        Retry::At(add_ms(now, claim.config.retry.delay_ms(claim.job.attempts)))
    } else {
        Retry::Never
    };
    record_end(
        claim.store.fail_job(claim.lease, failure, retry, now),
        &claim.job.id,
    )
}

/// Maps the store's answer to a write that ends a claimed job. A lost lease, and
/// a publication that a revocation refused, leave the worker nothing to write.
fn record_end(
    written: Result<JobRecord, JobStoreError>,
    job: &JobId,
) -> Result<Step, JobStoreError> {
    match written {
        Ok(record) => Ok(Step::Finished(record)),
        Err(JobStoreError::LeaseLost | JobStoreError::Revoked(_)) => {
            Ok(Step::LeaseLost(job.clone()))
        }
        Err(error) => Err(error),
    }
}

/// Settles an attempt and returns its job as it is after the settlement.
fn settle_record<S: JobStore>(
    store: &S,
    clock: &dyn Clock,
    attempt: &AttemptIdentity,
) -> Result<JobRecord, JobStoreError> {
    match store.settle_attempt(attempt, clock.now())? {
        SettleOutcome::Settled(record) | SettleOutcome::AlreadySettled(record) => Ok(record),
    }
}

/// Reads the files of the pinned revision through the store and checks each one
/// against the revision's listing: its length and its SHA-256 digest (decision
/// 0057 rule 16). The head and links are never read.
fn read_input<S: ArtifactStore>(
    store: &S,
    job: &JobRecord,
    pace: &dyn Pace,
) -> Result<PinnedInput, JobFailure> {
    let listing = match store.revision(&job.artifact, job.revision) {
        // A revision always has files; none means the artifact was purged since
        // the job was claimed.
        Ok(Some(listing)) if !listing.files.is_empty() => listing,
        Ok(_) => return Err(JobFailure::RevisionUnavailable),
        Err(StoreError::Unavailable(_)) => return Err(JobFailure::StorageUnavailable),
        Err(_) => return Err(JobFailure::Internal),
    };
    let mut files = Vec::with_capacity(listing.files.len());
    for entry in &listing.files {
        pace.before_file();
        let content = match store.revision_file(&job.artifact, job.revision, &entry.path) {
            Ok(Some(content)) => content,
            Ok(None) => return Err(JobFailure::RevisionUnavailable),
            Err(StoreError::Unavailable(_)) => return Err(JobFailure::StorageUnavailable),
            Err(_) => return Err(JobFailure::Internal),
        };
        let length_matches = u64::try_from(content.len()).ok() == Some(entry.size);
        if !length_matches || Sha256::digest(&content).as_slice() != entry.sha256.as_slice() {
            return Err(JobFailure::RevisionUnavailable);
        }
        files.push((entry.path.clone(), content));
    }
    let package = FilePackage::new(files).map_err(|_| JobFailure::Internal)?;
    Ok(PinnedInput {
        package,
        listing: listing.files,
    })
}

/// Renews the lease on a schedule and watches for a cancel or a shutdown.
struct Checkpoints<'a> {
    store: &'a dyn JobStore,
    clock: &'a dyn Clock,
    lease: &'a Lease,
    config: &'a WorkerConfig,
    pace: &'a dyn Pace,
    last_renewal: Option<UnixMillis>,
}

impl Checkpoints<'_> {
    /// The first checkpoint, each one a heartbeat's interval after the last,
    /// and the final one before the result is written renew the lease and read
    /// the cancel request. `Ok(None)` means go on.
    fn reach(&mut self, last: bool) -> Option<Verified> {
        // The final checkpoint ignores a stop request: the verification is
        // complete, so its result is recorded and the worker stops after it
        // (it starts no new job). Only unfinished work is given back.
        if !last && self.pace.shutdown_requested() {
            return Some(Verified::Shutdown);
        }
        let now = self.clock.now();
        let due = self.last_renewal.is_none_or(|renewed| {
            now.0.saturating_sub(renewed.0)
                >= i64::try_from(self.config.heartbeat_ms).unwrap_or(i64::MAX)
        });
        if !(due || last) {
            return None;
        }
        match self
            .store
            .heartbeat_job(self.lease, now, self.config.lease_ms)
        {
            Ok(Checkpoint::Continue) => {
                self.last_renewal = Some(now);
                None
            }
            Ok(Checkpoint::CancelRequested) => Some(Verified::Cancelled),
            Err(JobStoreError::LeaseLost) => Some(Verified::LeaseLost),
            // A renewal that fails is tried again at the next checkpoint. The
            // write of the result does not look at the clock: it is accepted
            // while the worker's fence is the job's current claim, even after
            // the lease time passed, and rejected once another worker claimed
            // the job or a purge revoked it. The fence decides, not the time.
            Err(_) => None,
        }
    }
}

/// Reads the pinned revision's files one at a time and compares each with its
/// listing. Memory holds one file (a file is at most 5 MiB).
fn verify<S: ArtifactStore + JobStore>(
    store: &S,
    clock: &dyn Clock,
    lease: &Lease,
    job: &JobRecord,
    config: &WorkerConfig,
    pace: &dyn Pace,
) -> Result<Verified, JobFailure> {
    let listing = match store.revision(&job.artifact, job.revision) {
        // A revision always has files; none means the artifact was purged
        // since the job was claimed.
        Ok(Some(listing)) if !listing.files.is_empty() => listing,
        Ok(_) => return Err(JobFailure::RevisionUnavailable),
        Err(StoreError::Unavailable(_)) => return Err(JobFailure::StorageUnavailable),
        Err(_) => return Err(JobFailure::Internal),
    };
    let mut checkpoints = Checkpoints {
        store,
        clock,
        lease,
        config,
        pace,
        last_renewal: None,
    };
    let mut report = VerifyReport {
        files_checked: 0,
        bytes_checked: 0,
        mismatch_count: 0,
        findings: Vec::new(),
    };
    for entry in &listing.files {
        if let Some(stop) = checkpoints.reach(false) {
            return Ok(stop);
        }
        pace.before_file();
        let content = match store.revision_file(&job.artifact, job.revision, &entry.path) {
            Ok(content) => content,
            Err(StoreError::Unavailable(_)) => return Err(JobFailure::StorageUnavailable),
            Err(_) => return Err(JobFailure::Internal),
        };
        let problem = match &content {
            None => Some(Problem::ContentMissing),
            Some(bytes) if u64::try_from(bytes.len()).ok() != Some(entry.size) => {
                Some(Problem::SizeMismatch)
            }
            Some(bytes) if Sha256::digest(bytes).as_slice() != entry.sha256.as_slice() => {
                Some(Problem::DigestMismatch)
            }
            Some(_) => None,
        };
        report.files_checked += 1;
        report.bytes_checked += content
            .as_ref()
            .map_or(0, |bytes| u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        if let Some(problem) = problem {
            report.mismatch_count += 1;
            if report.findings.len() < MAX_FINDINGS {
                report.findings.push(Finding {
                    path: entry.path.clone(),
                    problem,
                });
            }
        }
    }
    Ok(checkpoints.reach(true).unwrap_or(Verified::Done(report)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_backoff_doubles_and_stops_at_the_cap() {
        let policy = RetryPolicy {
            base_ms: 1_000,
            cap_ms: 5_000,
        };
        let delays: Vec<u64> = (1..=5).map(|attempt| policy.delay_ms(attempt)).collect();
        assert_eq!(delays, [1_000, 2_000, 4_000, 5_000, 5_000]);
        assert_eq!(policy.delay_ms(0), 1_000, "no failed attempt yet");
        assert_eq!(policy.delay_ms(u32::MAX), 5_000, "no overflow");
    }

    #[test]
    fn a_lease_is_renewed_three_times_per_expiry() {
        let config = WorkerConfig::with_lease_ms(30_000);
        assert_eq!(config.heartbeat_ms, 10_000);
        assert_eq!(WorkerConfig::with_lease_ms(1).heartbeat_ms, 1);
    }
}
