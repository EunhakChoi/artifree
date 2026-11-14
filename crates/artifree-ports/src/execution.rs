//! The untrusted execution port of `run_profile` jobs (decision 0057 rule 15).
//!
//! Implemented: the port. The only adapter is the fixture profile's, compiled
//! only into a Linux build with the `enforcement-fixture` feature (decision
//! 0057 rule 2); a release build has no adapter, so its worker claims trusted
//! kinds and ends unknown-profile jobs only. The port carries no command, path
//! or launcher option (decision 0056 rule 1).

use std::fmt;

use artifree_domain::SourceRevision;
use artifree_domain::change::FeedId;
use artifree_domain::identity::{ActorId, ArtifactId, StateGeneration};
use artifree_domain::job::{AttemptIdentity, CheckpointAnswer, JobId, ProfileName, StopReason};
use artifree_domain::package::FilePackage;

use crate::artifact_store::FileEntry;
use crate::job_store::{AttemptRow, ProfileCounts};

/// The host identity that a held capacity and an attempt row carry (decision
/// 0057 rule 6). Each field is an opaque identity, never a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionHost {
    /// The machine identity.
    pub machine: String,
    /// The boot identity.
    pub boot: String,
    /// The cgroup namespace identity.
    pub cgroupns: String,
}

/// The store's checkpoint of one attempt, asked by the provider's guardian
/// before each renewal (decision 0057 rule 7).
pub trait Checkpoint {
    /// Renews the attempt's lease and answers whether it may go on.
    ///
    /// # Errors
    ///
    /// [`CheckpointUnavailable`] when the store did not answer. No renewal is
    /// sent, and the question is asked again later; it is not a stop.
    fn renew(&self) -> Result<CheckpointAnswer, CheckpointUnavailable>;
}

/// The store did not answer a checkpoint. This is not a stop: no renewal is
/// sent, and the guardian asks again half a window later (decision 0056,
/// guardian amendment).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckpointUnavailable;

impl fmt::Display for CheckpointUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the checkpoint is unavailable")
    }
}

impl std::error::Error for CheckpointUnavailable {}

/// The execution port of `run_profile` jobs. The core asks it for capacity
/// before each claim, and a held capacity launches the one job it was taken for
/// (decision 0057 rules 15 and 19).
pub trait Execution {
    /// Takes capacity for one attempt without waiting (decision 0057 rule 19).
    /// `unsettled` are the store's attempt rows that are not settled, read by
    /// the caller just before.
    fn capacity(&self, unsettled: &[AttemptRow]) -> Capacity;
}

/// The answer to a request for capacity (decision 0057 rule 19).
#[must_use]
#[derive(Debug)]
pub enum Capacity {
    /// The admission lock is held, host admission passed and reconciliation
    /// ran. Dropping it releases the lock.
    Held(Box<dyn HeldCapacity>),
    /// Another process holds the admission lock; nothing was spent.
    Busy,
    /// Admission, the quarantine or reconciliation refused, with a short fixed
    /// word for stderr; nothing was spent.
    Refused(&'static str),
}

/// Capacity held for one claim: the admission lock, host admission and the
/// reconciliation of decision 0057 rule 19. Only its launch runs a payload.
pub trait HeldCapacity: fmt::Debug {
    /// The host identity this capacity was admitted on.
    #[must_use]
    fn host(&self) -> &ExecutionHost;

    /// The execution directory's identity, 32 lowercase hex digits.
    #[must_use]
    fn directory(&self) -> &str;

    /// The tool root manifest digest, 64 lowercase hex digits.
    #[must_use]
    fn tool_digest(&self) -> &str;

    /// The fresh attempt identity drawn with this capacity. The claim and the
    /// launch both use it (decision 0057 rule 6), so every call gives the same
    /// identity.
    #[must_use]
    fn attempt(&self) -> AttemptIdentity;

    /// The unsettled attempts that reconciliation found removed, or record-less
    /// for this machine, cgroup namespace and directory. The caller settles them
    /// before its claim (decision 0057 rule 19).
    #[must_use]
    fn reconciled(&self) -> &[AttemptIdentity];

    /// Launches one pinned run on this capacity and returns once the attempt has
    /// ended. The admission lock stays held until the capacity is dropped. A
    /// second call launches nothing.
    fn launch(
        &mut self,
        run: &ProfileRun<'_>,
        input: PinnedInput,
        checkpoint: &dyn Checkpoint,
    ) -> LaunchOutcome;
}

/// One claimed `run_profile` job as a launch receives it: the job and its
/// fencing token, the attempt identity drawn with the capacity, and the pins
/// the run is bound to.
#[derive(Debug, Clone, Copy)]
pub struct ProfileRun<'a> {
    /// The claimed job.
    pub job: &'a JobId,
    /// The job's fencing token at the claim.
    pub fence: u64,
    /// The attempt identity, drawn with the capacity.
    pub attempt: AttemptIdentity,
    /// The actor that enqueued the job.
    pub actor: &'a ActorId,
    /// The actor's state generation pinned at the claim, or `None` when the
    /// actor had no users row (decision 0057 rule 5).
    pub generation: Option<StateGeneration>,
    /// The artifact whose revision runs.
    pub artifact: &'a ArtifactId,
    /// The pinned source revision.
    pub revision: SourceRevision,
    /// The reviewed profile to run.
    pub profile: &'a ProfileName,
    /// The lease length, in milliseconds, that the claim was made with.
    pub lease_ms: u64,
    /// The identity of the database the claim was made in, which the
    /// attempt's registry record names (decision 0042 rule 6).
    pub database: FeedId,
}

/// The pinned input of a run: the files of the pinned revision as the store
/// read them, with the listing they were checked against (decision 0057 rule
/// 16).
#[derive(Debug)]
pub struct PinnedInput {
    /// The validated files of the revision.
    pub package: FilePackage,
    /// The revision's listing: each file's path, size and SHA-256 digest, as the
    /// revision lists them.
    pub listing: Vec<FileEntry>,
}

/// The canonical values that an output manifest binds a run to (decision 0057
/// rule 17).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundRun {
    /// The actor identifier.
    pub actor: String,
    /// The actor's state generation, or `None` when the actor had no users row.
    pub generation: Option<u64>,
    /// The artifact identifier.
    pub artifact: String,
    /// The pinned source revision number.
    pub revision: u64,
    /// The job identifier.
    pub job: String,
    /// The job's fencing token.
    pub fence: u64,
    /// The attempt identity, 32 lowercase hex digits.
    pub attempt: String,
}

impl BoundRun {
    /// Takes the values that a run binds, in their canonical form.
    #[must_use]
    pub fn of(run: &ProfileRun<'_>) -> Self {
        Self {
            actor: run.actor.as_str().to_owned(),
            generation: run.generation.map(StateGeneration::get),
            artifact: run.artifact.as_str().to_owned(),
            revision: run.revision.0,
            job: run.job.as_str().to_owned(),
            fence: run.fence,
            attempt: run.attempt.hex(),
        }
    }
}

/// The output of a run that succeeded and was admitted: the counts, the profile
/// and tool digest the manifest names, and the values the run is bound to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileOutput {
    /// The files and bytes of the admitted output.
    pub counts: ProfileCounts,
    /// The profile the manifest names.
    pub profile: ProfileName,
    /// The tool root manifest digest the manifest names, 64 lowercase hex
    /// digits.
    pub tool_digest: String,
    /// The values the manifest binds the run to.
    pub bound: BoundRun,
}

/// Why a launch produced no output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchFailure {
    /// A checkpoint answered a stop; the renewals stopped and the group was
    /// ended.
    Stopped(StopReason),
    /// The payload did not start: the host, the quarantine, the registry, the
    /// input staging, the group or the guardian refused.
    Unavailable,
    /// The payload ran and its verdict failed or its output was refused
    /// (decision 0042 rule 7).
    Failed,
}

/// The end of one launch.
#[must_use]
#[derive(Debug)]
pub struct LaunchOutcome {
    /// Whether the attempt's group was seen gone and its record removed, or no
    /// record was written. Only then may the attempt be settled (decision 0057
    /// rules 10 and 19).
    pub cleanup_observed: bool,
    /// The output, or why there is none.
    pub result: Result<ProfileOutput, LaunchFailure>,
    /// A short fixed word for the operator's stderr, such as `succeeded` or
    /// `guardian-renewal-expired`; never a path or content.
    pub detail: &'static str,
}
