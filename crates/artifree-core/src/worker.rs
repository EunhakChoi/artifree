//! The worker side of persistent jobs (decision 0030): claim a job, verify the
//! pinned revision, and write the result once.
//!
//! [`run_next`] does one job and returns; the loop, the sleeping between
//! polls, and the signal handling belong to the process that runs it. Time
//! comes from a [`Clock`], never from the wall clock, so tests move it.
//!
//! The one job kind re-reads every file of a pinned revision through the
//! artifact store, recomputes its SHA-256 digest and size, and compares them
//! with the stored listing. It runs trusted in-repository code only. There is
//! no sandbox and no network use; running untrusted work (roadmap R02),
//! building React sources (R03), and calling model providers (P01) are not
//! implemented here.
//!
//! A worker that loses its lease (it stalled past the expiry and another
//! worker claimed the job, or a purge revoked the job) learns it at its next
//! store call, which is rejected with [`JobStoreError::LeaseLost`], and writes
//! nothing.

use artifree_domain::identity::UnixMillis;
use artifree_domain::job::{
    Finding, JobFailure, JobId, MAX_FINDINGS, Problem, VerifyReport, WorkerId,
};
use artifree_ports::artifact_store::{ArtifactStore, StoreError};
use artifree_ports::job_store::{
    Checkpoint, JobClaim, JobRecord, JobStore, JobStoreError, Lease, Retry,
};
use sha2::{Digest, Sha256};

use crate::jobs::Clock;

/// When a failed attempt is tried again: `base_ms` after the first attempt,
/// doubling after each, up to `cap_ms`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// The wait after the first failed attempt, in milliseconds.
    pub base_ms: u64,
    /// The longest wait, in milliseconds.
    pub cap_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            base_ms: 1_000,
            cap_ms: 60_000,
        }
    }
}

impl RetryPolicy {
    /// The wait after `attempt` failed attempts (1 for the first).
    #[must_use]
    pub fn delay_ms(&self, attempt: u32) -> u64 {
        let doublings = attempt.saturating_sub(1).min(32);
        self.base_ms
            .saturating_mul(1_u64 << doublings)
            .min(self.cap_ms)
    }
}

/// How a worker leases and retries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerConfig {
    /// How long a lease lasts from its last renewal, in milliseconds.
    pub lease_ms: u64,
    /// How often the lease is renewed while a job runs, in milliseconds. At
    /// most a third of the lease, so two renewals can be missed.
    pub heartbeat_ms: u64,
    /// The backoff between attempts.
    pub retry: RetryPolicy,
}

impl WorkerConfig {
    /// A configuration with a lease of `lease_ms` renewed every third of it.
    #[must_use]
    pub fn with_lease_ms(lease_ms: u64) -> Self {
        Self {
            lease_ms,
            heartbeat_ms: (lease_ms / 3).max(1),
            retry: RetryPolicy::default(),
        }
    }
}

/// What the process around a worker can tell it between files.
pub trait Pace {
    /// Whether the process was asked to stop. The worker then gives a job it
    /// has not finished verifying back and returns; a verification that is
    /// complete is recorded first.
    fn shutdown_requested(&self) -> bool;

    /// Called before each file is read. A test uses it to hold a job at a
    /// known point; the worker has no other use for it.
    fn before_file(&self) {}
}

/// What one call of [`run_next`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// No job was due.
    Idle,
    /// A job was claimed and left in the state of the record: succeeded,
    /// failed, retry scheduled, or cancelled.
    Finished(JobRecord),
    /// The process was asked to stop: the job was given back, and no attempt
    /// was used.
    Released(JobRecord),
    /// The worker lost the lease and wrote nothing.
    LeaseLost(JobId),
}

/// How the verification of a claimed job ended.
enum Verified {
    Done(VerifyReport),
    Cancelled,
    Shutdown,
    LeaseLost,
}

/// Claims the oldest due job and runs it to an end the store records, or
/// returns [`Step::Idle`].
///
/// A claim can also end jobs that nobody can run any more: an expired lease
/// with a cancel requested, or on its last attempt. Each such job is passed to
/// `on_settled` as the claim returns, before any job is run, so the process
/// around the worker can report every job that reached an end (the result is
/// not lost if the run that follows fails).
///
/// # Errors
///
/// [`JobStoreError::Unavailable`] when the store fails around the claim or the
/// result. The job then keeps its lease until it expires, and a later claim
/// runs it again: a result is never written twice, but a verification can run
/// more than once.
pub fn run_next<S: ArtifactStore + JobStore>(
    store: &S,
    clock: &dyn Clock,
    worker: &WorkerId,
    config: &WorkerConfig,
    pace: &dyn Pace,
    on_settled: &mut dyn FnMut(&JobRecord),
) -> Result<Step, JobStoreError> {
    let claim = store.claim_job(JobClaim {
        worker,
        now: clock.now(),
        lease_ms: config.lease_ms,
    })?;
    for record in &claim.settled {
        on_settled(record);
    }
    let Some(claimed) = claim.claimed else {
        return Ok(Step::Idle);
    };
    let (job, lease) = (claimed.job, claimed.lease);
    let ended = match verify(store, clock, &lease, &job, config, pace) {
        Ok(Verified::Done(report)) => store.complete_job(&lease, &report, clock.now()),
        Ok(Verified::Cancelled) => store.confirm_cancel(&lease, clock.now()),
        Ok(Verified::Shutdown) => {
            return match store.release_job(&lease, clock.now()) {
                Ok(record) => Ok(Step::Released(record)),
                Err(JobStoreError::LeaseLost) => Ok(Step::LeaseLost(lease.job)),
                Err(error) => Err(error),
            };
        }
        Ok(Verified::LeaseLost) => return Ok(Step::LeaseLost(lease.job)),
        Err(failure) => {
            let now = clock.now();
            let retry = if failure.is_retryable() {
                Retry::At(add_ms(now, config.retry.delay_ms(job.attempts)))
            } else {
                Retry::Never
            };
            store.fail_job(&lease, failure, retry, now)
        }
    };
    match ended {
        Ok(record) => Ok(Step::Finished(record)),
        Err(JobStoreError::LeaseLost) => Ok(Step::LeaseLost(lease.job)),
        Err(error) => Err(error),
    }
}

fn add_ms(time: UnixMillis, ms: u64) -> UnixMillis {
    UnixMillis(time.0.saturating_add(i64::try_from(ms).unwrap_or(i64::MAX)))
}

/// Renews the lease on a schedule and watches for a cancel or a shutdown.
struct Checkpoints<'a> {
    store: &'a dyn JobStore,
    clock: &'a dyn Clock,
    lease: &'a Lease,
    config: &'a WorkerConfig,
    pace: &'a dyn Pace,
    last_renewal: Option<UnixMillis>,
}

impl Checkpoints<'_> {
    /// The first checkpoint, each one a heartbeat's interval after the last,
    /// and the final one before the result is written renew the lease and read
    /// the cancel request. `Ok(None)` means go on.
    fn reach(&mut self, last: bool) -> Option<Verified> {
        // The final checkpoint ignores a stop request: the verification is
        // complete, so its result is recorded and the worker stops after it
        // (it starts no new job). Only unfinished work is given back.
        if !last && self.pace.shutdown_requested() {
            return Some(Verified::Shutdown);
        }
        let now = self.clock.now();
        let due = self.last_renewal.is_none_or(|renewed| {
            now.0.saturating_sub(renewed.0)
                >= i64::try_from(self.config.heartbeat_ms).unwrap_or(i64::MAX)
        });
        if !(due || last) {
            return None;
        }
        match self
            .store
            .heartbeat_job(self.lease, now, self.config.lease_ms)
        {
            Ok(Checkpoint::Continue) => {
                self.last_renewal = Some(now);
                None
            }
            Ok(Checkpoint::CancelRequested) => Some(Verified::Cancelled),
            Err(JobStoreError::LeaseLost) => Some(Verified::LeaseLost),
            // A renewal that fails is tried again at the next checkpoint. The
            // write of the result does not look at the clock: it is accepted
            // while the worker's fence is the job's current claim, even after
            // the lease time passed, and rejected once another worker claimed
            // the job or a purge revoked it. The fence decides, not the time.
            Err(_) => None,
        }
    }
}

/// Reads the pinned revision's files one at a time and compares each with its
/// listing. Memory holds one file (a file is at most 5 MiB).
fn verify<S: ArtifactStore + JobStore>(
    store: &S,
    clock: &dyn Clock,
    lease: &Lease,
    job: &JobRecord,
    config: &WorkerConfig,
    pace: &dyn Pace,
) -> Result<Verified, JobFailure> {
    let listing = match store.revision(&job.artifact, job.revision) {
        // A revision always has files; none means the artifact was purged
        // since the job was claimed.
        Ok(Some(listing)) if !listing.files.is_empty() => listing,
        Ok(_) => return Err(JobFailure::RevisionUnavailable),
        Err(StoreError::Unavailable(_)) => return Err(JobFailure::StorageUnavailable),
        Err(_) => return Err(JobFailure::Internal),
    };
    let mut checkpoints = Checkpoints {
        store,
        clock,
        lease,
        config,
        pace,
        last_renewal: None,
    };
    let mut report = VerifyReport {
        files_checked: 0,
        bytes_checked: 0,
        mismatch_count: 0,
        findings: Vec::new(),
    };
    for entry in &listing.files {
        if let Some(stop) = checkpoints.reach(false) {
            return Ok(stop);
        }
        pace.before_file();
        let content = match store.revision_file(&job.artifact, job.revision, &entry.path) {
            Ok(content) => content,
            Err(StoreError::Unavailable(_)) => return Err(JobFailure::StorageUnavailable),
            Err(_) => return Err(JobFailure::Internal),
        };
        let problem = match &content {
            None => Some(Problem::ContentMissing),
            Some(bytes) if u64::try_from(bytes.len()).ok() != Some(entry.size) => {
                Some(Problem::SizeMismatch)
            }
            Some(bytes) if Sha256::digest(bytes).as_slice() != entry.sha256.as_slice() => {
                Some(Problem::DigestMismatch)
            }
            Some(_) => None,
        };
        report.files_checked += 1;
        report.bytes_checked += content
            .as_ref()
            .map_or(0, |bytes| u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        if let Some(problem) = problem {
            report.mismatch_count += 1;
            if report.findings.len() < MAX_FINDINGS {
                report.findings.push(Finding {
                    path: entry.path.clone(),
                    problem,
                });
            }
        }
    }
    Ok(checkpoints.reach(true).unwrap_or(Verified::Done(report)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_backoff_doubles_and_stops_at_the_cap() {
        let policy = RetryPolicy {
            base_ms: 1_000,
            cap_ms: 5_000,
        };
        let delays: Vec<u64> = (1..=5).map(|attempt| policy.delay_ms(attempt)).collect();
        assert_eq!(delays, [1_000, 2_000, 4_000, 5_000, 5_000]);
        assert_eq!(policy.delay_ms(0), 1_000, "no failed attempt yet");
        assert_eq!(policy.delay_ms(u32::MAX), 5_000, "no overflow");
    }

    #[test]
    fn a_lease_is_renewed_three_times_per_expiry() {
        let config = WorkerConfig::with_lease_ms(30_000);
        assert_eq!(config.heartbeat_ms, 10_000);
        assert_eq!(WorkerConfig::with_lease_ms(1).heartbeat_ms, 1);
    }
}
