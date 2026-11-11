//! Supervision of the server's worker when the server stops (decision 0055, rules 2,
//! 3 and 6). The worker is a fake and time is the runtime's paused clock, so no test
//! starts a process or sleeps.

use std::future::{self, Future};
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use artifree_contracts::api::{
    WorkerExit, WorkerExitOutcome, WorkerMode, WorkerState, WorkerStatus,
};
use artifree_core::jobs::Clock;
use artifree_domain::identity::UnixMillis;
use artifree_server::shutdown::StopLatch;
use artifree_server::supervisor::{self, Launcher, WorkerChild, WorkerStatusHandle};
use tokio::io::{AsyncRead, ReadBuf};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// How many times a wait yields to the runtime before it gives up.
const YIELD_LIMIT: usize = 10_000;

/// How long joining a supervisor task may take, in paused time.
const JOIN_LIMIT: Duration = Duration::from_secs(120);

/// Decision 0055, rule 3: the first start after an exit comes one second later.
const FIRST_RESTART_DELAY: Duration = Duration::from_secs(1);

/// Decision 0055, rule 2: a worker has five seconds to exit after its stdin closes.
const STOP_GRACE: Duration = Duration::from_secs(5);

/// How a fake worker behaves once the supervisor starts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Behavior {
    /// Exits with status 0 once its standard input closes, as a worker does on a stop.
    ExitsWhenStdinCloses,
    /// Ignores its standard input and runs until it is killed.
    IgnoresStdinClose,
    /// Exits with a failure status as soon as it is started.
    FailsAtOnce,
}

/// What the fake workers did, shared by the launcher, the workers and the test.
#[derive(Debug, Default)]
struct Counts {
    starts: AtomicUsize,
    stdin_closes: AtomicUsize,
    kills: AtomicUsize,
    stdout_ended: AtomicBool,
}

impl Counts {
    fn starts(&self) -> usize {
        self.starts.load(Ordering::SeqCst)
    }

    fn stdin_closes(&self) -> usize {
        self.stdin_closes.load(Ordering::SeqCst)
    }

    fn kills(&self) -> usize {
        self.kills.load(Ordering::SeqCst)
    }

    fn stdout_ended(&self) -> bool {
        self.stdout_ended.load(Ordering::SeqCst)
    }
}

/// A clock the test sets, so that a time recorded earlier differs from a later one.
#[derive(Debug, Default)]
struct FakeClock(AtomicI64);

impl FakeClock {
    fn at(ms: i64) -> Self {
        Self(AtomicI64::new(ms))
    }

    fn set(&self, ms: i64) {
        self.0.store(ms, Ordering::SeqCst);
    }
}

impl Clock for FakeClock {
    fn now(&self) -> UnixMillis {
        UnixMillis(self.0.load(Ordering::SeqCst))
    }
}

/// What a fake worker process has been told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Signal {
    Running,
    StdinClosed,
    Killed,
}

/// Starts fake workers. The nth start behaves as `behaviors[n]` says, and every start
/// after the list as `fallback` says.
#[derive(Debug)]
struct FakeLauncher {
    counts: Arc<Counts>,
    behaviors: Vec<Behavior>,
    fallback: Behavior,
    stdout: Vec<u8>,
}

impl Launcher for FakeLauncher {
    type Child = FakeChild;

    fn launch(&self) -> io::Result<FakeChild> {
        let index = self.counts.starts.fetch_add(1, Ordering::SeqCst);
        let behavior = self.behaviors.get(index).copied().unwrap_or(self.fallback);
        let (state, _) = watch::channel(Signal::Running);
        Ok(FakeChild {
            behavior,
            state,
            stdout: Some(self.stdout.clone()),
            counts: Arc::clone(&self.counts),
        })
    }
}

/// One fake worker process, as the supervisor sees it.
#[derive(Debug)]
struct FakeChild {
    behavior: Behavior,
    state: watch::Sender<Signal>,
    stdout: Option<Vec<u8>>,
    counts: Arc<Counts>,
}

impl WorkerChild for FakeChild {
    fn take_stdout(&mut self) -> Option<Box<dyn AsyncRead + Send + Unpin>> {
        let bytes = self.stdout.take()?;
        let output: Box<dyn AsyncRead + Send + Unpin> = Box::new(Output {
            bytes: io::Cursor::new(bytes),
            counts: Arc::clone(&self.counts),
        });
        Some(output)
    }

    fn close_stdin(&mut self) {
        self.counts.stdin_closes.fetch_add(1, Ordering::SeqCst);
        self.state.send_modify(|signal| {
            if *signal == Signal::Running {
                *signal = Signal::StdinClosed;
            }
        });
    }

    async fn wait(&mut self) -> io::Result<bool> {
        if self.behavior == Behavior::FailsAtOnce {
            return Ok(false);
        }
        // A worker that exits on a closed stdin ends at `StdinClosed`; any worker ends
        // once it is killed.
        let ends_on_close = self.behavior == Behavior::ExitsWhenStdinCloses;
        let mut signals = self.state.subscribe();
        let signal = *signals
            .wait_for(|signal| match signal {
                Signal::Running => false,
                Signal::StdinClosed => ends_on_close,
                Signal::Killed => true,
            })
            .await
            .map_err(io::Error::other)?;
        Ok(signal == Signal::StdinClosed)
    }

    fn kill(&mut self) -> impl Future<Output = io::Result<()>> + Send {
        self.counts.kills.fetch_add(1, Ordering::SeqCst);
        self.state.send_modify(|signal| *signal = Signal::Killed);
        future::ready(Ok(()))
    }
}

/// A fake worker's standard output. It records when a reader reaches its end, which
/// happens only after the reader has taken every line before it.
#[derive(Debug)]
struct Output {
    bytes: io::Cursor<Vec<u8>>,
    counts: Arc<Counts>,
}

impl AsyncRead for Output {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let polled = Pin::new(&mut this.bytes).poll_read(cx, buf);
        if matches!(polled, Poll::Ready(Ok(()))) && buf.filled().len() == before {
            this.counts.stdout_ended.store(true, Ordering::SeqCst);
        }
        polled
    }
}

/// A supervised server's worker, started the way `serve` starts it, with the handles a
/// test needs.
struct Fixture {
    status: WorkerStatusHandle,
    counts: Arc<Counts>,
    clock: Arc<FakeClock>,
    draining: StopLatch,
    stop: StopLatch,
    task: JoinHandle<()>,
}

fn supervise(
    behaviors: Vec<Behavior>,
    fallback: Behavior,
    stdout: Vec<u8>,
    clock: Arc<FakeClock>,
) -> Fixture {
    let counts = Arc::new(Counts::default());
    let status = WorkerStatusHandle::supervised();
    let draining = StopLatch::new();
    let stop = StopLatch::new();
    let launcher = FakeLauncher {
        counts: Arc::clone(&counts),
        behaviors,
        fallback,
        stdout,
    };
    let task = supervisor::start(
        launcher,
        status.clone(),
        clock.clone(),
        draining.clone(),
        stop.clone(),
    );
    Fixture {
        status,
        counts,
        clock,
        draining,
        stop,
        task,
    }
}

/// The status of a supervised server with the given parts.
fn supervised_status(
    state: WorkerState,
    restarts: u64,
    last_exit: Option<WorkerExit>,
    dropped_lines: u64,
) -> WorkerStatus {
    WorkerStatus {
        mode: WorkerMode::Supervised,
        state,
        restarts,
        last_exit,
        dropped_lines,
    }
}

/// An exit that the server recorded at `at_ms` with a failure status.
fn failed_exit(at_ms: i64) -> WorkerExit {
    WorkerExit {
        at_ms,
        outcome: WorkerExitOutcome::Failed,
    }
}

/// A line of exactly `total` bytes that is valid JSON, so that only its length can make
/// the reader drop it.
fn oversized_json_line(total: usize) -> Vec<u8> {
    let prefix: &[u8] = br#"{"pad":""#;
    let suffix: &[u8] = br#""}"#;
    let padding = vec![b'x'; total - prefix.len() - suffix.len()];
    [prefix, padding.as_slice(), suffix].concat()
}

/// Yields to the runtime until `ready` holds, for at most `YIELD_LIMIT` yields. Yielding
/// does not move the paused clock, so a wait here never depends on time passing.
async fn yield_until(mut ready: impl FnMut() -> bool) -> bool {
    for _ in 0..YIELD_LIMIT {
        if ready() {
            return true;
        }
        tokio::task::yield_now().await;
    }
    ready()
}

/// Yields to the runtime `times` times, so that every task that can run now does.
async fn yield_times(times: usize) {
    for _ in 0..times {
        tokio::task::yield_now().await;
    }
}

/// Waits for a supervisor task, and fails rather than waiting forever.
async fn join(task: JoinHandle<()>) -> TestResult {
    tokio::time::timeout(JOIN_LIMIT, task).await??;
    Ok(())
}

/// Decision 0055, rules 2 and 3: a stop closes the worker's stdin once. A worker that
/// exits after that is not killed, is not started again, and its exit is not recorded.
#[tokio::test(start_paused = true)]
async fn stop_closes_stdin_and_does_not_kill_a_worker_that_exits() -> TestResult {
    // The first worker exits with a failure at once. The exit is recorded with the
    // clock's time of 5000 ms, and the next start waits one second.
    let clock = Arc::new(FakeClock::at(5_000));
    let fixture = supervise(
        vec![Behavior::FailsAtOnce, Behavior::ExitsWhenStdinCloses],
        Behavior::ExitsWhenStdinCloses,
        Vec::new(),
        clock,
    );
    assert!(
        yield_until(|| fixture.status.current().last_exit.is_some()).await,
        "the failed start was not recorded"
    );
    time::advance(FIRST_RESTART_DELAY).await;
    assert!(
        yield_until(|| fixture.counts.starts() == 2).await,
        "the second worker did not start one second after the failed start"
    );
    assert_eq!(
        fixture.status.current(),
        supervised_status(WorkerState::Running, 1, Some(failed_exit(5_000)), 0),
        "before the stop, the restarted worker runs and the failed start is recorded"
    );

    // The clock has moved on since the failed start. The stop must not record its own
    // exit with the new time.
    fixture.clock.set(9_000);
    fixture.stop.set();
    assert!(
        yield_until(|| fixture.status.current().state == WorkerState::Stopped).await,
        "the stop was not observed"
    );
    assert_eq!(
        fixture.counts.stdin_closes(),
        1,
        "a stop closes the worker's stdin once"
    );
    join(fixture.task).await?;
    assert_eq!(
        fixture.counts.kills(),
        0,
        "a worker that exits after its stdin closes is not killed"
    );

    time::advance(Duration::from_secs(600)).await;
    yield_times(100).await;
    assert_eq!(
        fixture.counts.starts(),
        2,
        "a stopped worker is not started again, even ten minutes later"
    );
    assert_eq!(
        fixture.status.current(),
        supervised_status(WorkerState::Stopped, 1, Some(failed_exit(5_000)), 0),
        "the exit caused by the stop is neither recorded nor counted as a restart"
    );
    Ok(())
}

/// Decision 0055, rule 2: a worker that ignores its stdin is killed exactly five seconds
/// after the stop, not earlier, and the stop then ends.
#[tokio::test(start_paused = true)]
async fn stop_kills_a_worker_that_ignores_stdin_after_exactly_five_seconds() -> TestResult {
    let fixture = supervise(
        Vec::new(),
        Behavior::IgnoresStdinClose,
        Vec::new(),
        Arc::new(FakeClock::default()),
    );
    fixture.stop.set();
    assert!(
        yield_until(|| fixture.status.current().state == WorkerState::Stopped).await,
        "the stop was not observed"
    );
    assert_eq!(
        fixture.counts.stdin_closes(),
        1,
        "a stop closes the worker's stdin before it waits for the worker"
    );
    assert_eq!(
        fixture.counts.kills(),
        0,
        "nothing is killed when the stop begins"
    );

    // One millisecond short of the grace period: the worker must still be alive.
    let one_millisecond = Duration::from_millis(1);
    time::advance(STOP_GRACE.saturating_sub(one_millisecond)).await;
    yield_times(100).await;
    assert_eq!(
        fixture.counts.kills(),
        0,
        "the worker was killed before five seconds had passed"
    );
    assert!(
        !fixture.task.is_finished(),
        "the supervisor ended before the grace period was over"
    );

    time::advance(one_millisecond).await;
    yield_until(|| fixture.counts.kills() == 1).await;
    assert_eq!(
        fixture.counts.kills(),
        1,
        "the worker is killed once, at exactly five seconds"
    );
    join(fixture.task).await?;
    assert_eq!(
        fixture.status.current(),
        supervised_status(WorkerState::Stopped, 0, None, 0),
        "the killed worker's exit is not recorded and nothing restarts"
    );
    assert_eq!(
        fixture.counts.starts(),
        1,
        "the stopped worker is not started again"
    );
    Ok(())
}

/// Decision 0055, rule 3: a stop during the delay before a start starts no worker, even
/// after the delay would have passed, and the state reads `stopped`.
#[tokio::test(start_paused = true)]
async fn stop_during_a_restart_delay_starts_no_further_worker() -> TestResult {
    let clock = Arc::new(FakeClock::at(5_000));
    let fixture = supervise(Vec::new(), Behavior::FailsAtOnce, Vec::new(), clock);
    assert!(
        yield_until(|| fixture.status.current().state == WorkerState::Restarting).await,
        "the failed worker was not waiting to be started again"
    );
    fixture.stop.set();
    assert!(
        yield_until(|| fixture.status.current().state == WorkerState::Stopped).await,
        "the stop during the restart delay was not observed"
    );

    // Ten minutes pass with the stop in force, so the delay's timer would have fired.
    time::advance(Duration::from_secs(600)).await;
    yield_times(100).await;
    assert_eq!(
        fixture.counts.starts(),
        1,
        "no worker starts after a stop, however long the delay"
    );
    join(fixture.task).await?;
    assert_eq!(
        fixture.status.current(),
        supervised_status(WorkerState::Stopped, 0, Some(failed_exit(5_000)), 0),
        "the failed start stays the last exit, and the stop is not a restart"
    );
    Ok(())
}

/// Decision 0055, rule 2: once the server has begun to stop, a worker that exits before
/// the server asks it to (a signal to the whole process group reaches it first) is not
/// started again, although the pipe is not closed yet.
#[tokio::test(start_paused = true)]
async fn an_exit_while_the_server_drains_starts_no_further_worker() -> TestResult {
    let clock = Arc::new(FakeClock::at(5_000));
    let fixture = supervise(Vec::new(), Behavior::FailsAtOnce, Vec::new(), clock);
    fixture.draining.set();
    assert!(
        yield_until(|| fixture.status.current().state == WorkerState::Stopped).await,
        "the supervisor did not end after the exit during the drain"
    );
    time::advance(Duration::from_secs(600)).await;
    yield_times(100).await;
    assert_eq!(
        fixture.counts.starts(),
        1,
        "a worker started during the drain"
    );
    join(fixture.task).await?;
    assert_eq!(
        fixture.status.current(),
        supervised_status(WorkerState::Stopped, 0, Some(failed_exit(5_000)), 0),
    );
    Ok(())
}

/// Decision 0055, rule 6: a stdout line that does not parse and a line longer than 64 KiB
/// are each counted once as dropped. A line that parses is not counted, and the worker
/// keeps running.
#[tokio::test(start_paused = true)]
async fn stdout_lines_that_do_not_parse_or_are_too_long_are_counted_as_dropped() -> TestResult {
    let mut stdout = Vec::new();
    stdout.extend_from_slice(b"not json\n");
    stdout.extend_from_slice(b"{\"event\":\"ready\"}\n");
    stdout.extend_from_slice(&oversized_json_line(70_000));
    stdout.push(b'\n');
    let fixture = supervise(
        Vec::new(),
        Behavior::ExitsWhenStdinCloses,
        stdout,
        Arc::new(FakeClock::default()),
    );
    assert!(
        yield_until(|| fixture.counts.stdout_ended()).await,
        "the reader did not reach the end of the worker's standard output"
    );
    assert_eq!(
        fixture.status.current(),
        supervised_status(WorkerState::Running, 0, None, 2),
        "the unparseable line and the 70,000-byte line are dropped; the ready line is not"
    );

    fixture.stop.set();
    assert!(
        yield_until(|| fixture.status.current().state == WorkerState::Stopped).await,
        "the stop was not observed"
    );
    join(fixture.task).await?;
    Ok(())
}
