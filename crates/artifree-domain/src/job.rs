//! Persistent job vocabulary (decision 0030).
//!
//! A job pins an actor, an artifact, one source revision, and a kind. The one
//! kind today, [`JobKind::VerifyRevision`], re-reads every file of the pinned
//! revision and compares it with the digest and size stored for it. It runs
//! trusted in-repository code only: untrusted execution, builds, and provider
//! sampling are not job kinds.

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

/// What a job does. Each kind is trusted in-repository code; adding one needs
/// a decision record and a schema step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    /// Re-reads every file of a pinned revision and compares its SHA-256
    /// digest and size with the stored listing.
    VerifyRevision,
}

impl JobKind {
    /// Stable lowercase name used in storage and wire contracts.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::VerifyRevision => "verify_revision",
        }
    }
}

impl FromStr for JobKind {
    type Err = InvalidValue;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        [Self::VerifyRevision]
            .into_iter()
            .find(|kind| kind.as_str() == value)
            .ok_or(InvalidValue {
                what: "job kind",
                rule: "expected verify_revision",
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
