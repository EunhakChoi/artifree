//! Durable storage for persistent jobs and their leases (decision 0030).
//!
//! An implementation must make each transition atomic and decide it from the
//! state it reads in the same step: a claim, a heartbeat, a result, and a
//! cancel never act on a state read earlier. A worker holds a [`Lease`], and
//! every call it makes names the lease's fencing token; the implementation
//! rejects the call with [`JobStoreError::LeaseLost`] when the job has been
//! claimed again, cancelled by a purge, or ended since, so a worker that lost
//! its lease cannot write a result.
//!
//! Times are given by the caller, which reads an injectable clock; the store
//! never reads one for a job.

use std::fmt;

use artifree_domain::SourceRevision;
use artifree_domain::identity::{ActorId, ArtifactId, UnixMillis};
use artifree_domain::job::{JobFailure, JobId, JobKind, JobState, VerifyReport, WorkerId};

use crate::artifact_store::{Delivery, RequestIdentity, StoreError};

/// The stored state of a job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRecord {
    /// Job identifier.
    pub id: JobId,
    /// The actor that enqueued it. Fixed when it was enqueued.
    pub actor: ActorId,
    /// The artifact whose revision it verifies.
    pub artifact: ArtifactId,
    /// The pinned source revision. Fixed when it was enqueued.
    pub revision: SourceRevision,
    /// What the job does.
    pub kind: JobKind,
    /// Where it is in its life.
    pub state: JobState,
    /// Whether a cancel was requested while it was running. The worker stops
    /// at its next checkpoint and the job ends as cancelled.
    pub cancel_requested: bool,
    /// How many times a worker claimed it (a graceful release gives one
    /// back).
    pub attempts: u32,
    /// How many claims it gets at most.
    pub max_attempts: u32,
    /// The earliest time a worker may claim it: its enqueue time, or the end
    /// of the backoff of [`JobState::RetryScheduled`].
    pub run_after: UnixMillis,
    /// When the worker's lease ends, while the job is running.
    pub lease_expires_at: Option<UnixMillis>,
    /// Who holds the lease, while the job is running.
    pub worker: Option<WorkerId>,
    /// Why it failed, once it failed.
    pub failure: Option<JobFailure>,
    /// The result, once it succeeded and the artifact was not purged.
    pub report: Option<VerifyReport>,
    /// When it was enqueued.
    pub created_at: UnixMillis,
    /// When a worker first claimed it.
    pub started_at: Option<UnixMillis>,
    /// When it reached a terminal state.
    pub finished_at: Option<UnixMillis>,
}

/// A request to enqueue a job for one revision of an artifact.
#[derive(Debug, Clone, Copy)]
pub struct NewJob<'a> {
    /// Retry identity; its actor is pinned to the job.
    pub request: RequestIdentity<'a>,
    /// The artifact. It must be outside the trash.
    pub artifact: &'a ArtifactId,
    /// The revision to pin. It must exist.
    pub revision: SourceRevision,
    /// What to do.
    pub kind: JobKind,
    /// When the request is made.
    pub now: UnixMillis,
}

/// A request to cancel a job.
#[derive(Debug, Clone, Copy)]
pub struct JobCancel<'a> {
    /// Retry identity of the cancelling actor.
    pub request: RequestIdentity<'a>,
    /// The job.
    pub job: &'a JobId,
    /// When the request is made.
    pub now: UnixMillis,
}

/// The durable result of an enqueue or a cancel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobReceipt {
    /// The job as it is now, also for a replay: a job moves on after the
    /// request that created or cancelled it.
    pub job: JobRecord,
    /// Whether this call wrote it or replayed an earlier request.
    pub delivery: Delivery,
}

/// Where a job list continues: after the job with this creation time and
/// identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobPosition {
    /// The creation time of the last job already listed.
    pub created_at: UnixMillis,
    /// Its identifier.
    pub id: JobId,
}

/// One page of an actor's jobs, newest first. Ties are broken by identifier.
#[derive(Debug, Clone, Copy)]
pub struct JobListing<'a> {
    /// The actor whose jobs are listed.
    pub actor: &'a ActorId,
    /// Only jobs of this artifact.
    pub artifact: Option<&'a ArtifactId>,
    /// Continue after this position.
    pub after: Option<&'a JobPosition>,
    /// At most this many jobs.
    pub limit: usize,
}

/// A worker asking for work.
#[derive(Debug, Clone, Copy)]
pub struct JobClaim<'a> {
    /// The claiming worker.
    pub worker: &'a WorkerId,
    /// Now, on the worker's clock.
    pub now: UnixMillis,
    /// How long the lease lasts from now, in milliseconds.
    pub lease_ms: u64,
}

/// The right to run one claim of a job: the job and the fencing token of the
/// claim. A later claim of the job has a higher token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    /// The claimed job.
    pub job: JobId,
    /// The fencing token: how many times the job has been claimed.
    pub fence: u64,
}

/// A job a worker claimed and the lease it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedJob {
    /// The job after the claim.
    pub job: JobRecord,
    /// The lease; pass it to every call about this run.
    pub lease: Lease,
}

/// What one claim did: the jobs it ended on the way and the job it claimed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Claim {
    /// Jobs the claim ended because they could only end: an expired lease with
    /// a cancel requested (now cancelled) or on its last attempt (now failed
    /// with [`JobFailure::LeaseExpired`]), in the order they were ended. They
    /// are reported so that a worker can tell its operator about each one.
    pub settled: Vec<JobRecord>,
    /// The claimed job, if one was due after those.
    pub claimed: Option<ClaimedJob>,
}

/// What a heartbeat found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checkpoint {
    /// The lease was renewed; go on.
    Continue,
    /// A cancel was requested; the lease was renewed so the worker can
    /// confirm it. Stop and call `confirm_cancel`.
    CancelRequested,
}

/// What a worker does when an attempt fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retry {
    /// Do not try again.
    Never,
    /// Try again at this time, if attempts are left; otherwise the job
    /// fails.
    At(UnixMillis),
}

/// A job store failure or a rejected conditional write.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum JobStoreError {
    /// The job, the artifact, or its revision does not exist, or the artifact
    /// is in the trash or purged, or belongs to another actor.
    NotFound,
    /// The actor already used the key for another request.
    RequestKeyReused,
    /// The job is in a state that does not allow the request.
    Conflict(JobState),
    /// The actor has the largest number of jobs waiting or running, counting
    /// only jobs of artifacts outside the trash.
    LimitReached,
    /// The worker's lease is not the job's current claim: the job was claimed
    /// again, cancelled by a purge, or already ended. The worker must stop
    /// and write nothing.
    LeaseLost,
    /// Storage could not complete the operation.
    Unavailable(String),
}

impl fmt::Display for JobStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => formatter.write_str("job not found"),
            Self::RequestKeyReused => {
                formatter.write_str("request key was already used for a different request")
            }
            Self::Conflict(state) => write!(formatter, "the job is {}", state.as_str()),
            Self::LimitReached => formatter.write_str("too many jobs are waiting or running"),
            Self::LeaseLost => formatter.write_str("the worker no longer holds the job's lease"),
            Self::Unavailable(reason) => write!(formatter, "storage unavailable: {reason}"),
        }
    }
}

impl std::error::Error for JobStoreError {}

impl From<StoreError> for JobStoreError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::NotFound => Self::NotFound,
            StoreError::RequestKeyReused => Self::RequestKeyReused,
            StoreError::Unavailable(reason) => Self::Unavailable(reason),
            other => Self::Unavailable(other.to_string()),
        }
    }
}

/// Durable job storage.
///
/// Methods do not check authorization; the application core does. A write
/// that names an artifact outside the trash and not purged is
/// [`JobStoreError::NotFound`] otherwise, before the key is looked at.
pub trait JobStore {
    /// Enqueues a job pinned to a revision, or returns the stored receipt of
    /// the same request. One step decides the artifact, the revision, the key,
    /// and the actor's limit.
    ///
    /// # Errors
    ///
    /// [`JobStoreError::NotFound`], [`JobStoreError::RequestKeyReused`],
    /// [`JobStoreError::LimitReached`], or [`JobStoreError::Unavailable`].
    fn enqueue_job(&self, request: NewJob<'_>) -> Result<JobReceipt, JobStoreError>;

    /// Cancels a job of the actor. A job that has not started ends as
    /// cancelled at once; a running job gets `cancel_requested` and ends as
    /// cancelled when its worker reaches a checkpoint. The same request
    /// replays. A job that ended is [`JobStoreError::Conflict`].
    ///
    /// # Errors
    ///
    /// [`JobStoreError::NotFound`], [`JobStoreError::RequestKeyReused`],
    /// [`JobStoreError::Conflict`], or [`JobStoreError::Unavailable`].
    fn cancel_job(&self, request: JobCancel<'_>) -> Result<JobReceipt, JobStoreError>;

    /// One job, as stored. It does not check the artifact or the actor.
    ///
    /// # Errors
    ///
    /// [`JobStoreError::Unavailable`].
    fn job(&self, id: &JobId) -> Result<Option<JobRecord>, JobStoreError>;

    /// Up to `listing.limit` jobs of an actor whose artifact is outside the
    /// trash and not purged, newest first.
    ///
    /// # Errors
    ///
    /// [`JobStoreError::Unavailable`].
    fn list_jobs(&self, listing: JobListing<'_>) -> Result<Vec<JobRecord>, JobStoreError>;

    /// Claims the oldest job that is due: a queued or retry-scheduled job
    /// whose time came, or a running job whose lease expired. Only jobs of an
    /// artifact outside the trash and not purged are due; the others wait
    /// untouched until the artifact is recovered. Likewise a job whose owning
    /// user is disabled is not due: it waits untouched (no attempt is used)
    /// until the user is enabled, and the check is part of the claim, not a
    /// separate read before it. A job already running when the user is
    /// disabled is not stopped. Jobs found on the way that
    /// can only end are ended and returned in [`Claim::settled`]: an expired
    /// lease with a cancel requested becomes cancelled, and one on its last
    /// attempt becomes failed with [`JobFailure::LeaseExpired`].
    /// [`Claim::claimed`] is `None` when nothing else is due.
    ///
    /// # Errors
    ///
    /// [`JobStoreError::Unavailable`].
    fn claim_job(&self, claim: JobClaim<'_>) -> Result<Claim, JobStoreError>;

    /// Renews the lease to `now + lease_ms` and reports whether a cancel was
    /// requested.
    ///
    /// # Errors
    ///
    /// [`JobStoreError::LeaseLost`] or [`JobStoreError::Unavailable`].
    fn heartbeat_job(
        &self,
        lease: &Lease,
        now: UnixMillis,
        lease_ms: u64,
    ) -> Result<Checkpoint, JobStoreError>;

    /// Records the result and ends the job as succeeded, once. When a cancel
    /// was requested the job ends as cancelled instead and the report is
    /// dropped. Returns the job as it ended.
    ///
    /// # Errors
    ///
    /// [`JobStoreError::LeaseLost`] or [`JobStoreError::Unavailable`].
    fn complete_job(
        &self,
        lease: &Lease,
        report: &VerifyReport,
        now: UnixMillis,
    ) -> Result<JobRecord, JobStoreError>;

    /// Records that the attempt failed. With [`Retry::At`] and attempts left
    /// the job is scheduled again; otherwise it ends as failed. A cancel that
    /// was requested ends the job as cancelled. Returns the job as it is now.
    ///
    /// # Errors
    ///
    /// [`JobStoreError::LeaseLost`] or [`JobStoreError::Unavailable`].
    fn fail_job(
        &self,
        lease: &Lease,
        failure: JobFailure,
        retry: Retry,
        now: UnixMillis,
    ) -> Result<JobRecord, JobStoreError>;

    /// Ends the job as cancelled after its worker saw the request.
    ///
    /// # Errors
    ///
    /// [`JobStoreError::LeaseLost`], [`JobStoreError::Conflict`] when no
    /// cancel was requested, or [`JobStoreError::Unavailable`].
    fn confirm_cancel(&self, lease: &Lease, now: UnixMillis) -> Result<JobRecord, JobStoreError>;

    /// Gives the job back to the queue because the worker is shutting down,
    /// without using an attempt. A job with a cancel requested ends as
    /// cancelled.
    ///
    /// # Errors
    ///
    /// [`JobStoreError::LeaseLost`] or [`JobStoreError::Unavailable`].
    fn release_job(&self, lease: &Lease, now: UnixMillis) -> Result<JobRecord, JobStoreError>;
}
