//! Persistent jobs (decision 0030): enqueue, read, list, and cancel through
//! [`ArtifactService`], like every other mutation. The worker's side is in
//! [`crate::worker`].
//!
//! A job pins the actor, one artifact, one source revision, and a kind, and
//! its request key is fingerprinted like the keys of artifact requests. Access
//! follows reads: an artifact in the trash, purged, missing, or another
//! actor's makes every job request of it `not found`, a retry included.
//! [`JobKind::VerifyRevision`] runs trusted in-repository code only; it is not
//! a way to run untrusted code, builds, or provider sampling, and nothing here
//! claims it is. [`JobKind::RunProfile`] is enqueued only by tests in this
//! build (see [`ArtifactService::enqueue_profile_job`]). Cancelling a job never
//! rolls back anything it did; a verification does nothing outside its own
//! record.

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use artifree_domain::SourceRevision;
use artifree_domain::identity::{ActorId, ArtifactId, InvalidValue, RequestKey, UnixMillis};
use artifree_domain::job::{JobId, JobKind, ProfileName};
use artifree_ports::artifact_store::{ArtifactStore, RequestIdentity};
use artifree_ports::job_store::{
    JobCancel, JobListing, JobPosition, JobReceipt, JobRecord, JobStore, JobStoreError, NewJob,
    NewProfileJob,
};
use sha2::{Digest, Sha256};

use crate::artifacts::{ArtifactError, ArtifactService, Fingerprint, Missing};

/// Where the current time comes from. Everything that stamps a job reads it
/// here, so tests move time without waiting for it.
pub trait Clock: fmt::Debug + Send + Sync {
    /// The current time in milliseconds since the Unix epoch.
    fn now(&self) -> UnixMillis;
}

/// The operating system's clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> UnixMillis {
        // A clock before 1970 or beyond the range reads as the nearest bound:
        // job times only order and expire things.
        let elapsed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis());
        UnixMillis(i64::try_from(elapsed).unwrap_or(i64::MAX))
    }
}

/// The number of jobs in one page, from 1 to [`JobPageSize::MAX`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobPageSize(usize);

impl JobPageSize {
    /// The largest page.
    pub const MAX: Self = Self(100);
    /// The page a caller gets without naming a size.
    pub const DEFAULT: Self = Self(50);

    /// The number of jobs.
    #[must_use]
    pub fn get(self) -> usize {
        self.0
    }
}

impl Default for JobPageSize {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl TryFrom<u64> for JobPageSize {
    type Error = InvalidValue;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        usize::try_from(value)
            .ok()
            .filter(|size| (1..=Self::MAX.0).contains(size))
            .map(Self)
            .ok_or(InvalidValue {
                what: "page size",
                rule: "must be 1-100",
            })
    }
}

/// One page of jobs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobPage {
    /// The jobs, newest first.
    pub jobs: Vec<JobRecord>,
    /// Continues the list; present only when more jobs follow.
    pub next_page_token: Option<String>,
}

const TOKEN_VERSION: &str = "j1";

impl<S: ArtifactStore + JobStore> ArtifactService<S> {
    /// Enqueues a job of `kind` pinned to `revision` of an artifact `actor`
    /// may read.
    ///
    /// Retrying with the same key and request returns the job as it is now,
    /// delivered as a replay. The same key for another request is
    /// [`ArtifactError::RequestKeyReused`].
    ///
    /// # Errors
    ///
    /// [`ArtifactError::NotFound`] for an artifact that is missing, in the
    /// trash, purged, or another actor's, and for a revision it does not
    /// have, [`ArtifactError::TooManyJobs`] when the actor has the largest
    /// number of jobs waiting or running, [`ArtifactError::RequestKeyReused`],
    /// or [`ArtifactError::Unavailable`].
    pub fn enqueue_job(
        &self,
        actor: &ActorId,
        key: &RequestKey,
        artifact: &ArtifactId,
        revision: SourceRevision,
        kind: JobKind,
        now: UnixMillis,
    ) -> Result<JobReceipt, ArtifactError> {
        let record = self.authorized(actor, artifact)?;
        if revision.0 == 0 || revision > record.current_revision {
            return Err(ArtifactError::NotFound(Missing::Revision));
        }
        let mut hasher = Fingerprint::new("enqueue_job");
        hasher.field(artifact.as_str().as_bytes());
        hasher.field(&revision.0.to_be_bytes());
        hasher.field(kind.as_str().as_bytes());
        self.store
            .enqueue_job(NewJob {
                request: RequestIdentity {
                    actor,
                    key,
                    fingerprint: hasher.finish(),
                },
                artifact,
                revision,
                kind,
                now,
            })
            .map_err(|error| job_error(error, Missing::Artifact))
    }

    /// Enqueues a `run_profile` job with `profile`, pinned like
    /// [`Self::enqueue_job`] pins a verification: actor, artifact, revision,
    /// kind and profile. Only tests call it. No HTTP route, MCP tool or CLI
    /// command reaches it (decision 0057 rule 3), and no worker of this build
    /// runs the kind, so a worker ends the job as `execution_unavailable`
    /// (decision 0057 rule 1). The job is hidden from [`Self::job`] and
    /// [`Self::jobs`] and does not count toward the actor's open jobs.
    ///
    /// # Errors
    ///
    /// As [`Self::enqueue_job`], except that the open-job bound does not apply.
    pub fn enqueue_profile_job(
        &self,
        actor: &ActorId,
        key: &RequestKey,
        artifact: &ArtifactId,
        revision: SourceRevision,
        profile: &ProfileName,
        now: UnixMillis,
    ) -> Result<JobReceipt, ArtifactError> {
        let record = self.authorized(actor, artifact)?;
        if revision.0 == 0 || revision > record.current_revision {
            return Err(ArtifactError::NotFound(Missing::Revision));
        }
        let mut hasher = Fingerprint::new("enqueue_job");
        hasher.field(artifact.as_str().as_bytes());
        hasher.field(&revision.0.to_be_bytes());
        hasher.field(JobKind::RunProfile.as_str().as_bytes());
        // The profile is pinned too, so the same key with another profile is
        // another request (decision 0030 rule 2).
        hasher.field(profile.as_str().as_bytes());
        self.store
            .enqueue_profile_job(NewProfileJob {
                request: RequestIdentity {
                    actor,
                    key,
                    fingerprint: hasher.finish(),
                },
                artifact,
                revision,
                profile,
                now,
            })
            .map_err(|error| job_error(error, Missing::Artifact))
    }

    /// A job `actor` enqueued whose artifact it may still read.
    ///
    /// # Errors
    ///
    /// [`ArtifactError::NotFound`] for a job that is missing, another
    /// actor's, or of an artifact in the trash or purged, or
    /// [`ArtifactError::Unavailable`].
    pub fn job(&self, actor: &ActorId, id: &JobId) -> Result<JobRecord, ArtifactError> {
        let job = self
            .store
            .job(id)
            .map_err(|error| job_error(error, Missing::Job))?
            .filter(|job| job.actor == *actor)
            .ok_or(ArtifactError::NotFound(Missing::Job))?;
        self.authorized(actor, &job.artifact)
            .map_err(|error| match error {
                ArtifactError::NotFound(_) => ArtifactError::NotFound(Missing::Job),
                other => other,
            })?;
        Ok(job)
    }

    /// One page of the actor's jobs, newest first: those of one artifact, or
    /// of all artifacts `actor` may read.
    ///
    /// # Errors
    ///
    /// [`ArtifactError::NotFound`] when `artifact` is not readable,
    /// [`ArtifactError::InvalidPageToken`], or [`ArtifactError::Unavailable`].
    pub fn jobs(
        &self,
        actor: &ActorId,
        artifact: Option<&ArtifactId>,
        page_token: Option<&str>,
        limit: JobPageSize,
    ) -> Result<JobPage, ArtifactError> {
        if let Some(artifact) = artifact {
            self.authorized(actor, artifact)?;
        }
        let after = page_token
            .map(|token| position(token, artifact).ok_or(ArtifactError::InvalidPageToken))
            .transpose()?;
        // One more than a page tells whether another page follows.
        let mut jobs = self
            .store
            .list_jobs(JobListing {
                actor,
                artifact,
                after: after.as_ref(),
                limit: limit.get() + 1,
            })
            .map_err(|error| job_error(error, Missing::Job))?;
        let next_page_token = if jobs.len() > limit.get() {
            jobs.truncate(limit.get());
            jobs.last().map(|last| page_token_after(artifact, last))
        } else {
            None
        };
        Ok(JobPage {
            jobs,
            next_page_token,
        })
    }

    /// Cancels a job of `actor`. A job that has not started ends as cancelled
    /// at once. A running job gets a cancel request; its worker stops at the
    /// next checkpoint and the job ends as cancelled, unless it ended first.
    /// This stops work. It does not undo effects of the work, and says
    /// nothing about them.
    ///
    /// Retrying with the same key returns the job as it is now, as a replay.
    ///
    /// # Errors
    ///
    /// [`ArtifactError::NotFound`] as for [`Self::job`],
    /// [`ArtifactError::JobEnded`] for a job that is already succeeded,
    /// failed, or cancelled, [`ArtifactError::RequestKeyReused`], or
    /// [`ArtifactError::Unavailable`].
    pub fn cancel_job(
        &self,
        actor: &ActorId,
        key: &RequestKey,
        id: &JobId,
        now: UnixMillis,
    ) -> Result<JobReceipt, ArtifactError> {
        let mut hasher = Fingerprint::new("cancel_job");
        hasher.field(id.as_str().as_bytes());
        self.store
            .cancel_job(JobCancel {
                request: RequestIdentity {
                    actor,
                    key,
                    fingerprint: hasher.finish(),
                },
                job: id,
                now,
            })
            .map_err(|error| job_error(error, Missing::Job))
    }
}

/// Maps a job store failure; `missing` is what a `NotFound` names.
fn job_error(error: JobStoreError, missing: Missing) -> ArtifactError {
    match error {
        JobStoreError::NotFound => ArtifactError::NotFound(missing),
        JobStoreError::RequestKeyReused => ArtifactError::RequestKeyReused,
        JobStoreError::Conflict(state) => ArtifactError::JobEnded { state },
        JobStoreError::LimitReached => ArtifactError::TooManyJobs,
        JobStoreError::Unavailable(reason) => ArtifactError::Unavailable(reason),
        // A lease belongs to a worker; no request of an actor holds one.
        other => ArtifactError::Unavailable(other.to_string()),
    }
}

/// The digest of what a list is about: one artifact, or all of them. A token
/// of one list does not continue another.
fn scope_digest(artifact: Option<&ArtifactId>) -> String {
    let mut hasher = Sha256::new();
    match artifact {
        Some(artifact) => {
            hasher.update(b"artifact:");
            hasher.update(artifact.as_str());
        }
        None => hasher.update(b"all"),
    }
    hasher.finalize()[..8]
        .iter()
        .flat_map(|byte| [byte >> 4, byte & 0x0f])
        .filter_map(|nibble| char::from_digit(u32::from(nibble), 16))
        .collect()
}

/// The token that continues a list after `last`.
fn page_token_after(artifact: Option<&ArtifactId>, last: &JobRecord) -> String {
    format!(
        "{TOKEN_VERSION}.{}.{}.{}",
        scope_digest(artifact),
        last.created_at.0,
        last.id
    )
}

/// The position a token continues from, or `None` when it is malformed or
/// belongs to another list. The creation time must be written the way the
/// token writes it, without a sign or leading zeros.
fn position(token: &str, artifact: Option<&ArtifactId>) -> Option<JobPosition> {
    let mut parts = token.splitn(4, '.');
    let (version, scope, created, id) =
        (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    let canonical = !created.is_empty()
        && created.bytes().all(|byte| byte.is_ascii_digit())
        && (created == "0" || !created.starts_with('0'));
    if version != TOKEN_VERSION || scope != scope_digest(artifact) || !canonical {
        return None;
    }
    Some(JobPosition {
        created_at: UnixMillis(created.parse().ok()?),
        id: id.parse().ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn record(created: i64) -> Result<JobRecord, Box<dyn std::error::Error>> {
        Ok(JobRecord {
            id: JobId::from_random([7; 16]),
            actor: "local".parse()?,
            artifact: ArtifactId::from_random([1; 16]),
            revision: SourceRevision(1),
            kind: JobKind::VerifyRevision,
            profile: None,
            state: artifree_domain::job::JobState::Queued,
            cancel_requested: false,
            attempts: 0,
            max_attempts: 3,
            run_after: UnixMillis(created),
            lease_expires_at: None,
            worker: None,
            failure: None,
            report: None,
            counts: None,
            created_at: UnixMillis(created),
            started_at: None,
            finished_at: None,
        })
    }

    #[test]
    fn a_token_continues_only_the_list_it_came_from() -> TestResult {
        let last = record(20)?;
        let other = ArtifactId::from_random([2; 16]);
        let token = page_token_after(Some(&last.artifact), &last);
        assert_eq!(
            position(&token, Some(&last.artifact)),
            Some(JobPosition {
                created_at: UnixMillis(20),
                id: last.id.clone()
            })
        );
        assert_eq!(position(&token, Some(&other)), None, "another artifact");
        assert_eq!(position(&token, None), None, "the global list");
        let global = page_token_after(None, &last);
        assert!(position(&global, None).is_some());
        assert_eq!(position(&global, Some(&last.artifact)), None);
        Ok(())
    }

    #[test]
    fn a_token_that_is_not_the_one_written_is_refused() -> TestResult {
        let last = record(20)?;
        let valid = page_token_after(None, &last);
        let digest = scope_digest(None);
        let id = last.id.to_string();
        for token in [
            String::new(),
            "j1".to_owned(),
            valid.replacen("j1", "j2", 1),
            valid.replace(".20.", ".x."),
            valid.replace(".20.", ".020."),
            valid.replace(".20.", ".+20."),
            valid.replace(".20.", ".-20."),
            valid.replace(".20.", ".."),
            format!("j1.{digest}.20.{id}0"),
            format!("j1.{digest}.20.{id}.extra"),
            format!("j1.{}.20.{id}", digest.to_uppercase()),
            format!("j1.{digest}.99999999999999999999.{id}"),
        ] {
            assert_eq!(position(&token, None), None, "{token:?}");
        }
        Ok(())
    }

    #[test]
    fn page_size_is_bounded() {
        assert!(JobPageSize::try_from(0).is_err());
        assert!(JobPageSize::try_from(101).is_err());
        assert_eq!(JobPageSize::try_from(100).map(JobPageSize::get), Ok(100));
        assert_eq!(JobPageSize::default().get(), 50);
    }
}
