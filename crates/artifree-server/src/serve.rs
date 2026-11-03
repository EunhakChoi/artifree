//! Local server process: one data directory, an API listener, and a separate
//! preview content listener.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use artifree_providers::data_dir;
use artifree_providers::storage::{DATABASE_FILE, SqliteStore};
use artifree_providers::tokens::SqliteTokenStore;
use tokio::net::TcpListener;

use crate::auth::AccessToken;
use crate::http::{self, AppState, Settings};
use crate::limits::RequestLimits;
use crate::one_shot::{self, StopWatch};
use crate::preview;
pub use crate::sweep::SweepReport;
use crate::web::WebAssets;

/// Default API address; loopback only.
pub const DEFAULT_API_ADDRESS: &str = "127.0.0.1:7410";
/// Default preview content address; loopback only.
pub const DEFAULT_CONTENT_ADDRESS: &str = "127.0.0.1:7411";
/// Default CSP `frame-ancestors`: web UIs on loopback.
pub const DEFAULT_FRAME_ANCESTORS: &str = "http://127.0.0.1:* http://localhost:* http://[::1]:*";
/// Default preview link lifetime.
pub const DEFAULT_PREVIEW_TTL: Duration = Duration::from_secs(15 * 60);
/// What ends a one-shot session besides its lifetime, as the platform has it:
/// Windows handles Ctrl+C only. One text for the notice and for `--help`.
pub const ONE_SHOT_STOP_SIGNALS: &str = if cfg!(unix) {
    "SIGINT, SIGTERM or SIGHUP"
} else {
    "Ctrl+C"
};
/// Longest accepted one-shot session lifetime: seven days.
pub const MAX_ONE_SHOT_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// Longest accepted preview link lifetime. Links are bearer credentials, and
/// the bound keeps expiry arithmetic far from overflow.
pub const MAX_PREVIEW_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Where `serve` keeps its data, and so how long the data lives.
#[derive(Debug, Clone)]
pub enum DataLocation {
    /// A directory the operator names and keeps (decision 0003).
    Persistent {
        /// Directory holding the database and access token.
        data_dir: PathBuf,
    },
    /// A new temporary directory, deleted when the process ends (decision
    /// 0039).
    OneShot(OneShot),
}

/// The one-shot session settings of `serve --one-shot-ttl-seconds`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OneShot {
    /// Total lifetime of the process, counted from the start of `serve`
    /// (start-up time included). From 1 second to [`MAX_ONE_SHOT_TTL`].
    pub ttl: Duration,
    /// Directory in which the session directory is created; the system
    /// temporary directory when `None`. It must already exist.
    pub session_root: Option<PathBuf>,
}

/// Options of `artifree-server serve`.
#[derive(Debug, Clone)]
pub struct ServeOptions {
    /// Where the database and access token live.
    pub data: DataLocation,
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
    /// Directory holding the database and the token file.
    pub data_dir: PathBuf,
    /// Present only in a one-shot session.
    pub session: Option<SessionReady>,
}

/// What a one-shot session adds to [`Ready`].
#[derive(Debug, Clone)]
pub struct SessionReady {
    /// The lifetime that was asked for, counted from the start of `serve`.
    pub ttl: Duration,
    /// What the sweep of earlier, killed sessions did.
    pub sweep: SweepReport,
}

/// The opened data directory of a server about to run.
#[derive(Debug)]
pub(crate) struct Opened {
    store: SqliteStore,
    token_store: SqliteTokenStore,
    token: AccessToken,
    data_dir: PathBuf,
    token_file: PathBuf,
}

/// Prepares `data_dir` and opens the database and the access token in it.
///
/// `stopping` is asked before each step that creates or changes a file (the
/// database, the token store, the token); when it says `true` nothing further
/// is started and the result is `Ok(None)`. A step that started is finished.
/// Persistent mode passes `|| false`.
pub(crate) fn open_data(
    data_dir: &Path,
    stopping: &dyn Fn() -> bool,
) -> Result<Option<Opened>, String> {
    data_dir::prepare_data_dir(data_dir)?;
    if stopping() {
        return Ok(None);
    }
    let db_path = data_dir.join(DATABASE_FILE);
    let store = SqliteStore::open(&db_path).map_err(|error| error.to_string())?;
    if stopping() {
        return Ok(None);
    }
    let token_store = SqliteTokenStore::open(&db_path).map_err(|error| error.to_string())?;
    let token_file = data_dir.join("access-token");
    if stopping() {
        return Ok(None);
    }
    let token = AccessToken::load_or_create(&token_file)
        .map_err(|error| format!("access token {}: {error}", token_file.display()))?;
    Ok(Some(Opened {
        store,
        token_store,
        token,
        data_dir: data_dir.to_path_buf(),
        token_file,
    }))
}

/// A runtime for the listeners.
pub(crate) fn build_runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("cannot start the async runtime: {error}"))
}

/// What ends the listeners: the process's own signals (persistent mode), or a
/// session's stop with a bounded drain (decision 0039).
#[derive(Debug, Clone)]
pub(crate) enum Shutdown {
    Process,
    Session(StopWatch),
}

impl Shutdown {
    async fn wait(&self) {
        match self {
            Self::Process => shutdown_signal().await,
            Self::Session(watch) => watch.wait().await,
        }
    }

    fn is_stopping(&self) -> bool {
        match self {
            Self::Process => false,
            Self::Session(watch) => watch.requested(),
        }
    }

    /// How long open connections may take after the stop, if bounded.
    fn drain(&self) -> Option<Duration> {
        match self {
            Self::Process => None,
            Self::Session(_) => Some(one_shot::DRAIN),
        }
    }
}

/// Opens storage, binds both listeners, reports readiness, and serves until
/// interrupted (Ctrl+C, or SIGTERM on Unix); a one-shot session also ends at
/// its TTL and deletes its data directory on the way out (decision 0039).
///
/// # Errors
///
/// When the data directory, database, token, runtime, or a listener cannot be
/// set up, when the data directory is accessible to other users (Unix), when
/// `on_ready` fails, when a listener fails while serving, or when a one-shot
/// session's directory cannot be removed.
pub fn run(
    options: ServeOptions,
    on_ready: impl FnOnce(&Ready) -> Result<(), String>,
) -> Result<(), String> {
    match options.data.clone() {
        DataLocation::Persistent { data_dir } => {
            let opened =
                open_data(&data_dir, &|| false)?.ok_or("the data directory was not opened")?;
            let runtime = build_runtime()?;
            runtime.block_on(serve_listeners(
                options,
                opened,
                None,
                Shutdown::Process,
                on_ready,
            ))
        }
        DataLocation::OneShot(one_shot) => one_shot::run(options, &one_shot, on_ready),
    }
}

pub(crate) async fn serve_listeners(
    options: ServeOptions,
    opened: Opened,
    session: Option<SessionReady>,
    shutdown: Shutdown,
    on_ready: impl FnOnce(&Ready) -> Result<(), String>,
) -> Result<(), String> {
    let Opened {
        store,
        token_store,
        token,
        data_dir,
        token_file,
    } = opened;
    let api = bind(options.api_address).await?;
    if shutdown.is_stopping() {
        return Ok(());
    }
    let content = bind(options.content_address).await?;
    let api_url = match options.api_origin.as_deref() {
        Some(origin) => origin.to_owned(),
        None => format!("http://{}", local_address(&api)?),
    };

    let loopback = match derive_loopback_address(&options) {
        Some(addr) => Some(bind(addr).await?),
        None => None,
    };
    if shutdown.is_stopping() {
        return Ok(());
    }
    let loopback_api_url = match loopback {
        Some(ref listener) => Some(format!("http://{}", local_address(listener)?)),
        None => None,
    };
    let content_origin = match options.content_origin.as_deref() {
        Some(origin) => origin.to_owned(),
        None => format!("http://{}", local_address(&content)?),
    };

    if shutdown.is_stopping() {
        // Nothing below is started once a stop is latched.
        return Ok(());
    }
    let settings = build_settings(&options, local_address(&api)?, content_origin.clone());
    let state = AppState::new(store, token_store, token, settings)?;
    let web_assets = options.web_dir.as_deref().map(WebAssets::new).transpose()?;
    let web_url = if web_assets.is_some() {
        Some(api_url.clone())
    } else {
        None
    };
    if shutdown.is_stopping() {
        // A stop (a signal or the TTL) arrived while starting: no readiness.
        return Ok(());
    }
    on_ready(&Ready {
        api_url,
        web_url,
        loopback_api_url,
        content_origin,
        token_file,
        data_dir,
        session,
    })?;
    serve_until_stopped(api, content, loopback, state, web_assets, shutdown).await
}

/// Serves the listeners until `shutdown`; a session's connections then get
/// their bounded drain.
async fn serve_until_stopped(
    api: TcpListener,
    content: TcpListener,
    loopback: Option<TcpListener>,
    state: AppState,
    web_assets: Option<WebAssets>,
    shutdown: Shutdown,
) -> Result<(), String> {
    let streams = state.clone();
    let (api_stop, loopback_stop, content_stop) =
        (shutdown.clone(), shutdown.clone(), shutdown.clone());
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
    let loopback_server = loopback.map(|loopback_listener| {
        axum::serve(
            loopback_listener,
            http::router_with_web(state.clone(), web_assets)
                .into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move { loopback_stop.wait().await })
    });
    let content_server = axum::serve(
        content,
        preview::router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move { content_stop.wait().await });
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
        )?;
        Ok(())
    };
    let Some(limit) = shutdown.drain() else {
        return serving.await;
    };
    // A client that never finishes must not keep the session's data alive:
    // after the stop, connections get `limit`, then serving is abandoned and
    // the caller drops the runtime, which drops them.
    tokio::select! {
        result = serving => result,
        () = async {
            shutdown.wait().await;
            tokio::time::sleep(limit).await;
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

/// Completes when the process receives Ctrl-C (SIGINT) or, on Unix, SIGTERM.
/// If a handler cannot be installed, that signal is never delivered here.
pub(crate) async fn shutdown_signal() {
    let interrupt = async {
        // If the handler cannot be installed, keep serving rather than exit.
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = interrupt => {}
        () = terminate => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::error::Error;

    type TestResult = Result<(), Box<dyn Error>>;

    struct Dir(PathBuf);

    impl Dir {
        fn new(name: &str) -> std::io::Result<Self> {
            let path =
                std::env::temp_dir().join(format!("artifree-open-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            data_dir::create_private_dir(&path)?;
            Ok(Self(path))
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// `open_data` with a stop that is latched from the `n`-th question on
    /// (`n` = 1 is the first question); returns whether it opened, and which
    /// of the database and the token exist afterwards.
    fn open_with_stop_at(name: &str, n: u32) -> Result<(bool, bool, bool), Box<dyn Error>> {
        let dir = Dir::new(name)?;
        let asked = Cell::new(0);
        let stopping = || {
            asked.set(asked.get() + 1);
            asked.get() >= n
        };
        let opened = open_data(&dir.0, &stopping)?.is_some();
        Ok((
            opened,
            dir.0.join(DATABASE_FILE).exists(),
            dir.0.join("access-token").exists(),
        ))
    }

    #[test]
    fn a_stop_before_a_step_keeps_that_step_and_the_later_ones_from_starting() -> TestResult {
        // Asked before the database, before the token store, before the token.
        assert_eq!(open_with_stop_at("first", 1)?, (false, false, false));
        assert_eq!(open_with_stop_at("second", 2)?, (false, true, false));
        assert_eq!(open_with_stop_at("third", 3)?, (false, true, false));
        assert_eq!(open_with_stop_at("never", 4)?, (true, true, true));
        Ok(())
    }
}
