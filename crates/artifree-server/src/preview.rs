//! Preview content origin.
//!
//! Artifact files are served from a separate origin than the API and web UI,
//! under an unguessable short-lived ticket bound to an actor, artifact, and
//! revision. Every request rechecks the actor's current access through the
//! application core. Responses carry a CSP `sandbox` without
//! `allow-same-origin`, so content runs in an opaque origin even when opened
//! directly, and may load only files of its own package.
//!
//! This is not a capability runtime: there is no SDK bridge, no network
//! egress, and no data or model access for artifact content.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use artifree_core::artifacts::ArtifactError;
use artifree_domain::SourceRevision;
use artifree_domain::identity::{ActorId, ArtifactId};
use artifree_domain::package::PackagePath;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::header::{self, HeaderValue};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;

use crate::auth::hex;
use crate::http::AppState;

/// Upper bound on live tickets, which limits memory held by unused links.
pub const MAX_LIVE_TICKETS: usize = 4096;

/// Sandbox flags for previewed content. `allow-same-origin` is absent so
/// content cannot use the content origin's storage or cookies, and
/// `allow-popups` is absent because a new window is not bound by this CSP and
/// could load any URL. Forms work for scripts, but `form-action 'none'`
/// blocks submission. Content can still navigate its own frame; see
/// decision 0003.
pub const SANDBOX: &str = "allow-scripts allow-forms allow-modals allow-downloads";

/// A newly issued preview link.
#[derive(Debug)]
pub struct IssuedTicket {
    /// Unguessable path segment of the preview URL.
    pub secret: String,
    /// Expiry in Unix milliseconds.
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone)]
struct Ticket {
    actor: ActorId,
    artifact: ArtifactId,
    revision: SourceRevision,
    expires: Instant,
}

/// Process-local preview tickets. They do not survive a restart; clients
/// request a new preview link instead.
#[derive(Debug)]
pub struct PreviewTickets {
    ttl: Duration,
    live: Mutex<HashMap<String, Ticket>>,
}

impl PreviewTickets {
    /// Creates an empty registry whose tickets last `ttl`.
    #[must_use]
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            live: Mutex::new(HashMap::new()),
        }
    }

    /// Issues a ticket, or returns `None` when randomness is unavailable, the
    /// registry is full of unexpired tickets, or its lock was poisoned.
    pub fn issue(
        &self,
        actor: ActorId,
        artifact: ArtifactId,
        revision: SourceRevision,
    ) -> Option<IssuedTicket> {
        let mut bytes = [0; 32];
        getrandom::fill(&mut bytes).ok()?;
        let secret = hex(&bytes);
        // Compute every expiry before taking the lock, so an unrepresentable
        // lifetime fails here instead of leaving a ticket nobody was given.
        let now = Instant::now();
        let expires = now.checked_add(self.ttl)?;
        let expires_at_ms = SystemTime::now()
            .checked_add(self.ttl)?
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())?;
        let mut live = self.live.lock().ok()?;
        live.retain(|_, ticket| ticket.expires > now);
        if live.len() >= MAX_LIVE_TICKETS {
            return None;
        }
        live.insert(
            secret.clone(),
            Ticket {
                actor,
                artifact,
                revision,
                expires,
            },
        );
        Some(IssuedTicket {
            secret,
            expires_at_ms,
        })
    }

    fn resolve(&self, secret: &str) -> Option<Ticket> {
        let live = self.live.lock().ok()?;
        live.get(secret)
            .filter(|ticket| ticket.expires > Instant::now())
            .cloned()
    }
}

/// Routes of the preview content origin.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/p/{ticket}", get(redirect_to_directory))
        .route("/p/{ticket}/", get(serve_entry))
        .route("/p/{ticket}/{*path}", get(serve_file))
        .fallback(|| async { plain(StatusCode::NOT_FOUND, "Not found") })
        .with_state(state)
}

async fn redirect_to_directory(Path(ticket): Path<String>) -> Response {
    if ticket.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Redirect::permanent(&format!("/p/{ticket}/")).into_response()
    } else {
        plain(StatusCode::NOT_FOUND, "Not found")
    }
}

async fn serve_entry(State(state): State<AppState>, Path(ticket): Path<String>) -> Response {
    serve(&state, &ticket, Some(PackagePath::entry())).await
}

async fn serve_file(
    State(state): State<AppState>,
    Path((ticket, path)): Path<(String, String)>,
) -> Response {
    serve(&state, &ticket, path.parse().ok()).await
}

async fn serve(state: &AppState, secret: &str, path: Option<PackagePath>) -> Response {
    let (Some(ticket), Some(path)) = (state.previews().resolve(secret), path) else {
        return plain(StatusCode::NOT_FOUND, "Not found");
    };
    let media_type = path.media_type();
    let Ticket {
        actor,
        artifact,
        revision,
        ..
    } = ticket;
    let result = state
        .run(move |artifacts| artifacts.read_file(&actor, &artifact, revision, &path))
        .await;
    match result {
        Ok(content) => {
            let mut headers = security_headers(state);
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static(media_type.essence()),
            );
            (StatusCode::OK, headers, content).into_response()
        }
        Err(ArtifactError::NotFound(_)) => plain(StatusCode::NOT_FOUND, "Not found"),
        Err(_) => plain(
            StatusCode::SERVICE_UNAVAILABLE,
            "Preview temporarily unavailable",
        ),
    }
}

/// Builds the content CSP from settings.
///
/// # Errors
///
/// When the configured origin or frame ancestors are not a valid header value.
pub fn content_policy(content_origin: &str, frame_ancestors: &str) -> Result<HeaderValue, String> {
    let policy = format!(
        "sandbox {SANDBOX}; default-src {content_origin} data: blob:; \
         script-src {content_origin} 'unsafe-inline' 'unsafe-eval' data: blob:; \
         style-src {content_origin} 'unsafe-inline' data:; connect-src {content_origin}; \
         form-action 'none'; base-uri 'none'; frame-ancestors {frame_ancestors}"
    );
    HeaderValue::from_str(&policy)
        .map_err(|_| "content origin and frame ancestors must be visible ASCII".to_owned())
}

fn security_headers(state: &AppState) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        state.content_policy().clone(),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    // Sandboxed content has an opaque origin, so its module scripts and
    // fetches of its own files are CORS requests from `null`. Anyone who can
    // send the request already holds the ticket that grants the bytes.
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers
}

fn plain(status: StatusCode, message: &'static str) -> Response {
    (
        status,
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        message,
    )
        .into_response()
}
