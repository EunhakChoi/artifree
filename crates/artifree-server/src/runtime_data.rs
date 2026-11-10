//! Application data reads for runtime frames (decision 0053, R14a).
//!
//! `data.get` is a frame call. It checks the binding, the connection's current
//! grant of `data.read` and the artifact's head before it judges the path, so a
//! caller refused by an earlier check learns nothing from how its path was
//! judged. It checks all three again after its store read, before it returns
//! the value (rule 8). The value leaves as its canonical JSON text, the form
//! the store keeps (rule 5). A read answers at most one space, so the text is
//! at most 1,048,576 bytes (rule 11). The calls address the calling user's own
//! data space, as the owner's data routes do.

use artifree_contracts::api::{RuntimeDataReadRequest, RuntimeDataValue};

use crate::http::AppState;
use crate::operations::{ApiError, data_path};
use crate::runtime_policy::Capability;
use crate::runtime_source::{FrameCall, authorize};

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
    let read = state
        .run(move |artifacts| artifacts.data(&actor, &artifact, &path))
        .await?;
    let text = read.value.map(|value| value.to_canonical());
    let answer = RuntimeDataValue {
        artifact_id: granted.artifact.to_string(),
        path: wire_path,
        data_version: read.version.0,
        exists: text.is_some(),
        value: text,
    };
    // The binding, the grant and the head again, before the value is returned.
    // No store access follows this check.
    authorize(state, call, Capability::DataRead).await?;
    Ok(answer)
}
