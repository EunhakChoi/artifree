//! Source reads and downloads for runtime frames (decision 0032).
//!
//! Every function here is one frame call proxied by the workspace page: the
//! frame holds no account token, so the page presents the view's
//! initialization secret on the frame's behalf. Each call checks the whole
//! binding, the connection's current grant of the one capability it needs,
//! the live artifact, and that the artifact's head is still the one the view
//! was pinned to. It checks all of that again after its asynchronous work,
//! so a revoke, a replaced view or a new revision that lands in between
//! releases no bytes.

use std::num::NonZeroU64;

use artifree_contracts::api::{
    ErrorCode, MAX_DOWNLOAD_NAME, MAX_RUNTIME_SOURCE_PART_BYTES, RuntimeDownloadKind,
    RuntimeDownloadOffer, RuntimeDownloadRequest, RuntimeSourceList, RuntimeSourcePart,
    RuntimeSourceReadRequest,
};
use artifree_core::parts::{Boundary, ByteRange};
use artifree_domain::SourceRevision;
use artifree_domain::identity::{ActorId, ArtifactId};
use artifree_domain::package::PackagePath;
use axum::http::StatusCode;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;

use crate::auth::hex;
use crate::http::AppState;
use crate::operations::{
    ApiError, artifact_id, missing_view, package_path, revision_dto, revision_number, validate_view,
};
use crate::preview::{RuntimeDenied, ViewKey};
use crate::runtime_policy::Capability;

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// One frame call: the route's names and the secret the frame presented.
pub(crate) struct FrameCall<'a> {
    pub(crate) actor: &'a ActorId,
    pub(crate) artifact: &'a str,
    pub(crate) revision: u64,
    pub(crate) view: &'a str,
    pub(crate) generation: u64,
    pub(crate) initialization: &'a str,
}

/// A download after the user confirmed it.
pub(crate) struct Download {
    pub(crate) bytes: Vec<u8>,
    pub(crate) content_type: &'static str,
    pub(crate) file_name: String,
}

struct Authorized {
    artifact: ArtifactId,
    revision: SourceRevision,
}

fn denied(reason: RuntimeDenied) -> ApiError {
    match reason {
        // The same answer for a forged, replaced, closed or expired view and
        // for a connection that is gone, so only a holder of the view's
        // secrets can learn anything else.
        RuntimeDenied::Binding => missing_view(),
        RuntimeDenied::Capability => {
            ApiError::forbidden("the runtime connection has not granted this capability")
        }
    }
}

/// The view's pinned head is not the artifact's head any more. The grant is
/// scoped to that head (decision 0020), so the workspace starts a new view
/// and the user consents again.
fn stale(current: SourceRevision) -> ApiError {
    let mut error = ApiError::new(
        StatusCode::CONFLICT,
        ErrorCode::RevisionConflict,
        "the view is older than the artifact's current source; start a new view",
    );
    error.body.current_revision = Some(current.0);
    error
}

/// The full live check of one call for one capability. Binding failures are
/// `404`, a missing grant after a matching binding is `403`, a moved head is
/// `409`, and a trashed or purged artifact is `404`.
async fn authorize(
    state: &AppState,
    call: &FrameCall<'_>,
    capability: Capability,
) -> Result<Authorized, ApiError> {
    validate_view(call.view, call.generation)?;
    let artifact = artifact_id(call.artifact)?;
    let revision = revision_number(call.revision)?;
    let key = ViewKey {
        actor: call.actor,
        artifact: &artifact,
        revision,
        view_id: call.view,
        generation: call.generation,
        initialization: call.initialization,
    };
    let head = state
        .previews()
        .runtime_access(&key, capability)
        .map_err(denied)?;
    let actor = call.actor.clone();
    let lookup = artifact.clone();
    let record = state
        .run(move |artifacts| artifacts.artifact(&actor, &lookup))
        .await?;
    if record.current_revision != head {
        return Err(stale(record.current_revision));
    }
    // The lookup was asynchronous: a revoke, a close or a replacement may
    // have landed meanwhile.
    state
        .previews()
        .runtime_access(&key, capability)
        .map_err(denied)?;
    Ok(Authorized { artifact, revision })
}

/// The files of the pinned revision.
pub(crate) async fn list(
    state: &AppState,
    call: &FrameCall<'_>,
) -> Result<RuntimeSourceList, ApiError> {
    let granted = authorize(state, call, Capability::SourceRead).await?;
    let actor = call.actor.clone();
    let (artifact, revision) = (granted.artifact.clone(), granted.revision);
    let record = state
        .run(move |artifacts| artifacts.revision(&actor, &artifact, revision))
        .await?;
    authorize(state, call, Capability::SourceRead).await?;
    Ok(RuntimeSourceList {
        revision: revision.0,
        files: revision_dto(record).files,
    })
}

/// One part of one file of the pinned revision, cut at bytes.
pub(crate) async fn read(
    state: &AppState,
    call: &FrameCall<'_>,
    request: &RuntimeSourceReadRequest,
) -> Result<RuntimeSourcePart, ApiError> {
    // Authorization first, so a caller without a binding learns nothing
    // from how its arguments were judged.
    let granted = authorize(state, call, Capability::SourceRead).await?;
    let length = NonZeroU64::new(request.length)
        .filter(|length| length.get() <= MAX_RUNTIME_SOURCE_PART_BYTES)
        .ok_or_else(|| ApiError::invalid("length must be from 1 to 1048576"))?;
    if request.offset > MAX_SAFE_INTEGER {
        return Err(ApiError::invalid("offset is not a safe integer"));
    }
    let path = package_path("path", &request.path)?;
    let actor = call.actor.clone();
    let (artifact, revision) = (granted.artifact.clone(), granted.revision);
    let read_path = path.clone();
    let range = ByteRange::From {
        offset: request.offset,
        length: Some(length),
    };
    let part = state
        .run(move |artifacts| {
            artifacts.read_file_part(
                &actor,
                &artifact,
                revision,
                &read_path,
                range,
                Boundary::Byte,
            )
        })
        .await?;
    authorize(state, call, Capability::SourceRead).await?;
    Ok(RuntimeSourcePart {
        path: path.to_string(),
        revision: revision.0,
        offset: part.offset,
        next_offset: part.next_offset(),
        size: part.size,
        sha256: hex(&part.sha256),
        content: BASE64.encode(&part.content),
    })
}

/// What a download of `kind` would be named, without handing any of it over.
pub(crate) async fn offer(
    state: &AppState,
    call: &FrameCall<'_>,
    request: &RuntimeDownloadRequest,
) -> Result<RuntimeDownloadOffer, ApiError> {
    let granted = authorize(state, call, Capability::SourceDownload).await?;
    let target = target(request)?;
    let offer = match target {
        Target::Archive => RuntimeDownloadOffer {
            kind: RuntimeDownloadKind::Archive,
            path: None,
            file_name: archive_name(&granted),
            size: None,
            sha256: None,
        },
        Target::File(path) => {
            let actor = call.actor.clone();
            let (artifact, revision) = (granted.artifact.clone(), granted.revision);
            let record = state
                .run(move |artifacts| artifacts.revision(&actor, &artifact, revision))
                .await?;
            let entry = record
                .files
                .iter()
                .find(|entry| entry.path == path)
                .ok_or_else(|| ApiError::not_found("file not found in revision"))?;
            RuntimeDownloadOffer {
                kind: RuntimeDownloadKind::File,
                path: Some(path.to_string()),
                file_name: file_name(&path),
                size: Some(entry.size),
                sha256: Some(hex(&entry.sha256)),
            }
        }
    };
    authorize(state, call, Capability::SourceDownload).await?;
    Ok(offer)
}

/// The bytes of a download the user confirmed: the stored file as it is, or
/// the source export of the pinned revision (decision 0015).
pub(crate) async fn download(
    state: &AppState,
    call: &FrameCall<'_>,
    request: &RuntimeDownloadRequest,
) -> Result<Download, ApiError> {
    let granted = authorize(state, call, Capability::SourceDownload).await?;
    let target = target(request)?;
    let result = match target {
        Target::Archive => {
            let export =
                crate::operations::export(state, call.actor.clone(), call.artifact, call.revision)
                    .await?;
            Download {
                bytes: export.archive,
                content_type: "application/zip",
                file_name: archive_name(&granted),
            }
        }
        Target::File(path) => {
            let actor = call.actor.clone();
            let (artifact, revision) = (granted.artifact.clone(), granted.revision);
            let read_path = path.clone();
            let part = state
                .run(move |artifacts| {
                    artifacts.read_file_part(
                        &actor,
                        &artifact,
                        revision,
                        &read_path,
                        ByteRange::WHOLE,
                        Boundary::Byte,
                    )
                })
                .await?;
            Download {
                bytes: part.content,
                content_type: "application/octet-stream",
                file_name: file_name(&path),
            }
        }
    };
    authorize(state, call, Capability::SourceDownload).await?;
    Ok(result)
}

enum Target {
    File(PackagePath),
    Archive,
}

fn target(request: &RuntimeDownloadRequest) -> Result<Target, ApiError> {
    match (request.kind, request.path.as_deref()) {
        (RuntimeDownloadKind::File, Some(path)) => package_path("path", path).map(Target::File),
        (RuntimeDownloadKind::Archive, None) => Ok(Target::Archive),
        _ => Err(ApiError::invalid(
            "a file download names a path and an archive download names none",
        )),
    }
}

fn archive_name(granted: &Authorized) -> String {
    format!("{}-r{}.zip", granted.artifact, granted.revision.0)
}

/// The last path segment as a download name.
fn file_name(path: &PackagePath) -> String {
    safe_name(path.as_str().rsplit('/').next().unwrap_or("file"))
}

/// A name reduced to ASCII letters, digits, `.`, `_` and `-`, not starting
/// with a dot, at most [`MAX_DOWNLOAD_NAME`] characters. A package path
/// segment already is all of that, so this never shortens a name the store
/// accepted; it stays because a response header is no place to trust a rule
/// that is enforced somewhere else.
fn safe_name(segment: &str) -> String {
    let mut name: String = segment
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .take(MAX_DOWNLOAD_NAME)
        .collect();
    if name.starts_with('.') {
        name.replace_range(0..1, "_");
    }
    if name.is_empty() {
        name.push_str("file");
    }
    name
}

#[cfg(test)]
mod tests {
    use artifree_domain::package::{MAX_SEGMENT_BYTES, PackagePath};

    use super::{MAX_DOWNLOAD_NAME, file_name, safe_name};

    #[test]
    fn a_download_name_is_the_last_segment_in_safe_ascii() {
        let path: PackagePath = "assets/deep/app.min.js"
            .parse()
            .unwrap_or_else(|error| panic!("fixture path: {error}"));
        assert_eq!(file_name(&path), "app.min.js");
    }

    #[test]
    fn the_longest_package_segment_fits_the_name_bound() {
        // The wire contract states 100 characters; a segment of 100 bytes is
        // the longest the store accepts, so no file name is ever shortened.
        assert_eq!(MAX_DOWNLOAD_NAME, MAX_SEGMENT_BYTES);
        let long = format!("{}.txt", "a".repeat(MAX_SEGMENT_BYTES - 4));
        let path: PackagePath = long
            .parse()
            .unwrap_or_else(|error| panic!("fixture path: {error}"));
        assert_eq!(file_name(&path), long);
        assert_eq!(file_name(&path).len(), MAX_DOWNLOAD_NAME);
    }

    #[test]
    fn a_name_that_would_break_a_header_or_a_save_dialog_is_reduced() {
        assert_eq!(safe_name("a\"b\r\nSet-Cookie: x"), "a_b__Set-Cookie__x");
        assert_eq!(safe_name("naïve 日本.txt"), "na_ve___.txt");
        assert_eq!(safe_name("../etc/passwd"), "_._etc_passwd");
        assert_eq!(safe_name(".hidden"), "_hidden");
        assert_eq!(safe_name(""), "file");
        let long = safe_name(&"x".repeat(MAX_DOWNLOAD_NAME * 2));
        assert_eq!(long.len(), MAX_DOWNLOAD_NAME);
        assert!(
            long.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        );
    }
}
