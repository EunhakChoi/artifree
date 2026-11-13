//! Persistent job vocabulary (decision 0030).
//!
//! A job pins an actor, an artifact, one source revision, and a kind. The
//! trusted kind, [`JobKind::VerifyRevision`], re-reads every file of the pinned
//! revision and compares it with the digest and size stored for it; it runs
//! trusted in-repository code only. The untrusted kind,
//! [`JobKind::RunProfile`], runs a reviewed profile in an execution provider
//! (decisions 0042 and 0057); no worker of this build executes one yet. Builds
//! and provider sampling are not job kinds.

use std::fmt;
use std::str::FromStr;

use crate::identity::{InvalidValue, hex_digit};
use crate::package::PackagePath;

const JOB_PREFIX: &str = "job_";
const JOB_HEX_LEN: usize = 32;

/// Most attempts a job gets: the first run and its retries. A claim of a job
/// whose lease expired counts as an attempt, so a job that keeps killing its
/// worker ends as failed instead of looping.
pub const MAX_ATTEMPTS: u32 = 3;

/// Most jobs one actor may have waiting or running at once. Enqueueing past
/// it is refused, so one caller cannot fill the queue for the others.
pub const MAX_ACTIVE_JOBS_PER_ACTOR: u64 = 16;

/// Most mismatches a verification result lists. The count of all mismatches is
/// kept, so a result says when its list is cut short.
pub const MAX_FINDINGS: usize = 50;

/// The most characters a profile name has (decision 0057 rule 1).
pub const MAX_PROFILE_NAME_CHARS: usize = 32;

/// An opaque job identifier: `job_` followed by 32 lowercase hex digits.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobId(String);

impl JobId {
    /// Builds an identifier from caller-supplied random bytes.
    #[must_use]
    pub fn from_random(bytes: [u8; 16]) -> Self {
        let mut value = String::with_capacity(JOB_PREFIX.len() + JOB_HEX_LEN);
        value.push_str(JOB_PREFIX);
        for byte in bytes {
            value.push(hex_digit(byte >> 4));
            value.push(hex_digit(byte & 0x0f));
        }
        Self(value)
    }

    /// The canonical string form.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for JobId {
    type Err = InvalidValue;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let valid = value.strip_prefix(JOB_PREFIX).is_some_and(|hex| {
            hex.len() == JOB_HEX_LEN
                && hex
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        });
        if valid {
            Ok(Self(value.to_owned()))
        } else {
            Err(InvalidValue {
                what: "job id",
                rule: "expected job_ followed by 32 lowercase hexadecimal digits",
            })
        }
    }
}

impl fmt::Display for JobId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The name a worker process gives itself: 1-64 lowercase letters, digits, or
/// hyphens. It is a label for operators, not a credential, and does not
/// authorize anything; the fencing token of a lease does.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WorkerId(String);

impl WorkerId {
    /// A name from random bytes, such as `worker-` and 8 hex digits.
    #[must_use]
    pub fn from_random(bytes: [u8; 4]) -> Self {
        let mut value = String::from("worker-");
        for byte in bytes {
            value.push(hex_digit(byte >> 4));
            value.push(hex_digit(byte & 0x0f));
        }
        Self(value)
    }

    /// The canonical string form.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for WorkerId {
    type Err = InvalidValue;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if (1..=64).contains(&value.len())
            && value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            Ok(Self(value.to_owned()))
        } else {
            Err(InvalidValue {
                what: "worker id",
                rule: "expected 1-64 lowercase letters, digits, or hyphens",
            })
        }
    }
}

impl fmt::Display for WorkerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The name of a reviewed operation profile (decision 0042 rule 2, decision
/// 0057 rule 1): 1-32 lowercase letters, digits, or hyphens, starting with a
/// letter. The schema holds no list of names; each build knows its own.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProfileName(String);

impl ProfileName {
    /// The canonical string form.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for ProfileName {
    type Err = InvalidValue;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let valid = (1..=MAX_PROFILE_NAME_CHARS).contains(&value.len())
            && value
                .bytes()
                .next()
                .is_some_and(|byte| byte.is_ascii_lowercase())
            && value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        if valid {
            Ok(Self(value.to_owned()))
        } else {
            Err(InvalidValue {
                what: "profile name",
                rule: "expected 1-32 lowercase letters, digits, or hyphens, starting with a letter",
            })
        }
    }
}

impl fmt::Display for ProfileName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The random 128-bit identity of one claimed `run_profile` attempt (decision
/// 0057 rule 6), stored as 32 lowercase hexadecimal digits. The caller draws
/// it; the store never generates one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AttemptIdentity([u8; 16]);

const ATTEMPT_HEX_LEN: usize = 32;

const INVALID_ATTEMPT_IDENTITY: InvalidValue = InvalidValue {
    what: "attempt identity",
    rule: "expected 32 lowercase hexadecimal digits",
};

impl AttemptIdentity {
    /// Builds an identity from caller-supplied random bytes.
    #[must_use]
    pub fn from_random(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// The raw bytes of the identity.
    #[must_use]
    pub fn bytes(&self) -> [u8; 16] {
        self.0
    }

    /// The canonical string form: 32 lowercase hexadecimal digits.
    #[must_use]
    pub fn hex(&self) -> String {
        let mut value = String::with_capacity(ATTEMPT_HEX_LEN);
        for byte in self.0 {
            value.push(hex_digit(byte >> 4));
            value.push(hex_digit(byte & 0x0f));
        }
        value
    }
}

impl FromStr for AttemptIdentity {
    type Err = InvalidValue;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != ATTEMPT_HEX_LEN {
            return Err(INVALID_ATTEMPT_IDENTITY);
        }
        let mut bytes = [0u8; 16];
        for (byte, pair) in bytes.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
            let [high, low] = pair else {
                return Err(INVALID_ATTEMPT_IDENTITY);
            };
            match (nibble(*high), nibble(*low)) {
                (Some(high), Some(low)) => *byte = (high << 4) | low,
                _ => return Err(INVALID_ATTEMPT_IDENTITY),
            }
        }
        Ok(Self(bytes))
    }
}

impl fmt::Display for AttemptIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.hex())
    }
}

/// The value of one lowercase hexadecimal digit, or `None` for any other byte.
fn nibble(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => digit.checked_sub(b'0'),
        b'a'..=b'f' => digit
            .checked_sub(b'a')
            .and_then(|value| value.checked_add(10)),
        _ => None,
    }
}

/// What a job does. `verify_revision` is trusted in-repository code;
/// `run_profile` is the untrusted kind of decision 0057. Adding a kind needs a
/// decision record and a schema step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    /// Re-reads every file of a pinned revision and compares its SHA-256
    /// digest and size with the stored listing.
    VerifyRevision,
    /// Runs a reviewed operation profile on the pinned revision in an untrusted
    /// execution provider (decisions 0042 and 0057). Only the store knows it so
    /// far: no wire contract names it and no worker of this build runs it.
    RunProfile,
}

impl JobKind {
    /// Stable lowercase name used in storage and wire contracts.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::VerifyRevision => "verify_revision",
            Self::RunProfile => "run_profile",
        }
    }
}

impl FromStr for JobKind {
    type Err = InvalidValue;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        [Self::VerifyRevision, Self::RunProfile]
            .into_iter()
            .find(|kind| kind.as_str() == value)
            .ok_or(InvalidValue {
                what: "job kind",
                rule: "expected verify_revision or run_profile",
            })
    }
}

/// Where a job is in its life. `Succeeded`, `Failed`, and `Cancelled` are
/// terminal: a job there never changes again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    /// Waiting for a worker to claim it.
    Queued,
    /// A worker holds its lease.
    Running,
    /// An attempt failed in a way worth repeating; the next one waits for the
    /// backoff to pass.
    RetryScheduled,
    /// The job ran to the end. A verification that found mismatches is still
    /// a success: the failure is in the artifact, not in the job.
    Succeeded,
    /// The job could not run to the end and will not be tried again.
    Failed,
    /// The job was stopped before it finished. Cancelling is not a rollback
    /// of anything the job did; this kind of job changes nothing outside its
    /// own record.
    Cancelled,
}

impl JobState {
    /// Stable lowercase name used in storage and wire contracts.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::RetryScheduled => "retry_scheduled",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Whether the job can no longer change.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

impl FromStr for JobState {
    type Err = InvalidValue;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        [
            Self::Queued,
            Self::Running,
            Self::RetryScheduled,
            Self::Succeeded,
            Self::Failed,
            Self::Cancelled,
        ]
        .into_iter()
        .find(|state| state.as_str() == value)
        .ok_or(InvalidValue {
            what: "job state",
            rule: "expected a job state name",
        })
    }
}

/// Why a job ended as failed. A fixed vocabulary: a job's error never holds
/// free text, so it cannot carry a path, a token, or content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobFailure {
    /// The pinned revision could not be read: its artifact was purged, or
    /// stored files no longer form a listing.
    RevisionUnavailable,
    /// Storage kept failing for every attempt.
    StorageUnavailable,
    /// A worker held the final attempt and stopped renewing its lease.
    LeaseExpired,
    /// A failure the worker did not expect.
    Internal,
    /// The worker's build does not know the job's profile, so the job cannot
    /// run anywhere this build runs (decision 0057 rule 1). It is never retried.
    ExecutionUnavailable,
}

impl JobFailure {
    /// Stable lowercase name used in storage and wire contracts.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RevisionUnavailable => "revision_unavailable",
            Self::StorageUnavailable => "storage_unavailable",
            Self::LeaseExpired => "lease_expired",
            Self::Internal => "internal",
            Self::ExecutionUnavailable => "execution_unavailable",
        }
    }

    /// Whether another attempt can succeed where this one failed.
    #[must_use]
    pub fn is_retryable(self) -> bool {
        matches!(self, Self::StorageUnavailable)
    }
}

impl FromStr for JobFailure {
    type Err = InvalidValue;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        [
            Self::RevisionUnavailable,
            Self::StorageUnavailable,
            Self::LeaseExpired,
            Self::Internal,
            Self::ExecutionUnavailable,
        ]
        .into_iter()
        .find(|failure| failure.as_str() == value)
        .ok_or(InvalidValue {
            what: "job failure",
            rule: "expected a job failure name",
        })
    }
}

/// What is wrong with one file of a verified revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    /// The stored content has another SHA-256 digest than the listing.
    DigestMismatch,
    /// The stored content has another size than the listing.
    SizeMismatch,
    /// The listing names a file whose content is not stored.
    ContentMissing,
}

impl Problem {
    /// Stable lowercase name used in storage and wire contracts.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DigestMismatch => "digest_mismatch",
            Self::SizeMismatch => "size_mismatch",
            Self::ContentMissing => "content_missing",
        }
    }
}

impl FromStr for Problem {
    type Err = InvalidValue;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        [
            Self::DigestMismatch,
            Self::SizeMismatch,
            Self::ContentMissing,
        ]
        .into_iter()
        .find(|problem| problem.as_str() == value)
        .ok_or(InvalidValue {
            what: "verification problem",
            rule: "expected a problem name",
        })
    }
}

/// One mismatch: the package path, which is relative to the artifact, and
/// what is wrong with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// The file, as the revision names it.
    pub path: PackagePath,
    /// What is wrong.
    pub problem: Problem,
}

/// The result of a verification that ran to the end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyReport {
    /// How many files of the revision were read.
    pub files_checked: u64,
    /// The sum of the sizes of the content that was read.
    pub bytes_checked: u64,
    /// How many files do not match their listing, counted without the bound
    /// of [`Self::findings`].
    pub mismatch_count: u64,
    /// The first mismatches in path order, at most [`MAX_FINDINGS`].
    pub findings: Vec<Finding>,
}

impl VerifyReport {
    /// Whether every file matched.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.mismatch_count == 0
    }

    /// Whether [`Self::findings`] lists fewer mismatches than there are.
    #[must_use]
    pub fn is_truncated(&self) -> bool {
        // A list is never longer than its count, so inequality means cut short.
        u64::try_from(self.findings.len()) != Ok(self.mismatch_count)
    }
}

/// Why publication of an attempt was revoked (decision 0057 rule 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Revocation {
    /// The job was asked to stop.
    Cancel,
    /// The artifact was moved to the trash while the attempt ran.
    Trash,
    /// The artifact was purged while the attempt ran.
    Purge,
    /// The actor's state generation moved on: it was disabled, or enabled
    /// again, after the attempt pinned its generation.
    Generation,
    /// The actor no longer has access to the artifact.
    Authority,
    /// A restore marked the attempt cleanup pending (decision 0057 rule 13). An
    /// ordinary expired lease stores no revocation (rules 10 and 19).
    Lease,
    /// The worker shut down while the attempt ran.
    Shutdown,
}

impl Revocation {
    /// Stable lowercase name used in storage and wire contracts.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cancel => "cancel",
            Self::Trash => "trash",
            Self::Purge => "purge",
            Self::Generation => "generation",
            Self::Authority => "authority",
            Self::Lease => "lease",
            Self::Shutdown => "shutdown",
        }
    }

    /// The checkpoint answer that a stored revocation gives. A stored
    /// `shutdown` is answered as `lease`, because rule 7 lists no shutdown
    /// answer and rule 10 settles both alike.
    #[must_use]
    pub fn stop_reason(self) -> StopReason {
        match self {
            Self::Cancel => StopReason::Cancel,
            Self::Trash => StopReason::Trash,
            Self::Purge => StopReason::Purge,
            Self::Generation => StopReason::Generation,
            Self::Authority => StopReason::Authority,
            Self::Lease | Self::Shutdown => StopReason::Lease,
        }
    }

    /// Whether settling an attempt revoked this way ends its job as
    /// `cancelled`. Decision 0057 rule 10 reads: "to `cancelled` for `cancel`,
    /// `trash`, `purge`, `generation` and `authority`, and to `retry_scheduled`
    /// or `failed` by its attempts for `lease` and `shutdown`".
    #[must_use]
    pub fn ends_cancelled(self) -> bool {
        matches!(
            self,
            Self::Cancel | Self::Trash | Self::Purge | Self::Generation | Self::Authority
        )
    }
}

impl FromStr for Revocation {
    type Err = InvalidValue;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        [
            Self::Cancel,
            Self::Trash,
            Self::Purge,
            Self::Generation,
            Self::Authority,
            Self::Lease,
            Self::Shutdown,
        ]
        .into_iter()
        .find(|revocation| revocation.as_str() == value)
        .ok_or(InvalidValue {
            what: "revocation",
            rule: "expected a revocation name",
        })
    }
}

/// Where a claimed attempt stands in its cleanup (decision 0057 rule 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptState {
    /// The attempt may still run.
    Live,
    /// The cleanup is not confirmed, so the attempt's job may not be claimed
    /// again and may not end (decision 0057 rule 10).
    CleanupPending,
    /// The cleanup is confirmed, and the attempt no longer blocks its job.
    Settled,
}

impl AttemptState {
    /// Stable lowercase name used in storage and wire contracts.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::CleanupPending => "cleanup_pending",
            Self::Settled => "settled",
        }
    }
}

impl FromStr for AttemptState {
    type Err = InvalidValue;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        [Self::Live, Self::CleanupPending, Self::Settled]
            .into_iter()
            .find(|state| state.as_str() == value)
            .ok_or(InvalidValue {
                what: "attempt state",
                rule: "expected an attempt state name",
            })
    }
}

/// Why a checkpoint or a publication of a `run_profile` attempt was refused
/// (decision 0057 rule 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// Cancellation was requested for the job.
    Cancel,
    /// The artifact is in the trash.
    Trash,
    /// The artifact was purged.
    Purge,
    /// The actor is disabled, or its state generation differs from the pinned
    /// one.
    Generation,
    /// The actor no longer has access to the artifact.
    Authority,
    /// The attempt is revoked as `lease` (by a restore, decision 0057 rule 13)
    /// or as `shutdown`. An ordinary expired lease revokes nothing: the attempt
    /// waits for the reconciler (rules 10 and 19).
    Lease,
    /// The job is no longer running with the attempt's fence.
    Fence,
}

impl StopReason {
    /// Stable lowercase name used in storage and wire contracts.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cancel => "cancel",
            Self::Trash => "trash",
            Self::Purge => "purge",
            Self::Generation => "generation",
            Self::Authority => "authority",
            Self::Lease => "lease",
            Self::Fence => "fence",
        }
    }

    /// The revocation that a refusal stores in its transaction, or `None` for
    /// `fence`, which stores none.
    #[must_use]
    pub fn revocation(self) -> Option<Revocation> {
        match self {
            Self::Cancel => Some(Revocation::Cancel),
            Self::Trash => Some(Revocation::Trash),
            Self::Purge => Some(Revocation::Purge),
            Self::Generation => Some(Revocation::Generation),
            Self::Authority => Some(Revocation::Authority),
            Self::Lease => Some(Revocation::Lease),
            Self::Fence => None,
        }
    }
}

/// The answer to a checkpoint of a `run_profile` attempt. It is `Continue` only
/// when the attempt is `live` and not revoked, the job is `running` with the
/// attempt's fence and no cancel requested, the artifact is neither trashed nor
/// purged, and the actor is enabled with the pinned generation and still has
/// access (decision 0057 rule 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointAnswer {
    /// The attempt may keep running.
    Continue,
    /// The attempt must stop, for the given reason.
    Stop(StopReason),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_id_round_trips_and_rejects_noncanonical_spellings() {
        let id = JobId::from_random([0xab; 16]);
        assert_eq!(id.as_str(), format!("job_{}", "ab".repeat(16)));
        assert_eq!(id.as_str().parse::<JobId>(), Ok(id));
        for value in [
            "",
            "job_",
            &format!("job_{}", "AB".repeat(16)),
            &format!("job_{}", "a".repeat(31)),
            &format!("job_{}", "a".repeat(33)),
            &format!("JOB_{}", "a".repeat(32)),
            &format!("art_{}", "a".repeat(32)),
            &format!("job_{}/", "a".repeat(31)),
        ] {
            assert!(value.parse::<JobId>().is_err(), "accepted {value:?}");
        }
    }

    #[test]
    fn names_round_trip_through_their_text_form() {
        for state in [
            JobState::Queued,
            JobState::Running,
            JobState::RetryScheduled,
            JobState::Succeeded,
            JobState::Failed,
            JobState::Cancelled,
        ] {
            assert_eq!(state.as_str().parse::<JobState>(), Ok(state));
        }
        for failure in [
            JobFailure::RevisionUnavailable,
            JobFailure::StorageUnavailable,
            JobFailure::LeaseExpired,
            JobFailure::Internal,
        ] {
            assert_eq!(failure.as_str().parse::<JobFailure>(), Ok(failure));
        }
        for problem in [
            Problem::DigestMismatch,
            Problem::SizeMismatch,
            Problem::ContentMissing,
        ] {
            assert_eq!(problem.as_str().parse::<Problem>(), Ok(problem));
        }
        assert_eq!(
            JobKind::VerifyRevision.as_str().parse::<JobKind>(),
            Ok(JobKind::VerifyRevision)
        );
        assert!("Queued".parse::<JobState>().is_err());
        assert!("build".parse::<JobKind>().is_err());
    }

    #[test]
    fn only_the_three_end_states_are_terminal() {
        let terminal: Vec<_> = [
            JobState::Queued,
            JobState::Running,
            JobState::RetryScheduled,
            JobState::Succeeded,
            JobState::Failed,
            JobState::Cancelled,
        ]
        .into_iter()
        .filter(|state| state.is_terminal())
        .collect();
        assert_eq!(
            terminal,
            [JobState::Succeeded, JobState::Failed, JobState::Cancelled]
        );
    }

    #[test]
    fn worker_id_is_a_bounded_label() {
        assert!("worker-0a1b2c3d".parse::<WorkerId>().is_ok());
        assert!(
            WorkerId::from_random([1, 2, 3, 4])
                .as_str()
                .parse::<WorkerId>()
                .is_ok()
        );
        for value in ["", "Worker", "has space", &"w".repeat(65)] {
            assert!(value.parse::<WorkerId>().is_err(), "accepted {value:?}");
        }
    }

    #[test]
    fn a_report_says_when_its_list_is_cut_short() {
        let finding = |path: &str| -> Result<Finding, Box<dyn std::error::Error>> {
            Ok(Finding {
                path: path.parse()?,
                problem: Problem::DigestMismatch,
            })
        };
        let whole = VerifyReport {
            files_checked: 2,
            bytes_checked: 10,
            mismatch_count: 1,
            findings: vec![finding("index.html").unwrap_or_else(|e| panic!("{e}"))],
        };
        assert!(!whole.is_ok());
        assert!(!whole.is_truncated());
        let cut = VerifyReport {
            mismatch_count: 51,
            ..whole.clone()
        };
        assert!(cut.is_truncated());
        let clean = VerifyReport {
            mismatch_count: 0,
            findings: Vec::new(),
            ..whole
        };
        assert!(clean.is_ok() && !clean.is_truncated());
    }
}
