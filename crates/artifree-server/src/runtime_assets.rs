//! Asset listing and reads for runtime frames (decision 0045, rule 12).
//!
//! These functions follow the source reads in [`crate::runtime_source`]. Each
//! one is a frame call: it checks the binding, the connection's current
//! `assets.read` grant and the artifact's head before any work, and checks the
//! binding and the grant again after its last store read, with no `.await`
//! after that check.
//!
//! A read's last store read is its association lookup, and its head is checked
//! before that lookup, not after. A deletion committed before the lookup
//! releases no bytes, and a grant change, a connection close or a replacement
//! committed before the synchronous check that follows the lookup releases none
//! either. Two windows stay open: a deletion committed after the lookup has read
//! the store, and a source save committed after the head check, since assets do
//! not depend on the source head. A reply already returned is not recalled.

use artifree_contracts::api::{
    AssetList, ErrorCode, MAX_RUNTIME_ASSET_PART_BYTES, RuntimeAssetPart, RuntimeAssetReadRequest,
};
use axum::http::StatusCode;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;

use crate::auth::hex;
use crate::http::AppState;
use crate::operations::{ApiError, asset_id, list_assets};
use crate::runtime_policy::Capability;
use crate::runtime_source::{FrameCall, authorize, still_permitted};

/// The largest offset a frame may name: 2^53 - 1, which a double-precision
/// client holds exactly (decision 0032).
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// The asset space of the artifact a view is bound to, with its version and
/// totals. Assets are not part of a source revision, so the listing is the
/// artifact's current asset space; a view whose artifact head moved past its
/// pinned revision is still refused as stale (`409`), as for source reads.
pub(crate) async fn list(state: &AppState, call: &FrameCall<'_>) -> Result<AssetList, ApiError> {
    authorize(state, call, Capability::AssetsRead).await?;
    let listing = list_assets(state, call.actor.clone(), call.artifact).await?;
    authorize(state, call, Capability::AssetsRead).await?;
    Ok(listing)
}

/// One part of one asset of the artifact a view is bound to, cut at bytes.
///
/// The part is sliced from the whole asset, and `sha256` is the digest of the
/// whole asset, not of the part. An offset past the end of the asset is `416`,
/// and an offset at the end reads an empty final part. An asset that is not
/// associated with the artifact is `404`, and then no bytes are returned.
pub(crate) async fn read(
    state: &AppState,
    call: &FrameCall<'_>,
    request: &RuntimeAssetReadRequest,
) -> Result<RuntimeAssetPart, ApiError> {
    // Authorization first, so a caller without a binding learns nothing from
    // how its arguments were judged.
    let granted = authorize(state, call, Capability::AssetsRead).await?;
    if !(1..=MAX_RUNTIME_ASSET_PART_BYTES).contains(&request.length) {
        return Err(ApiError::invalid(format!(
            "length must be from 1 to {MAX_RUNTIME_ASSET_PART_BYTES}"
        )));
    }
    if request.offset > MAX_SAFE_INTEGER {
        return Err(ApiError::invalid("offset is not a safe integer"));
    }
    let asset = asset_id(&request.asset_id)?;
    // Every part reads the whole asset: its digest covers all of it, and an
    // asset is at most 5 MiB.
    let (actor, artifact, wanted) = (call.actor.clone(), granted.artifact.clone(), asset.clone());
    let content = state
        .run_assets(move |service| service.asset_content(&actor, &artifact, &wanted))
        .await?;
    let size = content.metadata.size;
    if request.offset > size {
        return Err(ApiError::unsatisfiable(
            size,
            format!("the range starts past the end of the asset, which has {size} bytes"),
        ));
    }
    // Checked: a sum past `u64::MAX` is past the end of the asset too.
    let end = request
        .offset
        .checked_add(request.length)
        .map_or(size, |sum| sum.min(size));
    let Some(bytes) = stored_part(&content.bytes, size, request.offset, end) else {
        return Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            ErrorCode::Internal,
            "the stored asset is not as its metadata states",
        ));
    };
    // The final check of the binding, the grant and the head, before the
    // association lookup that follows it.
    authorize(state, call, Capability::AssetsRead).await?;
    // The association lookup is the last store read. Its id and digest must
    // both still match what was read, so bytes go out only for an asset that
    // is still associated. A deletion committed before the lookup releases no
    // bytes; one committed after it is not stopped (see the module docs).
    let (actor, artifact) = (call.actor.clone(), granted.artifact.clone());
    let digest = content.metadata.digest;
    let listed = state
        .run_assets(move |service| service.assets(&actor, &artifact))
        .await?;
    let associated = listed
        .assets
        .iter()
        .any(|entry| entry.id == asset && entry.digest == digest);
    if !associated {
        return Err(ApiError::not_found("asset not found"));
    }
    // Synchronous, with no `.await` after it: a grant change, a connection
    // close or a replacement committed before this check releases no bytes.
    // The head is not checked again, since assets do not depend on it.
    still_permitted(state, call, &granted, Capability::AssetsRead)?;
    Ok(RuntimeAssetPart {
        asset_id: asset.as_str().to_owned(),
        content_type: content.metadata.content_type.as_str().to_owned(),
        offset: request.offset,
        next_offset: (end < size).then_some(end),
        size,
        sha256: hex(&digest),
        content: BASE64.encode(bytes),
    })
}

/// The bytes from `start` to `end` of `stored`, which must hold exactly `size`
/// bytes as the metadata states. `None` when it does not, or when the range
/// does not lie within it.
fn stored_part(stored: &[u8], size: u64, start: u64, end: u64) -> Option<&[u8]> {
    if u64::try_from(stored.len()).ok()? != size {
        return None;
    }
    let start = usize::try_from(start).ok()?;
    let end = usize::try_from(end).ok()?;
    stored.get(start..end)
}
