//! Inbound HTTP API v1. Routing, authentication, and wire conversion live
//! here; application behavior stays in `artifree-core`.
//!
//! Every route requires `Authorization: Bearer <token>`. Mutating routes
//! require an `Idempotency-Key` header. Storage work runs on Tokio's blocking
//! pool because the core and store are synchronous.

use std::sync::Arc;
use std::time::Duration;

use artifree_contracts::api::{
    Artifact, ArtifactKind as WireKind, ArtifactList, CommitResponse, ContentEncoding,
    CreateArtifactRequest, ErrorBody, ErrorCode, ErrorResponse, FileChange as WireChange,
    FileUpload, Preview, Revision, RevisionFile, RevisionList, SaveRevisionRequest,
};
use artifree_core::artifacts::{ArtifactError, ArtifactService, Missing};
use artifree_domain::identity::{ActorId, ArtifactId, RequestKey, Title};
use artifree_domain::package::{FileChange, FilePackage, MAX_CHANGES, MAX_FILES, PackagePath};
use artifree_domain::{ArtifactKind, SourceRevision};
use artifree_ports::artifact_store::{ArtifactRecord, CommitReceipt, Delivery, RevisionRecord};
use artifree_providers::storage::SqliteStore;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{DefaultBodyLimit, FromRequestParts, Path, State};
use axum::http::StatusCode;
use axum::http::header::{self, HeaderValue};
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router, middleware};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;

use crate::auth::AccessToken;
use crate::preview::{self, PreviewTickets};

/// Largest accepted request body: a maximum package in base64 plus JSON framing.
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

/// Routes of HTTP API v1.
pub fn router(state: AppState) -> Router {
    Router::new()
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
        .layer(middleware::map_response(no_store))
        .with_state(state)
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

/// A JSON error response.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    body: ErrorBody,
}

impl ApiError {
    fn new(status: StatusCode, code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            status,
            body: ErrorBody {
                code,
                message: message.into(),
                current_revision: None,
            },
        }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, ErrorCode::InvalidRequest, message)
    }

    fn unprocessable(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            ErrorCode::InvalidRequest,
            message,
        )
    }
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

impl From<ArtifactError> for ApiError {
    fn from(error: ArtifactError) -> Self {
        let message = error.to_string();
        match error {
            ArtifactError::InvalidPackage(_) => Self::unprocessable(message),
            ArtifactError::NotFound(_) => {
                Self::new(StatusCode::NOT_FOUND, ErrorCode::NotFound, message)
            }
            ArtifactError::RevisionConflict { current } => {
                let mut conflict =
                    Self::new(StatusCode::CONFLICT, ErrorCode::RevisionConflict, message);
                conflict.body.current_revision = Some(current.0);
                conflict
            }
            ArtifactError::RequestKeyReused => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                ErrorCode::RequestKeyReused,
                message,
            ),
            ArtifactError::Unavailable(_) => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                ErrorCode::Unavailable,
                message,
            ),
            _ => Self::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                ErrorCode::Internal,
                message,
            ),
        }
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

/// The actor authenticated by the bearer token.
struct Authenticated(ActorId);

impl FromRequestParts<AppState> for Authenticated {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let presented = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(bearer_token);
        match presented {
            Some(token) if state.shared.token.matches(token) => {
                Ok(Self(state.shared.owner.clone()))
            }
            _ => Err(ApiError::new(
                StatusCode::UNAUTHORIZED,
                ErrorCode::Unauthenticated,
                "a valid bearer access token is required",
            )),
        }
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
        value
            .parse()
            .map(Self)
            .map_err(|error| ApiError::invalid(format!("Idempotency-Key: {error}")))
    }
}

fn artifact_id(value: &str) -> Result<ArtifactId, ApiError> {
    value.parse().map_err(|_| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            ErrorCode::NotFound,
            "artifact not found",
        )
    })
}

fn revision_number(value: u64) -> Result<SourceRevision, ApiError> {
    if value == 0 {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            ErrorCode::NotFound,
            "revision not found",
        ));
    }
    Ok(SourceRevision(value))
}

fn package_path(field: &str, value: &str) -> Result<PackagePath, ApiError> {
    value
        .parse()
        .map_err(|error| ApiError::unprocessable(format!("{field}: {error}")))
}

fn decode(field: &str, encoding: ContentEncoding, content: String) -> Result<Vec<u8>, ApiError> {
    match encoding {
        ContentEncoding::Utf8 => Ok(content.into_bytes()),
        ContentEncoding::Base64 => BASE64
            .decode(content)
            .map_err(|error| ApiError::invalid(format!("{field}: invalid base64: {error}"))),
    }
}

/// Rejects an oversized list before its entries are decoded. `limit` is
/// [`MAX_FILES`] for a new package and [`MAX_CHANGES`] for a change set; the
/// domain validates the resulting package either way.
fn too_many(what: &str, count: usize, limit: usize) -> Result<(), ApiError> {
    if count > limit {
        return Err(ApiError::unprocessable(format!(
            "at most {limit} {what} are allowed"
        )));
    }
    Ok(())
}

async fn create_artifact(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    IdempotencyKey(key): IdempotencyKey,
    body: Result<Json<CreateArtifactRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = body?;
    let title: Title = request
        .title
        .parse()
        .map_err(|error| ApiError::unprocessable(format!("title: {error}")))?;
    too_many("files", request.files.len(), MAX_FILES)?;
    let mut files = Vec::with_capacity(request.files.len());
    for (
        index,
        FileUpload {
            path,
            encoding,
            content,
        },
    ) in request.files.into_iter().enumerate()
    {
        let path = package_path(&format!("files[{index}].path"), &path)?;
        files.push((
            path,
            decode(&format!("files[{index}].content"), encoding, content)?,
        ));
    }
    let package = FilePackage::new(files)
        .map_err(|error| ApiError::unprocessable(format!("files: {error}")))?;
    let receipt = state
        .run(move |artifacts| artifacts.create(&actor, &key, &title, &package))
        .await?;
    let location = format!("/v1/artifacts/{}", receipt.artifact.id);
    Ok(created(&location, Json(commit_response(receipt))))
}

async fn save_revision(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    IdempotencyKey(key): IdempotencyKey,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<SaveRevisionRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Path(id) = path?;
    let id = artifact_id(&id)?;
    let Json(request) = body?;
    let base = SourceRevision(request.base_revision);
    too_many("changes", request.changes.len(), MAX_CHANGES)?;
    let mut changes = Vec::with_capacity(request.changes.len());
    for (index, change) in request.changes.into_iter().enumerate() {
        changes.push(match change {
            WireChange::Put {
                path,
                encoding,
                content,
            } => FileChange::Put {
                path: package_path(&format!("changes[{index}].path"), &path)?,
                content: decode(&format!("changes[{index}].content"), encoding, content)?,
            },
            WireChange::Delete { path } => FileChange::Delete {
                path: package_path(&format!("changes[{index}].path"), &path)?,
            },
        });
    }
    let receipt = state
        .run(move |artifacts| artifacts.save(&actor, &key, &id, base, changes))
        .await?;
    let location = format!(
        "/v1/artifacts/{}/revisions/{}",
        receipt.artifact.id, receipt.revision.revision.0
    );
    Ok(created(&location, Json(commit_response(receipt))))
}

async fn list_artifacts(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
) -> Result<Json<ArtifactList>, ApiError> {
    let records = state.run(move |artifacts| artifacts.list(&actor)).await?;
    Ok(Json(ArtifactList {
        artifacts: records.iter().map(artifact_dto).collect(),
    }))
}

async fn show_artifact(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<String>, PathRejection>,
) -> Result<Json<Artifact>, ApiError> {
    let Path(id) = path?;
    let id = artifact_id(&id)?;
    let record = state
        .run(move |artifacts| artifacts.artifact(&actor, &id))
        .await?;
    Ok(Json(artifact_dto(&record)))
}

async fn list_revisions(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<String>, PathRejection>,
) -> Result<Json<RevisionList>, ApiError> {
    let Path(id) = path?;
    let id = artifact_id(&id)?;
    let revisions = state
        .run(move |artifacts| artifacts.revisions(&actor, &id))
        .await?;
    Ok(Json(RevisionList {
        revisions: revisions.into_iter().map(revision_dto).collect(),
    }))
}

async fn show_revision(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<(String, u64)>, PathRejection>,
) -> Result<Json<Revision>, ApiError> {
    let Path((id, revision)) = path?;
    let id = artifact_id(&id)?;
    let revision = revision_number(revision)?;
    let revisions = state
        .run(move |artifacts| artifacts.revisions(&actor, &id))
        .await?;
    revisions
        .into_iter()
        .find(|record| record.revision == revision)
        .map(|record| Json(revision_dto(record)))
        .ok_or_else(|| ArtifactError::NotFound(Missing::Revision).into())
}

async fn read_file(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<(String, u64, String)>, PathRejection>,
) -> Result<Response, ApiError> {
    let Path((id, revision, file)) = path?;
    let id = artifact_id(&id)?;
    let revision = revision_number(revision)?;
    let file = package_path("path", &file)?;
    let media_type = file.media_type();
    let content = state
        .run(move |artifacts| artifacts.read_file(&actor, &id, revision, &file))
        .await?;
    Ok((
        [
            (header::CONTENT_TYPE, media_type.essence()),
            (header::CONTENT_DISPOSITION, "attachment"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (
                header::CONTENT_SECURITY_POLICY,
                "sandbox; default-src 'none'",
            ),
        ],
        content,
    )
        .into_response())
}

async fn create_preview(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<(String, u64)>, PathRejection>,
) -> Result<Response, ApiError> {
    let Path((id, revision)) = path?;
    let id = artifact_id(&id)?;
    let revision = revision_number(revision)?;
    let lookup_actor = actor.clone();
    let lookup_id = id.clone();
    let record = state
        .run(move |artifacts| artifacts.artifact(&lookup_actor, &lookup_id))
        .await?;
    if revision > record.current_revision {
        return Err(ArtifactError::NotFound(Missing::Revision).into());
    }
    let ticket = state.previews().issue(actor, id, revision).ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::Unavailable,
            "too many live previews; retry shortly",
        )
    })?;
    let url = format!("{}/p/{}/", state.settings().content_origin, ticket.secret);
    Ok(created(
        &url,
        Json(Preview {
            url: url.clone(),
            revision: revision.0,
            expires_at_ms: ticket.expires_at_ms,
        }),
    ))
}

fn created(location: &str, body: impl IntoResponse) -> Response {
    let mut response = (StatusCode::CREATED, body).into_response();
    if let Ok(value) = HeaderValue::from_str(location) {
        response.headers_mut().insert(header::LOCATION, value);
    }
    response
}

fn commit_response(receipt: CommitReceipt) -> CommitResponse {
    CommitResponse {
        artifact: artifact_dto(&receipt.artifact),
        revision: revision_dto(receipt.revision),
        replayed: receipt.delivery == Delivery::Replayed,
    }
}

fn artifact_dto(record: &ArtifactRecord) -> Artifact {
    Artifact {
        id: record.id.to_string(),
        kind: match record.kind {
            ArtifactKind::App => WireKind::App,
            ArtifactKind::Document => WireKind::Document,
            ArtifactKind::Slides => WireKind::Slides,
            ArtifactKind::Dashboard => WireKind::Dashboard,
            ArtifactKind::Design => WireKind::Design,
            ArtifactKind::Motion => WireKind::Motion,
        },
        title: record.title.as_str().to_owned(),
        current_revision: record.current_revision.0,
        created_at_ms: record.created_at.0,
        updated_at_ms: record.updated_at.0,
    }
}

fn revision_dto(record: RevisionRecord) -> Revision {
    Revision {
        revision: record.revision.0,
        created_by: record.created_by.to_string(),
        created_at_ms: record.created_at.0,
        files: record
            .files
            .into_iter()
            .map(|entry| RevisionFile {
                media_type: entry.path.media_type().essence().to_owned(),
                path: entry.path.to_string(),
                size: entry.size,
            })
            .collect(),
    }
}
