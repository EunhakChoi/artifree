//! The rows in which execution workers record that they are alive (decision
//! 0058 rule 20, slice R03cda). `serve` and its trusted worker share only the
//! database with an execution worker, which runs in its own unit under
//! another account (0057 rule 18), so the readiness is read from here.

use artifree_domain::identity::UnixMillis;
use artifree_domain::job::{JobId, WorkerId};
use artifree_ports::job_store::{
    ExecutionPresence, JobStoreError, READY_WINDOW_MS, SEEN_PRUNE_MS, SEEN_REWRITE_MS,
};
use rusqlite::{Connection, Transaction, params};

use crate::storage::{SqliteStore, unavailable};

/// The rewrite rule of [`ExecutionPresence::mark_seen`], for `?2` the time.
const STALE: &str = "last_seen <= ?2 - ?3 OR last_seen > ?2";

/// Whether `worker`'s row needs no write at `now`.
fn fresh(
    connection: &Connection,
    worker: &WorkerId,
    now: UnixMillis,
) -> Result<bool, JobStoreError> {
    connection
        .query_row(
            &format!(
                "SELECT EXISTS (SELECT 1 FROM execution_workers
                     WHERE worker = ?1 AND NOT ({STALE}))"
            ),
            params![worker.as_str(), now.0, SEEN_REWRITE_MS],
            |row| row.get(0),
        )
        .map_err(|error| unavailable(error).into())
}

/// Deletes the rows last seen more than an hour before `now`, or more than an
/// hour after it (a clock that stepped back).
fn prune(transaction: &Transaction<'_>, now: UnixMillis) -> Result<(), JobStoreError> {
    transaction
        .execute(
            "DELETE FROM execution_workers WHERE last_seen < ?1 - ?2 OR last_seen > ?1 + ?2",
            params![now.0, SEEN_PRUNE_MS],
        )
        .map_err(unavailable)?;
    Ok(())
}

/// Marks the worker that holds `job` seen at `now`, in the caller's
/// transaction, under the rewrite rule of [`ExecutionPresence::mark_seen`]. A
/// renewal of a running `run_profile` job's lease calls it, so a worker that
/// runs one long attempt keeps its row fresh.
pub(super) fn seen_by_job(
    transaction: &Transaction<'_>,
    job: &JobId,
    now: UnixMillis,
) -> Result<(), JobStoreError> {
    transaction
        .execute(
            &format!(
                "INSERT INTO execution_workers (worker, last_seen)
                     SELECT worker, ?2 FROM jobs WHERE id = ?1 AND worker IS NOT NULL
                 ON CONFLICT (worker) DO UPDATE SET last_seen = excluded.last_seen
                     WHERE {STALE}"
            ),
            params![job.as_str(), now.0, SEEN_REWRITE_MS],
        )
        .map_err(unavailable)?;
    prune(transaction, now)
}

impl ExecutionPresence for SqliteStore {
    fn mark_seen(&self, worker: &WorkerId, now: UnixMillis) -> Result<(), JobStoreError> {
        // An idle poll reads a fresh row and takes no write lock.
        if fresh(&self.connect()?, worker, now)? {
            return Ok(());
        }
        self.write(|transaction| {
            transaction
                .execute(
                    &format!(
                        "INSERT INTO execution_workers (worker, last_seen) VALUES (?1, ?2)
                         ON CONFLICT (worker) DO UPDATE SET last_seen = excluded.last_seen
                             WHERE {STALE}"
                    ),
                    params![worker.as_str(), now.0, SEEN_REWRITE_MS],
                )
                .map_err(unavailable)?;
            prune(transaction, now)
        })
    }

    fn forget_worker(&self, worker: &WorkerId) -> Result<(), JobStoreError> {
        // A refused worker polls again and again; without a row it takes no
        // write lock.
        let present: bool = self
            .connect()?
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM execution_workers WHERE worker = ?1)",
                [worker.as_str()],
                |row| row.get(0),
            )
            .map_err(unavailable)?;
        if !present {
            return Ok(());
        }
        self.write(|transaction| {
            transaction
                .execute(
                    "DELETE FROM execution_workers WHERE worker = ?1",
                    [worker.as_str()],
                )
                .map_err(unavailable)?;
            Ok(())
        })
    }

    fn execution_ready(&self, now: UnixMillis) -> Result<bool, JobStoreError> {
        self.connect()?
            .query_row(
                concat!("SELECT ", a_worker_is_ready!()),
                rusqlite::named_params! { ":now": now.0, ":window": READY_WINDOW_MS },
                |row| row.get(0),
            )
            .map_err(|error| unavailable(error).into())
    }
}
