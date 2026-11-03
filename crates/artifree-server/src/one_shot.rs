//! The one-shot session: a normal server on a temporary data directory with an
//! explicit lifetime, deleted on every way out (decision 0039).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::runtime::Runtime;
use tokio::sync::watch;

use crate::process_table::ProcessTable;
use crate::serve::{self, OneShot, Ready, ServeOptions, SessionReady};
use crate::session::Session;
use crate::sweep;

/// How long open connections get to finish after a stop. A client that stalls
/// must not keep the data alive past this.
pub(crate) const DRAIN: Duration = Duration::from_secs(5);
/// How long the runtime may take to drop blocking work once serving ended.
const RUNTIME_GRACE: Duration = Duration::from_secs(2);

/// Observes whether the session was asked to stop. Cheap to clone.
#[derive(Debug, Clone)]
pub(crate) struct StopWatch {
    receiver: watch::Receiver<bool>,
}

impl StopWatch {
    /// Whether a stop was requested.
    pub(crate) fn requested(&self) -> bool {
        *self.receiver.borrow()
    }

    /// Completes when a stop is requested. If the senders are gone nobody can
    /// ask any more, which also ends the session.
    pub(crate) async fn wait(&self) {
        let mut receiver = self.receiver.clone();
        receiver.wait_for(|stopped| *stopped).await.ok();
    }
}

/// Starts the signal handlers and the TTL timer that request the stop.
#[derive(Debug)]
struct Stopper {
    watch: StopWatch,
}

impl Stopper {
    /// Registers the signal handlers and starts the TTL timer on `runtime`.
    /// Registration is complete when this returns, so a signal sent after it is
    /// recorded even while the caller is busy with synchronous work.
    fn install(runtime: &Runtime, deadline: Instant) -> Result<Self, String> {
        let (sender, receiver) = watch::channel(false);
        let sender = Arc::new(sender);
        let _context = runtime.enter();
        let signals = Signals::register()?;
        let on_signal = Arc::clone(&sender);
        runtime.spawn(async move {
            signals.wait().await;
            request(&on_signal);
        });
        let on_ttl = Arc::clone(&sender);
        runtime.spawn(async move {
            tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
            request(&on_ttl);
        });
        Ok(Self {
            watch: StopWatch { receiver },
        })
    }

    /// Fails with [`Halt::Stopped`] when a stop was requested. Called between
    /// the synchronous start-up steps, which cannot be interrupted.
    fn checkpoint(&self, stage: Stage) -> Result<(), Halt> {
        hold(stage, &self.watch);
        if self.watch.requested() {
            Err(Halt::Stopped)
        } else {
            Ok(())
        }
    }
}

/// Asks for the stop. The tasks that call this hold the sender, so it lives
/// until both have run.
fn request(sender: &watch::Sender<bool>) {
    sender.send_replace(true);
}

#[cfg(unix)]
struct Signals {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
    hangup: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl Signals {
    fn register() -> Result<Self, String> {
        use tokio::signal::unix::{SignalKind, signal};
        let handler = |kind, name| {
            signal(kind).map_err(|error| format!("cannot handle {name} for the session: {error}"))
        };
        Ok(Self {
            interrupt: handler(SignalKind::interrupt(), "SIGINT")?,
            terminate: handler(SignalKind::terminate(), "SIGTERM")?,
            // Closing the terminal sends SIGHUP; it ends the session like SIGTERM.
            hangup: handler(SignalKind::hangup(), "SIGHUP")?,
        })
    }

    /// Completes at the first signal, or if a stream ends (a stop either way).
    async fn wait(mut self) {
        tokio::select! {
            _ = self.interrupt.recv() => {}
            _ = self.terminate.recv() => {}
            _ = self.hangup.recv() => {}
        }
    }
}

#[cfg(not(unix))]
struct Signals {
    ctrl_c: tokio::signal::windows::CtrlC,
}

#[cfg(not(unix))]
impl Signals {
    fn register() -> Result<Self, String> {
        tokio::signal::windows::ctrl_c()
            .map(|ctrl_c| Self { ctrl_c })
            .map_err(|error| format!("cannot handle Ctrl+C for the session: {error}"))
    }

    async fn wait(mut self) {
        self.ctrl_c.recv().await;
    }
}

/// The synchronous start-up steps at which a stop is looked for.
#[derive(Debug, Clone, Copy)]
enum Stage {
    BeforeDirectory,
    AfterDirectory,
    AfterSweep,
    AfterDatabase,
}

#[cfg(debug_assertions)]
impl Stage {
    fn name(self) -> &'static str {
        match self {
            Self::BeforeDirectory => "before-directory",
            Self::AfterDirectory => "after-directory",
            Self::AfterSweep => "after-sweep",
            Self::AfterDatabase => "after-database",
        }
    }
}

/// Test seam, present in debug builds only (decision 0039, item 10): with
/// `ARTIFREE_TEST_HOLD_STARTUP=<stage>=<file>` the server creates `<file>` on
/// reaching `<stage>` and waits there until a stop is requested, so a test can
/// send a signal at a known point of start-up instead of racing it.
#[cfg(debug_assertions)]
fn hold(stage: Stage, watch: &StopWatch) {
    const LIMIT: Duration = Duration::from_secs(60);
    let Some(spec) = std::env::var_os("ARTIFREE_TEST_HOLD_STARTUP") else {
        return;
    };
    let Some((name, file)) = spec.to_str().and_then(|spec| spec.split_once('=')) else {
        return;
    };
    if name != stage.name() || std::fs::write(file, b"held").is_err() {
        return;
    }
    let give_up = Instant::now() + LIMIT;
    while !watch.requested() && Instant::now() < give_up {
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Release builds contain no test seam.
#[cfg(not(debug_assertions))]
fn hold(_stage: Stage, _watch: &StopWatch) {}

/// How start-up ended without serving.
#[derive(Debug)]
enum Halt {
    /// A signal or the TTL arrived first; a clean end.
    Stopped,
    /// Something failed.
    Failed(String),
}

impl From<String> for Halt {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

/// Runs a one-shot session. The signal handlers exist before the session
/// directory does, and the directory is removed on every path out: the
/// runtime, with the connections and database handles it owns, is shut down
/// first, then [`Session::finish`] removes the directory and its failure is
/// reported. If this function unwinds, `Session`'s `Drop` removes it after the
/// runtime's, because `session` is declared before `runtime`.
pub(crate) fn run(
    options: ServeOptions,
    one_shot: &OneShot,
    on_ready: impl FnOnce(&Ready) -> Result<(), String>,
) -> Result<(), String> {
    let started = Instant::now();
    let deadline = started
        .checked_add(one_shot.ttl)
        .ok_or("the session lifetime is too long")?;
    let root = session_root(one_shot.session_root.as_deref())?;
    let mut session = Session::new();
    let runtime = serve::build_runtime()?;
    let stopper = Stopper::install(&runtime, deadline)?;
    let outcome = start(
        &runtime,
        &mut session,
        &root,
        options,
        one_shot.ttl,
        &stopper,
        on_ready,
    );
    let dir = session.path().map(Path::to_path_buf);
    // Connections and the database go first; the directory is deleted after.
    runtime.shutdown_timeout(RUNTIME_GRACE);
    let removal = session.finish();
    let removal = removal.map_err(|error| match dir {
        Some(dir) => format!(
            "could not remove the session directory {}: {error}",
            dir.display()
        ),
        None => format!("could not remove the session directory: {error}"),
    });
    match (outcome, removal) {
        (Ok(()) | Err(Halt::Stopped), Ok(())) => Ok(()),
        (Err(Halt::Failed(message)), Ok(())) => Err(message),
        (Ok(()) | Err(Halt::Stopped), Err(removal)) => Err(removal),
        (Err(Halt::Failed(message)), Err(removal)) => Err(format!("{message}; also {removal}")),
    }
}

/// The directory in which the session directory is created, absolute and
/// valid UTF-8 (the readiness line is JSON), and already existing.
fn session_root(given: Option<&Path>) -> Result<PathBuf, String> {
    let chosen = given.map_or_else(std::env::temp_dir, Path::to_path_buf);
    let root = std::path::absolute(&chosen).map_err(|error| {
        format!(
            "cannot resolve the session root {}: {error}",
            chosen.display()
        )
    })?;
    if root.to_str().is_none() {
        return Err(format!(
            "the session root {} is not valid UTF-8, so the readiness line cannot name it; set TMPDIR or --session-root to a UTF-8 path",
            root.display()
        ));
    }
    match std::fs::metadata(&root) {
        Ok(metadata) if metadata.is_dir() => Ok(root),
        Ok(_) => Err(format!(
            "the session root {} is not a directory",
            root.display()
        )),
        Err(error) => Err(format!(
            "cannot use the session root {}: {error}",
            root.display()
        )),
    }
}

fn start(
    runtime: &Runtime,
    session: &mut Session,
    root: &Path,
    options: ServeOptions,
    ttl: Duration,
    stopper: &Stopper,
    on_ready: impl FnOnce(&Ready) -> Result<(), String>,
) -> Result<(), Halt> {
    let table = ProcessTable::system();
    stopper.checkpoint(Stage::BeforeDirectory)?;
    let dir = session.begin(root, &table)?.to_path_buf();
    stopper.checkpoint(Stage::AfterDirectory)?;
    let stopping = || stopper.watch.requested();
    let swept = sweep::sweep(root, &dir, &table, &stopping);
    stopper.checkpoint(Stage::AfterSweep)?;
    let opened = serve::open_data(&dir, &stopping)?.ok_or(Halt::Stopped)?;
    stopper.checkpoint(Stage::AfterDatabase)?;
    runtime.block_on(serve::serve_listeners(
        options,
        opened,
        Some(SessionReady { ttl, sweep: swept }),
        serve::Shutdown::Session(stopper.watch.clone()),
        on_ready,
    ))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::changes;
    use crate::limits::RequestLimits;
    use std::error::Error;
    use std::net::TcpListener;

    /// Options for listeners on free loopback ports, except the content
    /// listener, which is given.
    fn options(content: std::net::SocketAddr) -> ServeOptions {
        ServeOptions {
            data: serve::DataLocation::OneShot(OneShot {
                ttl: Duration::from_secs(60),
                session_root: None,
            }),
            web_dir: None,
            api_address: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
            loopback_api_address: None,
            content_address: content,
            content_origin: None,
            frame_ancestors: "http://127.0.0.1:*".to_owned(),
            preview_ttl: Duration::from_secs(60),
            limits: RequestLimits::default(),
            change_streams: changes::DEFAULT_STREAMS_PER_ACTOR,
            api_origin: None,
            trusted_proxies: Vec::new(),
            allowed_hosts: Vec::new(),
            allowed_origins: Vec::new(),
            allow_remote_mcp: false,
        }
    }

    /// A stop that is already latched ends start-up after the first listener
    /// is bound: the second is never bound (its port is taken, so binding it
    /// would be an error), nothing is built, and readiness is not reported.
    #[test]
    fn a_latched_stop_ends_listener_start_up_before_the_next_step() -> Result<(), Box<dyn Error>> {
        let taken = TcpListener::bind("127.0.0.1:0")?;
        let dir = std::env::temp_dir().join(format!("artifree-latched-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        artifree_providers::data_dir::create_private_dir(&dir)?;
        let opened = serve::open_data(&dir, &|| false)?.ok_or("opened")?;
        let (sender, receiver) = watch::channel(true);
        let runtime = serve::build_runtime()?;
        let result = runtime.block_on(serve::serve_listeners(
            options(taken.local_addr()?),
            opened,
            None,
            serve::Shutdown::Session(StopWatch { receiver }),
            |_| Err("readiness must not be reported after a latched stop".to_owned()),
        ));
        drop(sender);
        runtime.shutdown_timeout(Duration::from_secs(1));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(result, Ok(()));
        Ok(())
    }
}
