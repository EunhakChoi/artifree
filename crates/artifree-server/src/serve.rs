//! Local server process: one data directory, an API listener, and a separate
//! preview content listener.

use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use artifree_providers::storage::SqliteStore;
use tokio::net::TcpListener;

use crate::auth::AccessToken;
use crate::http::{self, AppState, Settings};
use crate::limits::RequestLimits;
use crate::preview;

/// Default API address; loopback only.
pub const DEFAULT_API_ADDRESS: &str = "127.0.0.1:7410";
/// Default preview content address; loopback only.
pub const DEFAULT_CONTENT_ADDRESS: &str = "127.0.0.1:7411";
/// Default CSP `frame-ancestors`: web UIs on loopback.
pub const DEFAULT_FRAME_ANCESTORS: &str = "http://127.0.0.1:* http://localhost:*";
/// Default preview link lifetime.
pub const DEFAULT_PREVIEW_TTL: Duration = Duration::from_secs(15 * 60);
/// Longest accepted preview link lifetime. Links are bearer credentials, and
/// the bound keeps expiry arithmetic far from overflow.
pub const MAX_PREVIEW_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Options of `artifree-server serve`.
#[derive(Debug, Clone)]
pub struct ServeOptions {
    /// Directory holding the database and access token.
    pub data_dir: PathBuf,
    /// API listen address. Port 0 picks a free port.
    pub api_address: SocketAddr,
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
}

/// Addresses reported once both listeners accept connections.
#[derive(Debug, Clone)]
pub struct Ready {
    /// Base URL of the API.
    pub api_url: String,
    /// Origin of preview content.
    pub content_origin: String,
    /// File holding the access token.
    pub token_file: PathBuf,
}

/// Opens storage, binds both listeners, reports readiness, and serves until
/// interrupted (Ctrl+C, or SIGTERM on Unix).
///
/// # Errors
///
/// When the data directory, database, token, runtime, or a listener cannot be
/// set up, when the data directory is accessible to other users (Unix), or
/// when a listener fails while serving.
pub fn run(options: ServeOptions, on_ready: impl FnOnce(&Ready)) -> Result<(), String> {
    prepare_data_dir(&options.data_dir)?;
    let store = SqliteStore::open(&options.data_dir.join("artifree.sqlite3"))
        .map_err(|error| error.to_string())?;
    let token_file = options.data_dir.join("access-token");
    let token = AccessToken::load_or_create(&token_file)
        .map_err(|error| format!("access token {}: {error}", token_file.display()))?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("cannot start the async runtime: {error}"))?;
    runtime.block_on(async move {
        let api = bind(options.api_address).await?;
        let content = bind(options.content_address).await?;
        let api_url = format!("http://{}", local_address(&api)?);
        let content_origin = match options.content_origin {
            Some(origin) => origin,
            None => format!("http://{}", local_address(&content)?),
        };
        let state = AppState::new(
            store,
            token,
            Settings {
                content_origin: content_origin.clone(),
                preview_ttl: options.preview_ttl,
                frame_ancestors: options.frame_ancestors,
                limits: options.limits,
            },
        )?;
        on_ready(&Ready {
            api_url,
            content_origin,
            token_file,
        });
        let streams = state.clone();
        let api_server =
            axum::serve(api, http::router(state.clone())).with_graceful_shutdown(async move {
                shutdown_signal().await;
                // Open change streams would otherwise hold shutdown forever.
                streams.close_streams();
            });
        let content_server =
            axum::serve(content, preview::router(state)).with_graceful_shutdown(shutdown_signal());
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
        )?;
        Ok(())
    })
}

/// Creates the data directory for the current user only, or checks that an
/// existing one is. It holds the database, its WAL, and the access token, so
/// anyone who can enter it can read every artifact without the token. An
/// existing directory that is open to group or other users is refused rather
/// than changed, because it may be shared on purpose. On other platforms the
/// directory inherits its parent's access control.
fn prepare_data_dir(path: &Path) -> Result<(), String> {
    let cannot_create =
        |error: std::io::Error| format!("cannot create {}: {error}", path.display());
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .map_err(cannot_create)?;
        let metadata = fs::metadata(path)
            .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
        let mode = metadata.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(format!(
                "{} is accessible to other users (mode {mode:03o}); run chmod 700 on it or choose another --data-dir",
                path.display()
            ));
        }
    }
    #[cfg(not(unix))]
    fs::create_dir_all(path).map_err(cannot_create)?;
    Ok(())
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

async fn shutdown_signal() {
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
