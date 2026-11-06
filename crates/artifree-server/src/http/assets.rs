//! Authenticated bounded asset HTTP adapters; policy and mutation live in Rust core.

use super::{AppState, Authenticated, IdempotencyKey};
use crate::operations::{self, ApiError};
use artifree_contracts::api::{
    Asset, AssetList, AssetResponse, DeleteAssetRequest, DeleteAssetResponse, ErrorCode,
    UploadAssetRequest, UploadAssetResponse,
};
use artifree_core::assets::AssetUpload;
use artifree_domain::assets::{
    AssetId, AssetMediaType, AssetMetadata, AssetRejection, AssetVersion, MAX_ASSET_BYTES,
    MAX_SPACE_ASSETS, MAX_SPACE_BYTES,
};
use artifree_domain::identity::UnixMillis;
use artifree_ports::artifact_store::Delivery;
use artifree_ports::asset_store::AssetStoreError;
use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;

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
fn store_error(error: &AssetStoreError) -> ApiError {
    let message = error.to_string();
    match error {
        AssetStoreError::NotFound => {
            ApiError::new(StatusCode::NOT_FOUND, ErrorCode::NotFound, message)
        }
        AssetStoreError::RequestKeyReused => ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            ErrorCode::RequestKeyReused,
            message,
        ),
        AssetStoreError::Conflict { current } => {
            let mut error = ApiError::new(StatusCode::CONFLICT, ErrorCode::AssetConflict, message);
            error.body.current_asset_version = Some(current.0);
            error
        }
        AssetStoreError::SourceConflict { current } => {
            let mut error =
                ApiError::new(StatusCode::CONFLICT, ErrorCode::RevisionConflict, message);
            error.body.current_revision = Some(current.0);
            error
        }
        AssetStoreError::Rejected(reason) => ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            match reason {
                AssetRejection::UnsupportedType => ErrorCode::AssetTypeUnsupported,
                AssetRejection::LimitExceeded => ErrorCode::AssetLimitExceeded,
                _ => ErrorCode::InvalidAsset,
            },
            message,
        ),
        _ => ApiError::unavailable(
            "asset storage unavailable; retry an identical mutation with the same key",
        ),
    }
}
fn wire(metadata: &AssetMetadata) -> Asset {
    let sha256 = crate::auth::hex(&metadata.digest);
    Asset {
        id: metadata.id.as_str().to_owned(),
        content_type: metadata.content_type.as_str().to_owned(),
        size_bytes: metadata.size,
        sha256,
        created_at_ms: metadata.created_at.0,
    }
}
fn asset_id(value: &str) -> Result<AssetId, ApiError> {
    value.parse().map_err(|_| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            ErrorCode::NotFound,
            "asset not found",
        )
    })
}
fn base(value: u64) -> Result<AssetVersion, ApiError> {
    if value > 9_007_199_254_740_991 {
        return Err(ApiError::invalid(
            "base_asset_version must be a safe unsigned integer",
        ));
    }
    Ok(AssetVersion(value))
}
pub(super) async fn list(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<String>, PathRejection>,
) -> Result<Json<AssetList>, ApiError> {
    let Path(id) = path?;
    let artifact = operations::artifact_id(&id)?;
    let snapshot = state
        .run_assets(move |service| service.assets(&actor, &artifact))
        .await
        .map_err(|error| store_error(&error))?;
    Ok(Json(AssetList {
        asset_version: snapshot.version.0,
        assets: snapshot.assets.iter().map(wire).collect(),
        bytes: snapshot.bytes,
        max_assets: MAX_SPACE_ASSETS,
        max_bytes: MAX_SPACE_BYTES,
    }))
}
pub(super) async fn upload(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    IdempotencyKey(key): IdempotencyKey,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<UploadAssetRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Path(id) = path?;
    let artifact = operations::artifact_id(&id)?;
    let Json(request) = body.map_err(|error| body_error(&error))?;
    let base = base(request.base_asset_version)?;
    let media: AssetMediaType = request
        .content_type
        .parse()
        .map_err(|error| ApiError::invalid(format!("content_type: {error}")))?;
    // Bound before allocation; STANDARD refuses URL-safe alphabets, missing padding,
    // noncanonical trailing bits and whitespace. Empty input belongs to content policy.
    let max_encoded = MAX_ASSET_BYTES.div_ceil(3) * 4;
    if request.content_base64.len() > max_encoded {
        return Err(store_error(&AssetStoreError::Rejected(
            AssetRejection::LimitExceeded,
        )));
    }
    let bytes = STANDARD
        .decode(&request.content_base64)
        .map_err(|_| ApiError::invalid("content_base64 must be strict standard base64"))?;
    if bytes.len() > MAX_ASSET_BYTES {
        return Err(store_error(&AssetStoreError::Rejected(
            AssetRejection::LimitExceeded,
        )));
    }
    let now = UnixMillis(state.shared.settings.clock.now().0);
    let receipt = state
        .run_assets(move |service| {
            service.upload_asset(
                &actor,
                &key,
                &artifact,
                AssetUpload {
                    base,
                    content_type: &media,
                    bytes: &bytes,
                    now,
                },
            )
        })
        .await
        .map_err(|error| store_error(&error))?;
    let metadata = receipt
        .uploaded
        .ok_or_else(|| ApiError::unavailable("upload receipt has no metadata"))?;
    let location = format!("/v1/artifacts/{id}/assets/{}", metadata.id.as_str());
    let body = UploadAssetResponse {
        asset_version: receipt.version.0,
        asset: wire(&metadata),
        replayed: receipt.delivery == Delivery::Replayed,
    };
    Ok((
        StatusCode::CREATED,
        [(header::LOCATION, location)],
        Json(body),
    )
        .into_response())
}
pub(super) async fn metadata(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<(String, String)>, PathRejection>,
) -> Result<Json<AssetResponse>, ApiError> {
    let Path((artifact, id)) = path?;
    let artifact = operations::artifact_id(&artifact)?;
    let id = asset_id(&id)?;
    let content = state
        .run_assets(move |service| service.asset_content(&actor, &artifact, &id))
        .await
        .map_err(|error| store_error(&error))?;
    Ok(Json(AssetResponse {
        asset_version: content.version.0,
        asset: wire(&content.metadata),
    }))
}
pub(super) async fn content(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<(String, String)>, PathRejection>,
) -> Result<Response, ApiError> {
    let Path((artifact, id)) = path?;
    let artifact = operations::artifact_id(&artifact)?;
    let id = asset_id(&id)?;
    let content = state
        .run_assets(move |service| service.asset_content(&actor, &artifact, &id))
        .await
        .map_err(|error| store_error(&error))?;
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
    let artifact = operations::artifact_id(&artifact)?;
    let id = asset_id(&id)?;
    let Json(request) = body.map_err(|error| body_error(&error))?;
    let base = base(request.base_asset_version)?;
    let now = state.shared.settings.clock.now();
    let receipt = state
        .run_assets(move |service| service.delete_asset(&actor, &key, &artifact, base, &id, now))
        .await
        .map_err(|error| store_error(&error))?;
    Ok(Json(DeleteAssetResponse {
        asset_id: receipt.id.as_str().to_owned(),
        asset_version: receipt.version.0,
        deleted: receipt.deleted,
        replayed: receipt.delivery == Delivery::Replayed,
    }))
}
