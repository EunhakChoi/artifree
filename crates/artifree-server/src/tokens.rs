//! Handlers for token management API v1.
//!
//! Only administrators (the owner/bootstrap token) may create, list,
//! rotate, and revoke access tokens. Agent tokens are rejected with 403 Forbidden.

use std::time::{SystemTime, UNIX_EPOCH};

use artifree_contracts::api::{
    CreateTokenRequest, CreateTokenResponse, RotateTokenResponse, TokenInfo, TokenList,
};
use artifree_domain::identity::TokenId;
use artifree_ports::token_store::{NewTokenRecord, TokenStore, TokenStoreError};
use artifree_providers::tokens::hash_token_secret;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};

use crate::auth::AccessToken;
use crate::http::{AppState, RequireAdmin};
use crate::operations::ApiError;

/// Routes for token management.
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/tokens", get(list_tokens).post(create_token))
        .route("/v1/tokens/{id}/rotate", axum::routing::post(rotate_token))
        .route("/v1/tokens/{id}", axum::routing::delete(revoke_token))
}

/// GET /v1/tokens: list all tokens (requires admin).
async fn list_tokens(
    State(state): State<AppState>,
    RequireAdmin(_): RequireAdmin,
) -> Result<Response, ApiError> {
    let records = state
        .token_store()
        .list()
        .map_err(|err| ApiError::unavailable(err.to_string()))?;

    let mut tokens = Vec::with_capacity(records.len() + 1);
    // Bootstrap token at the top of the list
    tokens.push(TokenInfo {
        id: "bootstrap".into(),
        name: "Bootstrap Owner".into(),
        prefix: state.owner_token().prefix(),
        created_at: state.started_at().to_owned(),
        last_used_at: None,
        revoked_at: None,
        is_admin: true,
    });
    for record in records {
        tokens.push(TokenInfo {
            id: record.id.to_string(),
            name: record.name,
            prefix: record.prefix,
            created_at: record.created_at,
            last_used_at: record.last_used_at,
            revoked_at: record.revoked_at,
            is_admin: false,
        });
    }

    Ok(Json(TokenList { tokens }).into_response())
}

/// POST /v1/tokens: create a new agent token (requires admin).
async fn create_token(
    State(state): State<AppState>,
    RequireAdmin(_): RequireAdmin,
    Json(body): Json<CreateTokenRequest>,
) -> Result<Response, ApiError> {
    let name = body.name.trim();
    if name.is_empty() || name.len() > 64 {
        return Err(ApiError::invalid(
            "token name must be between 1 and 64 characters",
        ));
    }

    let mut random_id = [0u8; 16];
    getrandom::fill(&mut random_id).map_err(|err| ApiError::unavailable(err.to_string()))?;
    let id = TokenId::from_random(random_id);

    let token = AccessToken::generate_agent_token()
        .map_err(|err| ApiError::unavailable(err.to_string()))?;
    let secret = token.secret().to_owned();
    let secret_hash = hash_token_secret(&secret);
    let prefix = token.prefix();
    let created_at = iso_now();

    let record = state
        .token_store()
        .create(&NewTokenRecord {
            id: id.clone(),
            name: name.to_owned(),
            secret_hash,
            prefix: prefix.clone(),
            created_at: created_at.clone(),
        })
        .map_err(|err| match err {
            TokenStoreError::Conflict(msg) => ApiError::conflict(msg),
            TokenStoreError::NotFound(msg) => ApiError::not_found(msg),
            TokenStoreError::Unavailable(msg) => ApiError::unavailable(msg),
        })?;

    let info = TokenInfo {
        id: record.id.to_string(),
        name: record.name,
        prefix: record.prefix,
        created_at: record.created_at,
        last_used_at: None,
        revoked_at: None,
        is_admin: false,
    };

    Ok((
        StatusCode::CREATED,
        Json(CreateTokenResponse {
            token: info,
            secret,
        }),
    )
        .into_response())
}

/// POST /v1/tokens/{id}/rotate: rotate an agent token (requires admin).
async fn rotate_token(
    State(state): State<AppState>,
    RequireAdmin(_): RequireAdmin,
    Path(id_raw): Path<String>,
) -> Result<Response, ApiError> {
    if id_raw == "bootstrap" {
        return Err(ApiError::forbidden(
            "bootstrap token cannot be rotated via API",
        ));
    }
    let id: TokenId = id_raw.parse().map_err(|_| ApiError::not_found("token"))?;

    let new_token = AccessToken::generate_agent_token()
        .map_err(|err| ApiError::unavailable(err.to_string()))?;
    let new_secret = new_token.secret().to_owned();
    let new_hash = hash_token_secret(&new_secret);
    let new_prefix = new_token.prefix();

    let record = state
        .token_store()
        .rotate(&id, &new_hash, &new_prefix)
        .map_err(|err| match err {
            TokenStoreError::Conflict(msg) => ApiError::conflict(msg),
            TokenStoreError::NotFound(msg) => ApiError::not_found(msg),
            TokenStoreError::Unavailable(msg) => ApiError::unavailable(msg),
        })?;

    // Invalidate sessions and streams tied to previous secret
    state.feed().revoke_token(&id);
    state.previews().revoke_token(&id);
    state.runtime_sessions().revoke_token(&id);

    let info = TokenInfo {
        id: record.id.to_string(),
        name: record.name,
        prefix: record.prefix,
        created_at: record.created_at,
        last_used_at: None,
        revoked_at: None,
        is_admin: false,
    };

    Ok(Json(RotateTokenResponse {
        token: info,
        secret: new_secret,
    })
    .into_response())
}

/// DELETE /v1/tokens/{id}: revoke an agent token (requires admin).
async fn revoke_token(
    State(state): State<AppState>,
    RequireAdmin(_): RequireAdmin,
    Path(id_raw): Path<String>,
) -> Result<Response, ApiError> {
    if id_raw == "bootstrap" {
        return Err(ApiError::forbidden(
            "bootstrap token cannot be revoked via API",
        ));
    }
    let id: TokenId = id_raw.parse().map_err(|_| ApiError::not_found("token"))?;

    state
        .token_store()
        .revoke(&id, &iso_now())
        .map_err(|err| match err {
            TokenStoreError::Conflict(msg) => ApiError::conflict(msg),
            TokenStoreError::NotFound(msg) => ApiError::not_found(msg),
            TokenStoreError::Unavailable(msg) => ApiError::unavailable(msg),
        })?;

    // Cascading session & stream revocation
    state.feed().revoke_token(&id);
    state.previews().revoke_token(&id);
    state.runtime_sessions().revoke_token(&id);

    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Returns current UTC timestamp in ISO 8601 format (`YYYY-MM-DDTHH:MM:SSZ`).
pub(crate) fn iso_now() -> String {
    let timestamp_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format_iso8601(timestamp_secs)
}

fn format_iso8601(timestamp_secs: u64) -> String {
    let secs = timestamp_secs % 86400;
    let days = timestamp_secs / 86400;
    let hour = secs / 3600;
    let minute = (secs % 3600) / 60;
    let second = secs % 60;

    // Civil date from day count using Howard Hinnant's algorithm
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}T{hour:02}:{minute:02}:{second:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso8601_epoch() {
        assert_eq!(format_iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_iso8601(86400), "1970-01-02T00:00:00Z");
        // 2026-11-01 00:00:00 UTC = 1793491200
        assert_eq!(format_iso8601(1_793_491_200), "2026-11-01T00:00:00Z");
    }
}
