//! Preview content origin.
//!
//! Artifact files are served from a separate origin than the API and web UI,
//! under an unguessable short-lived ticket bound to an actor, artifact, and
//! revision. Every request rechecks the actor's current access through the
//! application core. Responses carry a CSP `sandbox` without
//! `allow-same-origin`, so content runs in an opaque origin even when opened
//! directly, and may load only files of its own package.
//!
//! This isolates content from the workspace, not from the network: loads and
//! connections are limited to the content origin, but a script can still
//! navigate its own frame to another site (decision 0003). The runtime
//! bridge is limited to the current artifact's metadata; it has no data or
//! model access.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
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
use serde::Serialize;

use crate::auth::hex;
use crate::http::AppState;
use crate::runtime_sessions::{GrantScope, RuntimeSession};

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
    runtime: Option<RuntimeTicket>,
}

#[derive(Debug, Clone)]
struct RuntimeTicket {
    view_id: String,
    generation: u64,
    initialization: String,
    head_revision: SourceRevision,
    authority: Option<RuntimeAuthority>,
    revoked: bool,
    document_served: bool,
    bootstrap_served: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeAuthority {
    pub(crate) session: Arc<RuntimeSession>,
    pub(crate) scope: GrantScope,
    pub(crate) version: u64,
}

/// A runtime ticket could not be issued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeIssueError {
    /// An equal or newer generation already owns the view.
    Stale,
    /// The ticket registry or randomness is unavailable.
    Unavailable,
}

/// The preview ticket and its independently generated initialization secret.
#[derive(Debug)]
pub(crate) struct IssuedRuntimeTicket {
    pub(crate) preview: IssuedTicket,
    pub(crate) initialization: String,
    pub(crate) lease_ms: u64,
}

#[derive(Serialize)]
struct Bootstrap {
    view_id: String,
    generation: u64,
    initialization: String,
    lease_ms: u64,
}

#[derive(Debug)]
struct Generation {
    number: u64,
    expires: Instant,
}

#[derive(Debug, Default)]
struct TicketRegistry {
    live: HashMap<String, Ticket>,
    generations: HashMap<(ActorId, String), Generation>,
    pending: HashMap<(ActorId, String), usize>,
}

impl TicketRegistry {
    fn prune(&mut self, now: Instant) {
        self.live.retain(|_, ticket| ticket.expires > now);
        self.generations
            .retain(|key, generation| generation.expires > now || self.pending.contains_key(key));
    }
}

/// Keeps a generation fence until its outstanding create has finished.
pub(crate) struct RuntimeCreation<'a> {
    tickets: &'a PreviewTickets,
    key: (ActorId, String),
}

impl Drop for RuntimeCreation<'_> {
    fn drop(&mut self) {
        if let Ok(mut registry) = self.tickets.registry.lock()
            && let Some(count) = registry.pending.get_mut(&self.key)
        {
            *count -= 1;
            if *count == 0 {
                registry.pending.remove(&self.key);
            }
        }
    }
}

/// Process-local preview tickets. They do not survive a restart; clients
/// request a new preview link instead.
#[derive(Debug)]
pub struct PreviewTickets {
    ttl: Duration,
    registry: Mutex<TicketRegistry>,
}

impl PreviewTickets {
    /// Creates an empty registry whose tickets last `ttl`.
    #[must_use]
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            registry: Mutex::new(TicketRegistry::default()),
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
        let secret = random_secret()?;
        // Compute every expiry before taking the lock, so an unrepresentable
        // lifetime fails here instead of leaving a ticket nobody was given.
        let now = Instant::now();
        let expires = now.checked_add(self.ttl)?;
        let expires_at_ms = SystemTime::now()
            .checked_add(self.ttl)?
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())?;
        let mut registry = self.registry.lock().ok()?;
        registry.prune(now);
        if registry.live.len() >= MAX_LIVE_TICKETS {
            return None;
        }
        registry.live.insert(
            secret.clone(),
            Ticket {
                actor,
                artifact,
                revision,
                expires,
                runtime: None,
            },
        );
        Some(IssuedTicket {
            secret,
            expires_at_ms,
        })
    }

    /// Starts a create before its asynchronous artifact lookup.
    pub(crate) fn begin_runtime(
        &self,
        actor: &ActorId,
        view_id: &str,
    ) -> Option<RuntimeCreation<'_>> {
        let mut registry = self.registry.lock().ok()?;
        registry.prune(Instant::now());
        let key = (actor.clone(), view_id.to_owned());
        if registry.pending.len() >= MAX_LIVE_TICKETS && !registry.pending.contains_key(&key) {
            return None;
        }
        let count = registry.pending.entry(key.clone()).or_default();
        *count = count.checked_add(1)?;
        Some(RuntimeCreation { tickets: self, key })
    }

    /// Replaces an older generation of the same actor's view atomically.
    pub(crate) fn issue_runtime(
        &self,
        actor: ActorId,
        artifact: ArtifactId,
        revision: SourceRevision,
        head_revision: SourceRevision,
        view_id: String,
        generation: u64,
    ) -> Result<IssuedRuntimeTicket, RuntimeIssueError> {
        let secret = random_secret().ok_or(RuntimeIssueError::Unavailable)?;
        let initialization = random_secret().ok_or(RuntimeIssueError::Unavailable)?;
        // The browser's timer accepts at most a signed 32-bit millisecond
        // delay. The server CLI already limits leases to one day.
        let lease_ms = u64::try_from(self.ttl.as_millis())
            .ok()
            .filter(|duration| *duration <= 2_147_483_647)
            .ok_or(RuntimeIssueError::Unavailable)?;
        let now = Instant::now();
        let expires = now
            .checked_add(self.ttl)
            .ok_or(RuntimeIssueError::Unavailable)?;
        let expires_at_ms = SystemTime::now()
            .checked_add(self.ttl)
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
            .ok_or(RuntimeIssueError::Unavailable)?;
        let mut registry = self
            .registry
            .lock()
            .map_err(|_| RuntimeIssueError::Unavailable)?;
        registry.prune(now);
        let key = (actor.clone(), view_id.clone());
        if registry
            .generations
            .get(&key)
            .is_some_and(|previous| previous.number >= generation)
        {
            return Err(RuntimeIssueError::Stale);
        }
        if registry.generations.len() >= MAX_LIVE_TICKETS
            && !registry.generations.contains_key(&key)
        {
            return Err(RuntimeIssueError::Unavailable);
        }
        // A close can acquire the lock after this request computed its
        // expiry. Never shorten the earlier fence when a newer view wins.
        let fence_expires = registry
            .generations
            .get(&key)
            .map_or(expires, |previous| previous.expires.max(expires));
        let same_view = |ticket: &Ticket| {
            ticket.actor == actor
                && ticket
                    .runtime
                    .as_ref()
                    .is_some_and(|view| view.view_id == view_id)
        };
        if registry.live.values().any(|ticket| {
            same_view(ticket)
                && ticket
                    .runtime
                    .as_ref()
                    .is_some_and(|view| view.generation >= generation)
        }) {
            return Err(RuntimeIssueError::Stale);
        }
        if registry.live.len() >= MAX_LIVE_TICKETS && !registry.live.values().any(same_view) {
            return Err(RuntimeIssueError::Unavailable);
        }
        registry.live.retain(|_, ticket| !same_view(ticket));
        registry.generations.insert(
            key,
            Generation {
                number: generation,
                expires: fence_expires,
            },
        );
        registry.live.insert(
            secret.clone(),
            Ticket {
                actor,
                artifact,
                revision,
                expires,
                runtime: Some(RuntimeTicket {
                    view_id,
                    generation,
                    initialization: initialization.clone(),
                    head_revision,
                    authority: None,
                    revoked: false,
                    document_served: false,
                    bootstrap_served: false,
                }),
            },
        );
        Ok(IssuedRuntimeTicket {
            preview: IssuedTicket {
                secret,
                expires_at_ms,
            },
            initialization,
            lease_ms,
        })
    }

    /// Attaches authority before the newly issued URL is returned to a caller.
    pub(crate) fn bind_authority(&self, secret: &str, authority: RuntimeAuthority) -> bool {
        let Ok(mut registry) = self.registry.lock() else {
            return false;
        };
        let Some(ticket) = registry.live.get_mut(secret) else {
            return false;
        };
        let Some(view) = ticket.runtime.as_mut() else {
            return false;
        };
        if ticket.expires <= Instant::now() || view.revoked {
            return false;
        }
        view.authority = Some(authority);
        true
    }

    fn runtime_binding(
        &self,
        actor: &ActorId,
        artifact: &ArtifactId,
        revision: SourceRevision,
        view_id: &str,
        generation: u64,
        initialization: &str,
    ) -> Option<(SourceRevision, Option<RuntimeAuthority>)> {
        let registry = self.registry.lock().ok()?;
        registry.live.values().find_map(|ticket| {
            let view = ticket.runtime.as_ref()?;
            (ticket.expires > Instant::now()
                && &ticket.actor == actor
                && &ticket.artifact == artifact
                && ticket.revision == revision
                && view.view_id == view_id
                && view.generation == generation
                && !view.revoked
                && view.bootstrap_served
                && secret_matches(&view.initialization, initialization))
            .then(|| (view.head_revision, view.authority.clone()))
        })
    }

    /// Checks a live binding and the connection's current grant. The registry
    /// lock is released before taking the session lock.
    pub(crate) fn runtime_access(
        &self,
        actor: &ActorId,
        artifact: &ArtifactId,
        revision: SourceRevision,
        view_id: &str,
        generation: u64,
        initialization: &str,
    ) -> Option<SourceRevision> {
        let (head, authority) = self.runtime_binding(
            actor,
            artifact,
            revision,
            view_id,
            generation,
            initialization,
        )?;
        let authority = authority?;
        authority
            .session
            .permits(&authority.scope, authority.version)
            .then_some(head)
    }

    #[cfg(test)]
    fn runtime_head(
        &self,
        actor: &ActorId,
        artifact: &ArtifactId,
        revision: SourceRevision,
        view_id: &str,
        generation: u64,
        initialization: &str,
    ) -> Option<SourceRevision> {
        self.runtime_binding(
            actor,
            artifact,
            revision,
            view_id,
            generation,
            initialization,
        )
        .map(|(head, _)| head)
    }

    /// Revokes only the named generation, never its replacement.
    pub(crate) fn close_runtime(
        &self,
        actor: &ActorId,
        view_id: &str,
        generation: u64,
    ) -> Option<bool> {
        let now = Instant::now();
        let expires = now.checked_add(self.ttl)?;
        let mut registry = self.registry.lock().ok()?;
        registry.prune(now);
        let key = (actor.clone(), view_id.to_owned());
        if registry.generations.len() >= MAX_LIVE_TICKETS
            && !registry.generations.contains_key(&key)
        {
            return None;
        }
        registry
            .generations
            .entry(key)
            .and_modify(|previous| {
                previous.number = previous.number.max(generation);
                previous.expires = previous.expires.max(expires);
            })
            .or_insert(Generation {
                number: generation,
                expires,
            });
        let Some(view) = registry.live.values_mut().find_map(|ticket| {
            if &ticket.actor != actor {
                return None;
            }
            ticket.runtime.as_mut().filter(|view| {
                view.view_id == view_id && view.generation == generation && !view.revoked
            })
        }) else {
            return Some(false);
        };
        // Keep the highest generation until expiry. An earlier create request
        // that arrives late must not resurrect a closed view.
        view.revoked = true;
        Some(true)
    }

    fn take_document(&self, secret: &str, enabled: bool) -> Option<Bootstrap> {
        let mut registry = self.registry.lock().ok()?;
        let ticket = registry.live.get_mut(secret)?;
        let now = Instant::now();
        if ticket.expires <= now {
            return None;
        }
        let lease_ms = u64::try_from(ticket.expires.duration_since(now).as_millis()).ok()?;
        let view = ticket.runtime.as_mut()?;
        if view.document_served || view.revoked {
            return None;
        }
        view.document_served = true;
        view.bootstrap_served = enabled;
        Some(Bootstrap {
            view_id: view.view_id.clone(),
            generation: view.generation,
            initialization: view.initialization.clone(),
            lease_ms,
        })
    }

    fn resolve(&self, secret: &str) -> Option<Ticket> {
        let registry = self.registry.lock().ok()?;
        registry
            .live
            .get(secret)
            .filter(|ticket| {
                ticket.expires > Instant::now()
                    && ticket.runtime.as_ref().is_none_or(|view| !view.revoked)
            })
            .cloned()
    }
}

fn random_secret() -> Option<String> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).ok()?;
    Some(hex(&bytes))
}

fn secret_matches(expected: &str, presented: &str) -> bool {
    if expected.len() != presented.len() {
        return false;
    }
    expected
        .bytes()
        .zip(presented.bytes())
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

// Restrict the opt-in prefix to a simple, unambiguous HTML structure. The
// marker is the first head child, so no artifact script can run before the
// one-use initialization. Anything else is served unchanged as a preview.
pub(crate) fn runtime_marker(content: &[u8]) -> Option<(usize, usize)> {
    const MARKER: &[u8] = b"<!-- artifree-runtime -->";
    let mut at = usize::from(content.starts_with(b"\xef\xbb\xbf")) * 3;
    whitespace(content, &mut at);
    if !content.get(at..at + 9)?.eq_ignore_ascii_case(b"<!doctype") {
        return None;
    }
    at += 9;
    if !html_space(*content.get(at)?) {
        return None;
    }
    whitespace(content, &mut at);
    if !content.get(at..at + 4)?.eq_ignore_ascii_case(b"html") {
        return None;
    }
    at += 4;
    whitespace(content, &mut at);
    if *content.get(at)? != b'>' {
        return None;
    }
    at += 1;
    whitespace(content, &mut at);
    at = opening_tag(content, at, b"html")?;
    whitespace(content, &mut at);
    at = opening_tag(content, at, b"head")?;
    whitespace(content, &mut at);
    (content.get(at..at + MARKER.len())? == MARKER).then_some((at, at + MARKER.len()))
}

fn whitespace(content: &[u8], at: &mut usize) {
    while content.get(*at).is_some_and(|byte| html_space(*byte)) {
        *at += 1;
    }
}

fn html_space(byte: u8) -> bool {
    matches!(byte, b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
}

fn opening_tag(content: &[u8], mut at: usize, name: &[u8]) -> Option<usize> {
    if *content.get(at)? != b'<'
        || !content
            .get(at + 1..at + 1 + name.len())?
            .eq_ignore_ascii_case(name)
    {
        return None;
    }
    at += 1 + name.len();
    loop {
        match *content.get(at)? {
            b'>' => return Some(at + 1),
            byte if html_space(byte) => whitespace(content, &mut at),
            _ => return None,
        }
        if *content.get(at)? == b'>' {
            return Some(at + 1);
        }
        let start = at;
        while content.get(at).is_some_and(|byte| {
            byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'_' | b':' | b'.')
        }) {
            at += 1;
        }
        if at == start {
            return None;
        }
        whitespace(content, &mut at);
        if *content.get(at)? == b'=' {
            at += 1;
            whitespace(content, &mut at);
            let quote = *content.get(at)?;
            if !matches!(quote, b'\'' | b'"') {
                return None;
            }
            at += 1;
            while *content.get(at)? != quote {
                if matches!(*content.get(at)?, b'<' | b'\0') {
                    return None;
                }
                at += 1;
            }
            at += 1;
        }
    }
}

/// Routes of the preview content origin.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/p/{ticket}", get(redirect_to_directory))
        .route("/p/{ticket}/", get(serve_entry))
        .route("/p/{ticket}/{*path}", get(serve_file))
        .route("/r/{ticket}/", get(serve_runtime_entry))
        .route("/r/{ticket}/{*path}", get(serve_file))
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

/// Runtime documents opt in at the start of their head. The bootstrap is
/// inserted before any author script, and the entry URL is single-use. This
/// prevents a later document in the same `WindowProxy` from fetching the first
/// document's initialization. Ordinary preview URLs keep their exact bytes.
async fn serve_runtime_entry(
    State(state): State<AppState>,
    Path(secret): Path<String>,
) -> Response {
    let Some(ticket) = state.previews().resolve(&secret) else {
        return plain(StatusCode::NOT_FOUND, "Not found");
    };
    if ticket.runtime.is_none() {
        return plain(StatusCode::NOT_FOUND, "Not found");
    }
    let result = state
        .run(move |artifacts| {
            artifacts.read_file(
                &ticket.actor,
                &ticket.artifact,
                ticket.revision,
                &PackagePath::entry(),
            )
        })
        .await;
    match result {
        Ok(content) => {
            let marker = runtime_marker(&content);
            let Some(initialization) = state.previews().take_document(&secret, marker.is_some())
            else {
                return plain(StatusCode::NOT_FOUND, "Not found");
            };
            let content = if let Some((start, end)) = marker {
                let Ok(config) = serde_json::to_string(&initialization) else {
                    return plain(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Preview temporarily unavailable",
                    );
                };
                let script = format!(
                    "<script>{}\n__ArtifreeBootstrap.start({config});</script>",
                    include_str!("runtime_bootstrap.generated")
                );
                let mut result = Vec::with_capacity(content.len() + script.len());
                result.extend_from_slice(&content[..start]);
                result.extend_from_slice(script.as_bytes());
                result.extend_from_slice(&content[end..]);
                result
            } else {
                content
            };
            let mut headers = security_headers(&state);
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
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

#[cfg(test)]
mod tests {
    use super::{PreviewTickets, RuntimeIssueError, runtime_marker};
    use artifree_domain::SourceRevision;
    use artifree_domain::identity::{ActorId, ArtifactId};
    use std::time::Duration;

    #[test]
    fn only_an_unambiguous_first_head_child_can_enable_runtime()
    -> Result<(), Box<dyn std::error::Error>> {
        let accepted = b"\xef\xbb\xbf<!DOCTYPE html>\n<html lang=\"en\"><head>\n<!-- artifree-runtime --><script>author()</script>";
        let (start, end) = runtime_marker(accepted).ok_or("valid marker not found")?;
        assert_eq!(&accepted[start..end], b"<!-- artifree-runtime -->");
        for source in [
            b"<script>first()</script><!-- artifree-runtime -->".as_slice(),
            b"<!doctype html><html><head><script>first()</script><!-- artifree-runtime -->",
            b"\x0b<!doctype html><html><head><!-- artifree-runtime -->",
            b"<!doctype html><html><head data-x=unquoted><!-- artifree-runtime -->",
        ] {
            assert!(runtime_marker(source).is_none());
        }
        assert!(runtime_marker(b"<!doctype html><html><head><!-- artifree-runtime -->").is_some());
        Ok(())
    }

    #[test]
    fn runtime_binding_rejects_wrong_fields() -> Result<(), Box<dyn std::error::Error>> {
        let actor: ActorId = "local".parse()?;
        let other: ActorId = "other".parse()?;
        let artifact = ArtifactId::from_random([1; 16]);
        let other_artifact = ArtifactId::from_random([2; 16]);
        let view = "0123456789abcdef0123456789abcdef";
        let tickets = PreviewTickets::new(Duration::from_secs(60));
        let first = tickets
            .issue_runtime(
                actor.clone(),
                artifact.clone(),
                SourceRevision(1),
                SourceRevision(3),
                view.into(),
                1,
            )
            .map_err(|error| format!("{error:?}"))?;
        assert_eq!(first.lease_ms, 60_000);
        let check = |actor: &ActorId,
                     artifact: &ArtifactId,
                     revision,
                     view: &str,
                     generation,
                     initialization: &str| {
            tickets.runtime_head(
                actor,
                artifact,
                SourceRevision(revision),
                view,
                generation,
                initialization,
            )
        };
        assert_eq!(
            check(&actor, &artifact, 1, view, 1, &first.initialization),
            None
        );
        let bootstrap = tickets
            .take_document(&first.preview.secret, true)
            .ok_or("runtime bootstrap was not issued")?;
        assert!(bootstrap.lease_ms > 0 && bootstrap.lease_ms <= first.lease_ms);
        assert!(
            tickets.take_document(&first.preview.secret, true).is_none(),
            "the runtime document is served only once"
        );
        assert_eq!(
            check(&actor, &artifact, 1, view, 1, &first.initialization),
            Some(SourceRevision(3))
        );
        assert_eq!(
            check(&other, &artifact, 1, view, 1, &first.initialization),
            None,
            "another actor cannot use the binding"
        );
        assert_eq!(
            check(&actor, &other_artifact, 1, view, 1, &first.initialization),
            None,
            "another artifact cannot use the binding"
        );
        assert_eq!(
            check(&actor, &artifact, 2, view, 1, &first.initialization),
            None
        );
        assert_eq!(
            check(&actor, &artifact, 1, "another", 1, &first.initialization),
            None
        );
        assert_eq!(
            check(&actor, &artifact, 1, view, 2, &first.initialization),
            None,
            "another generation cannot use the binding"
        );
        assert_eq!(check(&actor, &artifact, 1, view, 1, "incorrect"), None);
        Ok(())
    }

    #[test]
    fn runtime_replacement_and_close_reject_old_generations()
    -> Result<(), Box<dyn std::error::Error>> {
        let actor: ActorId = "local".parse()?;
        let artifact = ArtifactId::from_random([1; 16]);
        let view = "0123456789abcdef0123456789abcdef";
        let tickets = PreviewTickets::new(Duration::from_secs(60));
        let first = tickets
            .issue_runtime(
                actor.clone(),
                artifact.clone(),
                SourceRevision(1),
                SourceRevision(3),
                view.into(),
                1,
            )
            .map_err(|error| format!("{error:?}"))?;
        assert!(tickets.take_document(&first.preview.secret, true).is_some());
        let check = |generation, initialization: &str| {
            tickets.runtime_head(
                &actor,
                &artifact,
                SourceRevision(1),
                view,
                generation,
                initialization,
            )
        };
        let second = tickets
            .issue_runtime(
                actor.clone(),
                artifact.clone(),
                SourceRevision(1),
                SourceRevision(3),
                view.into(),
                2,
            )
            .map_err(|error| format!("{error:?}"))?;
        assert_eq!(check(1, &first.initialization), None);
        assert_eq!(tickets.close_runtime(&actor, view, 1), Some(false));
        assert!(
            tickets
                .take_document(&second.preview.secret, true)
                .is_some()
        );
        assert_eq!(tickets.close_runtime(&actor, view, 2), Some(true));
        assert_eq!(check(2, &second.initialization), None);
        assert!(matches!(
            tickets.issue_runtime(
                actor.clone(),
                artifact.clone(),
                SourceRevision(1),
                SourceRevision(3),
                view.into(),
                1
            ),
            Err(RuntimeIssueError::Stale)
        ));
        Ok(())
    }

    #[test]
    fn runtime_close_before_issue_rejects_the_late_generation()
    -> Result<(), Box<dyn std::error::Error>> {
        let actor: ActorId = "local".parse()?;
        let other: ActorId = "other".parse()?;
        let artifact = ArtifactId::from_random([1; 16]);
        let view = "0123456789abcdef0123456789abcdef";
        let tickets = PreviewTickets::new(Duration::from_secs(60));

        assert_eq!(tickets.close_runtime(&actor, view, 2), Some(false));
        assert!(
            matches!(
                tickets.issue_runtime(
                    actor.clone(),
                    artifact.clone(),
                    SourceRevision(1),
                    SourceRevision(1),
                    view.into(),
                    2,
                ),
                Err(RuntimeIssueError::Stale)
            ),
            "a close acknowledged before issuance must fence that generation"
        );
        assert!(
            tickets
                .issue_runtime(
                    other,
                    artifact.clone(),
                    SourceRevision(1),
                    SourceRevision(1),
                    view.into(),
                    1,
                )
                .is_ok(),
            "one actor's close must not fence another actor"
        );
        assert!(
            tickets
                .issue_runtime(
                    actor,
                    artifact,
                    SourceRevision(1),
                    SourceRevision(1),
                    view.into(),
                    3,
                )
                .is_ok(),
            "a newer generation may replace a closed one"
        );
        Ok(())
    }

    #[test]
    fn runtime_close_refuses_when_fence_capacity_is_exhausted()
    -> Result<(), Box<dyn std::error::Error>> {
        let actor: ActorId = "local".parse()?;
        let tickets = PreviewTickets::new(Duration::from_secs(60));
        for number in 0..super::MAX_LIVE_TICKETS {
            assert_eq!(
                tickets.close_runtime(&actor, &format!("{number:032x}"), 1),
                Some(false)
            );
        }
        assert_eq!(
            tickets.close_runtime(&actor, "ffffffffffffffffffffffffffffffff", 1),
            None,
            "a close cannot succeed without room to fence its generation"
        );
        Ok(())
    }

    #[test]
    fn runtime_fence_outlives_expiry_while_creation_is_pending()
    -> Result<(), Box<dyn std::error::Error>> {
        let actor: ActorId = "local".parse()?;
        let artifact = ArtifactId::from_random([1; 16]);
        let view = "0123456789abcdef0123456789abcdef";
        let tickets = PreviewTickets::new(Duration::ZERO);
        let pending = tickets
            .begin_runtime(&actor, view)
            .ok_or("could not reserve the pending create")?;
        assert_eq!(tickets.close_runtime(&actor, view, 2), Some(false));
        assert!(
            matches!(
                tickets.issue_runtime(
                    actor.clone(),
                    artifact.clone(),
                    SourceRevision(1),
                    SourceRevision(1),
                    view.into(),
                    2,
                ),
                Err(RuntimeIssueError::Stale)
            ),
            "expiry must not release a fence while its create is pending"
        );
        drop(pending);
        assert!(
            tickets
                .issue_runtime(
                    actor,
                    artifact,
                    SourceRevision(1),
                    SourceRevision(1),
                    view.into(),
                    2,
                )
                .is_ok(),
            "the expired fence can be collected after the create ends"
        );
        Ok(())
    }

    #[test]
    fn runtime_replacement_does_not_shorten_an_existing_fence()
    -> Result<(), Box<dyn std::error::Error>> {
        let actor: ActorId = "local".parse()?;
        let artifact = ArtifactId::from_random([1; 16]);
        let view = "0123456789abcdef0123456789abcdef";
        let mut tickets = PreviewTickets::new(Duration::from_secs(60));
        assert_eq!(tickets.close_runtime(&actor, view, 2), Some(false));
        // Model a newer create whose expiry was calculated before a close
        // acquired the registry lock and extended the previous fence.
        tickets.ttl = Duration::ZERO;
        assert!(
            tickets
                .issue_runtime(
                    actor.clone(),
                    artifact.clone(),
                    SourceRevision(1),
                    SourceRevision(1),
                    view.into(),
                    3,
                )
                .is_ok()
        );
        assert!(
            matches!(
                tickets.issue_runtime(
                    actor,
                    artifact,
                    SourceRevision(1),
                    SourceRevision(1),
                    view.into(),
                    2,
                ),
                Err(RuntimeIssueError::Stale)
            ),
            "a newer view must not shorten the previous generation fence"
        );
        Ok(())
    }

    #[test]
    fn runtime_rejects_an_expired_ticket() -> Result<(), Box<dyn std::error::Error>> {
        let actor: ActorId = "local".parse()?;
        let artifact = ArtifactId::from_random([1; 16]);
        let view = "0123456789abcdef0123456789abcdef";
        let expired = PreviewTickets::new(Duration::ZERO);
        let ticket = expired
            .issue_runtime(
                actor.clone(),
                artifact.clone(),
                SourceRevision(1),
                SourceRevision(1),
                view.into(),
                1,
            )
            .map_err(|error| format!("{error:?}"))?;
        assert!(
            expired
                .take_document(&ticket.preview.secret, true)
                .is_none(),
            "an expired runtime document cannot be served"
        );
        assert_eq!(
            expired.runtime_head(
                &actor,
                &artifact,
                SourceRevision(1),
                view,
                1,
                &ticket.initialization
            ),
            None
        );
        Ok(())
    }

    #[test]
    fn runtime_rejects_a_lease_too_long_for_browser_timers()
    -> Result<(), Box<dyn std::error::Error>> {
        let actor: ActorId = "local".parse()?;
        let artifact = ArtifactId::from_random([1; 16]);
        let maximum = PreviewTickets::new(Duration::from_millis(2_147_483_647));
        let allowed = maximum
            .issue_runtime(
                actor.clone(),
                artifact.clone(),
                SourceRevision(1),
                SourceRevision(1),
                "0123456789abcdef0123456789abcdef".into(),
                1,
            )
            .map_err(|error| format!("{error:?}"))?;
        assert_eq!(allowed.lease_ms, 2_147_483_647);
        let tickets = PreviewTickets::new(Duration::from_millis(2_147_483_648));
        assert!(matches!(
            tickets.issue_runtime(
                actor,
                artifact,
                SourceRevision(1),
                SourceRevision(1),
                "0123456789abcdef0123456789abcdef".into(),
                1
            ),
            Err(RuntimeIssueError::Unavailable)
        ));
        Ok(())
    }
}
