//! Frame call HTTP adapters (decisions 0019, 0032, 0033, 0045 and 0053): the
//! routes a runtime frame reaches through the workspace page, admitted under
//! the frame budget. Authorization and behavior live in `runtime_source`,
//! `runtime_assets`, `runtime_data` and `operations`.

use super::{
    AppState, Authenticated, MAX_FRAME_BODY_BYTES, MAX_FRAME_WRITE_BODY_BYTES, admit_frame,
    method_not_allowed, no_store,
};
use crate::operations::{self, ApiError};
use crate::runtime_assets;
use crate::runtime_data;
use crate::runtime_source::{self, Download, FrameCall};
use artifree_contracts::api::{
    AssetList, DeleteAssetResponse, ErrorCode, RuntimeAssetDeleteRequest,
    RuntimeAssetDownloadOffer, RuntimeAssetDownloadRequest, RuntimeAssetListRequest,
    RuntimeAssetPart, RuntimeAssetReadRequest, RuntimeAssetUploadRequest, RuntimeDataReadRequest,
    RuntimeDataValue, RuntimeDownloadOffer, RuntimeDownloadRequest, RuntimeMetadata,
    RuntimeMetadataRequest, RuntimeSourceList, RuntimeSourceListRequest, RuntimeSourcePart,
    RuntimeSourceReadRequest, RuntimeSourceWriteReceipt, RuntimeSourceWriteRequest,
    UploadAssetResponse,
};
use artifree_domain::identity::ActorId;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::http::header::{self, HeaderValue};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};

/// What a runtime frame can ask the workspace to ask the server (decisions
/// 0019 and 0032). The workspace page sends these with the account token on
/// the frame's behalf; the frame itself holds none. They are admitted under
/// the frame budget, not the owner's.
pub(super) fn routes(state: &AppState) -> Router<AppState> {
    const VIEW: &str = "/v1/artifacts/{id}/revisions/{revision}/runtime-views/{view}/{generation}";
    Router::new()
        .route(
            &format!("{VIEW}/metadata"),
            axum::routing::post(runtime_metadata),
        )
        .route(
            &format!("{VIEW}/source/list"),
            axum::routing::post(runtime_source_list),
        )
        .route(
            &format!("{VIEW}/source/read"),
            axum::routing::post(runtime_source_read),
        )
        .route(
            &format!("{VIEW}/source/offer"),
            axum::routing::post(runtime_source_offer),
        )
        .route(
            &format!("{VIEW}/source/download"),
            axum::routing::post(runtime_source_download),
        )
        .route(
            &format!("{VIEW}/source/write"),
            axum::routing::post(runtime_source_write)
                .layer(DefaultBodyLimit::max(MAX_FRAME_WRITE_BODY_BYTES)),
        )
        .route(
            &format!("{VIEW}/assets/list"),
            axum::routing::post(runtime_assets_list),
        )
        .route(
            &format!("{VIEW}/assets/read"),
            axum::routing::post(runtime_assets_read),
        )
        .route(
            &format!("{VIEW}/assets/offer"),
            axum::routing::post(runtime_assets_offer),
        )
        .route(
            &format!("{VIEW}/assets/download"),
            axum::routing::post(runtime_assets_download),
        )
        .route(
            &format!("{VIEW}/assets/upload"),
            axum::routing::post(runtime_assets_upload)
                .layer(DefaultBodyLimit::max(MAX_FRAME_WRITE_BODY_BYTES)),
        )
        .route(
            &format!("{VIEW}/assets/delete"),
            axum::routing::post(runtime_assets_delete),
        )
        .route(
            &format!("{VIEW}/data/read"),
            axum::routing::post(runtime_data_read),
        )
        .method_not_allowed_fallback(method_not_allowed)
        .layer(DefaultBodyLimit::max(MAX_FRAME_BODY_BYTES))
        .layer(middleware::map_response(no_store))
        .layer(middleware::from_fn_with_state(state.clone(), admit_frame))
}

async fn runtime_metadata(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: FramePath,
    body: Result<Json<RuntimeMetadataRequest>, JsonRejection>,
) -> Result<Json<RuntimeMetadata>, ApiError> {
    let frame = Frame::parse(path, body)?;
    operations::runtime_metadata(
        &state,
        actor,
        &frame.artifact,
        frame.revision,
        &frame.view,
        frame.generation,
        &frame.request.initialization,
    )
    .await
    .map(Json)
}

type FramePath = Result<Path<(String, u64, String, u64)>, PathRejection>;

/// A frame call's body is built by the workspace page from what a frame
/// sent, so a parse failure says only that, with no serde text that could
/// quote the frame's input.
fn frame_body_error(rejection: &JsonRejection) -> ApiError {
    if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
        return ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            ErrorCode::PayloadTooLarge,
            "the frame call is too large",
        );
    }
    ApiError::invalid("the frame call is not a valid request")
}

/// A frame call's route names and body, read with these rejections: a bad
/// path is refused first, then a bad body through [`frame_body_error`].
struct Frame<R> {
    artifact: String,
    revision: u64,
    view: String,
    generation: u64,
    request: R,
}

impl<R> Frame<R> {
    fn parse(path: FramePath, body: Result<Json<R>, JsonRejection>) -> Result<Self, ApiError> {
        let Path((artifact, revision, view, generation)) = path?;
        let Json(request) = body.map_err(|rejection| frame_body_error(&rejection))?;
        Ok(Self {
            artifact,
            revision,
            view,
            generation,
            request,
        })
    }

    /// The call `actor` makes on this route with the body's `initialization`.
    fn call<'a>(&'a self, actor: &'a ActorId, initialization: &'a str) -> FrameCall<'a> {
        FrameCall {
            actor,
            artifact: &self.artifact,
            revision: self.revision,
            view: &self.view,
            generation: self.generation,
            initialization,
        }
    }
}

async fn runtime_source_list(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: FramePath,
    body: Result<Json<RuntimeSourceListRequest>, JsonRejection>,
) -> Result<Json<RuntimeSourceList>, ApiError> {
    let frame = Frame::parse(path, body)?;
    let call = frame.call(&actor, &frame.request.initialization);
    runtime_source::list(&state, &call).await.map(Json)
}

async fn runtime_source_read(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: FramePath,
    body: Result<Json<RuntimeSourceReadRequest>, JsonRejection>,
) -> Result<Json<RuntimeSourcePart>, ApiError> {
    let frame = Frame::parse(path, body)?;
    let call = frame.call(&actor, &frame.request.initialization);
    runtime_source::read(&state, &call, &frame.request)
        .await
        .map(Json)
}

async fn runtime_source_offer(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: FramePath,
    body: Result<Json<RuntimeDownloadRequest>, JsonRejection>,
) -> Result<Json<RuntimeDownloadOffer>, ApiError> {
    let frame = Frame::parse(path, body)?;
    let call = frame.call(&actor, &frame.request.initialization);
    runtime_source::offer(&state, &call, &frame.request)
        .await
        .map(Json)
}

/// Saves one file for the frame as the next revision (decision 0033).
async fn runtime_source_write(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: FramePath,
    body: Result<Json<RuntimeSourceWriteRequest>, JsonRejection>,
) -> Result<Json<RuntimeSourceWriteReceipt>, ApiError> {
    let frame = Frame::parse(path, body)?;
    let call = frame.call(&actor, &frame.request.initialization);
    runtime_source::write(&state, &call, &frame.request)
        .await
        .map(Json)
}

/// The bytes of a source download the user confirmed (decision 0032). It is a
/// `POST` so that no link or navigation can reach it.
async fn runtime_source_download(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: FramePath,
    body: Result<Json<RuntimeDownloadRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let frame = Frame::parse(path, body)?;
    let call = frame.call(&actor, &frame.request.initialization);
    let download = runtime_source::download(&state, &call, &frame.request).await?;
    Ok(attachment_response(download))
}

/// The bytes of a download the user confirmed, as an attachment that no
/// browser renders: a closed content type, `nosniff`, and a sandbox policy
/// that also holds if the response is ever opened as a document. Source and
/// asset downloads answer with it.
fn attachment_response(download: Download) -> Response {
    let Ok(disposition) =
        HeaderValue::from_str(&format!("attachment; filename=\"{}\"", download.file_name))
    else {
        return ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            ErrorCode::Internal,
            "the download name is not a header value",
        )
        .into_response();
    };
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static(download.content_type),
            ),
            (header::CONTENT_DISPOSITION, disposition),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
            (
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static("sandbox; default-src 'none'"),
            ),
        ],
        download.bytes,
    )
        .into_response()
}

async fn runtime_assets_list(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: FramePath,
    body: Result<Json<RuntimeAssetListRequest>, JsonRejection>,
) -> Result<Json<AssetList>, ApiError> {
    let frame = Frame::parse(path, body)?;
    let call = frame.call(&actor, &frame.request.initialization);
    runtime_assets::list(&state, &call).await.map(Json)
}

async fn runtime_assets_read(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: FramePath,
    body: Result<Json<RuntimeAssetReadRequest>, JsonRejection>,
) -> Result<Json<RuntimeAssetPart>, ApiError> {
    let frame = Frame::parse(path, body)?;
    let call = frame.call(&actor, &frame.request.initialization);
    runtime_assets::read(&state, &call, &frame.request)
        .await
        .map(Json)
}

/// What a download of one asset would be named and how large it is, before any
/// of its bytes are read (decision 0045, rule 12).
async fn runtime_assets_offer(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: FramePath,
    body: Result<Json<RuntimeAssetDownloadRequest>, JsonRejection>,
) -> Result<Json<RuntimeAssetDownloadOffer>, ApiError> {
    let frame = Frame::parse(path, body)?;
    let call = frame.call(&actor, &frame.request.initialization);
    runtime_assets::offer(&state, &call, &frame.request)
        .await
        .map(Json)
}

/// The bytes of one asset of the frame's artifact, once the user confirmed the
/// download (decision 0045, rule 12). It is a `POST` so that no link or
/// navigation can reach it.
async fn runtime_assets_download(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: FramePath,
    body: Result<Json<RuntimeAssetDownloadRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let frame = Frame::parse(path, body)?;
    let call = frame.call(&actor, &frame.request.initialization);
    let download = runtime_assets::download(&state, &call, &frame.request).await?;
    Ok(attachment_response(download))
}

/// Uploads one asset for the frame (decision 0045, rule 12).
async fn runtime_assets_upload(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: FramePath,
    body: Result<Json<RuntimeAssetUploadRequest>, JsonRejection>,
) -> Result<Json<UploadAssetResponse>, ApiError> {
    let frame = Frame::parse(path, body)?;
    let call = frame.call(&actor, &frame.request.initialization);
    runtime_assets::upload(&state, &call, &frame.request)
        .await
        .map(Json)
}

/// Deletes one asset of the frame's artifact (decision 0045, rule 12).
async fn runtime_assets_delete(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: FramePath,
    body: Result<Json<RuntimeAssetDeleteRequest>, JsonRejection>,
) -> Result<Json<DeleteAssetResponse>, ApiError> {
    let frame = Frame::parse(path, body)?;
    let call = frame.call(&actor, &frame.request.initialization);
    runtime_assets::delete(&state, &call, &frame.request)
        .await
        .map(Json)
}

/// The value at one path of the caller's application data for the artifact a
/// view is bound to, as JSON text (decision 0053, rules 4 and 5).
async fn runtime_data_read(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: FramePath,
    body: Result<Json<RuntimeDataReadRequest>, JsonRejection>,
) -> Result<Json<RuntimeDataValue>, ApiError> {
    let frame = Frame::parse(path, body)?;
    let call = frame.call(&actor, &frame.request.initialization);
    runtime_data::read(&state, &call, &frame.request)
        .await
        .map(Json)
}
