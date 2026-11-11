//! The worker a persistent server starts and supervises (decision 0055, rules 1 to 3 and 5).

use std::env;
use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use artifree_contracts::api::{
    WorkerExit, WorkerExitOutcome, WorkerMode, WorkerState, WorkerStatus,
};
use artifree_core::jobs::Clock;
use tokio::io::AsyncRead;
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::{self, Instant};

use crate::shutdown::StopLatch;
use crate::worker_lines::{self, WorkerLine};

/// The delay before the first start after an exit or a failed start. Each later
/// start waits twice as long, up to `MAX_DELAY`.
pub const FIRST_DELAY: Duration = Duration::from_secs(1);

/// The longest wait between an exit or a failed start and the next start.
pub const MAX_DELAY: Duration = Duration::from_secs(60);

/// A worker that runs at least this long starts the delays over at `FIRST_DELAY`.
pub const RESET_AFTER: Duration = Duration::from_secs(60);

/// How long a worker may take to exit after its standard input closes, before it
/// is killed (decision 0055, rule 2).
pub const STOP_GRACE: Duration = Duration::from_secs(5);

/// The server's view of its worker: the status that `GET /v1/background` reports.
///
/// The supervisor changes it and the request handlers read it. Clones share one
/// status.
#[derive(Debug, Clone)]
pub struct WorkerStatusHandle {
    status: Arc<watch::Sender<WorkerStatus>>,
}

impl Default for WorkerStatusHandle {
    fn default() -> Self {
        Self::new(WorkerStatus {
            mode: WorkerMode::None,
            state: WorkerState::Stopped,
            restarts: 0,
            last_exit: None,
            dropped_lines: 0,
        })
    }
}

impl WorkerStatusHandle {
    /// A handle that holds `status`.
    #[must_use]
    pub fn new(status: WorkerStatus) -> Self {
        Self {
            status: Arc::new(watch::channel(status).0),
        }
    }

    /// A handle for a server that starts a worker. The worker is stopped until the
    /// supervisor starts one.
    #[must_use]
    pub fn supervised() -> Self {
        Self::new(WorkerStatus {
            mode: WorkerMode::Supervised,
            state: WorkerState::Stopped,
            restarts: 0,
            last_exit: None,
            dropped_lines: 0,
        })
    }

    /// A copy of the status as it is now.
    #[must_use]
    pub fn current(&self) -> WorkerStatus {
        self.status.borrow().clone()
    }

    /// Applies `change` to the status in one step, so a reader sees all of it or
    /// none of it.
    pub fn update(&self, change: impl FnOnce(&mut WorkerStatus)) {
        self.status.send_modify(change);
    }
}

/// Starts worker processes for the supervisor. The server uses [`ProcessLauncher`];
/// a test can pass its own launcher to drive the restart rules without processes.
pub trait Launcher: Send + Sync + 'static {
    /// The process that [`Launcher::launch`] starts.
    type Child: WorkerChild;

    /// Starts one worker process.
    ///
    /// # Errors
    ///
    /// Returns an error when the process cannot be started.
    fn launch(&self) -> io::Result<Self::Child>;
}

/// One started worker process, as the supervisor uses it.
pub trait WorkerChild: Send + 'static {
    /// Takes the process's standard output. Every call after the first returns `None`.
    fn take_stdout(&mut self) -> Option<Box<dyn AsyncRead + Send + Unpin>>;

    /// Closes the process's standard input. A supervised worker reads the end of
    /// its standard input as a stop request.
    fn close_stdin(&mut self);

    /// Waits for the process to exit. `Ok(true)` means status 0, `Ok(false)` any
    /// other exit, and an error means that waiting itself failed.
    fn wait(&mut self) -> impl Future<Output = io::Result<bool>> + Send;

    /// Kills the process and waits until it has exited.
    fn kill(&mut self) -> impl Future<Output = io::Result<()>> + Send;
}

/// Starts the server's own executable as `worker run --data-dir <dir> --supervised`.
///
/// The worker's standard input and standard output are pipes to the server, and its
/// standard error is the server's own.
#[derive(Debug)]
pub struct ProcessLauncher {
    data_dir: PathBuf,
}

impl ProcessLauncher {
    /// A launcher for the worker of the data directory `data_dir`.
    #[must_use]
    pub fn new(data_dir: PathBuf) -> Self {
        Self { data_dir }
    }
}

impl Launcher for ProcessLauncher {
    type Child = ProcessChild;

    fn launch(&self) -> io::Result<ProcessChild> {
        // Resolved at every start, so a failure here is a failed start (rule 3).
        let program = env::current_exe()?;
        let mut child = Command::new(program)
            .arg("worker")
            .arg("run")
            .arg("--data-dir")
            .arg(&self.data_dir)
            .arg("--supervised")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;
        let stdin = child.stdin.take();
        Ok(ProcessChild { child, stdin })
    }
}

/// A worker process that [`ProcessLauncher`] started.
#[derive(Debug)]
pub struct ProcessChild {
    child: Child,
    stdin: Option<ChildStdin>,
}

impl WorkerChild for ProcessChild {
    fn take_stdout(&mut self) -> Option<Box<dyn AsyncRead + Send + Unpin>> {
        self.child
            .stdout
            .take()
            .map(|stdout| Box::new(stdout) as Box<dyn AsyncRead + Send + Unpin>)
    }

    fn close_stdin(&mut self) {
        drop(self.stdin.take());
    }

    async fn wait(&mut self) -> io::Result<bool> {
        self.child.wait().await.map(|status| status.success())
    }

    async fn kill(&mut self) -> io::Result<()> {
        self.child.kill().await
    }
}

/// Starts one worker now and supervises it in a task on the Tokio runtime until
/// `stop` is set.
///
/// The first start happens before this returns, so a worker that could start
/// exists when the caller reports the server as ready. After a stop, the task
/// closes the worker's standard input, waits for it for at most `STOP_GRACE`,
/// kills it if it is still running, and ends.
///
/// `draining` is the server's own stop request, which comes before `stop`: the
/// server first stops its listeners and only then asks the worker to stop (rule
/// 2). A worker that exits in between is not started again, because a signal
/// sent to the whole process group (Ctrl+C in a terminal) reaches the worker
/// too, and a new worker would take a job only to give it back.
///
/// # Panics
///
/// Panics when no Tokio runtime is running on the calling thread, because the
/// supervising task is spawned there.
#[must_use]
pub fn start<L: Launcher>(
    launcher: L,
    status: WorkerStatusHandle,
    clock: Arc<dyn Clock>,
    draining: StopLatch,
    stop: StopLatch,
) -> JoinHandle<()> {
    let first = launch(&launcher, &status, clock.as_ref());
    tokio::spawn(supervise(launcher, first, status, clock, draining, stop))
}

/// Starts one worker and records the result: `running` when it started, or a
/// failed start as the last exit. Returns the child when one started.
fn launch<L: Launcher>(
    launcher: &L,
    status: &WorkerStatusHandle,
    clock: &dyn Clock,
) -> Option<L::Child> {
    if let Ok(child) = launcher.launch() {
        status.update(|s| s.state = WorkerState::Running);
        Some(child)
    } else {
        // The error text is not kept: rule 5 reports only that the start failed,
        // and the library prints nothing by itself.
        status.update(|s| {
            s.state = WorkerState::Restarting;
            s.last_exit = Some(WorkerExit {
                at_ms: clock.now().0,
                outcome: WorkerExitOutcome::NotStarted,
            });
        });
        None
    }
}

/// The loop behind [`start`]. It starts the worker again after every exit or failed
/// start that the server did not ask for, and stops the worker when `stop` is set.
async fn supervise<L: Launcher>(
    launcher: L,
    mut current: Option<L::Child>,
    status: WorkerStatusHandle,
    clock: Arc<dyn Clock>,
    draining: StopLatch,
    stop: StopLatch,
) {
    let mut delay = FIRST_DELAY;
    loop {
        if let Some(mut child) = current.take() {
            let started = Instant::now();
            if let Some(stdout) = child.take_stdout() {
                let reader_status = status.clone();
                // Nothing waits for the reader: it ends when the worker's standard
                // output ends.
                drop(tokio::spawn(async move {
                    worker_lines::read_lines(stdout, |line| {
                        // Parsed lines are not used yet (decision 0055, rule 10).
                        if matches!(line, WorkerLine::Dropped) {
                            reader_status
                                .update(|s| s.dropped_lines = s.dropped_lines.saturating_add(1));
                        }
                    })
                    .await;
                }));
            }
            let exit = tokio::select! {
                exit = child.wait() => Some(exit),
                () = stop.wait() => None,
            };
            let Some(exit) = exit else {
                // The server asked for the stop (rule 2), so this exit is neither
                // recorded nor restarted. The state reads `stopped` from here on.
                status.update(|s| s.state = WorkerState::Stopped);
                child.close_stdin();
                if time::timeout(STOP_GRACE, child.wait()).await.is_err() {
                    // A failed kill is not retried here; dropping the child tries again.
                    drop(child.kill().await);
                }
                break;
            };
            if started.elapsed() >= RESET_AFTER {
                delay = FIRST_DELAY;
            }
            let outcome = if matches!(exit, Ok(true)) {
                WorkerExitOutcome::Clean
            } else {
                WorkerExitOutcome::Failed
            };
            status.update(|s| {
                s.state = WorkerState::Restarting;
                s.last_exit = Some(WorkerExit {
                    at_ms: clock.now().0,
                    outcome,
                });
            });
        }
        let stopped = tokio::select! {
            () = time::sleep(delay) => false,
            () = stop.wait() => true,
            // The server is stopping: nothing is started any more.
            () = draining.wait() => true,
        };
        if stopped {
            break;
        }
        delay = (delay * 2).min(MAX_DELAY);
        status.update(|s| s.restarts = s.restarts.saturating_add(1));
        current = launch(&launcher, &status, clock.as_ref());
    }
    status.update(|s| s.state = WorkerState::Stopped);
}
