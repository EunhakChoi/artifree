//! The execution attempts of `run_profile` jobs (decision 0057 rules 5 to 13).
//! Nothing here appends a change row (rule 3) and nothing deletes an attempt
//! row (rule 6).

use artifree_domain::identity::{StateGeneration, UnixMillis};
use artifree_domain::job::{
    AttemptIdentity, AttemptState, CheckpointAnswer, JobFailure, JobId, JobState, ProfileName,
    Revocation, StopReason,
};
use artifree_ports::artifact_store::StoreError;
use artifree_ports::job_store::{
    AttemptRow, ExecutionClaim, JobRecord, JobStoreError, Lease, ProfileBinding, ProfileCounts,
    SettleOutcome,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::{end, written};
use crate::storage::{corrupt, unavailable};

/// The columns of an attempt row, in the order [`RawAttempt::from_row`] reads
/// them.
const ATTEMPT_COLUMNS: &str = "attempt, job_id, fence, actor_generation, profile, tool_digest,
    machine, boot, cgroupns, directory, state, revoked, created_at, settled_at";

/// An attempt row as stored, before its text columns are parsed.
struct RawAttempt {
    attempt: String,
    job: String,
    fence: i64,
    actor_generation: Option<i64>,
    profile: String,
    tool_digest: String,
    machine: String,
    boot: String,
    cgroupns: String,
    directory: String,
    state: String,
    revoked: Option<String>,
    created_at: i64,
    settled_at: Option<i64>,
}

impl RawAttempt {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            attempt: row.get(0)?,
            job: row.get(1)?,
            fence: row.get(2)?,
            actor_generation: row.get(3)?,
            profile: row.get(4)?,
            tool_digest: row.get(5)?,
            machine: row.get(6)?,
            boot: row.get(7)?,
            cgroupns: row.get(8)?,
            directory: row.get(9)?,
            state: row.get(10)?,
            revoked: row.get(11)?,
            created_at: row.get(12)?,
            settled_at: row.get(13)?,
        })
    }
}

/// The attempt, with its stored text parsed into its domain types.
fn into_row(raw: RawAttempt) -> Result<AttemptRow, StoreError> {
    let attempt: AttemptIdentity = raw.attempt.parse().map_err(corrupt)?;
    let job: JobId = raw.job.parse().map_err(corrupt)?;
    let profile: ProfileName = raw.profile.parse().map_err(corrupt)?;
    let state: AttemptState = raw.state.parse().map_err(corrupt)?;
    let revoked: Option<Revocation> = raw
        .revoked
        .map(|value| value.parse::<Revocation>().map_err(corrupt))
        .transpose()?;
    let actor_generation: Option<StateGeneration> = raw
        .actor_generation
        .map(|value| {
            u64::try_from(value).map_err(corrupt).and_then(|value| {
                StateGeneration::new(value).ok_or_else(|| corrupt("attempt generation"))
            })
        })
        .transpose()?;
    Ok(AttemptRow {
        attempt,
        job,
        fence: u64::try_from(raw.fence).map_err(corrupt)?,
        actor_generation,
        profile,
        tool_digest: raw.tool_digest,
        machine: raw.machine,
        boot: raw.boot,
        cgroupns: raw.cgroupns,
        directory: raw.directory,
        state,
        revoked,
        created_at: UnixMillis(raw.created_at),
        settled_at: raw.settled_at.map(UnixMillis),
    })
}

/// Checks a claim's execution identities before the claim writes anything, so
/// that a malformed one is refused and never stored (decision 0057 rule 6).
pub(super) fn check_execution(execution: &ExecutionClaim<'_>) -> Result<(), JobStoreError> {
    if !is_lowercase_hex(execution.tool_digest, 64) {
        return Err(JobStoreError::InvalidRequest(
            "the tool digest must be 64 lowercase hexadecimal digits",
        ));
    }
    if !is_lowercase_hex(execution.directory, 32) {
        return Err(JobStoreError::InvalidRequest(
            "the execution directory identity must be 32 lowercase hexadecimal digits",
        ));
    }
    for identity in [execution.machine, execution.boot, execution.cgroupns] {
        let visible = identity.bytes().all(|byte| byte.is_ascii_graphic());
        if !(1..=256).contains(&identity.len()) || !visible {
            return Err(JobStoreError::InvalidRequest(
                "a host identity must be 1-256 visible ASCII characters",
            ));
        }
    }
    Ok(())
}

/// Whether `value` is exactly `length` lowercase hexadecimal digits.
fn is_lowercase_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Stores the attempt of a claim, in the claim's transaction. The actor's
/// generation is read in the same statement (decision 0057 rule 5): an actor
/// with no `users` row pins none, and a row that appears later is a different
/// generation. A disabled actor's row still gives its generation.
pub(super) fn insert(
    transaction: &Transaction<'_>,
    job: &JobId,
    fence: u64,
    profile: &ProfileName,
    execution: &ExecutionClaim<'_>,
    now: UnixMillis,
) -> Result<(), StoreError> {
    transaction
        .execute(
            "INSERT INTO execution_attempts (attempt, job_id, fence, actor_generation, profile,
                 tool_digest, machine, boot, cgroupns, directory, state, revoked, created_at,
                 settled_at)
             VALUES (?1, ?2, ?3,
                 (SELECT u.state_generation FROM users u
                  WHERE u.id = (SELECT j.actor FROM jobs j WHERE j.id = ?2)),
                 ?4, ?5, ?6, ?7, ?8, ?9, 'live', NULL, ?10, NULL)",
            params![
                execution.attempt.hex(),
                job.as_str(),
                i64::try_from(fence).map_err(corrupt)?,
                profile.as_str(),
                execution.tool_digest,
                execution.machine,
                execution.boot,
                execution.cgroupns,
                execution.directory,
                now.0,
            ],
        )
        .map_err(unavailable)?;
    Ok(())
}

/// Whether the actor's current generation differs from the one the job's newest
/// attempt pinned (decision 0042 rule 6; decision 0057 rules 5 and 9). A job with
/// no attempt answers `false`. A NULL pin with a users row now present, or the
/// reverse, is a different generation (rule 5).
pub(super) fn generation_changed(
    transaction: &Transaction<'_>,
    job: &JobId,
) -> Result<bool, StoreError> {
    let pins: Option<(Option<i64>, Option<i64>)> = transaction
        .query_row(
            "SELECT e.actor_generation, u.state_generation
             FROM execution_attempts e
             JOIN jobs j ON j.id = e.job_id
             LEFT JOIN users u ON u.id = j.actor
             WHERE e.job_id = ?1
             ORDER BY e.fence DESC LIMIT 1",
            [job.as_str()],
            |raw| Ok((raw.get(0)?, raw.get(1)?)),
        )
        .optional()
        .map_err(unavailable)?;
    Ok(pins.is_some_and(|(pinned, current)| pinned != current))
}

/// Whether the job has an attempt that is not settled yet. The due set of a
/// `run_profile` job excludes such a job (decision 0057 rule 10).
pub(super) fn unsettled(connection: &Connection, job: &JobId) -> Result<bool, StoreError> {
    connection
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM execution_attempts
                 WHERE job_id = ?1 AND state <> 'settled')",
            [job.as_str()],
            |row| row.get(0),
        )
        .map_err(unavailable)
}

/// One attempt by its identity, or `None`. It reads no artifact or user.
pub(super) fn read_attempt(
    connection: &Connection,
    attempt: &AttemptIdentity,
) -> Result<Option<AttemptRow>, StoreError> {
    let raw = connection
        .query_row(
            &format!("SELECT {ATTEMPT_COLUMNS} FROM execution_attempts WHERE attempt = ?1"),
            [attempt.hex()],
            RawAttempt::from_row,
        )
        .optional()
        .map_err(unavailable)?;
    raw.map(into_row).transpose()
}

/// Every attempt that is not settled, oldest first. It reads no artifact or
/// user.
pub(super) fn unsettled_rows(connection: &Connection) -> Result<Vec<AttemptRow>, StoreError> {
    let sql = format!(
        "SELECT {ATTEMPT_COLUMNS} FROM execution_attempts
         WHERE state <> 'settled' ORDER BY created_at, attempt"
    );
    let mut statement = connection.prepare_cached(&sql).map_err(unavailable)?;
    let rows = statement
        .query_map([], RawAttempt::from_row)
        .map_err(unavailable)?;
    let mut attempts = Vec::new();
    for raw in rows {
        attempts.push(into_row(raw.map_err(unavailable)?)?);
    }
    Ok(attempts)
}

/// Rule 7: the reason an attempt must stop, or `None` when it may go on. A
/// revocation already stored answers first. Otherwise the current state is
/// read, in rule 7's order: a reason found there is stored as the revocation
/// in this transaction, so the stop is durable before the caller answers it.
/// The fence is the last check.
///
/// The job's state belongs to its current attempt, the one with the highest
/// fence. An older attempt, which a later claim replaced after it settled, is
/// answered `fence` before the job's state is read: a reason found there is
/// not stored on it and moves no job, so a late call of a replaced worker
/// cannot end the job of the attempt that holds it now (rule 10).
fn assess(
    transaction: &Transaction<'_>,
    lease: &Lease,
    row: &AttemptRow,
    now: UnixMillis,
) -> Result<Option<StopReason>, StoreError> {
    if let Some(revoked) = row.revoked {
        return Ok(Some(revoked.stop_reason()));
    }
    let newest: i64 = transaction
        .query_row(
            "SELECT max(fence) FROM execution_attempts WHERE job_id = ?1",
            [row.job.as_str()],
            |raw| raw.get(0),
        )
        .map_err(unavailable)?;
    if u64::try_from(newest).map_err(corrupt)? != row.fence {
        return Ok(Some(StopReason::Fence));
    }
    let (state, fence, cancel, trashed, purged, disabled, current, foreign): (
        String,
        i64,
        bool,
        bool,
        bool,
        bool,
        Option<i64>,
        bool,
    ) = transaction
        .query_row(
            "SELECT j.state, j.fence, j.cancel_requested,
                    a.deleted_at IS NOT NULL AND a.purged_at IS NULL,
                    a.purged_at IS NOT NULL,
                    u.id IS NOT NULL AND u.disabled_at IS NOT NULL,
                    u.state_generation,
                    a.owner <> j.actor
             FROM jobs j
             JOIN artifacts a ON a.id = j.artifact_id
             LEFT JOIN users u ON u.id = j.actor
             WHERE j.id = ?1",
            [row.job.as_str()],
            |raw| {
                Ok((
                    raw.get(0)?,
                    raw.get(1)?,
                    raw.get(2)?,
                    raw.get(3)?,
                    raw.get(4)?,
                    raw.get(5)?,
                    raw.get(6)?,
                    raw.get(7)?,
                ))
            },
        )
        .map_err(unavailable)?;
    let current = current.map(u64::try_from).transpose().map_err(corrupt)?;
    let pinned = row.actor_generation.map(StateGeneration::get);
    // A disable raises the generation, so a disabled user counts as a new
    // generation (rule 9). A NULL pin with a row now present is a different
    // generation (rule 5).
    let generation = disabled || current != pinned;
    let reason = if cancel {
        Some(StopReason::Cancel)
    } else if trashed {
        Some(StopReason::Trash)
    } else if purged {
        Some(StopReason::Purge)
    } else if generation {
        Some(StopReason::Generation)
    } else if foreign {
        // Authority is the store's mirror of the core's owner-only access
        // check. Ownership is fixed in R02ba, so only a raw edit reaches it.
        Some(StopReason::Authority)
    } else {
        None
    };
    if let Some(revocation) = reason.and_then(StopReason::revocation) {
        revoke(transaction, row, revocation, now)?;
    }
    if reason.is_some() {
        return Ok(reason);
    }
    let held = state == JobState::Running.as_str()
        && u64::try_from(fence).map_err(corrupt)? == lease.fence
        && row.fence == lease.fence;
    Ok((!held).then_some(StopReason::Fence))
}

/// Stores `revocation` on an attempt that has none. A settled attempt has had
/// its cleanup, so its job may end now (rule 10). An unsettled attempt waits
/// for its worker's settlement.
fn revoke(
    transaction: &Transaction<'_>,
    row: &AttemptRow,
    revocation: Revocation,
    now: UnixMillis,
) -> Result<(), StoreError> {
    transaction
        .execute(
            "UPDATE execution_attempts SET revoked = ?2 WHERE attempt = ?1 AND revoked IS NULL",
            params![row.attempt.hex(), revocation.as_str()],
        )
        .map_err(unavailable)?;
    if row.state == AttemptState::Settled {
        move_job(transaction, &row.job, revocation, now)?;
    }
    Ok(())
}

/// Moves a running job whose attempt was revoked and settled (rule 10): to
/// cancelled for a cancel, trash, purge, generation, or authority revocation,
/// and to retry-scheduled or failed by its attempts for a lease or shutdown
/// one. A job that is not running, or that still has an attempt that is not
/// settled, is left as it is: no transition happens while an attempt of the
/// job is unsettled (rule 10). No change row is written (rule 3).
fn move_job(
    transaction: &Transaction<'_>,
    job: &JobId,
    revocation: Revocation,
    now: UnixMillis,
) -> Result<(), StoreError> {
    let (state, attempts, max_attempts, unsettled): (String, i64, i64, bool) = transaction
        .query_row(
            "SELECT state, attempts, max_attempts,
                    EXISTS (SELECT 1 FROM execution_attempts e
                            WHERE e.job_id = jobs.id AND e.state <> 'settled')
             FROM jobs WHERE id = ?1",
            [job.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .map_err(unavailable)?;
    if state != JobState::Running.as_str() || unsettled {
        return Ok(());
    }
    if revocation.ends_cancelled() {
        return end(transaction, job, JobState::Cancelled, None, now);
    }
    if attempts < max_attempts {
        transaction
            .execute(
                "UPDATE jobs SET state = 'retry_scheduled', run_after = ?2, worker = NULL,
                     lease_expires_at = NULL
                 WHERE id = ?1",
                params![job.as_str(), now.0],
            )
            .map_err(unavailable)?;
        return Ok(());
    }
    end(
        transaction,
        job,
        JobState::Failed,
        Some(JobFailure::LeaseExpired),
        now,
    )
}

/// The checkpoint of a running attempt (decision 0057 rule 7). It answers
/// whether the worker may go on, and renews the lease to `now + lease_ms` when
/// it may, as a heartbeat does.
pub(super) fn checkpoint(
    transaction: &Transaction<'_>,
    lease: &Lease,
    attempt: &AttemptIdentity,
    now: UnixMillis,
    lease_ms: u64,
) -> Result<CheckpointAnswer, JobStoreError> {
    let row = read_attempt(transaction, attempt)?
        .filter(|row| row.job == lease.job)
        .ok_or(JobStoreError::NotFound)?;
    if let Some(reason) = assess(transaction, lease, &row, now)? {
        return Ok(CheckpointAnswer::Stop(reason));
    }
    if row.state != AttemptState::Live {
        return Ok(CheckpointAnswer::Stop(StopReason::Fence));
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
    Ok(CheckpointAnswer::Continue)
}

/// Marks an attempt settled, once its cleanup is done (decision 0057 rule 10).
/// A revoked attempt then moves its job. An unrevoked one leaves the job
/// running for its own worker. It takes no lease and no fence, and it reads no
/// artifact or user state.
pub(super) fn settle(
    transaction: &Transaction<'_>,
    attempt: &AttemptIdentity,
    now: UnixMillis,
) -> Result<SettleOutcome, JobStoreError> {
    let row = read_attempt(transaction, attempt)?.ok_or(JobStoreError::NotFound)?;
    if row.state == AttemptState::Settled {
        return Ok(SettleOutcome::AlreadySettled(written(
            transaction,
            &row.job,
        )?));
    }
    transaction
        .execute(
            "UPDATE execution_attempts SET state = 'settled', settled_at = ?2 WHERE attempt = ?1",
            params![attempt.hex(), now.0],
        )
        .map_err(unavailable)?;
    if let Some(revocation) = row.revoked {
        move_job(transaction, &row.job, revocation, now)?;
    }
    Ok(SettleOutcome::Settled(written(transaction, &row.job)?))
}

/// Marks a `live` attempt `cleanup_pending` when its worker did not observe its
/// cleanup (decision 0057 rules 10 and 19). Other states are left as they are.
pub(super) fn mark_cleanup_pending(
    transaction: &Transaction<'_>,
    attempt: &AttemptIdentity,
) -> Result<(), JobStoreError> {
    if read_attempt(transaction, attempt)?.is_none() {
        return Err(JobStoreError::NotFound);
    }
    transaction
        .execute(
            "UPDATE execution_attempts SET state = 'cleanup_pending' WHERE attempt = ?1 AND state = 'live'",
            params![attempt.hex()],
        )
        .map_err(unavailable)?;
    Ok(())
}

/// Ends a `run_profile` job as succeeded with its two counts (decision 0057
/// rule 12), when the binding names the profile and tool digest of the attempt
/// row; a binding of other tools is refused before anything is written. The outer error is a storage failure, which rolls the transaction
/// back. The inner error is a refusal, which the caller commits, so a revocation
/// stored by rule 7 persists.
pub(super) fn complete(
    transaction: &Transaction<'_>,
    lease: &Lease,
    attempt: &AttemptIdentity,
    binding: ProfileBinding<'_>,
    counts: ProfileCounts,
    now: UnixMillis,
) -> Result<Result<JobRecord, JobStoreError>, JobStoreError> {
    let Some(row) = read_attempt(transaction, attempt)?.filter(|row| row.job == lease.job) else {
        return Ok(Err(JobStoreError::NotFound));
    };
    if *binding.profile != row.profile || binding.tool_digest != row.tool_digest {
        return Ok(Err(JobStoreError::InvalidRequest(
            "the manifest binding names other tools than the attempt",
        )));
    }
    if let Some(reason) = assess(transaction, lease, &row, now)? {
        return Ok(Err(match reason {
            StopReason::Fence => JobStoreError::LeaseLost,
            other => JobStoreError::Revoked(other),
        }));
    }
    if row.state != AttemptState::Settled {
        return Ok(Err(JobStoreError::AttemptUnsettled));
    }
    let count = |value: u64| i64::try_from(value).map_err(corrupt);
    transaction
        .execute(
            "UPDATE jobs SET state = 'succeeded', worker = NULL, lease_expires_at = NULL,
                 files_checked = ?2, bytes_checked = ?3, mismatch_count = NULL, finished_at = ?4
             WHERE id = ?1",
            params![
                lease.job.as_str(),
                count(counts.files)?,
                count(counts.bytes)?,
                now.0,
            ],
        )
        .map_err(unavailable)?;
    Ok(Ok(written(transaction, &lease.job)?))
}
