//! Authenticated bounded asset HTTP adapters; policy and mutation live in Rust core.

use super::{AppState, Authenticated, IdempotencyKey};
use crate::operations::{self, ApiError};
use artifree_contracts::api::{
    AssetList, AssetResponse, DeleteAssetRequest, DeleteAssetResponse, ErrorCode,
    UploadAssetRequest,
};
use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

pub(super) fn routes() -> axum::Router<AppState> {
    use axum::extract::DefaultBodyLimit;
    use axum::routing::get;
    axum::Router::new()
        .route(
            "/v1/artifacts/{id}/assets",
            get(list)
                .post(upload)
                .layer(DefaultBodyLimit::max(MAX_BODY)),
        )
        .route(
            "/v1/artifacts/{id}/assets/{asset_id}",
            get(metadata)
                .delete(delete)
                .layer(DefaultBodyLimit::max(MAX_BODY)),
        )
        .route("/v1/artifacts/{id}/assets/{asset_id}/content", get(content))
}

pub(super) const MAX_BODY: usize = 8 * 1024 * 1024;
fn body_error(error: &JsonRejection) -> ApiError {
    if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
        ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            ErrorCode::PayloadTooLarge,
            "asset body exceeds 8 MiB",
        )
    } else {
        ApiError::invalid(error.body_text())
    }
}
pub(super) async fn list(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<String>, PathRejection>,
) -> Result<Json<AssetList>, ApiError> {
    let Path(id) = path?;
    Ok(Json(operations::list_assets(&state, actor, &id).await?))
}
pub(super) async fn upload(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    IdempotencyKey(key): IdempotencyKey,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<UploadAssetRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Path(id) = path?;
    // An unknown artifact is reported before a malformed body.
    operations::artifact_id(&id)?;
    let Json(request) = body.map_err(|error| body_error(&error))?;
    let receipt = operations::upload_asset(&state, actor, key, &id, request).await?;
    let location = format!("/v1/artifacts/{id}/assets/{}", receipt.asset.id);
    Ok((
        StatusCode::CREATED,
        [(header::LOCATION, location)],
        Json(receipt),
    )
        .into_response())
}
pub(super) async fn metadata(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<(String, String)>, PathRejection>,
) -> Result<Json<AssetResponse>, ApiError> {
    let Path((artifact, id)) = path?;
    let content = operations::asset_content(&state, actor, &artifact, &id).await?;
    Ok(Json(AssetResponse {
        asset_version: content.version.0,
        asset: operations::asset_dto(&content.metadata),
    }))
}
pub(super) async fn content(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<(String, String)>, PathRejection>,
) -> Result<Response, ApiError> {
    let Path((artifact, id)) = path?;
    let content = operations::asset_content(&state, actor, &artifact, &id).await?;
    Ok((
        [
            (
                header::CONTENT_TYPE,
                content.metadata.content_type.as_str().to_owned(),
            ),
            (
                header::CONTENT_DISPOSITION,
                format!(
                    "attachment; filename=asset-{}",
                    content.metadata.id.as_str()
                ),
            ),
            (
                header::CONTENT_SECURITY_POLICY,
                "sandbox; default-src 'none'".into(),
            ),
        ],
        content.bytes,
    )
        .into_response())
}
pub(super) async fn delete(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    IdempotencyKey(key): IdempotencyKey,
    path: Result<Path<(String, String)>, PathRejection>,
    body: Result<Json<DeleteAssetRequest>, JsonRejection>,
) -> Result<Json<DeleteAssetResponse>, ApiError> {
    let Path((artifact, id)) = path?;
    // An unknown artifact or asset is reported before a malformed body.
    operations::artifact_id(&artifact)?;
    operations::asset_id(&id)?;
    let Json(request) = body.map_err(|error| body_error(&error))?;
    let receipt = operations::delete_asset(&state, actor, key, &artifact, &id, request).await?;
    Ok(Json(receipt))
}
