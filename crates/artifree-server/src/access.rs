//! Handlers for the access routes of decision 0060 rule 6: the caller's own
//! id and label, an artifact's access list (slice S02e1), and the owner's
//! grant, role change and revocation (slice S02e2).
//!
//! Each read reveals only what rule 8 allows: an owner and the members of one
//! artifact see each other's id, label and role there, and only the owner is
//! told which members are disabled; the application core decides both.

use artifree_contracts::api::{
    AccessChangeResponse, AccessMember, AccessUser, ArtifactAccess, Me, MemberRole,
    RevokeAccessRequest, SetAccessRequest,
};
use artifree_core::artifacts::{AccessView, ArtifactError, Missing};
use artifree_domain::access::{AccessVersion, Need, Role};
use artifree_domain::identity::{ActorId, ArtifactId, RequestKey};
use artifree_ports::access_store::AccessChange;
use artifree_ports::artifact_store::Delivery;
use artifree_ports::user_store::UserStore;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};

use crate::http::{AppState, Authenticated, IdempotencyKey, RequireAccessChange};
use crate::operations::{self, ApiError};
use crate::users;

/// Routes for the access reads.
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/me", get(me))
        .route("/v1/artifacts/{id}/access", get(read_access))
        .route(
            "/v1/artifacts/{id}/access/{user_id}",
            axum::routing::put(set_access).delete(revoke_access),
        )
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

/// `PUT /v1/artifacts/{id}/access/{user_id}`: grants the user a role, or
/// changes the role they have. See [`change`].
async fn set_access(
    State(state): State<AppState>,
    _: RequireAccessChange,
    Authenticated(actor): Authenticated,
    IdempotencyKey(key): IdempotencyKey,
    path: Result<Path<(String, String)>, PathRejection>,
    body: Result<Json<SetAccessRequest>, JsonRejection>,
) -> Result<Json<AccessChangeResponse>, ApiError> {
    let Path((id, user)) = path?;
    let id = operations::artifact_id(&id)?;
    let request = admitted(&state, &actor, &id, body).await?;
    // A spelling no user can have is a user nobody registered: the one
    // answer of rule 6 for an unknown, a disabled and the owner's id.
    let user = registrable(&user).ok_or(ArtifactError::NotGrantable)?;
    let role = match request.role {
        MemberRole::Editor => Role::Editor,
        MemberRole::Viewer => Role::Viewer,
    };
    let base = AccessVersion(request.base_access_version);
    change(
        &state,
        actor,
        key,
        id,
        user,
        AccessChange::Grant(role),
        base,
    )
    .await
}

/// `DELETE /v1/artifacts/{id}/access/{user_id}`: ends the user's membership.
/// A user who is not a member is `404`. See [`change`].
async fn revoke_access(
    State(state): State<AppState>,
    _: RequireAccessChange,
    Authenticated(actor): Authenticated,
    IdempotencyKey(key): IdempotencyKey,
    path: Result<Path<(String, String)>, PathRejection>,
    body: Result<Json<RevokeAccessRequest>, JsonRejection>,
) -> Result<Json<AccessChangeResponse>, ApiError> {
    let Path((id, user)) = path?;
    let id = operations::artifact_id(&id)?;
    let request = admitted(&state, &actor, &id, body).await?;
    let user = registrable(&user).ok_or(ArtifactError::NotFound(Missing::Member))?;
    let base = AccessVersion(request.base_access_version);
    change(&state, actor, key, id, user, AccessChange::Revoke, base).await
}

fn registrable(user: &str) -> Option<ActorId> {
    user.parse::<ActorId>().ok().filter(ActorId::is_registrable)
}

/// The body, once the caller is known to own the artifact: rule 4's order is
/// the artifact's access (`404`), then the role (`403`), then the body, so a
/// member learns nothing about a change's body by sending it.
async fn admitted<T>(
    state: &AppState,
    actor: &ActorId,
    id: &ArtifactId,
    body: Result<Json<T>, JsonRejection>,
) -> Result<T, ApiError> {
    let (actor, artifact) = (actor.clone(), id.clone());
    let owns = state
        .run(move |artifacts| artifacts.allows(&actor, &artifact, Need::Own))
        .await?;
    if !owns {
        return Err(ArtifactError::Forbidden.into());
    }
    let Json(request) = body?;
    Ok(request)
}

/// Commits a change of access and, if this call committed it, removes from
/// memory what the user may no longer hold (rule 15): everything of the
/// artifact for a revocation, the capabilities a viewer is not offered for a
/// grant of `viewer`. A replay removes nothing: what it would remove was
/// removed when it committed, and anything held since was granted after it.
pub(crate) async fn change(
    state: &AppState,
    actor: ActorId,
    key: RequestKey,
    id: ArtifactId,
    user: ActorId,
    change: AccessChange,
    base: AccessVersion,
) -> Result<Json<AccessChangeResponse>, ApiError> {
    let now = state.clock().now();
    let (artifact, member) = (id.clone(), user.clone());
    let receipt = state
        .run(move |artifacts| {
            artifacts.change_access(&actor, &key, &artifact, &member, change, base, now)
        })
        .await?;
    if receipt.delivery == Delivery::Committed {
        match change {
            AccessChange::Revoke => state.revoke_members(std::slice::from_ref(&user), &id),
            AccessChange::Grant(Role::Viewer) => {
                state.downgrade_members(std::slice::from_ref(&user), &id);
            }
            AccessChange::Grant(Role::Editor) => {}
        }
        state.feed().committed();
    }
    Ok(Json(AccessChangeResponse {
        artifact_id: id.to_string(),
        user_id: user.to_string(),
        access_version: receipt.version.0,
        replayed: receipt.delivery == Delivery::Replayed,
    }))
}
