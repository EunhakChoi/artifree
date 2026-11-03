//! One-shot session mode (decision 0039): the normal server on a new private
//! temporary data directory that is removed when the session ends.
//!
//! Startup is one ordered list of [`Stage`]s run by `run_startup`. It asks
//! whether a stop was requested before every stage, so a stop at any point
//! skips the rest; nothing else starts a stage. Stages are synchronous, so no
//! await separates a stage from the next check.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use artifree_providers::data_dir;
use artifree_providers::storage::{DATABASE_FILE, SqliteStore};
use artifree_providers::tokens::SqliteTokenStore;
use tokio::net::TcpListener;
use tokio::runtime::Runtime;
use tokio::sync::watch;

use crate::auth::AccessToken;
use crate::http::{self, AppState, DatabaseUsers};
use crate::preview;
use crate::serve::{self, Ready, ServeOptions};
use crate::session_sweep::{self, MARKER_FILE, SweepReport};
use crate::web::WebAssets;

/// Name prefix of a session directory.
pub const SESSION_PREFIX: &str = "artifree-session-";
/// Longest accepted session lifetime, in seconds: seven days.
pub const MAX_TTL_SECONDS: u64 = 604_800;
/// How long open connections get to finish after a stop request.
pub const DRAIN: Duration = Duration::from_secs(5);
/// How long the directory waits, after the runtime is stopped, for blocking
/// database tasks that are still running. A task ends at the latest when
/// `SQLite`'s busy timeout (10 s for the artifact store) does.
pub const BLOCKING_WAIT: Duration = Duration::from_secs(20);

/// The signals that end a session, as shown to a person.
#[cfg(unix)]
pub const STOP_SIGNALS: &str = "SIGINT, SIGTERM, SIGHUP";
/// The signals that end a session, as shown to a person.
#[cfg(not(unix))]
pub const STOP_SIGNALS: &str = "Ctrl+C";

/// Parses `--one-shot-ttl-seconds`: ASCII digits only, 1 to
/// [`MAX_TTL_SECONDS`].
///
/// # Errors
///
/// A message naming the rule, never the value.
pub fn parse_ttl(value: &str) -> Result<Duration, String> {
    let rule = || {
        format!(
            "--one-shot-ttl-seconds must be a whole number of digits from 1 to {MAX_TTL_SECONDS}."
        )
    };
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(rule());
    }
    match value.parse::<u64>() {
        Ok(seconds @ 1..=MAX_TTL_SECONDS) => Ok(Duration::from_secs(seconds)),
        _ => Err(rule()),
    }
}

/// Options of a one-shot session.
#[derive(Debug, Clone)]
pub struct OneShotOptions {
    /// The server options. `data_dir` is ignored: the session directory
    /// replaces it.
    pub serve: ServeOptions,
    /// Parent of the session directory; the system temporary directory when
    /// `None`.
    pub session_root: Option<PathBuf>,
    /// Session lifetime, counted from the process start.
    pub ttl: Duration,
}

/// A request to end the session. Set by a signal, by the TTL timer, or by a
/// test; once set it stays set.
#[derive(Debug, Clone)]
pub struct StopLatch {
    flag: Arc<watch::Sender<bool>>,
}

impl Default for StopLatch {
    fn default() -> Self {
        Self::new()
    }
}

impl StopLatch {
    /// A latch that is not set.
    #[must_use]
    pub fn new() -> Self {
        Self {
            flag: Arc::new(watch::channel(false).0),
        }
    }

    /// Requests a stop.
    pub fn set(&self) {
        self.flag.send_replace(true);
    }

    /// Whether a stop was requested.
    #[must_use]
    pub fn is_set(&self) -> bool {
        *self.flag.borrow()
    }

    /// Completes once a stop was requested.
    pub async fn wait(&self) {
        let mut receiver = self.flag.subscribe();
        // The sender lives in this latch, so the channel cannot close.
        drop(receiver.wait_for(|set| *set).await);
    }
}

/// Makes signals and the lifetime set `stop`. Must run before the session
/// directory exists, so that an early signal is honored at the first
/// checkpoint.
fn install_stop_sources(
    runtime: &Runtime,
    stop: &StopLatch,
    deadline: Instant,
) -> Result<(), String> {
    let _entered = runtime.enter();
    let cannot = |error: io::Error| format!("cannot listen for {STOP_SIGNALS}: {error}");
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        for kind in [
            SignalKind::interrupt(),
            SignalKind::terminate(),
            SignalKind::hangup(),
        ] {
            let mut stream = signal(kind).map_err(cannot)?;
            let stop = stop.clone();
            runtime.spawn(async move {
                stream.recv().await;
                stop.set();
            });
        }
    }
    #[cfg(not(unix))]
    {
        let mut ctrl_c = tokio::signal::windows::ctrl_c().map_err(cannot)?;
        let stop = stop.clone();
        runtime.spawn(async move {
            ctrl_c.recv().await;
            stop.set();
        });
    }
    let stop = stop.clone();
    runtime.spawn(async move {
        tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
        stop.set();
    });
    Ok(())
}

/// A named step of startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Create the private directory and its marker.
    CreateDir,
    /// Remove the directories of dead sessions.
    Sweep,
    /// Open the database.
    OpenDatabase,
    /// Open the token store.
    OpenTokenStore,
    /// Create the owner token file.
    WriteToken,
    /// Bind the API listener.
    BindApi,
    /// Bind the preview content listener.
    BindContent,
    /// Bind the dedicated loopback listener (only when one is configured).
    BindLoopback,
    /// Build the application state.
    BuildState,
    /// Report readiness.
    Readiness,
}

impl Stage {
    /// Every stage, in order.
    pub const ALL: [Self; 10] = [
        Self::CreateDir,
        Self::Sweep,
        Self::OpenDatabase,
        Self::OpenTokenStore,
        Self::WriteToken,
        Self::BindApi,
        Self::BindContent,
        Self::BindLoopback,
        Self::BuildState,
        Self::Readiness,
    ];

    /// The stage's name, as shown by the test seam.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::CreateDir => "create-dir",
            Self::Sweep => "sweep",
            Self::OpenDatabase => "open-database",
            Self::OpenTokenStore => "open-token-store",
            Self::WriteToken => "write-token",
            Self::BindApi => "bind-api",
            Self::BindContent => "bind-content",
            Self::BindLoopback => "bind-loopback",
            Self::BuildState => "build-state",
            Self::Readiness => "readiness",
        }
    }
}

/// The stages to run: all of them, without the loopback bind when there is no
/// loopback listener.
fn plan(with_loopback: bool) -> Vec<Stage> {
    Stage::ALL
        .into_iter()
        .filter(|stage| with_loopback || *stage != Stage::BindLoopback)
        .collect()
}

/// How startup ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupEnd {
    /// Every stage ran.
    Complete,
    /// A stop was requested before this stage; it and all later ones did not run.
    Stopped(Stage),
}

/// Something that can run one stage.
trait Stages {
    fn run(&mut self, stage: Stage) -> Result<(), String>;
}

/// Runs `plan` in order. This is the only caller of [`Stages::run`]: a stop
/// request is checked before every stage, and a failed stage ends the run.
fn run_startup(
    plan: &[Stage],
    stop: &StopLatch,
    stages: &mut impl Stages,
) -> Result<StartupEnd, String> {
    for &stage in plan {
        hold_for_test(stage.name(), stop);
        if stop.is_set() {
            return Ok(StartupEnd::Stopped(stage));
        }
        stages.run(stage)?;
    }
    Ok(StartupEnd::Complete)
}

/// Test seam, compiled only with debug assertions: when
/// `ARTIFREE_TEST_HOLD_AT` names `point`, the process waits there, at most a
/// minute, until a stop is requested. Release builds have no such code.
#[cfg(debug_assertions)]
pub(crate) fn hold_for_test(point: &str, stop: &StopLatch) {
    const VARIABLE: &str = "ARTIFREE_TEST_HOLD_AT";
    if std::env::var_os(VARIABLE).is_none_or(|wanted| wanted != point) {
        return;
    }
    writeln!(io::stderr(), "one-shot test seam: holding before {point}").ok();
    let give_up = Instant::now() + Duration::from_secs(60);
    while !stop.is_set() && Instant::now() < give_up {
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Release builds have no test seam.
#[cfg(not(debug_assertions))]
pub(crate) fn hold_for_test(_point: &str, _stop: &StopLatch) {}

/// Owns the session directory until it is removed. Declared before the runtime
/// so that it is dropped after it, also when a panic unwinds.
#[derive(Debug, Default)]
struct DirGuard {
    path: Option<PathBuf>,
    users: Option<DatabaseUsers>,
}

impl DirGuard {
    fn adopt(&mut self, path: PathBuf) {
        self.path = Some(path);
    }

    /// Records who can still use the database, so that removal waits for them.
    fn watch(&mut self, users: DatabaseUsers) {
        self.users = Some(users);
    }

    /// Removes the directory now, after the database is no longer in use.
    fn remove(&mut self) -> Result<(), String> {
        self.remove_within(BLOCKING_WAIT, || {})
    }

    /// Waits at most `wait` until no blocking database task can still be
    /// running, then removes the directory. A task still running at the bound
    /// is an error, whether or not the removal then succeeds. A failed removal
    /// keeps the path, so that the guard's drop tries once more.
    ///
    /// `while_waiting` runs on every poll while a holder remains (only a test
    /// passes anything).
    fn remove_within(&mut self, wait: Duration, while_waiting: impl FnMut()) -> Result<(), String> {
        let Some(path) = self.path.take() else {
            return Ok(());
        };
        let still_running = self
            .users
            .take()
            .map_or(0, |users| wait_until_idle(&users, wait, while_waiting));
        let removal = match fs::remove_dir_all(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => {
                let message = format!(
                    "could not remove the session directory {}: {error}",
                    path.display()
                );
                self.path = Some(path);
                Err(message)
            }
        };
        match (still_running, removal) {
            (0, removal) => removal,
            (running, Ok(())) => Err(format!(
                "{running} database holder(s) were still running after {} seconds; the session directory was removed anyway",
                wait.as_secs()
            )),
            (running, Err(message)) => Err(format!(
                "{running} database holder(s) were still running after {} seconds; {message}",
                wait.as_secs()
            )),
        }
    }
}

/// Polls until `users` has no holder or `wait` is over; returns how many
/// holders remain. Says so on stderr when it has to wait.
fn wait_until_idle(
    users: &DatabaseUsers,
    wait: Duration,
    mut while_waiting: impl FnMut(),
) -> usize {
    let give_up = Instant::now() + wait;
    let mut announced = false;
    loop {
        let remaining = users.count();
        if remaining == 0 || Instant::now() >= give_up {
            return remaining;
        }
        if !announced {
            announced = true;
            writeln!(
                io::stderr(),
                "A database task of the session is still running; the data directory is removed when it ends, or after {} seconds.",
                wait.as_secs()
            )
            .ok();
        }
        while_waiting();
        std::thread::sleep(Duration::from_millis(10));
    }
}

impl Drop for DirGuard {
    fn drop(&mut self) {
        // The ordinary path calls `remove` and reports; this is the unwind path.
        drop(self.remove());
    }
}

/// Creates a new private session directory under `root` (which must exist).
fn create_session_dir(root: &Path) -> io::Result<PathBuf> {
    let root = std::path::absolute(root)?;
    for _ in 0..8 {
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random).map_err(|error| io::Error::other(error.to_string()))?;
        let id = u128::from_le_bytes(random);
        let path = root.join(format!("{SESSION_PREFIX}{id:032x}"));
        match data_dir::create_private_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no unused session directory name",
    ))
}

/// Writes the private marker file of this process into `dir`.
fn write_marker(dir: &Path) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(dir.join(MARKER_FILE))?;
    file.write_all(
        session_sweep::marker_text(std::process::id(), session_sweep::own_start_time()).as_bytes(),
    )?;
    file.sync_all()
}

/// A listener bound with `std` and handed to Tokio. Needs a runtime context.
fn bind(address: SocketAddr) -> Result<TcpListener, String> {
    let cannot = |error: io::Error| format!("cannot listen on {address}: {error}");
    let listener = std::net::TcpListener::bind(address).map_err(cannot)?;
    listener.set_nonblocking(true).map_err(cannot)?;
    TcpListener::from_std(listener).map_err(cannot)
}

/// Takes the output of an earlier stage.
fn earlier<T>(slot: &mut Option<T>, what: &str) -> Result<T, String> {
    slot.take()
        .ok_or_else(|| format!("internal error: {what} is missing at this stage"))
}

/// Everything a running session needs after startup.
struct Running {
    state: AppState,
    api: TcpListener,
    content: TcpListener,
    loopback: Option<TcpListener>,
    web_assets: Option<WebAssets>,
}

/// The state built by the stages.
struct Session<'a, F> {
    options: ServeOptions,
    root: Option<&'a Path>,
    stop: &'a StopLatch,
    guard: &'a mut DirGuard,
    on_ready: Option<F>,
    sweep: SweepReport,
    store: Option<SqliteStore>,
    token_store: Option<SqliteTokenStore>,
    token: Option<AccessToken>,
    api: Option<TcpListener>,
    content: Option<TcpListener>,
    loopback: Option<TcpListener>,
    state: Option<AppState>,
    web_assets: Option<WebAssets>,
}

impl<F> Session<'_, F>
where
    F: FnOnce(&Ready, &SweepReport) -> Result<(), String>,
{
    fn database_path(&self) -> PathBuf {
        self.options.data_dir.join(DATABASE_FILE)
    }

    fn create_dir(&mut self) -> Result<(), String> {
        let root = match self.root {
            Some(root) => root.to_owned(),
            None => std::env::temp_dir(),
        };
        let cannot = |error: io::Error| {
            format!(
                "cannot create a session directory under {}: {error}",
                root.display()
            )
        };
        let path = create_session_dir(&root).map_err(cannot)?;
        // Adopted at once: from here on the guard removes it, whatever happens.
        self.guard.adopt(path.clone());
        write_marker(&path)
            .map_err(|error| format!("cannot write the marker in {}: {error}", path.display()))?;
        self.options.data_dir = path;
        Ok(())
    }

    fn run_sweep(&mut self) {
        let own = self.options.data_dir.clone();
        let root = own.parent().unwrap_or(&own).to_owned();
        self.sweep = session_sweep::sweep(&root, &own, self.stop);
    }

    fn open_database(&mut self) -> Result<(), String> {
        let store = SqliteStore::open(&self.database_path()).map_err(|error| error.to_string())?;
        self.store = Some(store);
        Ok(())
    }

    fn open_token_store(&mut self) -> Result<(), String> {
        let store =
            SqliteTokenStore::open(&self.database_path()).map_err(|error| error.to_string())?;
        self.token_store = Some(store);
        Ok(())
    }

    fn token_file(&self) -> PathBuf {
        self.options.data_dir.join("access-token")
    }

    fn write_token(&mut self) -> Result<(), String> {
        let file = self.token_file();
        let token = AccessToken::load_or_create(&file)
            .map_err(|error| format!("access token {}: {error}", file.display()))?;
        self.token = Some(token);
        Ok(())
    }

    fn bind_loopback(&mut self) -> Result<(), String> {
        let address = serve::derive_loopback_address(&self.options)
            .ok_or("internal error: no loopback address to bind")?;
        self.loopback = Some(bind(address)?);
        Ok(())
    }

    fn api_listener(&self) -> Result<&TcpListener, String> {
        self.api
            .as_ref()
            .ok_or_else(|| "internal error: the API listener is missing".to_owned())
    }

    fn content_origin(&self) -> Result<String, String> {
        if let Some(origin) = self.options.content_origin.as_deref() {
            return Ok(origin.to_owned());
        }
        let content = self
            .content
            .as_ref()
            .ok_or("internal error: the content listener is missing")?;
        Ok(format!("http://{}", serve::local_address(content)?))
    }

    fn api_url(&self) -> Result<String, String> {
        match self.options.api_origin.as_deref() {
            Some(origin) => Ok(origin.to_owned()),
            None => Ok(format!(
                "http://{}",
                serve::local_address(self.api_listener()?)?
            )),
        }
    }

    fn build_state(&mut self) -> Result<(), String> {
        let api_address = serve::local_address(self.api_listener()?)?;
        let settings = serve::build_settings(&self.options, api_address, self.content_origin()?);
        let store = earlier(&mut self.store, "the database")?;
        let token_store = earlier(&mut self.token_store, "the token store")?;
        let token = earlier(&mut self.token, "the token")?;
        let state = AppState::new(store, token_store, token, settings)?;
        self.guard.watch(state.database_users());
        self.state = Some(state);
        self.web_assets = self
            .options
            .web_dir
            .as_deref()
            .map(WebAssets::new)
            .transpose()?;
        Ok(())
    }

    fn readiness(&mut self) -> Result<(), String> {
        let api_url = self.api_url()?;
        let loopback_api_url = match self.loopback.as_ref() {
            Some(listener) => Some(format!("http://{}", serve::local_address(listener)?)),
            None => None,
        };
        let ready = Ready {
            web_url: self.web_assets.as_ref().map(|_| api_url.clone()),
            api_url,
            loopback_api_url,
            content_origin: self.content_origin()?,
            token_file: self.token_file(),
            data_dir: self.options.data_dir.clone(),
        };
        let on_ready = earlier(&mut self.on_ready, "the readiness callback")?;
        on_ready(&ready, &self.sweep)
    }

    fn into_running(mut self) -> Result<Running, String> {
        Ok(Running {
            state: earlier(&mut self.state, "the application state")?,
            api: earlier(&mut self.api, "the API listener")?,
            content: earlier(&mut self.content, "the content listener")?,
            loopback: self.loopback.take(),
            web_assets: self.web_assets.take(),
        })
    }
}

impl<F> Stages for Session<'_, F>
where
    F: FnOnce(&Ready, &SweepReport) -> Result<(), String>,
{
    fn run(&mut self, stage: Stage) -> Result<(), String> {
        match stage {
            Stage::CreateDir => self.create_dir(),
            Stage::Sweep => {
                self.run_sweep();
                Ok(())
            }
            Stage::OpenDatabase => self.open_database(),
            Stage::OpenTokenStore => self.open_token_store(),
            Stage::WriteToken => self.write_token(),
            Stage::BindApi => {
                self.api = Some(bind(self.options.api_address)?);
                Ok(())
            }
            Stage::BindContent => {
                self.content = Some(bind(self.options.content_address)?);
                Ok(())
            }
            Stage::BindLoopback => self.bind_loopback(),
            Stage::BuildState => self.build_state(),
            Stage::Readiness => self.readiness(),
        }
    }
}

/// Serves until a stop is requested, then drains for at most [`DRAIN`].
async fn serve_until_stop(running: Running, stop: &StopLatch) -> Result<(), String> {
    let Running {
        state,
        api,
        content,
        loopback,
        web_assets,
    } = running;
    let streams = state.clone();
    let on_stop = |stop: &StopLatch| {
        let stop = stop.clone();
        async move { stop.wait().await }
    };
    let api_stop = on_stop(stop);
    let api_server = axum::serve(
        api,
        http::router_with_web(state.clone(), web_assets.clone())
            .into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        api_stop.await;
        // Open change streams would otherwise keep the server running.
        streams.close_streams();
    });
    let loopback_server = loopback.map(|listener| {
        axum::serve(
            listener,
            http::router_with_web(state.clone(), web_assets)
                .into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(on_stop(stop))
    });
    let content_server = axum::serve(
        content,
        preview::router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(on_stop(stop));
    let all = async {
        tokio::try_join!(
            async {
                api_server
                    .await
                    .map_err(|error| format!("API listener failed: {error}"))
            },
            async {
                content_server
                    .await
                    .map_err(|error| format!("content listener failed: {error}"))
            },
            async {
                match loopback_server {
                    Some(server) => server
                        .await
                        .map_err(|error| format!("loopback API listener failed: {error}")),
                    None => Ok(()),
                }
            },
        )
        .map(|_| ())
    };
    tokio::pin!(all);
    tokio::select! {
        result = &mut all => result,
        () = async {
            stop.wait().await;
            tokio::time::sleep(DRAIN).await;
        } => Ok(()),
    }
}

/// Runs everything between the runtime and the directory: startup, then
/// serving. Its locals (database, listeners, state) are gone when it returns.
fn serve_session<F>(
    runtime: &Runtime,
    stop: &StopLatch,
    guard: &mut DirGuard,
    options: &OneShotOptions,
    on_ready: F,
) -> Result<(), String>
where
    F: FnOnce(&Ready, &SweepReport) -> Result<(), String>,
{
    let _entered = runtime.enter();
    let mut session = Session {
        options: options.serve.clone(),
        root: options.session_root.as_deref(),
        stop,
        guard,
        on_ready: Some(on_ready),
        sweep: SweepReport::default(),
        store: None,
        token_store: None,
        token: None,
        api: None,
        content: None,
        loopback: None,
        state: None,
        web_assets: None,
    };
    let with_loopback = serve::derive_loopback_address(&session.options).is_some();
    match run_startup(&plan(with_loopback), stop, &mut session)? {
        StartupEnd::Stopped(_) => Ok(()),
        StartupEnd::Complete => runtime.block_on(serve_until_stop(session.into_running()?, stop)),
    }
}

/// Runs a one-shot session until its lifetime ends or it is told to stop. The
/// directory is removed on every way out, a panic included. `started` is when
/// the process started: the lifetime counts from it.
///
/// `on_ready` is called when both listeners accept connections; an error from
/// it ends the session.
///
/// # Errors
///
/// When the session cannot start, a listener fails, `on_ready` fails, or the
/// directory cannot be removed. A stop request is not an error.
pub fn run<F>(started: Instant, options: &OneShotOptions, on_ready: F) -> Result<(), String>
where
    F: FnOnce(&Ready, &SweepReport) -> Result<(), String>,
{
    // Declared first, so it is dropped last: after the runtime and with it
    // every task and database handle.
    let mut guard = DirGuard::default();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("cannot start the async runtime: {error}"))?;
    let stop = StopLatch::new();
    install_stop_sources(&runtime, &stop, started + options.ttl)?;
    let served = serve_session(&runtime, &stop, &mut guard, options, on_ready);
    // Tasks are dropped here; blocking database tasks are waited for by the
    // guard, which knows exactly which ones matter.
    runtime.shutdown_timeout(Duration::from_millis(250));
    let removed = guard.remove();
    match (served, removed) {
        (Ok(()), removed) => removed,
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(removal)) => Err(format!("{error}; {removal}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn ttl_accepts_digits_in_range_only() {
        assert_eq!(parse_ttl("1"), Ok(Duration::from_secs(1)));
        assert_eq!(parse_ttl("604800"), Ok(Duration::from_secs(604_800)));
        for bad in [
            "",
            "0",
            "604801",
            "+5",
            "-5",
            " 5",
            "5 ",
            "5s",
            "1e3",
            "0x10",
            "٣",
            "99999999999999999999999",
        ] {
            let error = parse_ttl(bad).err();
            assert!(
                error.is_some_and(|message| message.starts_with("--one-shot-ttl-seconds")),
                "{bad:?} must be refused"
            );
        }
    }

    #[test]
    fn plan_has_the_loopback_bind_only_when_configured() {
        assert_eq!(plan(true), Stage::ALL.to_vec());
        assert!(!plan(false).contains(&Stage::BindLoopback));
        assert_eq!(plan(false).len(), Stage::ALL.len() - 1);
    }

    /// Records the stages it was asked to run; can latch a stop or fail while
    /// running a chosen stage.
    struct Fake<'a> {
        ran: RefCell<Vec<Stage>>,
        stop: &'a StopLatch,
        stop_during: Option<Stage>,
        fail_at: Option<Stage>,
    }

    impl Stages for Fake<'_> {
        fn run(&mut self, stage: Stage) -> Result<(), String> {
            self.ran.borrow_mut().push(stage);
            if self.stop_during == Some(stage) {
                self.stop.set();
            }
            if self.fail_at == Some(stage) {
                return Err(format!("{} failed", stage.name()));
            }
            Ok(())
        }
    }

    fn drive(
        stop: &StopLatch,
        stop_during: Option<Stage>,
        fail_at: Option<Stage>,
    ) -> (Result<StartupEnd, String>, Vec<Stage>) {
        let mut fake = Fake {
            ran: RefCell::new(Vec::new()),
            stop,
            stop_during,
            fail_at,
        };
        let result = run_startup(&Stage::ALL, stop, &mut fake);
        (result, fake.ran.into_inner())
    }

    #[test]
    fn without_a_stop_every_stage_runs_in_order() {
        let (result, ran) = drive(&StopLatch::new(), None, None);
        assert_eq!(result, Ok(StartupEnd::Complete));
        assert_eq!(ran, Stage::ALL.to_vec());
    }

    #[test]
    fn a_stop_before_any_stage_runs_none() {
        let stop = StopLatch::new();
        stop.set();
        let (result, ran) = drive(&stop, None, None);
        assert_eq!(result, Ok(StartupEnd::Stopped(Stage::CreateDir)));
        assert!(ran.is_empty(), "{ran:?}");
    }

    #[test]
    fn a_stop_during_any_stage_skips_every_later_stage() {
        for (index, stage) in Stage::ALL.into_iter().enumerate() {
            let (result, ran) = drive(&StopLatch::new(), Some(stage), None);
            if let Some(next) = Stage::ALL.get(index + 1) {
                assert_eq!(result, Ok(StartupEnd::Stopped(*next)), "stop in {stage:?}");
            } else {
                assert_eq!(result, Ok(StartupEnd::Complete), "stop in the last stage");
            }
            assert_eq!(
                ran,
                Stage::ALL[..=index].to_vec(),
                "stages after {stage:?} must not run"
            );
        }
    }

    #[test]
    fn a_failed_stage_ends_startup_with_its_error() {
        for (index, stage) in Stage::ALL.into_iter().enumerate() {
            let (result, ran) = drive(&StopLatch::new(), None, Some(stage));
            assert_eq!(result, Err(format!("{} failed", stage.name())));
            assert_eq!(ran, Stage::ALL[..=index].to_vec(), "after {stage:?}");
        }
    }

    #[test]
    fn the_guard_removes_the_directory_also_when_a_panic_unwinds() -> Result<(), io::Error> {
        let root = std::env::temp_dir();
        let path = create_session_dir(&root)?;
        write_marker(&path)?;
        let probe = path.clone();
        let outcome = std::panic::catch_unwind(move || {
            let mut guard = DirGuard::default();
            guard.adopt(probe);
            std::panic::resume_unwind(Box::new("simulated panic"));
        });
        assert!(outcome.is_err());
        assert!(!path.exists(), "the guard must remove {}", path.display());
        Ok(())
    }

    /// A session directory with a database and a holder of its identity store.
    fn directory_with_holder() -> Result<(PathBuf, Arc<SqliteTokenStore>), String> {
        let path = create_session_dir(&std::env::temp_dir()).map_err(|e| e.to_string())?;
        let database = path.join(DATABASE_FILE);
        SqliteStore::open(&database).map_err(|e| e.to_string())?;
        let holder = SqliteTokenStore::open(&database).map_err(|e| e.to_string())?;
        Ok((path, Arc::new(holder)))
    }

    #[test]
    fn removal_waits_for_a_database_holder_to_finish() -> Result<(), String> {
        let (path, holder) = directory_with_holder()?;
        let mut guard = DirGuard::default();
        guard.adopt(path.clone());
        guard.watch(DatabaseUsers::of(&holder));
        // The "blocking task" is seen by the third poll, then finishes.
        let mut holder = Some(holder);
        let mut polls = 0_u32;
        let mut existed_while_held = false;
        let result = guard.remove_within(Duration::from_secs(30), || {
            polls += 1;
            if polls == 3 {
                existed_while_held = path.exists();
                holder = None;
            }
        });
        assert_eq!(result, Ok(()));
        assert!(
            polls >= 3,
            "removal did not wait for the holder: {polls} polls"
        );
        assert!(
            existed_while_held,
            "the directory must still exist while a holder is running"
        );
        assert!(!path.exists());
        Ok(())
    }

    #[test]
    fn a_holder_still_running_at_the_bound_is_an_explicit_error() -> Result<(), String> {
        let (path, holder) = directory_with_holder()?;
        let mut guard = DirGuard::default();
        guard.adopt(path.clone());
        guard.watch(DatabaseUsers::of(&holder));
        let error = guard
            .remove_within(Duration::from_millis(100), || {})
            .err()
            .ok_or("a holder that never finishes must not read as success")?;
        assert!(error.contains("still running"), "{error}");
        drop(holder);
        // The directory was still removed (or the error says it was not).
        assert!(
            !path.exists() || error.contains("could not remove"),
            "{error}"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_removal_keeps_the_path_for_a_second_try() -> Result<(), io::Error> {
        let path = std::env::temp_dir().join("artifree-guard-retry-test-file");
        fs::write(&path, b"a file, not a directory")?;
        let mut guard = DirGuard::default();
        guard.adopt(path.clone());
        assert!(guard.remove().is_err(), "remove_dir_all of a file fails");
        assert_eq!(guard.path.as_deref(), Some(path.as_path()));
        guard.path = None;
        fs::remove_file(&path)
    }

    #[cfg(unix)]
    #[test]
    fn the_session_directory_and_marker_are_private() -> Result<(), io::Error> {
        use std::os::unix::fs::PermissionsExt;
        let path = create_session_dir(&std::env::temp_dir())?;
        let mut guard = DirGuard::default();
        guard.adopt(path.clone());
        write_marker(&path)?;
        let mode = |p: &Path| fs::metadata(p).map(|m| m.permissions().mode() & 0o777);
        assert_eq!(mode(&path)?, 0o700);
        assert_eq!(mode(&path.join(MARKER_FILE))?, 0o600);
        assert!(
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(SESSION_PREFIX)
                    && name.len() == SESSION_PREFIX.len() + 32)
        );
        Ok(())
    }

    #[test]
    fn a_missing_session_root_creates_nothing() {
        let root = std::env::temp_dir().join("artifree-no-such-root-for-o06");
        assert!(create_session_dir(&root).is_err());
        assert!(!root.exists());
    }
}
