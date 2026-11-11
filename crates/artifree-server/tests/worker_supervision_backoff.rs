//! The restart rules of the worker supervisor (decision 0055, rules 1, 3 and 5):
//! the delays between starts, the reset after a long run, what the status reports
//! for each exit and failed start, and the clock those reports are stamped with.
//! The launcher and the worker processes are scripted, and time is the runtime's
//! paused clock, so no test waits for real time.

use std::error::Error;
use std::future::{self, Future};
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::time::Duration;

use artifree_contracts::api::{
    WorkerExit, WorkerExitOutcome, WorkerMode, WorkerState, WorkerStatus,
};
use artifree_core::jobs::Clock;
use artifree_domain::identity::UnixMillis;
use artifree_server::shutdown::StopLatch;
use artifree_server::supervisor::{self, Launcher, WorkerChild, WorkerStatusHandle};
use tokio::io::AsyncRead;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;
use tokio::time::{self, Instant};

type TestResult = Result<(), Box<dyn Error>>;

/// The longest wait for the next launch. It is far longer than the 60 second delay that
/// the rules allow, so a launch that never comes fails the test instead of hanging it.
/// A delay that is too long but still arrives reaches the assertion on the delays.
const LAUNCH_WAIT: Duration = Duration::from_secs(600);

/// The clock every test reads. A test sets it to move the time that exits are stamped
/// with.
#[derive(Debug)]
struct ManualClock(AtomicI64);

impl ManualClock {
    fn new(millis: i64) -> Arc<Self> {
        Arc::new(Self(AtomicI64::new(millis)))
    }

    fn set(&self, millis: i64) {
        self.0.store(millis, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> UnixMillis {
        UnixMillis(self.0.load(Ordering::SeqCst))
    }
}

/// How a scripted worker process ends.
#[derive(Debug, Clone, Copy)]
enum Exit {
    /// Its wait succeeds, with `true` for status 0.
    Status(bool),
    /// Its wait itself fails.
    WaitFails,
}

impl Exit {
    fn result(self) -> io::Result<bool> {
        match self {
            Self::Status(success) => Ok(success),
            Self::WaitFails => Err(io::Error::other("waiting for the worker failed")),
        }
    }
}

/// What one launch of the scripted launcher does.
#[derive(Debug, Clone, Copy)]
enum Plan {
    /// The process cannot be started.
    Fail,
    /// The process starts, runs for `run_for`, and then ends with `exit`.
    Run { run_for: Duration, exit: Exit },
    /// The process starts and never ends on its own.
    Hang,
}

/// One launch, as the launcher saw it.
#[derive(Debug, Clone, Copy)]
struct Launch {
    /// The runtime's time when the launch happened.
    at: Instant,
    /// The restart count in the status at that moment.
    restarts: u64,
}

/// A scripted worker process. `end` is `None` for a process that never ends on its own.
#[derive(Debug)]
struct ScriptedChild {
    end: Option<(Duration, Exit)>,
}

impl WorkerChild for ScriptedChild {
    fn take_stdout(&mut self) -> Option<Box<dyn AsyncRead + Send + Unpin>> {
        None
    }

    fn close_stdin(&mut self) {}

    async fn wait(&mut self) -> io::Result<bool> {
        match self.end {
            Some((run_for, exit)) => {
                time::sleep(run_for).await;
                exit.result()
            }
            None => future::pending().await,
        }
    }

    fn kill(&mut self) -> impl Future<Output = io::Result<()>> + Send {
        future::ready(Ok(()))
    }
}

/// Launches follow `plans` in order and then `after`. Each launch is reported to the
/// test, with the restart count that the status shows at that moment.
struct ScriptedLauncher {
    plans: Vec<Plan>,
    after: Plan,
    launched: AtomicUsize,
    status: WorkerStatusHandle,
    reports: UnboundedSender<Launch>,
}

impl ScriptedLauncher {
    fn new(
        plans: Vec<Plan>,
        after: Plan,
        status: WorkerStatusHandle,
    ) -> (Self, UnboundedReceiver<Launch>) {
        let (reports, received) = mpsc::unbounded_channel();
        let launcher = Self {
            plans,
            after,
            launched: AtomicUsize::new(0),
            status,
            reports,
        };
        (launcher, received)
    }
}

impl Launcher for ScriptedLauncher {
    type Child = ScriptedChild;

    fn launch(&self) -> io::Result<ScriptedChild> {
        let index = self.launched.fetch_add(1, Ordering::SeqCst);
        let plan = self.plans.get(index).copied().unwrap_or(self.after);
        let report = Launch {
            at: Instant::now(),
            restarts: self.status.current().restarts,
        };
        // The test reads launches while it runs. A report sent after it stopped
        // reading has no one left to tell, so the send error is not needed.
        let _ = self.reports.send(report);
        match plan {
            Plan::Fail => Err(io::Error::other("the worker cannot start")),
            Plan::Run { run_for, exit } => Ok(ScriptedChild {
                end: Some((run_for, exit)),
            }),
            Plan::Hang => Ok(ScriptedChild { end: None }),
        }
    }
}

/// One supervisor under test: its status, the launches it reports, and the stop that
/// ends it.
struct Supervision {
    status: WorkerStatusHandle,
    launches: UnboundedReceiver<Launch>,
    stop: StopLatch,
    task: JoinHandle<()>,
}

impl Supervision {
    /// Starts a supervisor whose launches follow `plans`, and then `after`.
    fn start(plans: Vec<Plan>, after: Plan, clock: Arc<ManualClock>) -> Self {
        let status = WorkerStatusHandle::supervised();
        let (launcher, received) = ScriptedLauncher::new(plans, after, status.clone());
        let stop = StopLatch::new();
        let task = supervisor::start(
            launcher,
            status.clone(),
            clock,
            StopLatch::new(),
            stop.clone(),
        );
        Self {
            status,
            launches: received,
            stop,
            task,
        }
    }

    /// The next launch. The wait runs on the paused clock and ends after `LAUNCH_WAIT`.
    async fn next_launch(&mut self) -> Result<Launch, Box<dyn Error>> {
        let launch = time::timeout(LAUNCH_WAIT, self.launches.recv()).await?;
        let launch = launch.ok_or("the supervisor stopped launching workers")?;
        Ok(launch)
    }

    /// The next `count` launches, in order.
    async fn next_launches(&mut self, count: usize) -> Result<Vec<Launch>, Box<dyn Error>> {
        let mut launches = Vec::with_capacity(count);
        for _ in 0..count {
            launches.push(self.next_launch().await?);
        }
        Ok(launches)
    }

    /// Stops the supervisor and waits until its task has ended.
    async fn finish(self) -> TestResult {
        self.stop.set();
        self.task.await?;
        Ok(())
    }
}

/// The time of each launch, counted from the first one.
fn offsets(launches: &[Launch]) -> Vec<Duration> {
    let Some(first) = launches.first() else {
        return Vec::new();
    };
    launches
        .iter()
        .map(|launch| launch.at.duration_since(first.at))
        .collect()
}

/// The time from each launch to the launch after it.
fn gaps(launches: &[Launch]) -> Vec<Duration> {
    launches
        .iter()
        .zip(launches.iter().skip(1))
        .map(|(earlier, later)| later.at.duration_since(earlier.at))
        .collect()
}

/// The restart count that each launch saw.
fn restarts_seen(launches: &[Launch]) -> Vec<u64> {
    launches.iter().map(|launch| launch.restarts).collect()
}

/// A worker exits at once. The status shows that exit while the supervisor waits, and
/// the next start follows 1 second after the exit (rule 3). The exit is recorded with
/// its outcome and the clock's time (rule 5).
async fn an_exit_is_recorded_and_restarted_after_one_second(
    exit: Exit,
    outcome: WorkerExitOutcome,
) -> TestResult {
    let mut supervision = Supervision::start(
        Vec::new(),
        Plan::Run {
            run_for: Duration::ZERO,
            exit,
        },
        ManualClock::new(1_234),
    );
    let first = supervision.next_launch().await?;
    // Half a second in, the first worker has exited and the next start is still a
    // second away.
    time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        supervision.status.current(),
        WorkerStatus {
            mode: WorkerMode::Supervised,
            state: WorkerState::Restarting,
            restarts: 0,
            last_exit: Some(WorkerExit {
                at_ms: 1_234,
                outcome,
            }),
            dropped_lines: 0,
        },
        "after the first worker exits, the status must show it as restarting, with that exit"
    );
    let second = supervision.next_launch().await?;
    assert_eq!(
        second.at.duration_since(first.at),
        Duration::from_secs(1),
        "the start after an exit must follow it 1 second later"
    );
    assert_eq!(
        second.restarts, 1,
        "the start after the first exit is the first restart"
    );
    supervision.finish().await
}

/// A worker that exits with status 0 is recorded as clean.
#[tokio::test(start_paused = true)]
async fn a_clean_exit_is_recorded_as_clean_and_restarted_after_one_second() -> TestResult {
    an_exit_is_recorded_and_restarted_after_one_second(Exit::Status(true), WorkerExitOutcome::Clean)
        .await
}

/// A worker that exits with any other status is recorded as failed.
#[tokio::test(start_paused = true)]
async fn a_nonzero_exit_is_recorded_as_failed_and_restarted_after_one_second() -> TestResult {
    an_exit_is_recorded_and_restarted_after_one_second(
        Exit::Status(false),
        WorkerExitOutcome::Failed,
    )
    .await
}

/// A worker whose exit cannot be waited for is recorded as failed.
#[tokio::test(start_paused = true)]
async fn a_failed_wait_is_recorded_as_failed_and_restarted_after_one_second() -> TestResult {
    an_exit_is_recorded_and_restarted_after_one_second(Exit::WaitFails, WorkerExitOutcome::Failed)
        .await
}

/// Every start fails. The supervisor keeps starting the worker 1, 2, 4, 8, 16 and 32
/// seconds apart, then every 60 seconds, and reports each failed start (rules 3 and 5).
#[tokio::test(start_paused = true)]
async fn failed_starts_are_retried_with_delays_that_double_up_to_a_minute() -> TestResult {
    let mut supervision = Supervision::start(Vec::new(), Plan::Fail, ManualClock::new(1_234));
    let launches = supervision.next_launches(11).await?;
    let expected_gaps = [1, 2, 4, 8, 16, 32, 60, 60, 60, 60].map(Duration::from_secs);
    assert_eq!(
        gaps(&launches),
        expected_gaps,
        "the delays between starts must double up to 32 seconds, then stay at 60"
    );
    assert_eq!(
        restarts_seen(&launches),
        (0..=10).collect::<Vec<u64>>(),
        "each start must see the number of starts after the first that came before it"
    );
    assert_eq!(
        supervision.status.current(),
        WorkerStatus {
            mode: WorkerMode::Supervised,
            state: WorkerState::Restarting,
            restarts: 10,
            last_exit: Some(WorkerExit {
                at_ms: 1_234,
                outcome: WorkerExitOutcome::NotStarted,
            }),
            dropped_lines: 0,
        },
        "after a failed start the status must read restarting, with that start as last exit"
    );
    supervision.finish().await
}

/// A failed start is stamped with the clock's time at the moment it fails (rule 5).
#[tokio::test(start_paused = true)]
async fn a_failed_start_is_stamped_with_the_clock_at_the_time_it_fails() -> TestResult {
    let clock = ManualClock::new(1_234);
    let mut supervision = Supervision::start(Vec::new(), Plan::Fail, Arc::clone(&clock));
    supervision.next_launch().await?;
    assert_eq!(
        supervision.status.current().last_exit,
        Some(WorkerExit {
            at_ms: 1_234,
            outcome: WorkerExitOutcome::NotStarted,
        }),
        "the first failed start must be stamped with the clock's time, 1234"
    );
    clock.set(5_678);
    supervision.next_launch().await?;
    assert_eq!(
        supervision.status.current().last_exit,
        Some(WorkerExit {
            at_ms: 5_678,
            outcome: WorkerExitOutcome::NotStarted,
        }),
        "the next failed start must be stamped with the clock's time then, 5678"
    );
    supervision.finish().await
}

/// `start` makes the first launch and sets the status to running before it returns
/// (rule 1).
#[tokio::test(start_paused = true)]
async fn start_makes_the_first_launch_before_it_returns() -> TestResult {
    let mut supervision = Supervision::start(Vec::new(), Plan::Hang, ManualClock::new(1_234));
    // Nothing has awaited since `start` returned, so the first launch is already made.
    assert_eq!(
        supervision.status.current(),
        WorkerStatus {
            mode: WorkerMode::Supervised,
            state: WorkerState::Running,
            restarts: 0,
            last_exit: None,
            dropped_lines: 0,
        },
        "a worker that started must be reported as running before start returns"
    );
    assert_eq!(
        supervision
            .launches
            .try_recv()
            .map(|launch| launch.restarts),
        Ok(0),
        "the first launch must be reported before start returns, with no restart counted"
    );
    assert_eq!(
        supervision
            .launches
            .try_recv()
            .map(|launch| launch.restarts),
        Err(TryRecvError::Empty),
        "start must make exactly one launch"
    );
    supervision.finish().await
}

/// Two failed starts, then a worker that runs for `run_for` and fails, then one more
/// start that fails. Returns the four launches.
async fn launches_around_a_run(run_for: Duration) -> Result<Vec<Launch>, Box<dyn Error>> {
    let plans = vec![
        Plan::Fail,
        Plan::Fail,
        Plan::Run {
            run_for,
            exit: Exit::Status(false),
        },
    ];
    let mut supervision = Supervision::start(plans, Plan::Fail, ManualClock::new(1_234));
    let launches = supervision.next_launches(4).await?;
    supervision.finish().await?;
    Ok(launches)
}

/// A worker that runs for a full minute resets the delay. The start after its exit comes
/// 1 second later, not the 4 seconds that the doubling had reached (rule 3).
#[tokio::test(start_paused = true)]
async fn a_worker_that_runs_for_a_minute_resets_the_delay_to_one_second() -> TestResult {
    let launches = launches_around_a_run(Duration::from_secs(60)).await?;
    assert_eq!(
        offsets(&launches),
        [0, 1, 3, 64].map(Duration::from_secs),
        "the start after a 60 second run must come 1 second after its exit, at 64 seconds"
    );
    Ok(())
}

/// A worker that runs for just under a minute does not reset the delay. The start after
/// its exit waits the 4 seconds that the doubling had reached (rule 3).
#[tokio::test(start_paused = true)]
async fn a_worker_that_runs_for_just_under_a_minute_keeps_the_doubled_delay() -> TestResult {
    let launches = launches_around_a_run(Duration::from_millis(59_999)).await?;
    assert_eq!(
        offsets(&launches),
        [
            Duration::ZERO,
            Duration::from_secs(1),
            Duration::from_secs(3),
            Duration::from_millis(66_999),
        ],
        "the start after a 59.999 second run must come 4 seconds after its exit, at 66.999 seconds"
    );
    Ok(())
}
