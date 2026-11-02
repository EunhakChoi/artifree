//! The worker process loop (decision 0030): it opens the same database as
//! the server and runs persistent jobs from [`artifree_core::worker`] one
//! after another.
//!
//! The worker is trusted in-repository code. It has no network access of its
//! own and no credential: it reads and writes the database file through the
//! same store layer as the server, with the same busy timeout and
//! write-ahead log. It does not sandbox anything, and untrusted execution
//! (roadmap R02), React builds (R03), and provider sampling (P01) are not
//! implemented. Two workers, or a worker and a server, may share a data
//! directory; each transition is one atomic database transaction.
//!
//! The loop is synchronous. A process that wants a graceful stop sets the
//! shared flag; the worker gives the job it holds back to the queue at its
//! next checkpoint, uses no attempt, and returns.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use artifree_core::jobs::Clock;
use artifree_core::worker::{self, Pace, Step, WorkerConfig};
use artifree_domain::job::WorkerId;
use artifree_ports::job_store::{JobRecord, JobStoreError};
use artifree_providers::storage::{DATABASE_FILE, SqliteStore};

/// How often a sleeping worker looks at the shutdown flag.
const SLICE: Duration = Duration::from_millis(20);

/// Store failures in a row after which the worker gives up. A database that
/// cannot be used is not a transient problem a loop can wait out.
const MAX_CONSECUTIVE_FAILURES: u32 = 5;

/// Settings of `artifree-server worker run`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerOptions {
    /// The data directory of the server whose jobs to run. It must hold the
    /// database already; a worker never creates one.
    pub data_dir: PathBuf,
    /// Stop when no job is due instead of waiting for more. A job waiting for
    /// a retry in the future is not due.
    pub once: bool,
    /// How long to wait between looks at an empty queue.
    pub poll: Duration,
    /// How long a lease lasts without a renewal.
    pub lease: Duration,
    /// Testing only: after each claim, wait until this file exists before
    /// reading the job's files. It holds a job at a known point so a test can
    /// stop the process there.
    pub hold_file: Option<PathBuf>,
}

/// Why the worker returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopped {
    /// `once` was set and no job was due.
    Drained,
    /// The process was asked to stop.
    Signal,
}

/// Something the worker did, for the process around it to report.
#[derive(Debug)]
pub enum WorkerEvent<'a> {
    /// The database is open and the worker is looking for jobs.
    Ready {
        /// The name this worker gave itself.
        worker: &'a WorkerId,
    },
    /// A job was claimed and left in the state of the record.
    Job(&'a JobRecord),
    /// A job was given back because the process is stopping.
    Released(&'a JobRecord),
    /// The worker lost the lease of a job and wrote nothing.
    LeaseLost(&'a artifree_domain::job::JobId),
    /// A store call failed; the worker tries again.
    Warning(&'a JobStoreError),
}

/// The flag and the test hold, as the core's [`Pace`].
struct ProcessPace<'a> {
    shutdown: &'a AtomicBool,
    hold_file: Option<&'a Path>,
}

impl Pace for ProcessPace<'_> {
    fn shutdown_requested(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }

    fn before_file(&self) {
        let Some(path) = self.hold_file else { return };
        while !path.exists() && !self.shutdown_requested() {
            std::thread::sleep(SLICE);
        }
    }
}

/// Runs jobs until `once` finds the queue empty or `shutdown` is set.
///
/// # Errors
///
/// A message when the database does not exist or cannot be opened (a newer or
/// foreign schema is refused), or when the store failed
/// five times in a row.
pub fn run(
    options: &WorkerOptions,
    clock: &dyn Clock,
    shutdown: &AtomicBool,
    on_event: &mut dyn FnMut(&WorkerEvent<'_>),
) -> Result<Stopped, String> {
    let database = options.data_dir.join(DATABASE_FILE);
    // A worker never creates a database: an empty file would be taken for a
    // new one by the store, so it is refused here, unopened and unchanged.
    let initialized =
        std::fs::metadata(&database).is_ok_and(|meta| meta.is_file() && meta.len() > 0);
    if !initialized {
        return Err(format!(
            "there is no initialized database in {}; start the server once with this data directory",
            options.data_dir.display()
        ));
    }
    let store = SqliteStore::open(&database).map_err(|error| error.to_string())?;
    let mut random = [0_u8; 4];
    getrandom::fill(&mut random).map_err(|error| format!("no system randomness: {error}"))?;
    let worker = WorkerId::from_random(random);
    let lease_ms = u64::try_from(options.lease.as_millis()).unwrap_or(u64::MAX);
    let config = WorkerConfig::with_lease_ms(lease_ms);
    let pace = ProcessPace {
        shutdown,
        hold_file: options.hold_file.as_deref(),
    };
    on_event(&WorkerEvent::Ready { worker: &worker });
    let mut failures = 0_u32;
    loop {
        if shutdown.load(Ordering::Acquire) {
            return Ok(Stopped::Signal);
        }
        let mut settled = |record: &JobRecord| on_event(&WorkerEvent::Job(record));
        match worker::run_next(&store, clock, &worker, &config, &pace, &mut settled) {
            Ok(Step::Idle) => {
                failures = 0;
                if options.once {
                    return Ok(Stopped::Drained);
                }
                if !sleep(options.poll, shutdown) {
                    return Ok(Stopped::Signal);
                }
            }
            Ok(Step::Finished(record)) => {
                failures = 0;
                on_event(&WorkerEvent::Job(&record));
            }
            Ok(Step::Released(record)) => {
                on_event(&WorkerEvent::Released(&record));
                return Ok(Stopped::Signal);
            }
            Ok(Step::LeaseLost(job)) => {
                failures = 0;
                on_event(&WorkerEvent::LeaseLost(&job));
            }
            Err(error) => {
                failures += 1;
                on_event(&WorkerEvent::Warning(&error));
                if failures >= MAX_CONSECUTIVE_FAILURES {
                    return Err(format!(
                        "the store failed {failures} times in a row: {error}"
                    ));
                }
                if !sleep(options.poll, shutdown) {
                    return Ok(Stopped::Signal);
                }
            }
        }
    }
}

/// Sleeps for `total` in slices; `false` when `shutdown` was set meanwhile.
fn sleep(total: Duration, shutdown: &AtomicBool) -> bool {
    let mut left = total;
    while !left.is_zero() {
        if shutdown.load(Ordering::Acquire) {
            return false;
        }
        let slice = left.min(SLICE);
        std::thread::sleep(slice);
        left -= slice;
    }
    !shutdown.load(Ordering::Acquire)
}
