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
//! A `run_profile` job (decision 0057) also gets an [`AttemptIdentity`] for
//! each claim. Its checkpoint, publication and settlement are keyed by that
//! identity, and its terminal transitions wait until the attempt is settled.
//!
//! Times are given by the caller, which reads an injectable clock; the store
//! never reads one for a job.

use std::fmt;

use artifree_domain::SourceRevision;
use artifree_domain::change::FeedId;
use artifree_domain::identity::{ActorId, ArtifactId, StateGeneration, UnixMillis};
use artifree_domain::job::{
    AttemptIdentity, AttemptState, CheckpointAnswer, JobFailure, JobId, JobKind, JobState,
    ProfileName, Revocation, StopReason, VerifyReport, WorkerId,
};

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
    /// The reviewed profile of a `run_profile` job (decision 0057 rule 1).
    /// `None` for `verify_revision`.
    pub profile: Option<ProfileName>,
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
    /// The result of a `verify_revision` job, once it succeeded and the
    /// artifact was not purged.
    pub report: Option<VerifyReport>,
    /// The result of a `run_profile` job, once it succeeded and the artifact
    /// was not purged: the two counts and nothing else (decision 0057 rule 12).
    pub counts: Option<ProfileCounts>,
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

/// A request to enqueue a `run_profile` job for one revision of an artifact.
/// The job carries its profile; it does not count toward the actor's limit
/// (decision 0057 rule 3).
#[derive(Debug, Clone, Copy)]
pub struct NewProfileJob<'a> {
    /// Retry identity; its actor is pinned to the job.
    pub request: RequestIdentity<'a>,
    /// The artifact. It must be outside the trash.
    pub artifact: &'a ArtifactId,
    /// The revision to pin. It must exist.
    pub revision: SourceRevision,
    /// The reviewed profile to run. It is pinned to the job.
    pub profile: &'a ProfileName,
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
    /// The profiles this build knows. A due `run_profile` job whose profile is
    /// not listed is ended as failed with [`JobFailure::ExecutionUnavailable`],
    /// without an attempt (decision 0057 rule 1). Existing callers pass `&[]`.
    pub known_profiles: &'a [ProfileName],
    /// The worker's execution identities. It is present only for a worker that
    /// holds the admission lock and passed host admission (decision 0057 rule
    /// 19; the caller's contract). A known-profile job is claimed only with it,
    /// and is otherwise left queued. Existing callers pass `None`.
    pub execution: Option<ExecutionClaim<'a>>,
}

/// The execution identities of a claiming worker, which a `run_profile` claim
/// records in its attempt (decision 0057 rules 6 and 7). They are opaque
/// identities, never paths.
#[derive(Debug, Clone, Copy)]
pub struct ExecutionClaim<'a> {
    /// The new attempt's identity. The caller draws it at random.
    pub attempt: AttemptIdentity,
    /// The tool root manifest digest, 64 lowercase hex (decision 0042 rule 3).
    pub tool_digest: &'a str,
    /// The host identity of the claiming worker: the machine.
    pub machine: &'a str,
    /// The host identity of the claiming worker: the boot.
    pub boot: &'a str,
    /// The host identity of the claiming worker: the cgroup namespace.
    pub cgroupns: &'a str,
    /// The execution directory's random identity, 32 lowercase hex.
    pub directory: &'a str,
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
    /// with [`JobFailure::LeaseExpired`]), and a `run_profile` job whose
    /// profile is not known (now failed with [`JobFailure::ExecutionUnavailable`]),
    /// in the order they were ended. They are reported so that a worker can
    /// tell its operator about each one.
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

/// The only result of a `run_profile` job (decision 0057 rule 12): the number
/// of files and bytes of the admitted output. No digest is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProfileCounts {
    /// Files of the admitted output.
    pub files: u64,
    /// Bytes of the admitted output.
    pub bytes: u64,
}

/// What the manifest binding of a `run_profile` attempt names of its tools
/// (decision 0057 rule 12): publication compares both with the attempt row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProfileBinding<'a> {
    /// The profile the binding names.
    pub profile: &'a ProfileName,
    /// The tool root manifest digest the binding names, 64 lowercase hex.
    pub tool_digest: &'a str,
}

/// One stored execution attempt of a `run_profile` job (decision 0057 rule 6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptRow {
    /// The attempt's identity.
    pub attempt: AttemptIdentity,
    /// The job it belongs to. The row outlives a purge of the artifact
    /// (decision 0030 rule 15).
    pub job: JobId,
    /// The job's fencing token at the claim.
    pub fence: u64,
    /// The actor's state generation pinned at the claim (decision 0057 rule 5).
    /// `None` when the actor had no users row.
    pub actor_generation: Option<StateGeneration>,
    /// The reviewed profile the attempt runs.
    pub profile: ProfileName,
    /// The tool root manifest digest, 64 lowercase hex.
    pub tool_digest: String,
    /// The host identity of the claiming worker: the machine.
    pub machine: String,
    /// The host identity of the claiming worker: the boot.
    pub boot: String,
    /// The host identity of the claiming worker: the cgroup namespace.
    pub cgroupns: String,
    /// The execution directory's random identity, 32 lowercase hex.
    pub directory: String,
    /// Where the attempt stands in its cleanup.
    pub state: AttemptState,
    /// Why publication of the attempt was revoked, once it was.
    pub revoked: Option<Revocation>,
    /// When the attempt was claimed.
    pub created_at: UnixMillis,
    /// When the attempt was settled, once it was.
    pub settled_at: Option<UnixMillis>,
}

/// The result of [`JobStore::settle_attempt`]. The job as it is after the
/// settlement; `AlreadySettled` means the call changed nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettleOutcome {
    /// The attempt was settled by this call.
    Settled(JobRecord),
    /// The attempt was settled already.
    AlreadySettled(JobRecord),
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
    /// The job's execution attempt is not settled; no terminal transition
    /// happens before cleanup (decision 0057 rule 10).
    AttemptUnsettled,
    /// Publication of the attempt was refused for this reason (decision 0057
    /// rules 7 and 12).
    Revoked(StopReason),
    /// The request breaks a rule of this API, named by the text: a malformed
    /// execution identity, or a call meant for the other kind of job.
    InvalidRequest(&'static str),
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
            Self::AttemptUnsettled => {
                formatter.write_str("the job's execution attempt is not settled")
            }
            Self::Revoked(reason) => {
                write!(formatter, "publication was revoked: {}", reason.as_str())
            }
            Self::InvalidRequest(rule) => write!(formatter, "invalid job request: {rule}"),
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
    /// and the actor's limit. A `run_profile` request is refused with
    /// [`JobStoreError::InvalidRequest`]; use [`JobStore::enqueue_profile_job`].
    ///
    /// # Errors
    ///
    /// [`JobStoreError::NotFound`], [`JobStoreError::RequestKeyReused`],
    /// [`JobStoreError::LimitReached`], [`JobStoreError::InvalidRequest`], or
    /// [`JobStoreError::Unavailable`].
    fn enqueue_job(&self, request: NewJob<'_>) -> Result<JobReceipt, JobStoreError>;

    /// Enqueues a `run_profile` job, as [`JobStore::enqueue_job`] does for
    /// `verify_revision`: the same request keys, the same artifact and
    /// revision checks, and the same replay. It does not count toward the
    /// actor's limit (decision 0057 rule 3). Only tests call it; no HTTP, MCP
    /// or CLI path does.
    ///
    /// # Errors
    ///
    /// [`JobStoreError::NotFound`], [`JobStoreError::RequestKeyReused`], or
    /// [`JobStoreError::Unavailable`].
    fn enqueue_profile_job(&self, request: NewProfileJob<'_>) -> Result<JobReceipt, JobStoreError>;

    /// Cancels a job of the actor. A job that has not started ends as
    /// cancelled at once; a running job gets `cancel_requested` and ends as
    /// cancelled when its worker reaches a checkpoint. The same request
    /// replays. A job that ended is [`JobStoreError::Conflict`]. A
    /// `run_profile` job is [`JobStoreError::NotFound`], and no request key is
    /// recorded.
    ///
    /// # Errors
    ///
    /// [`JobStoreError::NotFound`], [`JobStoreError::RequestKeyReused`],
    /// [`JobStoreError::Conflict`], or [`JobStoreError::Unavailable`].
    fn cancel_job(&self, request: JobCancel<'_>) -> Result<JobReceipt, JobStoreError>;

    /// One job, as stored. It does not check the artifact or the actor. A
    /// `run_profile` job is not returned: the answer is `None`, as for a job
    /// that does not exist.
    ///
    /// # Errors
    ///
    /// [`JobStoreError::Unavailable`].
    fn job(&self, id: &JobId) -> Result<Option<JobRecord>, JobStoreError>;

    /// Up to `listing.limit` jobs of an actor whose artifact is outside the
    /// trash and not purged, newest first. `run_profile` jobs are not
    /// returned.
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
    /// separate read before it. A `verify_revision` job already running when
    /// the user is disabled is not stopped; a `run_profile` attempt is refused
    /// at its next checkpoint (decision 0057 rule 9).
    ///
    /// A `run_profile` job with an attempt that is not settled is not due, even
    /// with an expired lease (decision 0057 rule 10). A `run_profile` job whose
    /// profile is not in `known_profiles` is ended as failed with
    /// [`JobFailure::ExecutionUnavailable`], counting no attempt (decision 0057
    /// rule 1). A known `run_profile` job is claimed only when `execution` is
    /// present, and then records its attempt; without it the job is left
    /// queued.
    ///
    /// Jobs found on the way that can only end are ended and returned in
    /// [`Claim::settled`]: an expired lease with a cancel requested becomes
    /// cancelled, and one on its last attempt becomes failed with
    /// [`JobFailure::LeaseExpired`]. [`Claim::claimed`] is `None` when nothing
    /// else is due.
    ///
    /// # Errors
    ///
    /// [`JobStoreError::Unavailable`].
    fn claim_job(&self, claim: JobClaim<'_>) -> Result<Claim, JobStoreError>;

    /// Renews the lease to `now + lease_ms` and reports whether a cancel was
    /// requested. A `run_profile` job is [`JobStoreError::InvalidRequest`];
    /// use [`JobStore::checkpoint_attempt`].
    ///
    /// # Errors
    ///
    /// [`JobStoreError::LeaseLost`], [`JobStoreError::InvalidRequest`], or
    /// [`JobStoreError::Unavailable`].
    fn heartbeat_job(
        &self,
        lease: &Lease,
        now: UnixMillis,
        lease_ms: u64,
    ) -> Result<Checkpoint, JobStoreError>;

    /// Records the result and ends the job as succeeded, once. When a cancel
    /// was requested the job ends as cancelled instead and the report is
    /// dropped. Returns the job as it ended. A `run_profile` job is
    /// [`JobStoreError::InvalidRequest`]; use [`JobStore::complete_profile_job`].
    ///
    /// # Errors
    ///
    /// [`JobStoreError::LeaseLost`], [`JobStoreError::InvalidRequest`], or
    /// [`JobStoreError::Unavailable`].
    fn complete_job(
        &self,
        lease: &Lease,
        report: &VerifyReport,
        now: UnixMillis,
    ) -> Result<JobRecord, JobStoreError>;

    /// Records that the attempt failed. With [`Retry::At`] and attempts left
    /// the job is scheduled again; otherwise it ends as failed. A cancel that
    /// was requested ends the job as cancelled. Returns the job as it is now.
    /// For a `run_profile` job, [`JobStoreError::AttemptUnsettled`] while its
    /// attempt is not settled.
    ///
    /// # Errors
    ///
    /// [`JobStoreError::LeaseLost`], [`JobStoreError::AttemptUnsettled`], or
    /// [`JobStoreError::Unavailable`].
    fn fail_job(
        &self,
        lease: &Lease,
        failure: JobFailure,
        retry: Retry,
        now: UnixMillis,
    ) -> Result<JobRecord, JobStoreError>;

    /// Ends the job as cancelled after its worker saw the request. For a
    /// `run_profile` job, [`JobStoreError::AttemptUnsettled`] while its attempt
    /// is not settled.
    ///
    /// # Errors
    ///
    /// [`JobStoreError::LeaseLost`], [`JobStoreError::Conflict`] when no
    /// cancel was requested, [`JobStoreError::AttemptUnsettled`], or
    /// [`JobStoreError::Unavailable`].
    fn confirm_cancel(&self, lease: &Lease, now: UnixMillis) -> Result<JobRecord, JobStoreError>;

    /// Gives the job back to the queue because the worker is shutting down,
    /// without using an attempt. A job with a cancel requested ends as
    /// cancelled. For a `run_profile` job, [`JobStoreError::AttemptUnsettled`]
    /// while its attempt is not settled.
    ///
    /// # Errors
    ///
    /// [`JobStoreError::LeaseLost`], [`JobStoreError::AttemptUnsettled`], or
    /// [`JobStoreError::Unavailable`].
    fn release_job(&self, lease: &Lease, now: UnixMillis) -> Result<JobRecord, JobStoreError>;

    /// The checkpoint of a `run_profile` attempt (decision 0057 rule 7). The
    /// answer is the first reason that applies, in rule 7's order: a revocation
    /// already stored (a stored `shutdown` is answered as [`StopReason::Lease`],
    /// since rule 10 treats them alike), then the current state. A current-state
    /// reason is stored as the attempt's revocation in the same transaction.
    /// `Continue` renews the lease to `now + lease_ms`, as
    /// [`JobStore::heartbeat_job`] does. A `verify_revision` lease has no
    /// attempt, so it is [`JobStoreError::NotFound`]. An attempt that a later
    /// claim of its job replaced answers [`StopReason::Fence`] and stores nothing.
    ///
    /// # Errors
    ///
    /// [`JobStoreError::NotFound`] when no attempt with this identity belongs
    /// to `lease.job`, or [`JobStoreError::Unavailable`].
    fn checkpoint_attempt(
        &self,
        lease: &Lease,
        attempt: &AttemptIdentity,
        now: UnixMillis,
        lease_ms: u64,
    ) -> Result<CheckpointAnswer, JobStoreError>;

    /// Marks the attempt settled, keyed by its identity only (no lease and no
    /// fence), and moves the job of a revoked attempt (decision 0057 rule 10).
    /// It reads no artifact or user state. The caller holds the deployment
    /// lock and saw its group empty (decision 0057 rule 19); the store cannot
    /// check that. Settling an attempt that is settled already is
    /// [`SettleOutcome::AlreadySettled`].
    ///
    /// # Errors
    ///
    /// [`JobStoreError::NotFound`] when no attempt has this identity, or
    /// [`JobStoreError::Unavailable`].
    fn settle_attempt(
        &self,
        attempt: &AttemptIdentity,
        now: UnixMillis,
    ) -> Result<SettleOutcome, JobStoreError>;

    /// Marks a `live` attempt whose cleanup its worker did not observe
    /// `cleanup_pending` (decision 0057 rules 10 and 19): its record stays and
    /// the provider is quarantined, so every worker of this store, on any
    /// execution directory, sees a row it must not settle. It moves no job and
    /// settles nothing. An attempt that is `cleanup_pending` or `settled`
    /// already is left as it is.
    ///
    /// # Errors
    ///
    /// [`JobStoreError::NotFound`] when no attempt has this identity, or
    /// [`JobStoreError::Unavailable`].
    fn mark_cleanup_pending(&self, attempt: &AttemptIdentity) -> Result<(), JobStoreError>;

    /// Publication of a `run_profile` attempt (decision 0057 rule 12). The job
    /// succeeds with the two counts only when no rule 7 reason applies, the
    /// lease's fence is current, and the attempt is settled. The stored result
    /// holds the counts and nothing else. A refusal by a rule 7 reason is
    /// [`JobStoreError::Revoked`] and stores that revocation. A binding whose
    /// profile or tool digest differs from the attempt row's is
    /// [`JobStoreError::InvalidRequest`] and stores nothing. A `verify_revision`
    /// lease has no attempt, so it is [`JobStoreError::NotFound`].
    ///
    /// # Errors
    ///
    /// [`JobStoreError::NotFound`] when no attempt with this identity belongs
    /// to `lease.job`, [`JobStoreError::Revoked`], [`JobStoreError::LeaseLost`],
    /// [`JobStoreError::AttemptUnsettled`], [`JobStoreError::InvalidRequest`]
    /// for a binding of other tools, or [`JobStoreError::Unavailable`].
    fn complete_profile_job(
        &self,
        lease: &Lease,
        attempt: &AttemptIdentity,
        binding: ProfileBinding<'_>,
        counts: ProfileCounts,
        now: UnixMillis,
    ) -> Result<JobRecord, JobStoreError>;

    /// One attempt row, read from the attempt store only.
    ///
    /// # Errors
    ///
    /// [`JobStoreError::Unavailable`].
    fn attempt_row(&self, attempt: &AttemptIdentity) -> Result<Option<AttemptRow>, JobStoreError>;

    /// Every attempt row that is not settled, oldest first, read from the
    /// attempt store only. Used by the reconciliation of decision 0057 rule 19.
    ///
    /// # Errors
    ///
    /// [`JobStoreError::Unavailable`].
    fn unsettled_attempts(&self) -> Result<Vec<AttemptRow>, JobStoreError>;

    /// The identity of this store's database: the random identity of its
    /// change feed, which a restore renews (decisions 0006 and 0027 rule 7).
    /// An attempt's registry record names it (decision 0042 rule 6).
    ///
    /// # Errors
    ///
    /// [`JobStoreError::Unavailable`].
    fn database_identity(&self) -> Result<FeedId, JobStoreError>;
}
