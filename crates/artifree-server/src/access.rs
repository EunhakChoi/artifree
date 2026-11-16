//! Handlers for the access reads of decision 0060 rule 6 (slice S02e1): the
//! caller's own id and label, and an artifact's access list.
//!
//! Changing access (grant, role change, revoke) is not part of this slice.
//! Each read reveals only what rule 8 allows: an owner and the members of one
//! artifact see each other's id, label and role there, and only the owner is
//! told which members are disabled; the application core decides both.

use artifree_contracts::api::{AccessMember, AccessUser, ArtifactAccess, Me, MemberRole};
use artifree_core::artifacts::AccessView;
use artifree_domain::access::Role;
use artifree_ports::user_store::UserStore;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};

use crate::http::{AppState, Authenticated};
use crate::operations::{self, ApiError};
use crate::users;

/// Routes for the access reads.
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/me", get(me))
        .route("/v1/artifacts/{id}/access", get(read_access))
}

/// GET /v1/me: the caller's id and label, so that a user can hand their id
/// to an owner. It names no artifact and is open to every authenticated
/// caller, an agent token of the owner profile included.
async fn me(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
) -> Result<Json<Me>, ApiError> {
    let id = actor.clone();
    let user = state
        .identity(move |store| store.user(&id))
        .await?
        .map_err(users::store_error)?
        // A caller authenticated, so its user exists; one removed meanwhile
        // is answered as a token nobody issued.
        .ok_or_else(ApiError::unauthenticated)?;
    Ok(Json(Me {
        id: actor.to_string(),
        label: user.label,
    }))
}

/// GET /v1/artifacts/{id}/access: the owner, the members and the access
/// version, for the owner and the members. Anyone else gets the `404` of an
/// unknown artifact (rule 4).
async fn read_access(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<String>, PathRejection>,
) -> Result<Json<ArtifactAccess>, ApiError> {
    let Path(id) = path?;
    let id = operations::artifact_id(&id)?;
    let artifact_id = id.to_string();
    let view = state
        .run(move |artifacts| artifacts.access(&actor, &id))
        .await?;
    Ok(Json(access_body(artifact_id, view)))
}

fn access_body(artifact_id: String, view: AccessView) -> ArtifactAccess {
    ArtifactAccess {
        artifact_id,
        owner: AccessUser {
            id: view.owner.to_string(),
            label: view.owner_label.to_string(),
        },
        members: view
            .members
            .into_iter()
            .map(|member| AccessMember {
                id: member.user.to_string(),
                label: member.label.to_string(),
                role: match member.role {
                    Role::Editor => MemberRole::Editor,
                    Role::Viewer => MemberRole::Viewer,
                },
                disabled: member.disabled,
            })
            .collect(),
        access_version: view.version.0,
    }
}
