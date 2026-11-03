//! Local server process: one data directory, an API listener, and a separate
//! preview content listener.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use artifree_providers::data_dir;
use artifree_providers::session_dir::SessionDir;
use artifree_providers::storage::{DATABASE_FILE, SqliteStore};
use artifree_providers::tokens::SqliteTokenStore;
use tokio::net::TcpListener;
use tokio::sync::watch;

use crate::auth::AccessToken;
use crate::http::{self, AppState, Settings};
use crate::limits::RequestLimits;
use crate::preview;
use crate::web::WebAssets;

/// Default API address; loopback only.
pub const DEFAULT_API_ADDRESS: &str = "127.0.0.1:7410";
/// Default preview content address; loopback only.
pub const DEFAULT_CONTENT_ADDRESS: &str = "127.0.0.1:7411";
/// Default CSP `frame-ancestors`: web UIs on loopback.
pub const DEFAULT_FRAME_ANCESTORS: &str = "http://127.0.0.1:* http://localhost:* http://[::1]:*";
/// Default preview link lifetime.
pub const DEFAULT_PREVIEW_TTL: Duration = Duration::from_secs(15 * 60);
/// Longest accepted preview link lifetime. Links are bearer credentials, and
/// the bound keeps expiry arithmetic far from overflow.
pub const MAX_PREVIEW_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Name of the access token file inside the data directory.
const TOKEN_FILE: &str = "access-token";
/// Longest accepted one-shot session lifetime (decision 0038).
pub const MAX_SESSION_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// How long a stopping one-shot session waits for open connections before it
/// gives up on them, so that its data is removed even if a client never leaves.
pub const SESSION_DRAIN_GRACE: Duration = Duration::from_secs(5);

/// Where the server keeps its data.
#[derive(Debug, Clone)]
pub enum Storage {
    /// A directory the operator chose. It is kept when the server stops.
    Persistent {
        /// Directory holding the database and access token.
        data_dir: PathBuf,
    },
    /// A temporary directory for one session that is deleted when the server
    /// stops (decision 0038).
    OneShot(OneShot),
}

/// Settings of a one-shot session.
#[derive(Debug, Clone)]
pub struct OneShot {
    /// How long the session runs before the server stops and its data is
    /// removed. This is a data lifetime, not a tunnel or link lifetime.
    pub ttl: Duration,
    /// Directory that holds session directories; the system temporary
    /// directory when absent.
    pub root: Option<PathBuf>,
}

/// Options of `artifree-server serve`.
#[derive(Debug, Clone)]
pub struct ServeOptions {
    /// Where the database and access token live.
    pub storage: Storage,
    /// Directory holding production web assets to serve alongside the API.
    pub web_dir: Option<PathBuf>,
    /// API listen address. Port 0 picks a free port.
    pub api_address: SocketAddr,
    /// Optional loopback API address for local agents when `api_address` is non-loopback.
    pub loopback_api_address: Option<SocketAddr>,
    /// Preview content listen address. Port 0 picks a free port.
    pub content_address: SocketAddr,
    /// Public origin of the content listener, when it differs from the bound
    /// address (for example behind a reverse proxy).
    pub content_origin: Option<String>,
    /// CSP `frame-ancestors` source list for previews.
    pub frame_ancestors: String,
    /// Preview link lifetime.
    pub preview_ttl: Duration,
    /// Per-actor request budget for the API and MCP.
    pub limits: RequestLimits,
    /// Change streams one actor may keep open.
    pub change_streams: u32,
    /// Public origin of the API and web workspace (e.g. `https://artifree.example.com`).
    pub api_origin: Option<String>,
    /// Trusted reverse proxy networks.
    pub trusted_proxies: Vec<crate::proxy::IpNetwork>,
    /// Allowed Host header values.
    pub allowed_hosts: Vec<String>,
    /// Additional allowed CORS origins for the web interface.
    pub allowed_origins: Vec<String>,
    /// Whether remote MCP access is permitted.
    pub allow_remote_mcp: bool,
}

/// Addresses reported once both listeners accept connections.
#[derive(Debug, Clone)]
pub struct Ready {
    /// Base URL of the API.
    pub api_url: String,
    /// Base URL of the web workspace, if web assets are served.
    pub web_url: Option<String>,
    /// Base URL of the loopback API, if a dedicated loopback listener was started.
    pub loopback_api_url: Option<String>,
    /// Origin of preview content.
    pub content_origin: String,
    /// File holding the access token.
    pub token_file: PathBuf,
    /// Directory holding the database and access token.
    pub data_dir: PathBuf,
    /// Present only in a one-shot session; its data is deleted at the end.
    pub one_shot: Option<OneShotReady>,
}

/// What a one-shot session reports once it is ready.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OneShotReady {
    /// The session lifetime; the server stops and deletes its data after it.
    pub ttl: Duration,
    /// Directories of earlier sessions that ended without cleaning up and
    /// were removed at start.
    pub stale_sessions_removed: usize,
    /// Such directories that could not be removed.
    pub stale_sessions_failed: usize,
}

/// The parts of a one-shot session the serving code needs.
#[derive(Debug, Clone, Copy)]
struct SessionRun {
    deadline: Instant,
    ready: OneShotReady,
}

/// Opens storage, binds both listeners, reports readiness, and serves until
/// interrupted (Ctrl+C, or SIGTERM on Unix). A one-shot session also stops
/// when its lifetime ends, and then deletes its data directory.
///
/// # Errors
///
/// When the data directory, database, token, runtime, or a listener cannot be
/// set up, when the data directory is accessible to other users (Unix), when
/// a listener fails while serving, or when a one-shot session directory cannot
/// be removed.
pub fn run(options: ServeOptions, on_ready: impl FnOnce(&Ready)) -> Result<(), String> {
    match options.storage.clone() {
        Storage::Persistent { data_dir } => serve_data_dir(options, &data_dir, None, on_ready),
        Storage::OneShot(one_shot) => run_one_shot(options, &one_shot, on_ready),
    }
}

/// Runs one session in a new temporary directory and removes the directory
/// again, whether the session ends by its lifetime, a signal, or an error.
fn run_one_shot(
    options: ServeOptions,
    one_shot: &OneShot,
    on_ready: impl FnOnce(&Ready),
) -> Result<(), String> {
    let started = Instant::now();
    let root = one_shot.root.clone().unwrap_or_else(std::env::temp_dir);
    let session = SessionDir::create(&root).map_err(|error| error.to_string())?;
    let swept = session.sweep_stale();
    let run = SessionRun {
        deadline: started + one_shot.ttl,
        ready: OneShotReady {
            ttl: one_shot.ttl,
            stale_sessions_removed: swept.removed,
            stale_sessions_failed: swept.failed,
        },
    };
    // The serving code owns the database and the runtime and has dropped both
    // by the time it returns, so the directory is removed after they are closed.
    let served = serve_data_dir(options, &session.data_dir(), Some(run), on_ready);
    let removed = session.close().map_err(|error| {
        format!(
            "cannot remove the one-shot session directory: {error}; the next session removes it"
        )
    });
    match (served, removed) {
        (Err(served), Err(removed)) => Err(format!("{served}; {removed}")),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

fn serve_data_dir(
    options: ServeOptions,
    data_dir: &Path,
    session: Option<SessionRun>,
    on_ready: impl FnOnce(&Ready),
) -> Result<(), String> {
    data_dir::prepare_data_dir(data_dir)?;
    let db_path = data_dir.join(DATABASE_FILE);
    let store = SqliteStore::open(&db_path).map_err(|error| error.to_string())?;
    let token_store = SqliteTokenStore::open(&db_path).map_err(|error| error.to_string())?;
    let token_file = data_dir.join(TOKEN_FILE);
    let token = AccessToken::load_or_create(&token_file)
        .map_err(|error| format!("access token {}: {error}", token_file.display()))?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("cannot start the async runtime: {error}"))?;
    let served = runtime.block_on(serve_listeners(
        options,
        store,
        token_store,
        token,
        data_dir.to_path_buf(),
        session,
        on_ready,
    ));
    if session.is_some() {
        // A task that outlives the grace period is abandoned so that the
        // session's data can be removed.
        runtime.shutdown_timeout(SESSION_DRAIN_GRACE);
    }
    served
}

async fn serve_listeners(
    options: ServeOptions,
    store: SqliteStore,
    token_store: SqliteTokenStore,
    token: AccessToken,
    data_dir: PathBuf,
    session: Option<SessionRun>,
    on_ready: impl FnOnce(&Ready),
) -> Result<(), String> {
    let stop = StopSignal::install(session.map(|run| run.deadline));
    let api = bind(options.api_address).await?;
    let content = bind(options.content_address).await?;
    let api_url = match options.api_origin.as_deref() {
        Some(origin) => origin.to_owned(),
        None => format!("http://{}", local_address(&api)?),
    };

    let loopback = match derive_loopback_address(&options) {
        Some(addr) => Some(bind(addr).await?),
        None => None,
    };
    let loopback_api_url = match loopback {
        Some(ref listener) => Some(format!("http://{}", local_address(listener)?)),
        None => None,
    };
    let content_origin = match options.content_origin.as_deref() {
        Some(origin) => origin.to_owned(),
        None => format!("http://{}", local_address(&content)?),
    };

    let settings = build_settings(&options, local_address(&api)?, content_origin.clone());
    let state = AppState::new(store, token_store, token, settings)?;
    let web_assets = options.web_dir.as_deref().map(WebAssets::new).transpose()?;
    let web_url = if web_assets.is_some() {
        Some(api_url.clone())
    } else {
        None
    };
    on_ready(&Ready {
        api_url,
        web_url,
        loopback_api_url,
        content_origin,
        token_file: data_dir.join(TOKEN_FILE),
        data_dir,
        one_shot: session.map(|run| run.ready),
    });
    let streams = state.clone();
    let api_stop = stop.clone();
    let api_server = axum::serve(
        api,
        http::router_with_web(state.clone(), web_assets.clone())
            .into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        api_stop.wait().await;
        // Open change streams would otherwise keep the server running.
        streams.close_streams();
    });
    let loopback_stop = stop.clone();
    let loopback_server = loopback.map(|loopback_listener| {
        axum::serve(
            loopback_listener,
            http::router_with_web(state.clone(), web_assets)
                .into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(loopback_stop.wait())
    });
    let content_server = axum::serve(
        content,
        preview::router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(stop.clone().wait());
    let serving = async {
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
                if let Some(server) = loopback_server {
                    server
                        .await
                        .map_err(|error| format!("loopback API listener failed: {error}"))
                } else {
                    Ok(())
                }
            },
        )
        .map(|_| ())
    };
    if session.is_none() {
        return serving.await;
    }
    // A one-shot session waits a bounded time for open connections once it
    // has been told to stop, so that its data is always removed.
    tokio::select! {
        served = serving => served,
        () = async {
            stop.wait().await;
            tokio::time::sleep(SESSION_DRAIN_GRACE).await;
        } => Ok(()),
    }
}

fn build_settings(
    options: &ServeOptions,
    bound_api_addr: SocketAddr,
    content_origin: String,
) -> Settings {
    let mut allowed_hosts = options.allowed_hosts.clone();
    let bound_api_host = bound_api_addr.to_string();
    if !allowed_hosts
        .iter()
        .any(|h| h.eq_ignore_ascii_case(&bound_api_host))
    {
        allowed_hosts.push(bound_api_host);
    }
    Settings {
        content_origin,
        preview_ttl: options.preview_ttl,
        frame_ancestors: options.frame_ancestors.clone(),
        limits: options.limits,
        frame_limits: RequestLimits::FRAME,
        room_limits: RequestLimits::ROOM,
        room_clock: std::sync::Arc::new(crate::rooms::ProcessClock::new()),
        change_streams: options.change_streams,
        api_origin: options.api_origin.clone(),
        trusted_proxies: options.trusted_proxies.clone(),
        allowed_hosts,
        allowed_origins: options.allowed_origins.clone(),
        allow_remote_mcp: options.allow_remote_mcp,
        clock: std::sync::Arc::new(artifree_core::jobs::SystemClock),
    }
}

/// Determines the loopback address to bind for local MCP peer isolation in LAN mode.
#[must_use]
pub fn derive_loopback_address(options: &ServeOptions) -> Option<SocketAddr> {
    options.loopback_api_address.or_else(|| {
        let ip = options.api_address.ip();
        if ip.is_loopback() || ip.is_unspecified() {
            None
        } else {
            let loopback_ip = if options.api_address.is_ipv6() {
                IpAddr::V6(Ipv6Addr::LOCALHOST)
            } else {
                IpAddr::V4(Ipv4Addr::LOCALHOST)
            };
            Some(SocketAddr::new(loopback_ip, options.api_address.port()))
        }
    })
}

async fn bind(address: SocketAddr) -> Result<TcpListener, String> {
    TcpListener::bind(address)
        .await
        .map_err(|error| format!("cannot listen on {address}: {error}"))
}

fn local_address(listener: &TcpListener) -> Result<SocketAddr, String> {
    listener
        .local_addr()
        .map_err(|error| format!("cannot read the bound address: {error}"))
}

/// When the server must stop: on Ctrl-C (SIGINT) or, on Unix, SIGTERM, and in a
/// one-shot session also when its lifetime ends.
#[derive(Debug, Clone)]
struct StopSignal {
    stopped: watch::Receiver<bool>,
}

impl StopSignal {
    /// Installs the signal handlers now and starts watching for a stop. This
    /// runs before the readiness line is printed, so a supervisor that signals
    /// the server as soon as it sees that line is not killed by the default
    /// action, which would skip the removal of a one-shot session's data.
    fn install(deadline: Option<Instant>) -> Self {
        let (sender, stopped) = watch::channel(false);
        let signals = Signals::install();
        tokio::spawn(async move {
            let expiry = async {
                match deadline {
                    Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::select! {
                () = signals.received() => {}
                () = expiry => {}
            }
            // Nobody left to tell means the servers have already ended.
            sender.send_replace(true);
        });
        Self { stopped }
    }

    /// Completes once the server must stop.
    async fn wait(mut self) {
        // The sender is dropped only when the runtime ends, which also stops
        // the servers; either outcome means stop.
        let _ = self.stopped.wait_for(|stopped| *stopped).await;
    }
}

/// The handlers for Ctrl-C (SIGINT) and, on Unix, SIGTERM. A handler that
/// cannot be installed leaves that signal with no effect here.
struct Signals {
    #[cfg(unix)]
    interrupt: Option<tokio::signal::unix::Signal>,
    #[cfg(unix)]
    terminate: Option<tokio::signal::unix::Signal>,
}

impl Signals {
    #[cfg(unix)]
    fn install() -> Self {
        use tokio::signal::unix::{SignalKind, signal};
        Self {
            interrupt: signal(SignalKind::interrupt()).ok(),
            terminate: signal(SignalKind::terminate()).ok(),
        }
    }

    #[cfg(not(unix))]
    fn install() -> Self {
        Self {}
    }

    /// Completes when one of the signals is received.
    #[cfg(unix)]
    async fn received(mut self) {
        async fn first(signal: &mut Option<tokio::signal::unix::Signal>) {
            match signal {
                Some(signal) => {
                    signal.recv().await;
                }
                None => std::future::pending::<()>().await,
            }
        }
        tokio::select! {
            () = first(&mut self.interrupt) => {}
            () = first(&mut self.terminate) => {}
        }
    }

    /// Completes when Ctrl-C is received. If the handler cannot be installed,
    /// keeps serving rather than exiting.
    #[cfg(not(unix))]
    async fn received(self) {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}
