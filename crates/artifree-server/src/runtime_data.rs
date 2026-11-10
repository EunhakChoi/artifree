//! Application data for runtime frames (decision 0053, R14a and R14b).
//!
//! `data.get` is a frame call. It checks the binding, the connection's current
//! grant of `data.read` and the artifact's head before it judges the path, so a
//! caller refused by an earlier check learns nothing from how its path was
//! judged. It checks all three again after its store read, before it returns
//! the value (rule 8). The value leaves as its canonical JSON text, the form
//! the store keeps (rule 5). A read answers at most one space, so the text is
//! at most 1,048,576 bytes (rule 11). The calls address the calling user's own
//! data space, as the owner's data routes do.
//!
//! `data.set`, `data.update` and `data.remove` are frame calls that need the
//! connection's grant of `data.write`. They check the binding, the grant and
//! the head, in that order, and only then judge the path, the retry identity,
//! the base version and the JSON text (rule 8). A write goes through the data
//! service with the frame's base version and a key only a frame can produce
//! (rule 9). The store checks the view's source head inside its transaction, so
//! a write from a view that is no longer current is refused, and a retry after
//! the head moved is refused too, not replayed (rule 10). The grant is checked
//! again on the blocking thread before the store is asked and once more after
//! the commit; a grant revoked in between withholds the receipt, and the commit
//! stands (rule 10). A write that changes the space is recorded in the change
//! feed and wakes the streams, as the owner's write does (rule 12).

use artifree_contracts::api::{
    DataWriteResponse, RuntimeDataDeleteRequest, RuntimeDataReadRequest, RuntimeDataSetRequest,
    RuntimeDataUpdateRequest, RuntimeDataValue,
};
use artifree_core::data::DataMutation;
use artifree_domain::AppDataVersion;
use artifree_domain::data::DataChange;
use artifree_domain::identity::RequestKey;
use artifree_ports::artifact_store::Delivery;

use crate::http::AppState;
use crate::operations::{ApiError, data_base, data_path, data_text};
use crate::runtime_policy::Capability;
use crate::runtime_source::{Authorized, FrameCall, HeldCall, authorize, still_permitted};

/// The value at `request.path` of the caller's data space of the artifact a
/// view is bound to, as canonical JSON text. A path with nothing at it, or one
/// that goes through a value that is not an object, is a successful read with
/// `exists` false and no value (decision 0031, rule 5).
pub(crate) async fn read(
    state: &AppState,
    call: &FrameCall<'_>,
    request: &RuntimeDataReadRequest,
) -> Result<RuntimeDataValue, ApiError> {
    // Authorization first: the binding (404), the grant (403), the head (409).
    // The path is judged only after all three pass.
    let granted = authorize(state, call, Capability::DataRead).await?;
    let path = data_path("path", &request.path)?;
    let wire_path = path.to_string();
    let (actor, artifact) = (call.actor.clone(), granted.artifact.clone());
    // The canonical text is written on the blocking thread, with the read.
    let (version, text) = state
        .run(move |artifacts| {
            let read = artifacts.data(&actor, &artifact, &path)?;
            Ok((read.version, read.value.map(|value| value.to_canonical())))
        })
        .await?;
    let answer = RuntimeDataValue {
        artifact_id: granted.artifact.to_string(),
        path: wire_path,
        data_version: version.0,
        exists: text.is_some(),
        value: text,
    };
    // The binding, the grant and the head again, before the value is returned.
    // No store access follows this check.
    authorize(state, call, Capability::DataRead).await?;
    Ok(answer)
}

/// Replaces the value at `request.path` of the caller's data space of the
/// artifact a view is bound to, for a frame with the `data.write` grant. `null`
/// is stored as a value. The answer is the owner's [`DataWriteResponse`]
/// (decision 0031).
pub(crate) async fn set(
    state: &AppState,
    call: &FrameCall<'_>,
    request: &RuntimeDataSetRequest,
) -> Result<DataWriteResponse, ApiError> {
    // Authorization first, so a caller without a binding or the grant learns
    // nothing from how its arguments were judged.
    let granted = authorize(state, call, Capability::DataWrite).await?;
    let path = data_path("path", &request.path)?;
    let key = RequestKey::runtime_data_write(
        &granted.artifact,
        call.view,
        call.generation,
        &request.retry_id,
    )
    .map_err(|error| ApiError::invalid(format!("retry_id: {error}")))?;
    let base = data_base(request.base_data_version)?;
    let value = data_text("value", &request.value)?;
    write(
        state,
        call,
        &granted,
        key,
        base,
        DataChange::Set { path, value },
    )
    .await
}

/// Applies the JSON Merge Patch in `request.patch` at `request.path` of the
/// caller's data space of the artifact a view is bound to, for a frame with the
/// `data.write` grant (RFC 7396, decision 0031). The answer is the owner's
/// [`DataWriteResponse`].
pub(crate) async fn update(
    state: &AppState,
    call: &FrameCall<'_>,
    request: &RuntimeDataUpdateRequest,
) -> Result<DataWriteResponse, ApiError> {
    // The same checks, in the same order, as `set`.
    let granted = authorize(state, call, Capability::DataWrite).await?;
    let path = data_path("path", &request.path)?;
    let key = RequestKey::runtime_data_write(
        &granted.artifact,
        call.view,
        call.generation,
        &request.retry_id,
    )
    .map_err(|error| ApiError::invalid(format!("retry_id: {error}")))?;
    let base = data_base(request.base_data_version)?;
    let merge = data_text("patch", &request.patch)?;
    write(
        state,
        call,
        &granted,
        key,
        base,
        DataChange::Merge { path, patch: merge },
    )
    .await
}

/// Removes the value at `request.path` of the caller's data space of the
/// artifact a view is bound to, for a frame with the `data.write` grant. A path
/// with nothing at it changes nothing. The answer is the owner's
/// [`DataWriteResponse`].
pub(crate) async fn remove(
    state: &AppState,
    call: &FrameCall<'_>,
    request: &RuntimeDataDeleteRequest,
) -> Result<DataWriteResponse, ApiError> {
    // The same checks, in the same order, as `set`.
    let granted = authorize(state, call, Capability::DataWrite).await?;
    let path = data_path("path", &request.path)?;
    let key = RequestKey::runtime_data_write(
        &granted.artifact,
        call.view,
        call.generation,
        &request.retry_id,
    )
    .map_err(|error| ApiError::invalid(format!("retry_id: {error}")))?;
    let base = data_base(request.base_data_version)?;
    write(
        state,
        call,
        &granted,
        key,
        base,
        DataChange::Remove { path },
    )
    .await
}

/// Commits one data write for a frame and returns its receipt. The store fences
/// the view's pinned source head inside the commit (rule 10). The grant is
/// checked again on the blocking thread just before the store is asked, so a
/// denial releases nothing to it.
async fn write(
    state: &AppState,
    call: &FrameCall<'_>,
    granted: &Authorized,
    key: RequestKey,
    base: AppDataVersion,
    change: DataChange,
) -> Result<DataWriteResponse, ApiError> {
    let now = state.clock().now();
    let wire_path = match &change {
        DataChange::Set { path, .. }
        | DataChange::Merge { path, .. }
        | DataChange::Remove { path } => path.to_string(),
    };
    let (actor, artifact, source) = (
        call.actor.clone(),
        granted.artifact.clone(),
        granted.pinned(),
    );
    let held = HeldCall::new(call, granted);
    let app = state.clone();
    let outcome = state
        .run(move |service| {
            // The outer result is the store's and the inner one the binding's.
            if let Err(denial) = held.permitted(app.previews(), Capability::DataWrite) {
                return Ok(Err(denial));
            }
            service
                .runtime_write_data(
                    &actor,
                    &key,
                    &artifact,
                    DataMutation {
                        base,
                        change: &change,
                        now,
                        fence: None,
                    },
                    source,
                )
                .map(Ok)
        })
        .await??;
    // A write that changed the space appended a `data_changed` row to the
    // actor's feed (decision 0040); wake the streams as a commit does. A replay,
    // and a write that changed nothing, recorded no row.
    if outcome.delivery == Delivery::Committed && outcome.changed {
        state.feed().committed();
    }
    // A commit that landed is not undone, but a grant that went meanwhile
    // withholds the receipt.
    still_permitted(state, call, granted, Capability::DataWrite)?;
    Ok(DataWriteResponse {
        artifact_id: granted.artifact.to_string(),
        path: wire_path,
        data_version: outcome.version.0,
        changed: outcome.changed,
        replayed: outcome.delivery == Delivery::Replayed,
    })
}
