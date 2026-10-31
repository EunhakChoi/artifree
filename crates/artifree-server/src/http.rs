//! Inbound HTTP API v1. Routing, authentication, and HTTP framing live here;
//! wire conversion is shared with MCP in `operations`, and application
//! behavior stays in `artifree-core`.
//!
//! Every route requires `Authorization: Bearer <token>`. Artifact mutations
//! require an `Idempotency-Key` header. Ephemeral runtime connections and
//! version-guarded grants have their own retry contracts. Storage work runs on Tokio's blocking
//! pool because the core and store are synchronous. The router also mounts
//! the MCP endpoint (`crate::mcp`) and the change feed (`crate::changes`)
//! behind the same token.

use std::future::poll_fn;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use artifree_contracts::api::{
    AgentSessionList, Artifact, ArtifactList, CreateArtifactRequest, CreateRuntimeViewRequest,
    DuplicateArtifactRequest, ErrorCode, ErrorResponse, Handoff, HandoffActionRequest,
    ListArtifactsQuery, RestoreRevisionRequest, Revision, RevisionList, RuntimeConnection,
    RuntimeGrant, RuntimeMetadata, RuntimeMetadataRequest, SaveRevisionRequest,
    SetRuntimeGrantRequest, StageHandoffRequest, UpdateArtifactRequest, UpdateArtifactResponse,
};
use artifree_core::artifacts::{ArtifactError, ArtifactService};
use artifree_core::parts::{Boundary, ByteRange};
use artifree_domain::identity::{ActorId, RequestKey};
use artifree_providers::storage::SqliteStore;
use axum::body::{Body, Bytes, HttpBody};
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{DefaultBodyLimit, FromRequestParts, Path, Query, Request, State};
use axum::http::header::{self, HeaderValue};
use axum::http::request::Parts;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};

use crate::auth::AccessToken;
use crate::changes::{self, Feed};
use crate::handoffs::Handoffs;
use crate::limits::{Limiter, Refusal, RequestLimits};
use crate::mcp;
use crate::operations::{self, ApiError};
use crate::preview::{self, PreviewTickets};
use crate::ranges::{self, Requested};
use crate::runtime_sessions::RuntimeSessions;
use crate::web::WebAssets;

/// Largest accepted request body: a maximum package in base64 plus JSON
/// framing. Clients send a file as UTF-8 text only when its JSON escaping is
/// no larger than base64, so this bound holds for either encoding.
pub const MAX_BODY_BYTES: usize = 36 * 1024 * 1024;

/// Server settings that adapters need at request time.
#[derive(Debug, Clone)]
pub struct Settings {
    /// Origin of the preview content server, such as `http://127.0.0.1:7411`.
    /// It must differ from the API and web origins.
    pub content_origin: String,
    /// Lifetime of a preview link.
    pub preview_ttl: Duration,
    /// CSP `frame-ancestors` source list allowed to embed previews.
    pub frame_ancestors: String,
    /// Per-actor request budget shared by HTTP and MCP.
    pub limits: RequestLimits,
    /// Change streams one actor may keep open, at least 1.
    pub change_streams: u32,
}

/// Shared request state for the API and content routers.
#[derive(Debug, Clone)]
pub struct AppState {
    shared: Arc<Shared>,
}

#[derive(Debug)]
struct Shared {
    artifacts: ArtifactService<SqliteStore>,
    token: AccessToken,
    owner: ActorId,
    previews: PreviewTickets,
    runtime_sessions: RuntimeSessions,
    handoffs: Handoffs,
    limiter: Limiter,
    feed: Feed,
    draining: AtomicUsize,
    settings: Settings,
    content_policy: HeaderValue,
}

impl AppState {
    /// Combines the application service, the local token, and settings.
    ///
    /// # Errors
    ///
    /// When the content origin or frame ancestors cannot form a header, or
    /// a limit is zero.
    pub fn new(store: SqliteStore, token: AccessToken, settings: Settings) -> Result<Self, String> {
        let mut settings = settings;
        if !settings.content_origin.is_empty() {
            settings.content_origin = normalize_origin(&settings.content_origin)?;
        }
        let owner: ActorId = "local"
            .parse()
            .map_err(|error| format!("built-in owner actor: {error}"))?;
        let content_policy =
            preview::content_policy(&settings.content_origin, &settings.frame_ancestors)?;
        if settings.change_streams == 0 {
            return Err("change streams per actor must be at least 1".into());
        }
        Ok(Self {
            shared: Arc::new(Shared {
                artifacts: ArtifactService::new(store),
                token,
                owner,
                previews: PreviewTickets::new(settings.preview_ttl),
                runtime_sessions: RuntimeSessions::new(Duration::from_secs(24 * 60 * 60)),
                handoffs: Handoffs::new(Duration::from_secs(60 * 60)),
                limiter: Limiter::new(settings.limits.validate()?),
                feed: Feed::new(settings.change_streams),
                draining: AtomicUsize::new(0),
                settings,
                content_policy,
            }),
        })
    }

    /// Ends open change streams so that a graceful shutdown need not wait
    /// for their readers. Streams opened afterwards end at once.
    pub fn close_streams(&self) {
        self.shared.feed.close();
    }

    pub(crate) fn feed(&self) -> &Feed {
        &self.shared.feed
    }

    pub(crate) fn previews(&self) -> &PreviewTickets {
        &self.shared.previews
    }

    pub(crate) fn runtime_sessions(&self) -> &RuntimeSessions {
        &self.shared.runtime_sessions
    }

    pub(crate) fn handoffs(&self) -> &Handoffs {
        &self.shared.handoffs
    }

    pub(crate) fn settings(&self) -> &Settings {
        &self.shared.settings
    }

    pub(crate) fn content_policy(&self) -> &HeaderValue {
        &self.shared.content_policy
    }

    /// The actor whose bearer token is in `headers`. Every HTTP route and
    /// the MCP endpoint authenticate through this one check.
    pub(crate) fn authenticate(&self, headers: &HeaderMap) -> Result<ActorId, ApiError> {
        let presented = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(bearer_token);
        match presented {
            Some(token) if self.shared.token.matches(token) => Ok(self.shared.owner.clone()),
            _ => Err(ApiError::unauthenticated()),
        }
    }

    /// Runs a synchronous application command off the async executor.
    pub(crate) async fn run<T, F>(&self, command: F) -> Result<T, ArtifactError>
    where
        T: Send + 'static,
        F: FnOnce(&ArtifactService<SqliteStore>) -> Result<T, ArtifactError> + Send + 'static,
    {
        let shared = Arc::clone(&self.shared);
        tokio::task::spawn_blocking(move || command(&shared.artifacts))
            .await
            .unwrap_or_else(|_| Err(ArtifactError::Unavailable("worker task failed".into())))
    }
}

/// Routes of HTTP API v1 and the MCP endpoint without web assets.
pub fn router(state: AppState) -> Router {
    router_with_web(state, None)
}

/// Routes of HTTP API v1 and the MCP endpoint mounted behind authentication and limits.
fn api_v1_routes(state: &AppState) -> Router<AppState> {
    Router::new()
        .merge(mcp::routes(state.clone()))
        .route("/v1/artifacts", get(list_artifacts).post(create_artifact))
        .route(
            "/v1/artifacts/{id}",
            get(show_artifact).patch(update_artifact),
        )
        .route(
            "/v1/artifacts/{id}/revisions",
            get(list_revisions).post(save_revision),
        )
        .route(
            "/v1/artifacts/{id}/revisions/{revision}",
            get(show_revision),
        )
        .route(
            "/v1/artifacts/{id}/revisions/{revision}/files/{*path}",
            get(read_file),
        )
        .route(
            "/v1/artifacts/{id}/revisions/{revision}/export",
            get(export_revision),
        )
        .route(
            "/v1/artifacts/{id}/revisions/{revision}/restore",
            axum::routing::post(restore_revision),
        )
        .route(
            "/v1/artifacts/{id}/revisions/{revision}/duplicate",
            axum::routing::post(duplicate_artifact),
        )
        .route(
            "/v1/artifacts/{id}/revisions/{revision}/previews",
            axum::routing::post(create_preview),
        )
        .route(
            "/v1/artifacts/{id}/revisions/{revision}/runtime-views",
            axum::routing::post(create_runtime_view),
        )
        .route(
            "/v1/artifacts/{id}/revisions/{revision}/runtime-views/{view}/{generation}/metadata",
            axum::routing::post(runtime_metadata),
        )
        .route(
            "/v1/runtime-views/{view}/{generation}",
            axum::routing::delete(close_runtime_view),
        )
        .route(
            "/v1/runtime-sessions",
            axum::routing::post(open_runtime_session),
        )
        .route(
            "/v1/runtime-sessions/{session}",
            axum::routing::delete(close_runtime_session),
        )
        .route(
            "/v1/runtime-sessions/{session}/artifacts/{id}/revisions/{revision}/grant",
            axum::routing::put(set_runtime_grant),
        )
        .route("/v1/agent-sessions", get(agent_sessions))
        .route("/v1/handoffs", axum::routing::post(stage_handoff))
        .route("/v1/handoffs/{id}", get(get_handoff))
        .route("/v1/handoffs/{id}/send", axum::routing::post(send_handoff))
        .route(
            "/v1/handoffs/{id}/cancel",
            axum::routing::post(cancel_handoff),
        )
        .route("/v1/changes", get(changes::stream))
        .method_not_allowed_fallback(method_not_allowed)
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(middleware::map_response(no_store))
        .layer(middleware::from_fn_with_state(state.clone(), admit))
}

/// Routes of HTTP API v1, the MCP endpoint, and optional production web assets.
pub fn router_with_web(state: AppState, web_assets: Option<WebAssets>) -> Router {
    let api_routes = api_v1_routes(&state);

    let fallback_state = state.clone();
    let fallback_assets = web_assets;
    let fallback_handler = move |req: Request| {
        let state = fallback_state.clone();
        let assets = fallback_assets.clone();
        async move {
            let path = req.uri().path().to_owned();
            if path == "/healthz" {
                return StatusCode::METHOD_NOT_ALLOWED.into_response();
            }
            let is_api = path.starts_with("/v1/") || path == "/v1" || path.starts_with("/mcp");
            let owner = state.shared.owner.clone();
            if is_api || assets.is_none() {
                return run_admitted(&state, req, move |req| async move {
                    let actor = req
                        .extensions()
                        .get::<Authenticated>()
                        .cloned()
                        .unwrap_or(Authenticated(owner));
                    no_such_route(&actor).into_response()
                })
                .await;
            }
            let method = req.method().clone();
            if let Some(assets) = assets {
                assets.handle(&method, &path).await
            } else {
                no_such_route(&Authenticated(owner)).into_response()
            }
        }
    };

    Router::new()
        .merge(api_routes)
        .route("/healthz", get(healthz_get).head(healthz_head))
        .fallback(fallback_handler)
        .layer(middleware::from_fn_with_state(state.clone(), cors))
        .with_state(state)
}

/// GET /healthz handler: returns 200 OK "ok\n" without authentication or admission limits.
async fn healthz_get() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        "ok\n",
    )
}

/// HEAD /healthz handler: returns 200 OK with empty body.
async fn healthz_head() -> impl IntoResponse {
    StatusCode::OK
}

/// Authenticates the request, then admits it under the actor's limits. A
/// refused request never reaches the application, so this attempt commits
/// nothing; its idempotency key is free unless an earlier attempt used it.
async fn admit(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    let actor = match state.authenticate(request.headers()) {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let permit = match state.shared.limiter.admit(&actor) {
        Ok(permit) => permit,
        Err(refusal) => {
            discard(&state, request.into_body()).await;
            return refused(refusal);
        }
    };
    request.extensions_mut().insert(Authenticated(actor));
    // The request runs in its own task, which keeps the permit until the
    // handler finishes. A client that disconnects cancels only this wait, so
    // it cannot free the slot while storage work continues.
    let task = tokio::spawn(async move {
        let response = next.run(request).await;
        drop(permit);
        response
    });
    task.await.unwrap_or_else(|_| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            ErrorCode::Internal,
            "the request failed; retry with the same key to learn its outcome",
        )
        .into_response()
    })
}

/// How long a refused request's body is read before the refusal is sent.
pub const MAX_DRAIN_TIME: Duration = Duration::from_secs(2);
/// Refused bodies read at the same time, across all actors. Refused requests
/// hold no permit, so this bounds the connections and tasks they can occupy.
pub const MAX_DRAINING: usize = 32;

/// Reads and drops a refused request's body so that a client still sending
/// it reads the refusal rather than a reset connection. Nothing is kept in
/// memory, and the reading stops at the API body limit, after
/// [`MAX_DRAIN_TIME`], or at once when [`MAX_DRAINING`] bodies are already
/// being read; a slow or endless sender then gets the refusal regardless.
async fn discard(state: &AppState, mut body: Body) {
    let Some(_reading) = Draining::start(&state.shared.draining) else {
        return;
    };
    let drain = async {
        let mut read = 0;
        while read <= MAX_BODY_BYTES {
            match poll_fn(|context| Pin::new(&mut body).poll_frame(context)).await {
                Some(Ok(frame)) => read += frame.data_ref().map_or(0, Bytes::len),
                _ => break,
            }
        }
    };
    // A timeout stops reading; the refusal is sent either way.
    let _finished_in_time: bool = tokio::time::timeout(MAX_DRAIN_TIME, drain).await.is_ok();
}

/// One refused body counted against [`MAX_DRAINING`]. The count is released
/// on drop, so a request cancelled mid-read, such as when its connection is
/// dropped, does not keep it.
struct Draining<'count>(&'count AtomicUsize);

impl<'count> Draining<'count> {
    /// Counts one more body, or returns `None` when the limit is reached.
    fn start(count: &'count AtomicUsize) -> Option<Self> {
        let reading = Self(count);
        (count.fetch_add(1, Ordering::AcqRel) < MAX_DRAINING).then_some(reading)
    }
}

impl Drop for Draining<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn refused(refusal: Refusal) -> Response {
    let wait = match refusal {
        Refusal::Rate(wait) => wait,
        Refusal::Busy => Duration::from_secs(1),
        Refusal::Unavailable => {
            return ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                ErrorCode::Unavailable,
                refusal.to_string(),
            )
            .into_response();
        }
    };
    // Whole seconds, rounded up: retrying at the stated time succeeds.
    let seconds = wait.as_secs() + u64::from(wait.subsec_nanos() > 0);
    let mut response = ApiError::new(
        StatusCode::TOO_MANY_REQUESTS,
        ErrorCode::RateLimited,
        refusal.to_string(),
    )
    .into_response();
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from(seconds.max(1)));
    response
}

// Unmatched requests authenticate first, like every route, so that callers
// without the token cannot probe which routes and methods exist.
fn no_such_route(_: &Authenticated) -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, ErrorCode::NotFound, "no such route")
}

// Axum adds the `Allow` header listing the route's methods.
async fn method_not_allowed(_: Authenticated) -> ApiError {
    ApiError::new(
        StatusCode::METHOD_NOT_ALLOWED,
        ErrorCode::InvalidRequest,
        "this route does not support the request method",
    )
}

/// Runs a handler under authentication and rate limiting admission permits.
async fn run_admitted<F, Fut>(state: &AppState, mut request: Request, inner: F) -> Response
where
    F: FnOnce(Request) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Response> + Send + 'static,
{
    let actor = match state.authenticate(request.headers()) {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let permit = match state.shared.limiter.admit(&actor) {
        Ok(permit) => permit,
        Err(refusal) => {
            discard(state, request.into_body()).await;
            return refused(refusal);
        }
    };
    request.extensions_mut().insert(Authenticated(actor));
    let response = inner(request).await;
    drop(permit);
    response
}

/// CORS middleware handling preflights and response headers on `/v1/*` API endpoints.
async fn cors(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let path = req.uri().path();
    let is_api = path.starts_with("/v1/") || path == "/v1";

    if !is_api {
        return next.run(req).await;
    }

    let origin = req.headers().get(header::ORIGIN).cloned();

    let Some(origin_val) = origin else {
        return next.run(req).await;
    };

    let Ok(origin_str) = origin_val.to_str() else {
        return ApiError::new(
            StatusCode::FORBIDDEN,
            ErrorCode::InvalidRequest,
            "origin header is not valid ASCII",
        )
        .into_response();
    };

    if origin_str == "null" {
        return ApiError::new(
            StatusCode::FORBIDDEN,
            ErrorCode::InvalidRequest,
            "requests from preview origin or null origin are forbidden",
        )
        .into_response();
    }

    let Ok(normalized_origin) = normalize_origin(origin_str) else {
        return ApiError::new(
            StatusCode::FORBIDDEN,
            ErrorCode::InvalidRequest,
            "origin header is not a valid HTTP or HTTPS origin",
        )
        .into_response();
    };

    if normalized_origin == state.shared.settings.content_origin {
        return ApiError::new(
            StatusCode::FORBIDDEN,
            ErrorCode::InvalidRequest,
            "requests from preview origin or null origin are forbidden",
        )
        .into_response();
    }

    if req.method() == Method::OPTIONS {
        return Response::builder()
            .status(StatusCode::NO_CONTENT)
            .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin_val)
            .header(
                header::ACCESS_CONTROL_ALLOW_METHODS,
                "GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS",
            )
            .header(
                header::ACCESS_CONTROL_ALLOW_HEADERS,
                "Authorization, Content-Type, If-Match, Idempotency-Key, Last-Event-ID",
            )
            .header(header::ACCESS_CONTROL_MAX_AGE, "86400")
            .header(
                header::ACCESS_CONTROL_EXPOSE_HEADERS,
                "Content-Range, Retry-After, Location",
            )
            .body(axum::body::Body::empty())
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }

    let mut response = next.run(req).await;
    response
        .headers_mut()
        .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin_val);
    response.headers_mut().insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static("Content-Range, Retry-After, Location"),
    );
    response
}

/// Normalizes an origin URL into canonical `<scheme>://<host>[:<port>]` format
/// per standard URL origin serialization (RFC 5952 canonical compressed IPv6 and omitted default ports).
///
/// # Errors
/// Returns an error if `raw` is not a valid absolute HTTP or HTTPS URL, or represents an opaque origin.
pub fn normalize_origin(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("origin must not be empty".to_owned());
    }
    let uri: Uri = trimmed
        .parse()
        .map_err(|e| format!("invalid URL {raw:?}: {e}"))?;

    let Some(scheme) = uri.scheme_str() else {
        return Err(format!("origin {raw:?} must include an absolute scheme"));
    };
    let scheme_lower = scheme.to_ascii_lowercase();
    if scheme_lower != "http" && scheme_lower != "https" {
        return Err(format!(
            "origin {raw:?} must use http or https scheme, got {scheme:?}"
        ));
    }

    let Some(authority) = uri.authority() else {
        return Err(format!("origin {raw:?} must include an authority/host"));
    };

    let host = authority.host();
    if host.is_empty() {
        return Err(format!("origin {raw:?} host must not be empty"));
    }

    let canonical_host =
        if let Some(v6_str) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
            let ip: Ipv6Addr = v6_str
                .parse()
                .map_err(|e| format!("invalid IPv6 host {host:?} in origin {raw:?}: {e}"))?;
            format!("[{ip}]")
        } else if let Ok(ip) = host.parse::<Ipv4Addr>() {
            ip.to_string()
        } else {
            host.to_ascii_lowercase()
        };

    let port = authority.port_u16();
    let is_default_port = matches!(
        (scheme_lower.as_str(), port),
        ("http", Some(80)) | ("https", Some(443)) | (_, None)
    );

    if is_default_port {
        Ok(format!("{scheme_lower}://{canonical_host}"))
    } else if let Some(p) = port {
        Ok(format!("{scheme_lower}://{canonical_host}:{p}"))
    } else {
        Ok(format!("{scheme_lower}://{canonical_host}"))
    }
}

async fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let retry = self.body.code == ErrorCode::Unavailable;
        let mut response = (self.status, Json(ErrorResponse { error: self.body })).into_response();
        if self.status == StatusCode::UNAUTHORIZED {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        if retry {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
        }
        if let Some(Ok(value)) = self
            .range_size
            .map(|size| HeaderValue::try_from(format!("bytes */{size}")))
        {
            response.headers_mut().insert(header::CONTENT_RANGE, value);
        }
        response
    }
}

impl From<JsonRejection> for ApiError {
    fn from(rejection: JsonRejection) -> Self {
        if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
            return Self::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                ErrorCode::PayloadTooLarge,
                format!("request body exceeds {MAX_BODY_BYTES} bytes"),
            );
        }
        Self::invalid(rejection.body_text())
    }
}

impl From<QueryRejection> for ApiError {
    fn from(rejection: QueryRejection) -> Self {
        Self::invalid(rejection.body_text())
    }
}

impl From<PathRejection> for ApiError {
    fn from(rejection: PathRejection) -> Self {
        Self::invalid(rejection.body_text())
    }
}

/// The actor authenticated by the bearer token and admitted by its limits.
/// Only the `admit` middleware creates it; a request without it is refused.
#[derive(Debug, Clone)]
pub(crate) struct Authenticated(pub(crate) ActorId);

impl<S: Sync> FromRequestParts<S> for Authenticated {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, ApiError> {
        parts
            .extensions
            .get::<Self>()
            .cloned()
            .ok_or_else(ApiError::unauthenticated)
    }
}

/// The credentials of a `Bearer` authorization value. The scheme is
/// case-insensitive and separated from the token by one or more spaces
/// (RFC 9110 section 11.4).
fn bearer_token(value: &str) -> Option<&str> {
    let (scheme, token) = value.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| token.trim_start_matches(' '))
}

/// The required `Idempotency-Key` header.
struct IdempotencyKey(RequestKey);

impl<S: Sync> FromRequestParts<S> for IdempotencyKey {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, ApiError> {
        let value = parts
            .headers
            .get("idempotency-key")
            .ok_or_else(|| ApiError::invalid("the Idempotency-Key header is required"))?
            .to_str()
            .map_err(|_| ApiError::invalid("Idempotency-Key must be ASCII"))?;
        operations::request_key("Idempotency-Key", value).map(Self)
    }
}

async fn create_artifact(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    IdempotencyKey(key): IdempotencyKey,
    body: Result<Json<CreateArtifactRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = body?;
    let response = operations::create(&state, actor, key, request).await?;
    let location = format!("/v1/artifacts/{}", response.artifact.id);
    Ok(created(&location, Json(response)))
}

async fn save_revision(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    IdempotencyKey(key): IdempotencyKey,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<SaveRevisionRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Path(id) = path?;
    // An identifier that cannot name an artifact is reported before a
    // malformed body; whether the artifact exists is checked with the request.
    operations::artifact_id(&id)?;
    let Json(request) = body?;
    let response = operations::save(&state, actor, key, &id, request).await?;
    let location = format!(
        "/v1/artifacts/{}/revisions/{}",
        response.artifact.id, response.revision.revision
    );
    Ok(created(&location, Json(response)))
}

async fn restore_revision(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    IdempotencyKey(key): IdempotencyKey,
    path: Result<Path<(String, u64)>, PathRejection>,
    body: Result<Json<RestoreRevisionRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Path((id, revision)) = path?;
    // An identifier or revision number that cannot name one is reported
    // before a malformed body; whether they exist is checked with the request.
    operations::artifact_id(&id)?;
    operations::revision_number(revision)?;
    let Json(request) = body?;
    let response = operations::restore(&state, actor, key, &id, revision, request).await?;
    let location = format!(
        "/v1/artifacts/{}/revisions/{}",
        response.artifact.id, response.revision.revision
    );
    Ok(created(&location, Json(response)))
}

async fn duplicate_artifact(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    IdempotencyKey(key): IdempotencyKey,
    path: Result<Path<(String, u64)>, PathRejection>,
    body: Result<Json<DuplicateArtifactRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Path((id, revision)) = path?;
    // An identifier or revision number that cannot name one is reported
    // before a malformed body; whether they exist is checked with the request.
    operations::artifact_id(&id)?;
    operations::revision_number(revision)?;
    let Json(request) = body?;
    let response = operations::duplicate(&state, actor, key, &id, revision, request).await?;
    let location = format!("/v1/artifacts/{}", response.artifact.id);
    Ok(created(&location, Json(response)))
}

async fn list_artifacts(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    query: Result<Query<ListArtifactsQuery>, QueryRejection>,
) -> Result<Json<ArtifactList>, ApiError> {
    let Query(query) = query?;
    Ok(Json(operations::list(&state, actor, query).await?))
}

async fn update_artifact(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    IdempotencyKey(key): IdempotencyKey,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<UpdateArtifactRequest>, JsonRejection>,
) -> Result<Json<UpdateArtifactResponse>, ApiError> {
    let Path(id) = path?;
    // An identifier that cannot name an artifact is reported before a
    // malformed body; whether the artifact exists is checked with the request.
    operations::artifact_id(&id)?;
    let Json(request) = body?;
    Ok(Json(
        operations::update(&state, actor, key, &id, request).await?,
    ))
}

async fn show_artifact(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<String>, PathRejection>,
) -> Result<Json<Artifact>, ApiError> {
    let Path(id) = path?;
    Ok(Json(operations::show(&state, actor, &id).await?))
}

async fn list_revisions(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<String>, PathRejection>,
) -> Result<Json<RevisionList>, ApiError> {
    let Path(id) = path?;
    Ok(Json(operations::revisions(&state, actor, &id).await?))
}

async fn show_revision(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<(String, u64)>, PathRejection>,
) -> Result<Json<Revision>, ApiError> {
    let Path((id, revision)) = path?;
    Ok(Json(
        operations::revision(&state, actor, &id, revision).await?,
    ))
}

/// A pinned file, or one byte range of it (decision 0016). A range that
/// starts at or past the end of the file, and `-0`, select nothing: `416`.
async fn read_file(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    headers: HeaderMap,
    path: Result<Path<(String, u64, String)>, PathRejection>,
) -> Result<Response, ApiError> {
    let Path((id, revision, file)) = path?;
    let requested =
        ranges::requested(&headers).map_err(|error| ApiError::invalid(error.to_string()))?;
    let range = match requested {
        Requested::Part(range) => range,
        // The whole file is read for `-0` too, so that it is answered with
        // the file's size, after every check a read makes.
        Requested::Whole | Requested::Nothing => ByteRange::WHOLE,
    };
    let file =
        operations::read_file(&state, actor, &id, revision, &file, range, Boundary::Byte).await?;
    let part = file.part;
    let ranged = requested != Requested::Whole;
    if ranged && (requested == Requested::Nothing || part.content.is_empty()) {
        return Err(ApiError::unsatisfiable(
            part.size,
            format!(
                "the range selects no bytes of the file, which has {} bytes",
                part.size
            ),
        ));
    }
    let content_range = ranged.then(|| {
        let last = part.offset + part.content.len() as u64 - 1;
        format!("bytes {}-{last}/{}", part.offset, part.size)
    });
    let status = if ranged {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    let mut response = (
        status,
        [
            (header::CONTENT_TYPE, file.path.media_type().essence()),
            (header::CONTENT_DISPOSITION, "attachment"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (
                header::CONTENT_SECURITY_POLICY,
                "sandbox; default-src 'none'",
            ),
            (header::ACCEPT_RANGES, "bytes"),
        ],
        part.content,
    )
        .into_response();
    if let Some(Ok(value)) = content_range.map(HeaderValue::try_from) {
        response.headers_mut().insert(header::CONTENT_RANGE, value);
    }
    Ok(response)
}

async fn export_revision(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<(String, u64)>, PathRejection>,
) -> Result<Response, ApiError> {
    let Path((id, revision)) = path?;
    let export = operations::export(&state, actor, &id, revision).await?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}\"", export.file_name),
            ),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
        ],
        export.archive,
    )
        .into_response())
}

async fn create_preview(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<(String, u64)>, PathRejection>,
) -> Result<Response, ApiError> {
    let Path((id, revision)) = path?;
    let preview = operations::preview(&state, actor, &id, revision).await?;
    let url = preview.url.clone();
    Ok(created(&url, Json(preview)))
}

async fn create_runtime_view(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<(String, u64)>, PathRejection>,
    body: Result<Json<CreateRuntimeViewRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Path((id, revision)) = path?;
    let Json(request) = body?;
    let view = operations::create_runtime_view(&state, actor, &id, revision, request).await?;
    let url = view.url.clone();
    Ok(created(&url, Json(view)))
}

async fn runtime_metadata(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<(String, u64, String, u64)>, PathRejection>,
    body: Result<Json<RuntimeMetadataRequest>, JsonRejection>,
) -> Result<Json<RuntimeMetadata>, ApiError> {
    let Path((id, revision, view, generation)) = path?;
    let Json(request) = body?;
    operations::runtime_metadata(
        &state,
        actor,
        &id,
        revision,
        &view,
        generation,
        &request.initialization,
    )
    .await
    .map(Json)
}

async fn close_runtime_view(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<(String, u64)>, PathRejection>,
) -> Result<StatusCode, ApiError> {
    let Path((view, generation)) = path?;
    operations::close_runtime_view(&state, &actor, &view, generation)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn open_runtime_session(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
) -> Result<Json<RuntimeConnection>, ApiError> {
    operations::open_runtime_session(&state, &actor).map(Json)
}

async fn close_runtime_session(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<String>, PathRejection>,
) -> Result<StatusCode, ApiError> {
    let Path(session) = path?;
    operations::close_runtime_session(&state, &actor, &session)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn set_runtime_grant(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<(String, String, u64)>, PathRejection>,
    body: Result<Json<SetRuntimeGrantRequest>, JsonRejection>,
) -> Result<Json<RuntimeGrant>, ApiError> {
    let Path((session, id, revision)) = path?;
    let Json(request) = body?;
    operations::set_runtime_grant(&state, actor, &session, &id, revision, request)
        .await
        .map(Json)
}

async fn agent_sessions(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    query: Result<Query<HandoffActionRequest>, QueryRejection>,
) -> Result<Json<AgentSessionList>, ApiError> {
    let Query(request) = query?;
    operations::agent_sessions(&state, &actor, &request.workspace_session_id).map(Json)
}

async fn stage_handoff(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    body: Result<Json<StageHandoffRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = body?;
    let workspace = request.workspace_session_id.clone();
    let handoff = operations::stage_handoff(&state, actor, request).await?;
    Ok(created(
        &format!(
            "/v1/handoffs/{}?workspace_session_id={workspace}",
            handoff.id
        ),
        Json(handoff),
    ))
}

async fn get_handoff(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<HandoffActionRequest>, QueryRejection>,
) -> Result<Json<Handoff>, ApiError> {
    let Path(id) = path?;
    let Query(request) = query?;
    operations::get_handoff(&state, &actor, &request.workspace_session_id, &id).map(Json)
}

async fn send_handoff(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<HandoffActionRequest>, JsonRejection>,
) -> Result<Json<Handoff>, ApiError> {
    let Path(id) = path?;
    let Json(request) = body?;
    operations::send_handoff(&state, actor, &request.workspace_session_id, &id)
        .await
        .map(Json)
}

async fn cancel_handoff(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<HandoffActionRequest>, JsonRejection>,
) -> Result<Json<Handoff>, ApiError> {
    let Path(id) = path?;
    let Json(request) = body?;
    operations::cancel_handoff(&state, &actor, &request.workspace_session_id, &id).map(Json)
}

fn created(location: &str, body: impl IntoResponse) -> Response {
    let mut response = (StatusCode::CREATED, body).into_response();
    if let Ok(value) = HeaderValue::from_str(location) {
        response.headers_mut().insert(header::LOCATION, value);
    }
    response
}
