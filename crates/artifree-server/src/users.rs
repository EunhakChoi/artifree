//! Handlers for user management API v1 (decision 0036).
//!
//! Only administrators (the bootstrap token of the local owner profile) may
//! register, list, disable, and enable users; a token issued to any user,
//! including the owner profile's own agent tokens, is rejected with `403`.
//! The routes have no counterpart among the MCP tools: an agent cannot reach
//! them.

use artifree_contracts::api::{CreateUserRequest, UserInfo, UserList};
use artifree_domain::identity::{ActorId, UserLabel};
use artifree_ports::user_store::{NewUser, UserRecord, UserStore, UserStoreError};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};

use crate::http::{AppState, RequireAdmin};
use crate::operations::ApiError;
use crate::tokens::iso_now;

/// Routes for user management.
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/users", get(list_users).post(create_user))
        .route("/v1/users/{id}/disable", post(disable_user))
        .route("/v1/users/{id}/enable", post(enable_user))
}

fn info(record: UserRecord) -> UserInfo {
    UserInfo {
        is_owner: record.id.is_owner(),
        id: record.id.to_string(),
        label: record.label,
        created_at: record.created_at,
        disabled_at: record.disabled_at,
    }
}

fn store_error(error: UserStoreError) -> ApiError {
    match error {
        UserStoreError::NotFound(_) => ApiError::not_found("user"),
        UserStoreError::Conflict(message) => ApiError::conflict(message),
        UserStoreError::LimitReached => ApiError::conflict(error.to_string()),
        UserStoreError::Unavailable(message) => ApiError::unavailable(message),
    }
}

/// A user identifier from a route. A spelling that no user can have is the
/// same `404` as an unknown user.
fn user_id(text: &str) -> Result<ActorId, ApiError> {
    text.parse::<ActorId>()
        .ok()
        .filter(ActorId::is_registrable)
        .ok_or_else(|| ApiError::not_found("user"))
}

/// GET /v1/users: the registered users (requires admin).
async fn list_users(
    State(state): State<AppState>,
    RequireAdmin(_): RequireAdmin,
) -> Result<Response, ApiError> {
    let users = state.token_store().users().map_err(store_error)?;
    Ok(Json(UserList {
        users: users.into_iter().map(info).collect(),
    })
    .into_response())
}

/// POST /v1/users: registers a user (requires admin).
async fn create_user(
    State(state): State<AppState>,
    RequireAdmin(_): RequireAdmin,
    body: Result<Json<CreateUserRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(body) = body?;
    let label: UserLabel = body
        .label
        .parse()
        .map_err(|error| ApiError::invalid(format!("label: {error}")))?;
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|err| ApiError::unavailable(err.to_string()))?;
    let record = state
        .token_store()
        .register(&NewUser {
            id: ActorId::user(random),
            label,
            created_at: iso_now(),
        })
        .map_err(store_error)?;
    Ok((StatusCode::CREATED, Json(info(record))).into_response())
}

/// POST /v1/users/{id}/disable: refuses the user's tokens from now on and
/// closes what the server holds open for it (requires admin).
async fn disable_user(
    State(state): State<AppState>,
    RequireAdmin(_): RequireAdmin,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let id = user_id(&id)?;
    let record = state
        .token_store()
        .set_disabled(&id, Some(&iso_now()))
        .map_err(store_error)?;
    // After the registry changed: a request that authenticated before it
    // confirms the user again when it registers something (`ensure_enabled`).
    state.revoke_actor(&id);
    Ok(Json(info(record)).into_response())
}

/// POST /v1/users/{id}/enable: lets the user's tokens that are not revoked
/// authenticate again (requires admin). What was closed stays closed.
async fn enable_user(
    State(state): State<AppState>,
    RequireAdmin(_): RequireAdmin,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let id = user_id(&id)?;
    let record = state
        .token_store()
        .set_disabled(&id, None)
        .map_err(store_error)?;
    Ok(Json(info(record)).into_response())
}
