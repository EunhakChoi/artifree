//! Durable storage for the builds of `run_profile` jobs (decision 0058 rules 9
//! to 13 and 15).
//!
//! A build is the output of one `run_profile` job, kept for its artifact,
//! revision and profile. It is published in the transaction that ends the job
//! as succeeded, so no build exists without its job's success, and at most one
//! exists for the three: the first is kept. An artifact keeps at most
//! [`MAX_BUILDS_PER_ARTIFACT`] builds.
//!
//! A failed build keeps bounded diagnostics, and only the newest failed
//! `run_profile` job of an artifact and profile keeps any. Diagnostics can name
//! source text, so a caller gives them only to whoever may read the job's
//! revision (rule 13).
//!
//! Builds and diagnostics of an artifact in the trash are kept and read by
//! none; a purge deletes them (rule 12).

use artifree_domain::SourceRevision;
use artifree_domain::identity::{ArtifactId, UnixMillis};
use artifree_domain::job::{AttemptIdentity, BuildDiagnostic, JobFailure, JobId, ProfileName};
use artifree_domain::package::FilePackage;

use crate::job_store::{JobRecord, JobStoreError, Lease, ProfileBinding};

/// The most builds an artifact keeps (decision 0058 rule 12).
pub const MAX_BUILDS_PER_ARTIFACT: usize = 8;

/// One stored build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildRecord {
    /// The artifact it was built from.
    pub artifact: ArtifactId,
    /// The revision it was built from.
    pub revision: SourceRevision,
    /// The profile that built it.
    pub profile: ProfileName,
    /// The job that published it.
    pub job: JobId,
    /// The tool root manifest digest of the attempt, 64 lowercase hex.
    pub tool_digest: String,
    /// How many files it has.
    pub file_count: u64,
    /// How many bytes its files have together.
    pub total_bytes: u64,
    /// When it was published.
    pub published_at: UnixMillis,
}

/// What [`BuildStore::publish_build`] did with the files (decision 0058 rule 10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Publication {
    /// The files were published as this new build.
    Published(BuildRecord),
    /// A build for the artifact, revision and profile existed already. It is
    /// unchanged, and the files were not stored.
    AlreadyBuilt(BuildRecord),
}

/// A request to publish the output of a `run_profile` attempt.
#[derive(Debug, Clone, Copy)]
pub struct BuildPublication<'a> {
    /// The worker's lease of the job.
    pub lease: &'a Lease,
    /// The attempt whose output it is.
    pub attempt: &'a AttemptIdentity,
    /// What the attempt's manifest binding names (decision 0057 rule 12).
    pub binding: ProfileBinding<'a>,
    /// The composed output.
    pub files: &'a FilePackage,
    /// The caller's clock.
    pub now: UnixMillis,
}

/// Durable storage for builds and their diagnostics.
pub trait BuildStore {
    /// Ends a `run_profile` job as succeeded and publishes its output, in one
    /// transaction (decision 0058 rule 11). The job is checked as
    /// `JobStore::complete_profile_job` checks it, and its counts are the
    /// output's file and byte counts. When a build for the job's artifact,
    /// revision and profile exists, the job still succeeds and the existing
    /// build is kept (rule 10). Otherwise the build is stored, and the
    /// artifact's oldest builds by publication time are removed until it has
    /// at most [`MAX_BUILDS_PER_ARTIFACT`], never the latest build of a
    /// profile and never this one (rule 12). A refusal stores no file.
    ///
    /// # Errors
    ///
    /// The errors of `JobStore::complete_profile_job`, which store nothing
    /// but a revocation, and [`JobStoreError::Unavailable`].
    fn publish_build(
        &self,
        publication: BuildPublication<'_>,
    ) -> Result<(JobRecord, Publication), JobStoreError>;

    /// Ends a `run_profile` job as failed without a retry (decision 0058 rule
    /// 18). A `build_failed` job keeps `diagnostics` in their order; any other
    /// failure takes none. A job whose cancel was requested ends cancelled
    /// instead and keeps none. Either way the diagnostics of the artifact's
    /// other `run_profile` jobs of the profile are deleted when the job fails
    /// (rule 13).
    ///
    /// # Errors
    ///
    /// [`JobStoreError::InvalidRequest`] for a job that is not `run_profile`,
    /// a failure that is not one of rule 18's, `build_failed` without 1 to
    /// [`artifree_domain::job::MAX_DIAGNOSTICS`] diagnostics, or diagnostics
    /// with another failure; [`JobStoreError::LeaseLost`],
    /// [`JobStoreError::AttemptUnsettled`], or [`JobStoreError::Unavailable`].
    fn fail_build(
        &self,
        lease: &Lease,
        failure: JobFailure,
        diagnostics: &[BuildDiagnostic],
        now: UnixMillis,
    ) -> Result<JobRecord, JobStoreError>;

    /// The build of one revision for a profile, or `None` when there is none
    /// or the artifact is in the trash or purged.
    ///
    /// # Errors
    ///
    /// [`JobStoreError::Unavailable`].
    fn build(
        &self,
        artifact: &ArtifactId,
        revision: SourceRevision,
        profile: &ProfileName,
    ) -> Result<Option<BuildRecord>, JobStoreError>;

    /// The latest build of an artifact for a profile: the one with the highest
    /// revision (decision 0058 rule 15). `None` as for [`BuildStore::build`].
    ///
    /// # Errors
    ///
    /// [`JobStoreError::Unavailable`].
    fn latest_build(
        &self,
        artifact: &ArtifactId,
        profile: &ProfileName,
    ) -> Result<Option<BuildRecord>, JobStoreError>;

    /// The files of one build. `None` as for [`BuildStore::build`].
    ///
    /// # Errors
    ///
    /// [`JobStoreError::Unavailable`].
    fn build_files(
        &self,
        artifact: &ArtifactId,
        revision: SourceRevision,
        profile: &ProfileName,
    ) -> Result<Option<FilePackage>, JobStoreError>;

    /// The diagnostics a job keeps, in their order: empty for a job that keeps
    /// none, and for a job of an artifact in the trash or purged.
    ///
    /// # Errors
    ///
    /// [`JobStoreError::Unavailable`].
    fn diagnostics(&self, job: &JobId) -> Result<Vec<BuildDiagnostic>, JobStoreError>;
}
