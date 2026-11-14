//! The builds of `run_profile` jobs and the diagnostics of failed ones
//! (decision 0058 rules 9 to 13 and 15).
//!
//! A build's files are rows of `blobs`, joined by `build_files.digest`, so
//! every predicate that keeps a blob also counts build files (rule 12). A
//! build removed by the bound of eight leaves its blobs to the existing
//! reclamation of unreferenced content.

use artifree_domain::SourceRevision;
use artifree_domain::identity::{ArtifactId, UnixMillis};
use artifree_domain::job::{
    AttemptIdentity, BuildDiagnostic, JobFailure, JobId, JobState, MAX_DIAGNOSTICS, ProfileName,
    SourceLocation,
};
use artifree_domain::package::{FilePackage, PackagePath};
use artifree_ports::artifact_store::StoreError;
use artifree_ports::build_store::{
    BuildPublication, BuildRecord, BuildStore, MAX_BUILDS_PER_ARTIFACT, Publication,
};
use artifree_ports::job_store::{JobRecord, JobStoreError, Lease, ProfileCounts, Retry};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use sha2::{Digest, Sha256};

use super::{artifact_in_place, attempts, fail, held, profile_job};
use crate::storage::{
    SqliteStore, corrupt, sql_revision, stored_path, stored_revision, unavailable,
};

/// The columns of a build row, in the order [`build_from_row`] reads them.
const BUILD_COLUMNS: &str =
    "artifact_id, revision, profile, job_id, tool_digest, file_count, total_bytes, published_at";

fn build_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawBuild> {
    Ok(RawBuild {
        artifact: row.get(0)?,
        revision: row.get(1)?,
        profile: row.get(2)?,
        job: row.get(3)?,
        tool_digest: row.get(4)?,
        file_count: row.get(5)?,
        total_bytes: row.get(6)?,
        published_at: row.get(7)?,
    })
}

/// A build row as `SQLite` returns it.
struct RawBuild {
    artifact: String,
    revision: i64,
    profile: String,
    job: String,
    tool_digest: String,
    file_count: i64,
    total_bytes: i64,
    published_at: i64,
}

impl RawBuild {
    fn into_record(self) -> Result<BuildRecord, StoreError> {
        Ok(BuildRecord {
            artifact: self.artifact.parse().map_err(corrupt)?,
            revision: stored_revision(self.revision)?,
            profile: self.profile.parse().map_err(corrupt)?,
            job: self.job.parse().map_err(corrupt)?,
            tool_digest: self.tool_digest,
            file_count: u64::try_from(self.file_count).map_err(corrupt)?,
            total_bytes: u64::try_from(self.total_bytes).map_err(corrupt)?,
            published_at: UnixMillis(self.published_at),
        })
    }
}

/// The stored build of the three, whether or not the artifact is in place.
fn stored_build(
    connection: &Connection,
    artifact: &ArtifactId,
    revision: SourceRevision,
    profile: &ProfileName,
) -> Result<Option<BuildRecord>, StoreError> {
    connection
        .query_row(
            &format!(
                "SELECT {BUILD_COLUMNS} FROM builds
                 WHERE artifact_id = ?1 AND revision = ?2 AND profile = ?3"
            ),
            params![artifact.as_str(), sql_revision(revision)?, profile.as_str()],
            build_from_row,
        )
        .optional()
        .map_err(unavailable)?
        .map(RawBuild::into_record)
        .transpose()
}

/// A build's files with their digests and counts, computed before the write
/// transaction so that hashing up to 25 MiB does not hold the write lock.
struct Hashed<'a> {
    files: Vec<(&'a PackagePath, [u8; 32], &'a [u8])>,
    counts: ProfileCounts,
}

impl<'a> Hashed<'a> {
    fn of(package: &'a FilePackage) -> Result<Self, StoreError> {
        let mut bytes = 0u64;
        let mut files = Vec::with_capacity(package.len());
        for (path, content) in package.files() {
            bytes = bytes.saturating_add(u64::try_from(content.len()).map_err(corrupt)?);
            files.push((path, Sha256::digest(content).into(), content));
        }
        Ok(Self {
            counts: ProfileCounts {
                files: u64::try_from(files.len()).map_err(corrupt)?,
                bytes,
            },
            files,
        })
    }
}

/// Ends the job as succeeded and publishes its output (decision 0058 rules 10
/// to 12). The outer error is a storage failure, which rolls the transaction
/// back; the inner error is a refusal of [`attempts::complete`], which the
/// caller commits so that a stored revocation persists. A refusal is decided
/// before any file is written.
fn publish(
    transaction: &Transaction<'_>,
    publication: BuildPublication<'_>,
    hashed: &Hashed<'_>,
) -> Result<Result<(JobRecord, Publication), JobStoreError>, JobStoreError> {
    let job = match attempts::complete(
        transaction,
        publication.lease,
        publication.attempt,
        publication.binding,
        hashed.counts,
        publication.now,
    )? {
        Ok(job) => job,
        Err(refusal) => return Ok(Err(refusal)),
    };
    let profile = job
        .profile
        .clone()
        .ok_or_else(|| corrupt("a job with an attempt has no profile"))?;
    if let Some(existing) = stored_build(transaction, &job.artifact, job.revision, &profile)? {
        return Ok(Ok((job, Publication::AlreadyBuilt(existing))));
    }
    insert_build(transaction, &job, &profile, publication, hashed)?;
    prune(transaction, &job.artifact, job.revision, &profile)?;
    let record = stored_build(transaction, &job.artifact, job.revision, &profile)?
        .ok_or_else(|| corrupt("a build vanished inside its transaction"))?;
    Ok(Ok((job, Publication::Published(record))))
}

fn insert_build(
    transaction: &Transaction<'_>,
    job: &JobRecord,
    profile: &ProfileName,
    publication: BuildPublication<'_>,
    hashed: &Hashed<'_>,
) -> Result<(), StoreError> {
    let revision = sql_revision(job.revision)?;
    let counts = hashed.counts;
    transaction
        .execute(
            "INSERT INTO builds (artifact_id, revision, profile, job_id, tool_digest, file_count,
                 total_bytes, published_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                job.artifact.as_str(),
                revision,
                profile.as_str(),
                job.id.as_str(),
                publication.binding.tool_digest,
                i64::try_from(counts.files).map_err(corrupt)?,
                i64::try_from(counts.bytes).map_err(corrupt)?,
                publication.now.0,
            ],
        )
        .map_err(unavailable)?;
    let mut blob = transaction
        .prepare_cached("INSERT OR IGNORE INTO blobs (digest, content) VALUES (?1, ?2)")
        .map_err(unavailable)?;
    let mut file = transaction
        .prepare_cached(
            "INSERT INTO build_files (artifact_id, revision, profile, path, size, digest)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .map_err(unavailable)?;
    for (path, digest, content) in &hashed.files {
        blob.execute(params![digest.as_slice(), content])
            .map_err(unavailable)?;
        file.execute(params![
            job.artifact.as_str(),
            revision,
            profile.as_str(),
            path.as_str(),
            i64::try_from(content.len()).map_err(corrupt)?,
            digest.as_slice(),
        ])
        .map_err(unavailable)?;
    }
    Ok(())
}

/// Removes the artifact's oldest builds by publication time until it has at
/// most [`MAX_BUILDS_PER_ARTIFACT`], never the latest build of a profile (the
/// highest revision, decision 0058 rule 15) and never the one just published.
fn prune(
    transaction: &Transaction<'_>,
    artifact: &ArtifactId,
    revision: SourceRevision,
    profile: &ProfileName,
) -> Result<(), StoreError> {
    let count: i64 = transaction
        .query_row(
            "SELECT count(*) FROM builds WHERE artifact_id = ?1",
            [artifact.as_str()],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    let excess = count.saturating_sub(i64::try_from(MAX_BUILDS_PER_ARTIFACT).map_err(corrupt)?);
    if excess <= 0 {
        return Ok(());
    }
    let removed: Vec<(i64, String)> = transaction
        .prepare(
            "SELECT b.revision, b.profile FROM builds b
             WHERE b.artifact_id = ?1
               AND NOT (b.revision = ?2 AND b.profile = ?3)
               AND b.revision < (SELECT max(l.revision) FROM builds l
                                 WHERE l.artifact_id = b.artifact_id AND l.profile = b.profile)
             ORDER BY b.published_at, b.revision, b.profile
             LIMIT ?4",
        )
        .map_err(unavailable)?
        .query_map(
            params![
                artifact.as_str(),
                sql_revision(revision)?,
                profile.as_str(),
                excess
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(unavailable)?
        .collect::<Result<_, _>>()
        .map_err(unavailable)?;
    for (old_revision, old_profile) in removed {
        for statement in [
            "DELETE FROM build_files WHERE artifact_id = ?1 AND revision = ?2 AND profile = ?3",
            "DELETE FROM builds WHERE artifact_id = ?1 AND revision = ?2 AND profile = ?3",
        ] {
            transaction
                .execute(
                    statement,
                    params![artifact.as_str(), old_revision, old_profile],
                )
                .map_err(unavailable)?;
        }
    }
    Ok(())
}

/// Ends a `run_profile` job as failed with a build failure and keeps its
/// diagnostics when it is the newest failed job (decision 0058 rules 13 and
/// 18). A failure out of those rules is refused before anything is read. The
/// outer error is a storage failure or that refusal, which roll back; the
/// inner one is a refusal of [`attempts::ending`], which the caller commits so
/// that a revocation stored by rule 7 persists.
fn fail_build(
    transaction: &Transaction<'_>,
    lease: &Lease,
    attempt: &AttemptIdentity,
    failure: JobFailure,
    diagnostics: &[BuildDiagnostic],
    now: UnixMillis,
) -> Result<Result<JobRecord, JobStoreError>, JobStoreError> {
    check_build_failure(transaction, lease, failure, diagnostics)?;
    if let Err(refusal) = attempts::ending(transaction, lease, attempt, None, now)? {
        return Ok(Err(refusal));
    }
    let record = fail(transaction, lease, failure, Retry::Never, now)?;
    if record.state == JobState::Failed && !newer_failure(transaction, &lease.job)? {
        insert_diagnostics(transaction, &lease.job, diagnostics)?;
    }
    Ok(Ok(record))
}

/// Refuses a build failure that [`BuildStore::fail_build`] does not take,
/// before anything is written (decision 0058 rules 13 and 18).
fn check_build_failure(
    transaction: &Transaction<'_>,
    lease: &Lease,
    failure: JobFailure,
    diagnostics: &[BuildDiagnostic],
) -> Result<(), JobStoreError> {
    if !failure.is_build_failure() {
        return Err(JobStoreError::InvalidRequest(
            "a build ends with build_failed, limit_exceeded or output_rejected",
        ));
    }
    let expected = if failure == JobFailure::BuildFailed {
        // A non-zero exit whose messages split into none is still
        // `build_failed` (rule 18); rule 13 bounds the count from above only.
        0..=MAX_DIAGNOSTICS
    } else {
        0..=0
    };
    if !expected.contains(&diagnostics.len()) {
        return Err(JobStoreError::InvalidRequest(
            "build_failed keeps at most 20 diagnostics and other failures keep none",
        ));
    }
    held(transaction, lease)?;
    if !profile_job(transaction, &lease.job)? {
        return Err(JobStoreError::InvalidRequest(
            "only a run_profile job ends with a build failure",
        ));
    }
    Ok(())
}

fn insert_diagnostics(
    transaction: &Transaction<'_>,
    job: &JobId,
    diagnostics: &[BuildDiagnostic],
) -> Result<(), StoreError> {
    let mut insert = transaction
        .prepare_cached(
            "INSERT INTO job_diagnostics (job_id, ordinal, path, line, column_number, message)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .map_err(unavailable)?;
    for (ordinal, diagnostic) in diagnostics.iter().enumerate() {
        let location = diagnostic.location.as_ref();
        insert
            .execute(params![
                job.as_str(),
                i64::try_from(ordinal).map_err(corrupt)?,
                location.map(|at| at.path.as_str()),
                location.map(|at| at.line),
                location.map(|at| at.column),
                diagnostic.message.as_str(),
            ])
            .map_err(unavailable)?;
    }
    Ok(())
}

/// Deletes the diagnostics of the `run_profile` jobs of `job`'s artifact and
/// profile that were enqueued before it, as `job` fails: only the newest
/// failed job keeps any (decision 0058 rule 13). "Newest" is enqueue order,
/// the order of rule 16's head status, so a job enqueued earlier that fails
/// later, after a retry, does not delete a newer job's diagnostics. For a
/// `verify_revision` job it deletes nothing.
pub(super) fn keep_newest_diagnostics(
    transaction: &Transaction<'_>,
    job: &JobId,
) -> Result<(), StoreError> {
    transaction
        .execute(
            "DELETE FROM job_diagnostics WHERE job_id IN
                 (SELECT o.id FROM jobs o JOIN jobs j ON j.id = ?1
                  WHERE o.artifact_id = j.artifact_id AND o.kind = 'run_profile'
                    AND o.profile = j.profile AND (o.created_at, o.id) < (j.created_at, j.id))",
            [job.as_str()],
        )
        .map_err(unavailable)?;
    Ok(())
}

/// Whether a `run_profile` job of `job`'s artifact and profile that was
/// enqueued after it has failed already, so that `job` is not the newest
/// failed job and keeps no diagnostics (decision 0058 rule 13).
fn newer_failure(connection: &Connection, job: &JobId) -> Result<bool, StoreError> {
    connection
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM jobs o JOIN jobs j ON j.id = ?1
                 WHERE o.artifact_id = j.artifact_id AND o.kind = 'run_profile'
                   AND o.profile = j.profile AND o.state = 'failed'
                   AND (o.created_at, o.id) > (j.created_at, j.id))",
            [job.as_str()],
            |row| row.get(0),
        )
        .map_err(unavailable)
}

fn read_diagnostics(
    connection: &Connection,
    job: &JobId,
) -> Result<Vec<BuildDiagnostic>, StoreError> {
    let mut statement = connection
        .prepare_cached(
            "SELECT path, line, column_number, message FROM job_diagnostics
             WHERE job_id = ?1 ORDER BY ordinal",
        )
        .map_err(unavailable)?;
    let rows = statement
        .query_map([job.as_str()], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(unavailable)?;
    let mut diagnostics = Vec::new();
    for row in rows {
        let (path, line, column, message) = row.map_err(unavailable)?;
        let location = match (path, line, column) {
            (Some(path), Some(line), Some(column)) => Some(SourceLocation {
                path: stored_path(&path)?,
                line: u32::try_from(line).map_err(corrupt)?,
                column: u32::try_from(column).map_err(corrupt)?,
            }),
            (None, None, None) => None,
            _ => return Err(corrupt("a diagnostic has part of a location")),
        };
        diagnostics.push(BuildDiagnostic {
            location,
            message: message.parse().map_err(corrupt)?,
        });
    }
    Ok(diagnostics)
}

fn read_files(
    connection: &Connection,
    artifact: &ArtifactId,
    revision: SourceRevision,
    profile: &ProfileName,
) -> Result<Option<FilePackage>, StoreError> {
    let mut statement = connection
        .prepare(
            "SELECT f.path, b.content FROM build_files f
             JOIN blobs b ON b.digest = f.digest
             WHERE f.artifact_id = ?1 AND f.revision = ?2 AND f.profile = ?3
             ORDER BY f.path",
        )
        .map_err(unavailable)?;
    let rows = statement
        .query_map(
            params![artifact.as_str(), sql_revision(revision)?, profile.as_str()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .map_err(unavailable)?;
    let mut files = Vec::new();
    for row in rows {
        let (path, content) = row.map_err(unavailable)?;
        files.push((stored_path(&path)?, content));
    }
    if files.is_empty() {
        return Ok(None);
    }
    FilePackage::new(files)
        .map(Some)
        .map_err(|error| StoreError::Unavailable(format!("stored build is invalid: {error}")))
}

impl BuildStore for SqliteStore {
    fn publish_build(
        &self,
        publication: BuildPublication<'_>,
    ) -> Result<(JobRecord, Publication), JobStoreError> {
        // As for `complete_profile_job`: a refusal commits what it wrote (a
        // stored revocation); only a failure of storage rolls back.
        let hashed = Hashed::of(publication.files)?;
        self.write(|transaction| publish(transaction, publication, &hashed))
            .and_then(|answer| answer)
    }

    fn fail_build(
        &self,
        lease: &Lease,
        attempt: &AttemptIdentity,
        failure: JobFailure,
        diagnostics: &[BuildDiagnostic],
        now: UnixMillis,
    ) -> Result<JobRecord, JobStoreError> {
        self.write(|transaction| fail_build(transaction, lease, attempt, failure, diagnostics, now))
            .and_then(|answer| answer)
    }

    fn build(
        &self,
        artifact: &ArtifactId,
        revision: SourceRevision,
        profile: &ProfileName,
    ) -> Result<Option<BuildRecord>, JobStoreError> {
        let mut connection = self.connect()?;
        let snapshot = connection.transaction().map_err(unavailable)?;
        if !artifact_in_place(&snapshot, artifact)? {
            return Ok(None);
        }
        Ok(stored_build(&snapshot, artifact, revision, profile)?)
    }

    fn latest_build(
        &self,
        artifact: &ArtifactId,
        profile: &ProfileName,
    ) -> Result<Option<BuildRecord>, JobStoreError> {
        let mut connection = self.connect()?;
        let snapshot = connection.transaction().map_err(unavailable)?;
        if !artifact_in_place(&snapshot, artifact)? {
            return Ok(None);
        }
        let raw = snapshot
            .query_row(
                &format!(
                    "SELECT {BUILD_COLUMNS} FROM builds WHERE artifact_id = ?1 AND profile = ?2
                     ORDER BY revision DESC LIMIT 1"
                ),
                params![artifact.as_str(), profile.as_str()],
                build_from_row,
            )
            .optional()
            .map_err(unavailable)?;
        Ok(raw.map(RawBuild::into_record).transpose()?)
    }

    fn build_files(
        &self,
        artifact: &ArtifactId,
        revision: SourceRevision,
        profile: &ProfileName,
    ) -> Result<Option<FilePackage>, JobStoreError> {
        let mut connection = self.connect()?;
        let snapshot = connection.transaction().map_err(unavailable)?;
        if !artifact_in_place(&snapshot, artifact)? {
            return Ok(None);
        }
        Ok(read_files(&snapshot, artifact, revision, profile)?)
    }

    fn diagnostics(&self, job: &JobId) -> Result<Vec<BuildDiagnostic>, JobStoreError> {
        let mut connection = self.connect()?;
        let snapshot = connection.transaction().map_err(unavailable)?;
        let artifact: Option<String> = snapshot
            .query_row(
                "SELECT artifact_id FROM jobs WHERE id = ?1",
                [job.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(unavailable)?;
        let Some(artifact) = artifact else {
            return Ok(Vec::new());
        };
        let artifact: ArtifactId = artifact.parse().map_err(corrupt)?;
        if !artifact_in_place(&snapshot, &artifact)? {
            return Ok(Vec::new());
        }
        Ok(read_diagnostics(&snapshot, job)?)
    }
}
