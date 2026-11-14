//! Reads of builds (decision 0058 rules 13 to 16): the build a build preview
//! serves, its files, and the head status.
//!
//! Access follows reads, as for revisions: an artifact in the trash, purged,
//! missing or another actor's is `not found`, and a build preview link stops
//! serving when access ends because every file read checks it again. The
//! head status gives a failed job's diagnostics, which can name source text,
//! only to a caller who may read the head revision, which is the job's
//! (rule 13). The job reads show a failed job's diagnostics (slice R03da);
//! the build preview and the head status reach the wire in slice R03db.

use artifree_domain::SourceRevision;
use artifree_domain::identity::{ActorId, ArtifactId};
use artifree_domain::job::{BuildDiagnostic, JobFailure, JobId, JobKind, JobState, ProfileName};
use artifree_domain::package::PackagePath;
use artifree_ports::artifact_store::ArtifactStore;
use artifree_ports::build_store::{BuildRecord, BuildSnapshot, BuildStore};
use artifree_ports::job_store::{JobRecord, JobStore};

use crate::artifacts::{ArtifactError, ArtifactService, Missing};
use crate::jobs::job_error;

/// Where the head revision's build stands (decision 0058 rule 16).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadBuild {
    /// The head revision has a build; it is the latest build.
    Built,
    /// The newest job for the head is queued, scheduled for a retry or
    /// running.
    Pending {
        /// That job.
        job: JobId,
    },
    /// The newest job for the head failed.
    Failed {
        /// That job.
        job: JobId,
        /// Why it failed.
        failure: JobFailure,
        /// The diagnostics it keeps, in their order: none unless it is still
        /// the artifact's newest failed job of the profile (rule 13).
        diagnostics: Vec<BuildDiagnostic>,
    },
    /// No job was enqueued for the head, or its newest one was cancelled or
    /// succeeded without a build.
    None,
}

/// The builds of an artifact for one profile, as the reads report them
/// (decision 0058 rules 15 and 16).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildStatus {
    /// The head revision when it was read.
    pub head: SourceRevision,
    /// The latest build: the one with the highest revision. A newer revision
    /// without a build leaves it in place.
    pub latest: Option<BuildRecord>,
    /// Where the head revision's build stands.
    pub head_build: HeadBuild,
}

impl<S: ArtifactStore + JobStore + BuildStore> ArtifactService<S> {
    /// The build a build preview of `revision` serves (decision 0058 rule
    /// 14), checked as a source preview checks its revision.
    ///
    /// # Errors
    ///
    /// [`ArtifactError::NotFound`] for an artifact that is not readable, a
    /// revision it does not have, or, with [`Missing::Build`], a revision
    /// without a build for `profile`; or [`ArtifactError::Unavailable`].
    pub fn preview_build(
        &self,
        actor: &ActorId,
        id: &ArtifactId,
        revision: SourceRevision,
        profile: &ProfileName,
    ) -> Result<BuildRecord, ArtifactError> {
        let record = self.authorized(actor, id)?;
        if revision.0 == 0 || revision > record.current_revision {
            return Err(ArtifactError::NotFound(Missing::Revision));
        }
        self.store
            .build(id, revision, profile)
            .map_err(store_error)?
            .ok_or(ArtifactError::NotFound(Missing::Build))
    }

    /// One file of a build. Access is checked on every read, so a build
    /// preview link stops serving when access ends or the artifact moves to
    /// the trash, as a source preview link does.
    ///
    /// # Errors
    ///
    /// [`ArtifactError::NotFound`] for an artifact that is not readable or a
    /// revision it does not have, and with [`Missing::File`] for a file the
    /// build does not hold or a build that no longer exists; or
    /// [`ArtifactError::Unavailable`].
    pub fn read_build_file(
        &self,
        actor: &ActorId,
        id: &ArtifactId,
        revision: SourceRevision,
        profile: &ProfileName,
        path: &PackagePath,
    ) -> Result<Vec<u8>, ArtifactError> {
        let record = self.authorized(actor, id)?;
        if revision.0 == 0 || revision > record.current_revision {
            return Err(ArtifactError::NotFound(Missing::Revision));
        }
        self.store
            .build_file(id, revision, profile, path)
            .map_err(store_error)?
            .ok_or(ArtifactError::NotFound(Missing::File))
    }

    /// The latest build of an artifact for `profile` and where its head
    /// revision's build stands (decision 0058 rules 15 and 16). The head, the
    /// builds and the jobs are read in one snapshot of the store, so a
    /// revision committed meanwhile cannot make the latest build newer than
    /// the head.
    ///
    /// # Errors
    ///
    /// [`ArtifactError::NotFound`] for an artifact that is not readable, or
    /// [`ArtifactError::Unavailable`].
    pub fn build_status(
        &self,
        actor: &ActorId,
        id: &ArtifactId,
        profile: &ProfileName,
    ) -> Result<BuildStatus, ArtifactError> {
        self.authorized(actor, id)?;
        // The artifact can move to the trash between the two reads; it is
        // then as missing as it is to the next request.
        let snapshot = self
            .store
            .build_snapshot(id, profile)
            .map_err(store_error)?
            .ok_or(ArtifactError::NotFound(Missing::Artifact))?;
        Ok(BuildStatus {
            head: snapshot.head,
            head_build: head_build(&snapshot)?,
            latest: snapshot.latest,
        })
    }

    /// The diagnostics a job read shows with `job` (decision 0058 rule 13 and
    /// its R03d split note): those it keeps, for a failed `run_profile` job,
    /// and none for any other. `job` must come from a call that answers only
    /// the actor who enqueued it while it may read the artifact outside the
    /// trash: [`Self::job`], [`Self::jobs`], or the receipt of
    /// [`Self::enqueue_build`], [`Self::enqueue_job`] or [`Self::cancel_job`],
    /// whose store writes refuse an artifact in the trash and whose core
    /// checks the actor first. That is the read rule of 13, so no other check
    /// is made here.
    ///
    /// # Errors
    ///
    /// [`ArtifactError::Unavailable`].
    pub fn job_diagnostics(&self, job: &JobRecord) -> Result<Vec<BuildDiagnostic>, ArtifactError> {
        if job.kind != JobKind::RunProfile || job.state != JobState::Failed {
            return Ok(Vec::new());
        }
        self.store
            .diagnostics(&job.id)
            .map_err(|error| job_error(error, Missing::Job))
    }
}

/// Rule 16 on one snapshot. The latest build has the highest revision, and
/// no revision is newer than the head, so the head has a build exactly when
/// the latest build is the head's.
fn head_build(snapshot: &BuildSnapshot) -> Result<HeadBuild, ArtifactError> {
    if snapshot
        .latest
        .as_ref()
        .is_some_and(|build| build.revision == snapshot.head)
    {
        return Ok(HeadBuild::Built);
    }
    let Some(job) = &snapshot.newest_job else {
        return Ok(HeadBuild::None);
    };
    Ok(match job.state {
        JobState::Queued | JobState::RetryScheduled | JobState::Running => HeadBuild::Pending {
            job: job.id.clone(),
        },
        JobState::Failed => HeadBuild::Failed {
            job: job.id.clone(),
            failure: job.failure.ok_or_else(|| {
                ArtifactError::Unavailable("a failed job names no failure".to_owned())
            })?,
            diagnostics: snapshot.diagnostics.clone(),
        },
        JobState::Succeeded | JobState::Cancelled => HeadBuild::None,
    })
}

/// The store's errors of these reads, which take no request key or lease.
fn store_error(error: artifree_ports::job_store::JobStoreError) -> ArtifactError {
    job_error(error, Missing::Artifact)
}
