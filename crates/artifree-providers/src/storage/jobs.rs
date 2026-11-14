//! Persistent jobs in the artifact database (decision 0030).
//!
//! Each transition is one `BEGIN IMMEDIATE` transaction that reads the job's
//! state and writes the next one, so two connections, in one process or two,
//! cannot both claim a job or both write its result. A worker names the
//! fencing token of its claim in every call (`jobs.fence`, raised by each
//! claim and never lowered); a call whose token is not the job's current claim
//! is rejected in the same transaction as the write it would have made.
//!
//! A job of an artifact in the trash is opaque, like the artifact: it is not
//! counted toward the actor's limit and a claim does not take it, so it waits
//! untouched until the artifact is recovered (decision 0030, 4).
//!
//! Job times are given by the caller, which reads an injectable clock. These
//! tables hold no content digest: a job reaches file content only through the
//! `revision_files` rows of its pinned revision.

use artifree_domain::SourceRevision;
use artifree_domain::change::FeedId;
use artifree_domain::identity::{ArtifactId, UnixMillis};
use artifree_domain::job::{
    AttemptIdentity, CheckpointAnswer, Finding, JobFailure, JobId, JobKind, JobState,
    MAX_ACTIVE_JOBS_PER_ACTOR, MAX_ATTEMPTS, Problem, ProfileName, VerifyReport, WorkerId,
};
use artifree_ports::artifact_store::{Delivery, RequestIdentity, StoreError};
use artifree_ports::job_store::{
    AttemptRow, Checkpoint, Claim, ClaimedJob, JobCancel, JobClaim, JobListing, JobReceipt,
    JobRecord, JobStore, JobStoreError, Lease, NewJob, NewProfileJob, ProfileBinding,
    ProfileCounts, Retry, SettleOutcome,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::{
    SqliteStore, corrupt, key_used_elsewhere, random_bytes, record_job_change, unavailable,
};

mod attempts;

/// The columns of a job row, in the order [`RawJob::from_row`] reads them.
const COLUMNS: &str = "id, actor, artifact_id, revision, kind, state, cancel_requested, attempts,
    max_attempts, run_after, lease_expires_at, worker, failure, files_checked, bytes_checked,
    mismatch_count, created_at, started_at, finished_at, profile";

/// A job row as stored, before its text columns are parsed.
struct RawJob {
    id: String,
    actor: String,
    artifact: String,
    revision: i64,
    kind: String,
    state: String,
    cancel_requested: bool,
    attempts: i64,
    max_attempts: i64,
    run_after: i64,
    lease_expires_at: Option<i64>,
    worker: Option<String>,
    failure: Option<String>,
    files_checked: Option<i64>,
    bytes_checked: Option<i64>,
    mismatch_count: Option<i64>,
    created_at: i64,
    started_at: Option<i64>,
    finished_at: Option<i64>,
    profile: Option<String>,
}

impl RawJob {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            actor: row.get(1)?,
            artifact: row.get(2)?,
            revision: row.get(3)?,
            kind: row.get(4)?,
            state: row.get(5)?,
            cancel_requested: row.get(6)?,
            attempts: row.get(7)?,
            max_attempts: row.get(8)?,
            run_after: row.get(9)?,
            lease_expires_at: row.get(10)?,
            worker: row.get(11)?,
            failure: row.get(12)?,
            files_checked: row.get(13)?,
            bytes_checked: row.get(14)?,
            mismatch_count: row.get(15)?,
            created_at: row.get(16)?,
            started_at: row.get(17)?,
            finished_at: row.get(18)?,
            profile: row.get(19)?,
        })
    }

    /// The record, with the findings the caller read for it.
    fn into_record(self, findings: Vec<Finding>) -> Result<JobRecord, StoreError> {
        let count = |value: i64| u64::try_from(value).map_err(corrupt);
        let report = match (self.files_checked, self.bytes_checked, self.mismatch_count) {
            (Some(files), Some(bytes), Some(mismatches)) => Some(VerifyReport {
                files_checked: count(files)?,
                bytes_checked: count(bytes)?,
                mismatch_count: count(mismatches)?,
                findings,
            }),
            _ => None,
        };
        // A run_profile result stores no mismatch count (decision 0057 rule 12).
        let counts = match (self.files_checked, self.bytes_checked, self.mismatch_count) {
            (Some(files), Some(bytes), None) => Some(ProfileCounts {
                files: count(files)?,
                bytes: count(bytes)?,
            }),
            _ => None,
        };
        Ok(JobRecord {
            id: self.id.parse().map_err(corrupt)?,
            actor: self.actor.parse().map_err(corrupt)?,
            artifact: self.artifact.parse().map_err(corrupt)?,
            revision: SourceRevision(count(self.revision)?),
            kind: self.kind.parse().map_err(corrupt)?,
            state: self.state.parse().map_err(corrupt)?,
            cancel_requested: self.cancel_requested,
            attempts: u32::try_from(self.attempts).map_err(corrupt)?,
            max_attempts: u32::try_from(self.max_attempts).map_err(corrupt)?,
            run_after: UnixMillis(self.run_after),
            lease_expires_at: self.lease_expires_at.map(UnixMillis),
            worker: self
                .worker
                .map(|worker| worker.parse::<WorkerId>().map_err(corrupt))
                .transpose()?,
            failure: self
                .failure
                .map(|failure| failure.parse::<JobFailure>().map_err(corrupt))
                .transpose()?,
            report,
            counts,
            created_at: UnixMillis(self.created_at),
            started_at: self.started_at.map(UnixMillis),
            finished_at: self.finished_at.map(UnixMillis),
            profile: self
                .profile
                .map(|profile| profile.parse::<ProfileName>().map_err(corrupt))
                .transpose()?,
        })
    }
}

/// One job with its findings, or `None`.
fn read_job(connection: &Connection, id: &JobId) -> Result<Option<JobRecord>, StoreError> {
    let raw = connection
        .query_row(
            &format!("SELECT {COLUMNS} FROM jobs WHERE id = ?1"),
            [id.as_str()],
            RawJob::from_row,
        )
        .optional()
        .map_err(unavailable)?;
    raw.map(|raw| {
        let findings = read_findings(connection, id)?;
        raw.into_record(findings)
    })
    .transpose()
}

fn read_findings(connection: &Connection, id: &JobId) -> Result<Vec<Finding>, StoreError> {
    let mut statement = connection
        .prepare_cached("SELECT path, problem FROM job_findings WHERE job_id = ?1 ORDER BY ordinal")
        .map_err(unavailable)?;
    let rows = statement
        .query_map([id.as_str()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(unavailable)?;
    let mut findings = Vec::new();
    for row in rows {
        let (path, problem) = row.map_err(unavailable)?;
        findings.push(Finding {
            path: path.parse().map_err(corrupt)?,
            problem: problem.parse::<Problem>().map_err(corrupt)?,
        });
    }
    Ok(findings)
}

/// The job, which a transition has just written.
fn written(connection: &Connection, id: &JobId) -> Result<JobRecord, StoreError> {
    read_job(connection, id)?.ok_or_else(|| corrupt("a job vanished inside its transaction"))
}

/// Whether the artifact exists outside the trash and is not purged: the
/// state in which a job can be enqueued, cancelled, and read.
pub(super) fn artifact_in_place(
    connection: &Connection,
    id: &ArtifactId,
) -> Result<bool, StoreError> {
    connection
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM artifacts
                            WHERE id = ?1 AND purged_at IS NULL AND deleted_at IS NULL)",
            [id.as_str()],
            |row| row.get(0),
        )
        .map_err(unavailable)
}

/// What the actor's key says: the job of an earlier request with this
/// fingerprint, `Reused` when another request used the key, or nothing.
enum Key {
    Unused,
    Same(JobId),
    Reused,
}

fn key_of(connection: &Connection, request: RequestIdentity<'_>) -> Result<Key, StoreError> {
    let stored: Option<(Vec<u8>, String)> = connection
        .query_row(
            "SELECT fingerprint, job_id FROM job_requests WHERE actor = ?1 AND request_key = ?2",
            params![request.actor.as_str(), request.key.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(unavailable)?;
    if let Some((fingerprint, job)) = stored {
        return if fingerprint.as_slice() == request.fingerprint.0.as_slice() {
            Ok(Key::Same(job.parse().map_err(corrupt)?))
        } else {
            Ok(Key::Reused)
        };
    }
    // One key space per actor with every other kind of request (decisions
    // 0029, 0030, and 0031).
    let elsewhere = key_used_elsewhere(connection, request.actor, request.key, "job_requests")?;
    Ok(if elsewhere { Key::Reused } else { Key::Unused })
}

fn record_key(
    transaction: &Transaction<'_>,
    request: RequestIdentity<'_>,
    job: &JobId,
    now: UnixMillis,
) -> Result<(), StoreError> {
    transaction
        .execute(
            "INSERT INTO job_requests (actor, request_key, fingerprint, job_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                request.actor.as_str(),
                request.key.as_str(),
                request.fingerprint.0.as_slice(),
                job.as_str(),
                now.0
            ],
        )
        .map_err(unavailable)?;
    Ok(())
}

/// Enqueues a job. `profile` is the profile of a `run_profile` job and `None`
/// for a `verify_revision` job; the table checks that the two agree.
fn enqueue(
    transaction: &Transaction<'_>,
    request: NewJob<'_>,
    profile: Option<&ProfileName>,
) -> Result<JobReceipt, JobStoreError> {
    // The target first, as every write does (decision 0013): a job of an
    // artifact in the trash or purged is not found, a retry included.
    if !artifact_in_place(transaction, request.artifact)? {
        return Err(JobStoreError::NotFound);
    }
    let revision = i64::try_from(request.revision.0).map_err(corrupt)?;
    let pinned: bool = transaction
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM revisions WHERE artifact_id = ?1 AND revision = ?2)",
            params![request.artifact.as_str(), revision],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if !pinned {
        return Err(JobStoreError::NotFound);
    }
    match key_of(transaction, request.request)? {
        Key::Same(id) => {
            return Ok(JobReceipt {
                job: written(transaction, &id)?,
                delivery: Delivery::Replayed,
            });
        }
        Key::Reused => return Err(JobStoreError::RequestKeyReused),
        Key::Unused => {}
    }
    // Decision 0057 rule 3: run_profile rows do not count toward the actor's 16 open
    // jobs, and R03 sets the kind's own bound.
    if request.kind == JobKind::VerifyRevision {
        let active: i64 = transaction
            .query_row(
                "SELECT count(*) FROM jobs j JOIN artifacts a ON a.id = j.artifact_id
                 WHERE j.actor = ?1 AND j.state IN ('queued', 'running', 'retry_scheduled')
                   AND j.kind = 'verify_revision'
                   AND a.purged_at IS NULL AND a.deleted_at IS NULL",
                [request.request.actor.as_str()],
                |row| row.get(0),
            )
            .map_err(unavailable)?;
        if u64::try_from(active).map_err(corrupt)? >= MAX_ACTIVE_JOBS_PER_ACTOR {
            return Err(JobStoreError::LimitReached);
        }
    }
    let id = JobId::from_random(random_bytes()?);
    transaction
        .execute(
            "INSERT INTO jobs (id, actor, artifact_id, revision, kind, state, attempts,
                 max_attempts, fence, run_after, cancel_requested, created_at, profile)
             VALUES (?1, ?2, ?3, ?4, ?5, 'queued', 0, ?6, 0, ?7, 0, ?7, ?8)",
            params![
                id.as_str(),
                request.request.actor.as_str(),
                request.artifact.as_str(),
                revision,
                request.kind.as_str(),
                MAX_ATTEMPTS,
                request.now.0,
                profile.map(ProfileName::as_str)
            ],
        )
        .map_err(unavailable)?;
    record_key(transaction, request.request, &id, request.now)?;
    record_job_change(transaction, &id)?;
    Ok(JobReceipt {
        job: written(transaction, &id)?,
        delivery: Delivery::Committed,
    })
}

fn cancel(
    transaction: &Transaction<'_>,
    request: JobCancel<'_>,
) -> Result<JobReceipt, JobStoreError> {
    let Some(job) = read_job(transaction, request.job)? else {
        return Err(JobStoreError::NotFound);
    };
    // Decision 0057 rule 3: nobody can see or cancel a run_profile job, so the
    // request gets the same NotFound and records no key.
    if job.actor != *request.request.actor
        || job.kind != JobKind::VerifyRevision
        || !artifact_in_place(transaction, &job.artifact)?
    {
        return Err(JobStoreError::NotFound);
    }
    match key_of(transaction, request.request)? {
        Key::Same(id) => {
            return Ok(JobReceipt {
                job: written(transaction, &id)?,
                delivery: Delivery::Replayed,
            });
        }
        Key::Reused => return Err(JobStoreError::RequestKeyReused),
        Key::Unused => {}
    }
    let changed = match job.state {
        JobState::Queued | JobState::RetryScheduled => {
            transaction
                .execute(
                    "UPDATE jobs SET state = 'cancelled', finished_at = ?2 WHERE id = ?1",
                    params![request.job.as_str(), request.now.0],
                )
                .map_err(unavailable)?;
            true
        }
        JobState::Running => {
            transaction
                .execute(
                    "UPDATE jobs SET cancel_requested = 1 WHERE id = ?1",
                    [request.job.as_str()],
                )
                .map_err(unavailable)?;
            // The flag changes only on the first cancel request (decision 0055, rule 7).
            !job.cancel_requested
        }
        state @ (JobState::Succeeded | JobState::Failed | JobState::Cancelled) => {
            return Err(JobStoreError::Conflict(state));
        }
    };
    record_key(transaction, request.request, request.job, request.now)?;
    if changed {
        record_job_change(transaction, request.job)?;
    }
    Ok(JobReceipt {
        job: written(transaction, request.job)?,
        delivery: Delivery::Committed,
    })
}

/// A job due for a worker: its identifier and what decides how to take it.
struct Due {
    id: JobId,
    running: bool,
    cancel_requested: bool,
    attempts: i64,
    max_attempts: i64,
    /// Its creation time, which orders the claim's cursor with `id`.
    created_at: i64,
    /// Its profile name, set for a `run_profile` job.
    profile: Option<String>,
}

/// Which jobs a worker may take, for `?1` = now: a queued or retry-scheduled
/// job whose time came, or a running job whose lease expired, of an artifact
/// outside the trash and not purged. A job of an artifact in the trash is
/// opaque like the artifact (decision 0030): it is left as it is and becomes
/// due again when the artifact is recovered. The same holds for a job whose
/// owning user is disabled (decision 0036): it is not taken, no attempt is
/// used, and it is due again when the user is enabled. The user is read in the
/// same statement as the job, and the claim runs in a write transaction, so a
/// disable that commits before the claim is seen by it and one that commits
/// after it comes too late to matter (a job already running is not stopped).
/// An actor with no row in `users` is not disabled. A `run_profile` job with an
/// attempt that is not `settled` is not due: an expired lease does not make it
/// claimable (decision 0057, rule 10).
const DUE: &str = "FROM jobs j JOIN artifacts a ON a.id = j.artifact_id
     WHERE a.purged_at IS NULL AND a.deleted_at IS NULL
       AND NOT EXISTS (SELECT 1 FROM users u WHERE u.id = j.actor AND u.disabled_at IS NOT NULL)
       AND NOT EXISTS (SELECT 1 FROM execution_attempts e WHERE e.job_id = j.id AND e.state <> 'settled')
       AND ((j.state IN ('queued', 'retry_scheduled') AND j.run_after <= ?1)
         OR (j.state = 'running' AND j.lease_expires_at <= ?1))";

/// The columns [`next_due`] reads for one due job: identifier, state, cancel
/// request, attempts, maximum attempts, creation time and profile.
type DueRow = (String, String, bool, i64, i64, i64, Option<String>);

/// The first due job after `after`, the position (creation time and identifier)
/// of a job the claim already passed over, or the first due job when `after` is
/// `None`.
fn next_due(
    transaction: &Transaction<'_>,
    now: UnixMillis,
    after: Option<&(i64, JobId)>,
) -> Result<Option<Due>, StoreError> {
    let row: Option<DueRow> = transaction
        .query_row(
            &format!(
                "SELECT j.id, j.state, j.cancel_requested, j.attempts, j.max_attempts,
                     j.created_at, j.profile {DUE}
                   AND (?2 IS NULL OR j.created_at > ?2 OR (j.created_at = ?2 AND j.id > ?3))
                 ORDER BY j.created_at, j.id LIMIT 1"
            ),
            params![
                now.0,
                after.map(|(created_at, _)| *created_at),
                after.map(|(_, id)| id.as_str())
            ],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .optional()
        .map_err(unavailable)?;
    row.map(
        |(id, state, cancel_requested, attempts, max_attempts, created_at, profile)| {
            Ok(Due {
                id: id.parse().map_err(corrupt)?,
                running: state == JobState::Running.as_str(),
                cancel_requested,
                attempts,
                max_attempts,
                created_at,
                profile,
            })
        },
    )
    .transpose()
}

/// Ends a job in `state` (cancelled or failed), clearing its lease.
fn end(
    transaction: &Transaction<'_>,
    id: &JobId,
    state: JobState,
    failure: Option<JobFailure>,
    now: UnixMillis,
) -> Result<(), StoreError> {
    transaction
        .execute(
            "UPDATE jobs SET state = ?2, failure = ?3, worker = NULL, lease_expires_at = NULL,
                 finished_at = ?4
             WHERE id = ?1",
            params![
                id.as_str(),
                state.as_str(),
                failure.map(JobFailure::as_str),
                now.0
            ],
        )
        .map_err(unavailable)?;
    Ok(())
}

/// Whether any job is due. A read only: it is a hint that spares an idle
/// worker the write lock, and the claim decides again inside its transaction.
fn any_due(connection: &Connection, now: UnixMillis) -> Result<bool, StoreError> {
    connection
        .query_row(&format!("SELECT EXISTS (SELECT 1 {DUE})"), [now.0], |row| {
            row.get(0)
        })
        .map_err(unavailable)
}

fn claim(transaction: &Transaction<'_>, claim: JobClaim<'_>) -> Result<Claim, JobStoreError> {
    if let Some(execution) = &claim.execution {
        attempts::check_execution(execution)?;
    }
    let mut settled = Vec::new();
    let lease_ms = i64::try_from(claim.lease_ms).map_err(corrupt)?;
    let expires = claim
        .now
        .0
        .checked_add(lease_ms)
        .ok_or_else(|| corrupt("lease end"))?;
    // The position of the last job the claim left queued; the next due job is
    // read after it, so a job passed over is not read again.
    let mut after: Option<(i64, JobId)> = None;
    // Each pass returns, ends a job, or moves the cursor past a job it leaves queued.
    while let Some(due) = next_due(transaction, claim.now, after.as_ref())? {
        if due.running && due.cancel_requested {
            // The worker that was asked to stop is gone; nothing is left to run.
            end(transaction, &due.id, JobState::Cancelled, None, claim.now)?;
            record_job_change(transaction, &due.id)?;
            settled.push(written(transaction, &due.id)?);
            continue;
        }
        if due.running && due.attempts >= due.max_attempts {
            end(
                transaction,
                &due.id,
                JobState::Failed,
                Some(JobFailure::LeaseExpired),
                claim.now,
            )?;
            record_job_change(transaction, &due.id)?;
            settled.push(written(transaction, &due.id)?);
            continue;
        }
        let profile = due
            .profile
            .as_deref()
            .map(str::parse::<ProfileName>)
            .transpose()
            .map_err(corrupt)?;
        let run = match profile {
            None => None,
            // Decision 0057, rule 1: a profile this build does not know ends the
            // job at once, as a failure that is never retried, without an attempt.
            Some(profile) if !claim.known_profiles.contains(&profile) => {
                end(
                    transaction,
                    &due.id,
                    JobState::Failed,
                    Some(JobFailure::ExecutionUnavailable),
                    claim.now,
                )?;
                record_job_change(transaction, &due.id)?;
                settled.push(written(transaction, &due.id)?);
                continue;
            }
            Some(profile) => {
                // A job whose earlier attempt pinned another generation is not run again under
                // the new one: its attempts are settled (the DUE gate), so it ends cancelled
                // and needs a new keyed request (decision 0042 rule 6; decision 0057 rule 9).
                if due.attempts > 0 && attempts::generation_changed(transaction, &due.id)? {
                    end(transaction, &due.id, JobState::Cancelled, None, claim.now)?;
                    record_job_change(transaction, &due.id)?;
                    settled.push(written(transaction, &due.id)?);
                    continue;
                }
                // A known profile with no execution directory in this worker:
                // the job stays queued and the claim goes on.
                let Some(execution) = claim.execution.as_ref() else {
                    after = Some((due.created_at, due.id));
                    continue;
                };
                Some((profile, execution))
            }
        };
        transaction
            .execute(
                "UPDATE jobs SET state = 'running', attempts = attempts + 1, fence = fence + 1,
                     worker = ?2, lease_expires_at = ?3, started_at = coalesce(started_at, ?4)
                 WHERE id = ?1",
                params![due.id.as_str(), claim.worker.as_str(), expires, claim.now.0],
            )
            .map_err(unavailable)?;
        record_job_change(transaction, &due.id)?;
        let fence: i64 = transaction
            .query_row(
                "SELECT fence FROM jobs WHERE id = ?1",
                [due.id.as_str()],
                |row| row.get(0),
            )
            .map_err(unavailable)?;
        let fence = u64::try_from(fence).map_err(corrupt)?;
        if let Some((profile, execution)) = &run {
            attempts::insert(transaction, &due.id, fence, profile, execution, claim.now)?;
        }
        let job = written(transaction, &due.id)?;
        return Ok(Claim {
            settled,
            claimed: Some(ClaimedJob {
                job,
                lease: Lease { job: due.id, fence },
            }),
        });
    }
    Ok(Claim {
        settled,
        claimed: None,
    })
}

/// Checks that `lease` is the job's current claim and returns whether a
/// cancel was requested. Any other state is a lost lease: the job was claimed
/// again, revoked by a purge, or ended.
fn held(transaction: &Transaction<'_>, lease: &Lease) -> Result<bool, JobStoreError> {
    let row: Option<(String, i64, bool)> = transaction
        .query_row(
            "SELECT state, fence, cancel_requested FROM jobs WHERE id = ?1",
            [lease.job.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(unavailable)?;
    match row {
        Some((state, fence, cancel))
            if state == JobState::Running.as_str()
                && u64::try_from(fence).map_err(corrupt)? == lease.fence =>
        {
            Ok(cancel)
        }
        _ => Err(JobStoreError::LeaseLost),
    }
}

/// Whether the job is a `run_profile` job.
fn profile_job(connection: &Connection, id: &JobId) -> Result<bool, StoreError> {
    connection
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM jobs WHERE id = ?1 AND kind = 'run_profile')",
            [id.as_str()],
            |row| row.get(0),
        )
        .map_err(unavailable)
}

/// The terminal gate of decision 0057, rule 10: no transition of a `run_profile`
/// job to `succeeded`, `failed`, `cancelled`, or `retry_scheduled` happens while
/// its attempt is unsettled. Call it after [`held`] has checked the lease.
fn gate(transaction: &Transaction<'_>, lease: &Lease) -> Result<(), JobStoreError> {
    if profile_job(transaction, &lease.job)? && attempts::unsettled(transaction, &lease.job)? {
        Err(JobStoreError::AttemptUnsettled)
    } else {
        Ok(())
    }
}

fn heartbeat(
    transaction: &Transaction<'_>,
    lease: &Lease,
    now: UnixMillis,
    lease_ms: u64,
) -> Result<Checkpoint, JobStoreError> {
    let cancel = held(transaction, lease)?;
    if profile_job(transaction, &lease.job)? {
        return Err(JobStoreError::InvalidRequest(
            "a run_profile job checkpoints with checkpoint_attempt",
        ));
    }
    let expires = now
        .0
        .checked_add(i64::try_from(lease_ms).map_err(corrupt)?)
        .ok_or_else(|| corrupt("lease end"))?;
    transaction
        .execute(
            "UPDATE jobs SET lease_expires_at = ?2 WHERE id = ?1",
            params![lease.job.as_str(), expires],
        )
        .map_err(unavailable)?;
    Ok(if cancel {
        Checkpoint::CancelRequested
    } else {
        Checkpoint::Continue
    })
}

fn complete(
    transaction: &Transaction<'_>,
    lease: &Lease,
    report: &VerifyReport,
    now: UnixMillis,
) -> Result<JobRecord, JobStoreError> {
    let cancel = held(transaction, lease)?;
    if profile_job(transaction, &lease.job)? {
        return Err(JobStoreError::InvalidRequest(
            "a run_profile job completes with complete_profile_job",
        ));
    }
    if cancel {
        // A cancel came in before the result was written: the job is
        // cancelled and the result is not kept.
        end(transaction, &lease.job, JobState::Cancelled, None, now)?;
        record_job_change(transaction, &lease.job)?;
        return Ok(written(transaction, &lease.job)?);
    }
    let count = |value: u64| i64::try_from(value).map_err(corrupt);
    transaction
        .execute(
            "UPDATE jobs SET state = 'succeeded', worker = NULL, lease_expires_at = NULL,
                 files_checked = ?2, bytes_checked = ?3, mismatch_count = ?4, finished_at = ?5
             WHERE id = ?1",
            params![
                lease.job.as_str(),
                count(report.files_checked)?,
                count(report.bytes_checked)?,
                count(report.mismatch_count)?,
                now.0
            ],
        )
        .map_err(unavailable)?;
    for (ordinal, finding) in report.findings.iter().enumerate() {
        transaction
            .execute(
                "INSERT INTO job_findings (job_id, ordinal, path, problem) VALUES (?1, ?2, ?3, ?4)",
                params![
                    lease.job.as_str(),
                    count(ordinal as u64)?,
                    finding.path.as_str(),
                    finding.problem.as_str()
                ],
            )
            .map_err(unavailable)?;
    }
    record_job_change(transaction, &lease.job)?;
    Ok(written(transaction, &lease.job)?)
}

fn fail(
    transaction: &Transaction<'_>,
    lease: &Lease,
    failure: JobFailure,
    retry: Retry,
    now: UnixMillis,
) -> Result<JobRecord, JobStoreError> {
    let cancel = held(transaction, lease)?;
    gate(transaction, lease)?;
    if cancel {
        end(transaction, &lease.job, JobState::Cancelled, None, now)?;
        record_job_change(transaction, &lease.job)?;
        return Ok(written(transaction, &lease.job)?);
    }
    let (attempts, max_attempts): (i64, i64) = transaction
        .query_row(
            "SELECT attempts, max_attempts FROM jobs WHERE id = ?1",
            [lease.job.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(unavailable)?;
    match retry {
        Retry::At(at) if attempts < max_attempts => {
            transaction
                .execute(
                    "UPDATE jobs SET state = 'retry_scheduled', run_after = ?2, worker = NULL,
                         lease_expires_at = NULL
                     WHERE id = ?1",
                    params![lease.job.as_str(), at.0],
                )
                .map_err(unavailable)?;
        }
        Retry::At(_) | Retry::Never => {
            end(
                transaction,
                &lease.job,
                JobState::Failed,
                Some(failure),
                now,
            )?;
        }
    }
    record_job_change(transaction, &lease.job)?;
    Ok(written(transaction, &lease.job)?)
}

fn confirm_cancel(
    transaction: &Transaction<'_>,
    lease: &Lease,
    now: UnixMillis,
) -> Result<JobRecord, JobStoreError> {
    let cancel = held(transaction, lease)?;
    gate(transaction, lease)?;
    if !cancel {
        return Err(JobStoreError::Conflict(JobState::Running));
    }
    end(transaction, &lease.job, JobState::Cancelled, None, now)?;
    record_job_change(transaction, &lease.job)?;
    Ok(written(transaction, &lease.job)?)
}

fn release(
    transaction: &Transaction<'_>,
    lease: &Lease,
    now: UnixMillis,
) -> Result<JobRecord, JobStoreError> {
    let cancel = held(transaction, lease)?;
    gate(transaction, lease)?;
    if cancel {
        end(transaction, &lease.job, JobState::Cancelled, None, now)?;
    } else {
        // The attempt is given back; the fencing token is not, so the
        // released run still cannot write anything.
        transaction
            .execute(
                "UPDATE jobs SET state = 'queued', attempts = max(attempts - 1, 0), run_after = ?2,
                     worker = NULL, lease_expires_at = NULL
                 WHERE id = ?1",
                params![lease.job.as_str(), now.0],
            )
            .map_err(unavailable)?;
    }
    record_job_change(transaction, &lease.job)?;
    Ok(written(transaction, &lease.job)?)
}

/// Decision 0057 rule 8, in the trash transaction: every unsettled attempt of the
/// artifact's `run_profile` jobs is revoked (`trash`) unless it already was for a
/// reason that cancels, and its running job gets a cancel request and a raised
/// fence. A `lease` or `shutdown` revocation (a restore, or the worker's own
/// stop) is replaced by `trash`: those reasons retry the job at settlement, and a
/// trashed job must end cancelled, so that its recovery needs a fresh request. A
/// running job whose newest attempt is already settled but not published is
/// revoked and ends cancelled at once, since its cleanup is done. Queued and
/// retry-scheduled jobs are not changed: the trash hides them, and recovery
/// makes them due again.
/// It does not wait for the process. `verify_revision` jobs are untouched (0030's
/// continue-and-hide).
pub(super) fn revoke_for_trash(
    transaction: &Transaction<'_>,
    artifact: &ArtifactId,
    now: UnixMillis,
) -> Result<(), StoreError> {
    transaction
        .execute(
            "UPDATE jobs SET cancel_requested = 1, fence = fence + 1
             WHERE artifact_id = ?1 AND kind = 'run_profile' AND state = 'running'
               AND EXISTS (SELECT 1 FROM execution_attempts e
                           WHERE e.job_id = jobs.id AND e.state <> 'settled')",
            [artifact.as_str()],
        )
        .map_err(unavailable)?;
    transaction
        .execute(
            "UPDATE execution_attempts SET revoked = 'trash'
             WHERE state <> 'settled'
               AND (revoked IS NULL OR revoked IN ('lease', 'shutdown'))
               AND job_id IN (SELECT id FROM jobs
                              WHERE artifact_id = ?1 AND kind = 'run_profile')",
            [artifact.as_str()],
        )
        .map_err(unavailable)?;
    // A running job whose newest attempt is settled and not revoked has had its
    // cleanup but not its publication: it is revoked as `trash` and ends
    // cancelled now, so that neither its publication nor a reclaim after the
    // recovery can run it (rule 8).
    transaction
        .execute(
            "UPDATE execution_attempts SET revoked = 'trash'
             WHERE state = 'settled' AND revoked IS NULL
               AND fence = (SELECT max(e.fence) FROM execution_attempts e
                            WHERE e.job_id = execution_attempts.job_id)
               AND job_id IN (SELECT id FROM jobs
                              WHERE artifact_id = ?1 AND kind = 'run_profile'
                                AND state = 'running')",
            [artifact.as_str()],
        )
        .map_err(unavailable)?;
    transaction
        .execute(
            "UPDATE jobs SET state = 'cancelled', fence = fence + 1, worker = NULL,
                 lease_expires_at = NULL, finished_at = ?2
             WHERE artifact_id = ?1 AND kind = 'run_profile' AND state = 'running'
               AND NOT EXISTS (SELECT 1 FROM execution_attempts e
                               WHERE e.job_id = jobs.id AND e.state <> 'settled')
               AND EXISTS (SELECT 1 FROM execution_attempts e
                           WHERE e.job_id = jobs.id AND e.revoked = 'trash'
                             AND e.fence = (SELECT max(f.fence) FROM execution_attempts f
                                            WHERE f.job_id = jobs.id))",
            params![artifact.as_str(), now.0],
        )
        .map_err(unavailable)?;
    Ok(())
}

/// Ends the jobs of an artifact that is being purged and erases what they
/// found: waiting, scheduled, and running jobs become cancelled, and a running
/// job's fencing token is raised so its worker's next call is a lost lease.
/// The result counts and the findings (which name files of the artifact) of
/// every job of the artifact are deleted; the job headers stay, like the
/// revision headers a purge keeps. It appends no change row for the jobs it
/// ends: the `purged` change covers them (decision 0055, rule 9).
///
/// A `run_profile` job with an unsettled attempt is not ended here (decision 0057,
/// rule 11). Its fencing token is raised and each unsettled attempt becomes the
/// `cleanup_pending` tombstone, revoked as `purge` unless it already was for a
/// reason that cancels (a `lease` or `shutdown` revocation would retry the job at
/// settlement, on an artifact no claim can reach), and the job stays `running`
/// until the attempt settles and ends it cancelled. No statement deletes an
/// `execution_attempts` row, so the tombstone outlives the purge.
pub(super) fn revoke_for_purge(
    transaction: &Transaction<'_>,
    artifact: &ArtifactId,
    now: UnixMillis,
) -> Result<(), StoreError> {
    // Decision 0057 rule 11: the fence of a run_profile job with an unsettled
    // attempt is raised, and the attempt is marked as the tombstone.
    transaction
        .execute(
            "UPDATE jobs SET fence = fence + 1
             WHERE artifact_id = ?1 AND kind = 'run_profile' AND state = 'running'
               AND EXISTS (SELECT 1 FROM execution_attempts e
                           WHERE e.job_id = jobs.id AND e.state <> 'settled')",
            [artifact.as_str()],
        )
        .map_err(unavailable)?;
    transaction
        .execute(
            "UPDATE execution_attempts
             SET state = 'cleanup_pending',
                 revoked = CASE WHEN revoked IS NULL OR revoked IN ('lease', 'shutdown')
                                THEN 'purge' ELSE revoked END
             WHERE state <> 'settled'
               AND job_id IN (SELECT id FROM jobs WHERE artifact_id = ?1)",
            [artifact.as_str()],
        )
        .map_err(unavailable)?;
    // A job with an unsettled attempt is left running (decision 0057, rule 11).
    transaction
        .execute(
            "UPDATE jobs SET state = 'cancelled', fence = fence + 1, worker = NULL,
                 lease_expires_at = NULL, finished_at = ?2
             WHERE artifact_id = ?1 AND state IN ('queued', 'running', 'retry_scheduled')
               AND NOT EXISTS (SELECT 1 FROM execution_attempts e
                               WHERE e.job_id = jobs.id AND e.state <> 'settled')",
            params![artifact.as_str(), now.0],
        )
        .map_err(unavailable)?;
    transaction
        .execute(
            "DELETE FROM job_findings
             WHERE job_id IN (SELECT id FROM jobs WHERE artifact_id = ?1)",
            [artifact.as_str()],
        )
        .map_err(unavailable)?;
    transaction
        .execute(
            "UPDATE jobs SET files_checked = NULL, bytes_checked = NULL, mismatch_count = NULL
             WHERE artifact_id = ?1",
            [artifact.as_str()],
        )
        .map_err(unavailable)?;
    Ok(())
}

impl JobStore for SqliteStore {
    fn enqueue_job(&self, request: NewJob<'_>) -> Result<JobReceipt, JobStoreError> {
        if request.kind == JobKind::RunProfile {
            return Err(JobStoreError::InvalidRequest(
                "a run_profile job is enqueued with enqueue_profile_job",
            ));
        }
        self.write(|transaction| enqueue(transaction, request, None))
    }

    fn enqueue_profile_job(&self, request: NewProfileJob<'_>) -> Result<JobReceipt, JobStoreError> {
        let job = NewJob {
            request: request.request,
            artifact: request.artifact,
            revision: request.revision,
            kind: JobKind::RunProfile,
            now: request.now,
        };
        self.write(|transaction| enqueue(transaction, job, Some(request.profile)))
    }

    fn cancel_job(&self, request: JobCancel<'_>) -> Result<JobReceipt, JobStoreError> {
        self.write(|transaction| cancel(transaction, request))
    }

    fn job(&self, id: &JobId) -> Result<Option<JobRecord>, JobStoreError> {
        let mut connection = self.connect()?;
        let snapshot = connection.transaction().map_err(unavailable)?;
        // Decision 0057 rule 3: a run_profile job is not readable (GET /v1/jobs/{id} is 404).
        Ok(read_job(&snapshot, id)?.filter(|job| job.kind == JobKind::VerifyRevision))
    }

    fn list_jobs(&self, listing: JobListing<'_>) -> Result<Vec<JobRecord>, JobStoreError> {
        let mut connection = self.connect()?;
        // One snapshot, so a job and its findings agree.
        let snapshot = connection.transaction().map_err(unavailable)?;
        let ids: Vec<JobId> = {
            let mut statement = snapshot
                .prepare(
                    "SELECT j.id FROM jobs j JOIN artifacts a ON a.id = j.artifact_id
                     WHERE j.actor = ?1
                       AND j.kind = 'verify_revision'
                       AND a.purged_at IS NULL AND a.deleted_at IS NULL
                       AND (?2 IS NULL OR j.artifact_id = ?2)
                       AND (?3 IS NULL OR j.created_at < ?3 OR (j.created_at = ?3 AND j.id > ?4))
                     ORDER BY j.created_at DESC, j.id LIMIT ?5",
                )
                .map_err(unavailable)?;
            let rows = statement
                .query_map(
                    params![
                        listing.actor.as_str(),
                        listing.artifact.map(ArtifactId::as_str),
                        listing.after.map(|position| position.created_at.0),
                        listing.after.map(|position| position.id.as_str()),
                        i64::try_from(listing.limit).unwrap_or(i64::MAX)
                    ],
                    |row| row.get::<_, String>(0),
                )
                .map_err(unavailable)?;
            let mut ids = Vec::new();
            for row in rows {
                ids.push(row.map_err(unavailable)?.parse().map_err(corrupt)?);
            }
            ids
        };
        let mut jobs = Vec::with_capacity(ids.len());
        for id in &ids {
            jobs.push(written(&snapshot, id)?);
        }
        Ok(jobs)
    }

    fn claim_job(&self, request: JobClaim<'_>) -> Result<Claim, JobStoreError> {
        // An idle poll reads and returns; only a due job takes the write lock.
        if !any_due(&self.connect()?, request.now)? {
            return Ok(Claim::default());
        }
        self.write(|transaction| claim(transaction, request))
    }

    fn heartbeat_job(
        &self,
        lease: &Lease,
        now: UnixMillis,
        lease_ms: u64,
    ) -> Result<Checkpoint, JobStoreError> {
        self.write(|transaction| heartbeat(transaction, lease, now, lease_ms))
    }

    fn complete_job(
        &self,
        lease: &Lease,
        report: &VerifyReport,
        now: UnixMillis,
    ) -> Result<JobRecord, JobStoreError> {
        self.write(|transaction| complete(transaction, lease, report, now))
    }

    fn fail_job(
        &self,
        lease: &Lease,
        failure: JobFailure,
        retry: Retry,
        now: UnixMillis,
    ) -> Result<JobRecord, JobStoreError> {
        self.write(|transaction| fail(transaction, lease, failure, retry, now))
    }

    fn confirm_cancel(&self, lease: &Lease, now: UnixMillis) -> Result<JobRecord, JobStoreError> {
        self.write(|transaction| confirm_cancel(transaction, lease, now))
    }

    fn release_job(&self, lease: &Lease, now: UnixMillis) -> Result<JobRecord, JobStoreError> {
        self.write(|transaction| release(transaction, lease, now))
    }

    fn checkpoint_attempt(
        &self,
        lease: &Lease,
        attempt: &AttemptIdentity,
        now: UnixMillis,
        lease_ms: u64,
    ) -> Result<CheckpointAnswer, JobStoreError> {
        self.write(|transaction| attempts::checkpoint(transaction, lease, attempt, now, lease_ms))
    }

    fn settle_attempt(
        &self,
        attempt: &AttemptIdentity,
        now: UnixMillis,
    ) -> Result<SettleOutcome, JobStoreError> {
        self.write(|transaction| attempts::settle(transaction, attempt, now))
    }

    fn mark_cleanup_pending(&self, attempt: &AttemptIdentity) -> Result<(), JobStoreError> {
        self.write(|transaction| attempts::mark_cleanup_pending(transaction, attempt))
    }

    fn complete_profile_job(
        &self,
        lease: &Lease,
        attempt: &AttemptIdentity,
        binding: ProfileBinding<'_>,
        counts: ProfileCounts,
        now: UnixMillis,
    ) -> Result<JobRecord, JobStoreError> {
        // The inner result is a refusal, which commits what it wrote (a stored
        // revocation); only a failure of storage rolls back.
        self.write(|transaction| {
            attempts::complete(transaction, lease, attempt, binding, counts, now)
        })
        .and_then(|answer| answer)
    }

    fn attempt_row(&self, attempt: &AttemptIdentity) -> Result<Option<AttemptRow>, JobStoreError> {
        let mut connection = self.connect()?;
        let snapshot = connection.transaction().map_err(unavailable)?;
        Ok(attempts::read_attempt(&snapshot, attempt)?)
    }

    fn unsettled_attempts(&self) -> Result<Vec<AttemptRow>, JobStoreError> {
        let mut connection = self.connect()?;
        let snapshot = connection.transaction().map_err(unavailable)?;
        Ok(attempts::unsettled_rows(&snapshot)?)
    }

    // Decisions 0041 and 0043: the existing change-feed identity is the database
    // identity, and a supported restore renews it (decision 0027, rule 7).
    fn database_identity(&self) -> Result<FeedId, JobStoreError> {
        let connection = self.connect()?;
        let feed: Vec<u8> = connection
            .query_row("SELECT id FROM feed", [], |row| row.get(0))
            .map_err(unavailable)?;
        let bytes: [u8; 16] = feed.try_into().map_err(|_| corrupt("feed id length"))?;
        Ok(FeedId::from_random(bytes))
    }
}
