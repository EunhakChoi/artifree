//! Inbound HTTP API v1. Routing, authentication, and HTTP framing live here;
//! wire conversion is shared with MCP in `operations`, and application
//! behavior stays in `artifree-core`.
//!
//! Every route requires `Authorization: Bearer <token>`. Mutating routes
//! require an `Idempotency-Key` header. Storage work runs on Tokio's blocking
//! pool because the core and store are synchronous. The router also mounts
//! the MCP endpoint (`crate::mcp`) behind the same token.

use std::future::poll_fn;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use artifree_contracts::api::{
    Artifact, ArtifactList, CreateArtifactRequest, ErrorCode, ErrorResponse, Revision,
    RevisionList, SaveRevisionRequest,
};
use artifree_core::artifacts::{ArtifactError, ArtifactService};
use artifree_domain::identity::{ActorId, RequestKey};
use artifree_providers::storage::SqliteStore;
use axum::body::{Body, Bytes, HttpBody};
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{DefaultBodyLimit, FromRequestParts, Path, Request, State};
use axum::http::header::{self, HeaderValue};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};

use crate::auth::AccessToken;
use crate::limits::{Limiter, Refusal, RequestLimits};
use crate::mcp;
use crate::operations::{self, ApiError};
use crate::preview::{self, PreviewTickets};

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
    limiter: Limiter,
    settings: Settings,
    content_policy: HeaderValue,
}

impl AppState {
    /// Combines the application service, the local token, and settings.
    ///
    /// # Errors
    ///
    /// When the content origin or frame ancestors cannot form a header.
    pub fn new(store: SqliteStore, token: AccessToken, settings: Settings) -> Result<Self, String> {
        let owner: ActorId = "local"
            .parse()
            .map_err(|error| format!("built-in owner actor: {error}"))?;
        let content_policy =
            preview::content_policy(&settings.content_origin, &settings.frame_ancestors)?;
        Ok(Self {
            shared: Arc::new(Shared {
                artifacts: ArtifactService::new(store),
                token,
                owner,
                previews: PreviewTickets::new(settings.preview_ttl),
                limiter: Limiter::new(settings.limits.validate()?),
                settings,
                content_policy,
            }),
        })
    }

    pub(crate) fn previews(&self) -> &PreviewTickets {
        &self.shared.previews
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

/// Routes of HTTP API v1 and the MCP endpoint. Every request, including an
/// unmatched one, is authenticated and admitted by the actor's request limits
/// before anything else runs.
pub fn router(state: AppState) -> Router {
    Router::new()
        .merge(mcp::routes(state.clone()))
        .route("/v1/artifacts", get(list_artifacts).post(create_artifact))
        .route("/v1/artifacts/{id}", get(show_artifact))
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
            "/v1/artifacts/{id}/revisions/{revision}/previews",
            axum::routing::post(create_preview),
        )
        .method_not_allowed_fallback(method_not_allowed)
        .fallback(no_such_route)
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(middleware::from_fn_with_state(state.clone(), admit))
        .layer(middleware::map_response(no_store))
        .with_state(state)
}

/// Authenticates the request, then admits it under the actor's limits. A
/// refused request never reaches the application, so it commits nothing and
/// its idempotency key stays unused.
async fn admit(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    let actor = match state.authenticate(request.headers()) {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let permit = match state.shared.limiter.admit(&actor) {
        Ok(permit) => permit,
        Err(refusal) => {
            discard(request.into_body()).await;
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

/// Reads and drops a refused request's body, up to the API body limit, so a
/// client that is still sending it reads the refusal rather than a reset
/// connection. Nothing is kept in memory.
async fn discard(mut body: Body) {
    let mut read = 0;
    while read <= MAX_BODY_BYTES {
        match poll_fn(|context| Pin::new(&mut body).poll_frame(context)).await {
            Some(Ok(frame)) => read += frame.data_ref().map_or(0, Bytes::len),
            _ => break,
        }
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
async fn no_such_route(_: Authenticated) -> ApiError {
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
    // An unknown artifact is reported before a malformed body.
    operations::artifact_id(&id)?;
    let Json(request) = body?;
    let response = operations::save(&state, actor, key, &id, request).await?;
    let location = format!(
        "/v1/artifacts/{}/revisions/{}",
        response.artifact.id, response.revision.revision
    );
    Ok(created(&location, Json(response)))
}

async fn list_artifacts(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
) -> Result<Json<ArtifactList>, ApiError> {
    Ok(Json(operations::list(&state, actor).await?))
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

async fn read_file(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<(String, u64, String)>, PathRejection>,
) -> Result<Response, ApiError> {
    let Path((id, revision, file)) = path?;
    let file = operations::read_file(&state, actor, &id, revision, &file).await?;
    Ok((
        [
            (header::CONTENT_TYPE, file.path.media_type().essence()),
            (header::CONTENT_DISPOSITION, "attachment"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (
                header::CONTENT_SECURITY_POLICY,
                "sandbox; default-src 'none'",
            ),
        ],
        file.content,
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

fn created(location: &str, body: impl IntoResponse) -> Response {
    let mut response = (StatusCode::CREATED, body).into_response();
    if let Ok(value) = HeaderValue::from_str(location) {
        response.headers_mut().insert(header::LOCATION, value);
    }
    response
}
