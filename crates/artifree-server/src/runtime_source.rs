//! Source reads, downloads and writes for runtime frames (decisions 0032
//! and 0033).
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
    ErrorCode, MAX_DOWNLOAD_NAME, MAX_RUNTIME_SOURCE_PART_BYTES, MAX_RUNTIME_SOURCE_WRITE_BASE64,
    MAX_RUNTIME_SOURCE_WRITE_BYTES, RuntimeDownloadKind, RuntimeDownloadOffer,
    RuntimeDownloadRequest, RuntimeSourceList, RuntimeSourcePart, RuntimeSourceReadRequest,
    RuntimeSourceWriteReceipt, RuntimeSourceWriteRequest,
};
use artifree_core::artifacts::{ArtifactError, ArtifactService};
use artifree_core::parts::{Boundary, ByteRange, FilePart};
use artifree_domain::SourceRevision;
use artifree_domain::identity::{ActorId, ArtifactId, RequestKey};
use artifree_domain::package::{FileChange, PackageError, PackagePath};
use artifree_ports::artifact_store::{CommitReceipt, Delivery, RevisionRecord};
use artifree_providers::storage::SqliteStore;
use axum::http::StatusCode;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;

use crate::auth::hex;
use crate::http::AppState;
use crate::operations::{
    ApiError, artifact_id, missing_view, package_path, revision_dto, revision_number, validate_view,
};
use crate::preview::{PreviewTickets, RuntimeDenied, ViewKey};
use crate::runtime_policy::Capability;

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Downloads one actor may have under construction at once. A file is read
/// and an archive is built whole in memory (up to the package limit), so a
/// frame holding `source.download` must not be able to start many together;
/// the frame budget bounds how often it asks, this bounds how much it holds.
pub(crate) const MAX_DOWNLOAD_BUILDS: u32 = 2;

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

pub(crate) struct Authorized {
    pub(crate) artifact: ArtifactId,
    revision: SourceRevision,
    /// The head the view was pinned to when it was issued.
    pinned: SourceRevision,
    /// The artifact's head now.
    current: SourceRevision,
}

impl Authorized {
    /// The head the view was pinned to when it was issued.
    pub(crate) fn pinned(&self) -> SourceRevision {
        self.pinned
    }
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

/// Too many of the actor's downloads are being built.
pub(crate) fn busy() -> ApiError {
    ApiError::new(
        StatusCode::TOO_MANY_REQUESTS,
        ErrorCode::RateLimited,
        "too many downloads are being prepared; retry shortly",
    )
}

/// The binding and the connection's current grant of `capability`, with the
/// head the view was pinned to. Binding failures are `404`, a missing grant
/// after a matching binding is `403`.
fn permitted(
    state: &AppState,
    call: &FrameCall<'_>,
    artifact: &ArtifactId,
    revision: SourceRevision,
    capability: Capability,
) -> Result<SourceRevision, ApiError> {
    let key = ViewKey {
        actor: call.actor,
        artifact,
        revision,
        view_id: call.view,
        generation: call.generation,
        initialization: call.initialization,
    };
    state
        .previews()
        .runtime_access(&key, capability)
        .map_err(denied)
}

/// The binding and the connection's grant of `capability`, checked again
/// without a store read. A call that is already authorized makes this its
/// last step before the reply, with no `.await` after it.
pub(crate) fn still_permitted(
    state: &AppState,
    call: &FrameCall<'_>,
    granted: &Authorized,
    capability: Capability,
) -> Result<(), ApiError> {
    permitted(state, call, &granted.artifact, granted.revision, capability)?;
    Ok(())
}

/// One authorized call's binding with every part owned. A write checks it
/// again on the blocking task that commits it, which cannot borrow the
/// request's parts.
pub(crate) struct HeldCall {
    actor: ActorId,
    artifact: ArtifactId,
    revision: SourceRevision,
    view: String,
    generation: u64,
    initialization: String,
}

impl HeldCall {
    /// The binding of `call` as `granted` checked it.
    pub(crate) fn new(call: &FrameCall<'_>, granted: &Authorized) -> Self {
        Self {
            actor: call.actor.clone(),
            artifact: granted.artifact.clone(),
            revision: granted.revision,
            view: call.view.to_owned(),
            generation: call.generation,
            initialization: call.initialization.to_owned(),
        }
    }

    /// The binding and the connection's grant of `capability`, checked as
    /// [`still_permitted`] checks them.
    pub(crate) fn permitted(
        &self,
        previews: &PreviewTickets,
        capability: Capability,
    ) -> Result<(), ApiError> {
        let key = ViewKey {
            actor: &self.actor,
            artifact: &self.artifact,
            revision: self.revision,
            view_id: &self.view,
            generation: self.generation,
            initialization: &self.initialization,
        };
        previews.runtime_access(&key, capability).map_err(denied)?;
        Ok(())
    }
}

/// The live check of one call for one capability, without a fence on the
/// head: the shape of the route names, the binding, the grant and the
/// artifact outside the trash, with the binding and grant checked again
/// after the lookup. A write fences on the head with its commit instead.
async fn live(
    state: &AppState,
    call: &FrameCall<'_>,
    capability: Capability,
) -> Result<Authorized, ApiError> {
    validate_view(call.view, call.generation)?;
    let artifact = artifact_id(call.artifact)?;
    let revision = revision_number(call.revision)?;
    let pinned = permitted(state, call, &artifact, revision, capability)?;
    let actor = call.actor.clone();
    let lookup = artifact.clone();
    let record = state
        .run(move |artifacts| artifacts.artifact(&actor, &lookup))
        .await?;
    // The lookup was asynchronous: a revoke, a close or a replacement may
    // have landed meanwhile.
    permitted(state, call, &artifact, revision, capability)?;
    Ok(Authorized {
        artifact,
        revision,
        pinned,
        current: record.current_revision,
    })
}

/// The full live check of a read: [`live`], and the artifact's head is still
/// the one the view was pinned to (`409` otherwise).
pub(crate) async fn authorize(
    state: &AppState,
    call: &FrameCall<'_>,
    capability: Capability,
) -> Result<Authorized, ApiError> {
    let granted = live(state, call, capability).await?;
    if granted.current != granted.pinned {
        return Err(stale(granted.current));
    }
    Ok(granted)
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
    // Held until the bytes are returned or the work fails; dropping the slot
    // on any exit frees it. Taken after authorization, so a caller without a
    // grant cannot occupy one, and before any byte is read.
    let _build = state
        .download_builds()
        .take(call.actor)
        .map_err(|_| busy())?;
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

/// What the commit and its read-back found, from one blocking task.
struct Saved {
    receipt: CommitReceipt,
    /// The receipt for the frame, or why the new revision is not what was
    /// asked for. The revision exists either way.
    checked: Result<RuntimeSourceWriteReceipt, Mismatch>,
}

/// How the committed revision differs from the request. The frame is never
/// told which: a different file is a server fault, not the frame's to see.
#[derive(Debug, PartialEq, Eq)]
enum Mismatch {
    /// The revision is not the one after the view's.
    Number,
    /// The revisions could not be read back.
    Unreadable,
    /// A file other than the written one is not as it was.
    OtherFiles,
    /// The written file is missing, or its size or digest is not the sent one.
    Listing,
    /// The stored bytes are not the bytes sent.
    Bytes,
}

/// Saves one file as the next revision of the view's artifact, with the view's
/// revision as the base, and answers only after reading the new revision back
/// (decision 0033).
///
/// The commit is conditional on the base, so the head is fenced atomically
/// with the write and not by an earlier check: a view that is not the head
/// gets `409` with the current revision, and the retry of a save that
/// committed gets its first outcome back (the store replays it by key) even
/// though the head moved because of it. The key is derived here, from the
/// artifact, the view, its generation and the frame's retry identity, in the
/// namespace no other caller can produce (`RequestKey::runtime_write`).
pub(crate) async fn write(
    state: &AppState,
    call: &FrameCall<'_>,
    request: &RuntimeSourceWriteRequest,
) -> Result<RuntimeSourceWriteReceipt, ApiError> {
    // Authorization first, so a caller without a binding or the edit grant
    // learns nothing from how its arguments were judged.
    let granted = live(state, call, Capability::SourceWrite).await?;
    let path = package_path("path", &request.path)?;
    let key = RequestKey::runtime_write(
        &granted.artifact,
        call.view,
        call.generation,
        &request.retry_id,
    )
    .map_err(|error| ApiError::invalid(format!("retry_id: {error}")))?;
    let content = decoded(&request.content)?;
    let actor = call.actor.clone();
    let (artifact, base) = (granted.artifact.clone(), granted.revision);
    let saved = state
        .run(move |artifacts| {
            // `save` takes the change by value, and the read-back compares the
            // stored bytes with the sent ones, so this is the one copy of the
            // content that is made.
            let change = FileChange::Put {
                path: path.clone(),
                content: content.clone(),
            };
            let receipt = artifacts.save(&actor, &key, &artifact, base, vec![change])?;
            let checked = read_back(
                artifacts, &actor, &artifact, base, &receipt, &path, &content,
            );
            Ok(Saved { receipt, checked })
        })
        .await
        .map_err(write_error)?;
    // The commit is durable whatever happens next: tell the change feed.
    if saved.receipt.delivery == Delivery::Committed {
        state.feed().committed();
    }
    // The view may later be adopted onto the revision it wrote (decision
    // 0054, rule 4); a view that was replaced meanwhile records nothing. A
    // replayed save records too: the request that committed may have been
    // dropped before it reached this line.
    state.previews().record_written(
        &ViewKey {
            actor: call.actor,
            artifact: &granted.artifact,
            revision: granted.revision,
            view_id: call.view,
            generation: call.generation,
            initialization: call.initialization,
        },
        saved.receipt.revision.revision,
    );
    let receipt = saved.checked.map_err(|_mismatch| {
        // Never reached by a correct store; the revision exists, so say so
        // plainly instead of reporting a success that was not verified.
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            ErrorCode::Internal,
            "the saved revision could not be verified; check the artifact's history",
        )
    })?;
    // A revoke, a close or a replacement that landed during the commit does
    // not undo it, but the frame is told nothing of the outcome.
    permitted(
        state,
        call,
        &granted.artifact,
        granted.revision,
        Capability::SourceWrite,
    )?;
    Ok(receipt)
}

/// Reads the revision `receipt` made and judges it against the request.
fn read_back(
    artifacts: &ArtifactService<SqliteStore>,
    actor: &ActorId,
    artifact: &ArtifactId,
    base: SourceRevision,
    receipt: &CommitReceipt,
    path: &PackagePath,
    content: &[u8],
) -> Result<RuntimeSourceWriteReceipt, Mismatch> {
    let made = receipt.revision.revision;
    let unreadable = |_: ArtifactError| Mismatch::Unreadable;
    let before = artifacts
        .revision(actor, artifact, base)
        .map_err(unreadable)?;
    let after = artifacts
        .revision(actor, artifact, made)
        .map_err(unreadable)?;
    let stored = artifacts
        .read_file_part(
            actor,
            artifact,
            made,
            path,
            ByteRange::WHOLE,
            Boundary::Byte,
        )
        .map_err(unreadable)?;
    judge(base, made, path, content, [&before, &after], &stored)
}

/// Whether the revision `made` is what a save of `content` at `path` on
/// `base` must produce: the revision after `base`, every other file as it was
/// in `base`, and the written file holding exactly the bytes sent. The stored
/// bytes are compared with the sent ones, not only their digests.
fn judge(
    base: SourceRevision,
    made: SourceRevision,
    path: &PackagePath,
    content: &[u8],
    [before, after]: [&RevisionRecord; 2],
    stored: &FilePart,
) -> Result<RuntimeSourceWriteReceipt, Mismatch> {
    if made.0 != base.0.saturating_add(1) || after.revision != made || before.revision != base {
        return Err(Mismatch::Number);
    }
    let others = |record: &RevisionRecord| {
        record
            .files
            .iter()
            .filter(|file| file.path != *path)
            .cloned()
            .collect::<Vec<_>>()
    };
    if others(before) != others(after) {
        return Err(Mismatch::OtherFiles);
    }
    let size = content.len() as u64;
    let entry = after
        .files
        .iter()
        .find(|file| file.path == *path && file.size == size)
        .ok_or(Mismatch::Listing)?;
    if stored.content != content || stored.size != size || stored.sha256 != entry.sha256 {
        return Err(Mismatch::Bytes);
    }
    Ok(RuntimeSourceWriteReceipt {
        path: path.to_string(),
        revision: made.0,
        size,
        sha256: hex(&entry.sha256),
    })
}

/// The bytes of a frame's file: standard base64 with padding, of at most
/// `max_base64` characters of text and `max_bytes` bytes once decoded. The
/// route's body limit leaves room for text longer than that, so the length is
/// judged first and an over-long text is refused without allocating a buffer
/// for it.
pub(crate) fn decoded_within(
    text: &str,
    max_base64: u64,
    max_bytes: u64,
) -> Result<Vec<u8>, ApiError> {
    if text.len() as u64 > max_base64 {
        return Err(ApiError::invalid("content is larger than the write limit"));
    }
    let bytes = BASE64
        .decode(text)
        .map_err(|_| ApiError::invalid("content is not valid base64"))?;
    if bytes.len() as u64 > max_bytes {
        return Err(ApiError::invalid("content is larger than the write limit"));
    }
    Ok(bytes)
}

/// The bytes of a source write, within the source write limits.
fn decoded(text: &str) -> Result<Vec<u8>, ApiError> {
    decoded_within(
        text,
        MAX_RUNTIME_SOURCE_WRITE_BASE64,
        MAX_RUNTIME_SOURCE_WRITE_BYTES,
    )
}

/// A commit failure, worded without anything the frame supplied: the path is
/// not quoted back.
fn write_error(error: ArtifactError) -> ApiError {
    match error {
        ArtifactError::InvalidPackage(PackageError::Unchanged) => {
            ApiError::unprocessable("the file already holds these bytes; nothing was saved")
        }
        ArtifactError::InvalidPackage(_) => {
            ApiError::unprocessable("the change does not fit the package rules")
        }
        other => other.into(),
    }
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
pub(crate) fn safe_name(segment: &str) -> String {
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
    use artifree_core::parts::FilePart;
    use artifree_domain::SourceRevision;
    use artifree_domain::identity::{ActorId, UnixMillis};
    use artifree_domain::package::{MAX_SEGMENT_BYTES, PackagePath};
    use artifree_ports::artifact_store::{FileEntry, RevisionRecord};

    use super::{MAX_DOWNLOAD_NAME, Mismatch, file_name, judge, safe_name};

    fn path(text: &str) -> PackagePath {
        text.parse()
            .unwrap_or_else(|error| panic!("fixture path {text}: {error}"))
    }

    fn entry(name: &str, size: u64, fill: u8) -> FileEntry {
        FileEntry {
            path: path(name),
            size,
            sha256: [fill; 32],
        }
    }

    fn record(revision: u64, files: Vec<FileEntry>) -> RevisionRecord {
        RevisionRecord {
            revision: SourceRevision(revision),
            created_by: "local"
                .parse::<ActorId>()
                .unwrap_or_else(|error| panic!("fixture actor: {error}")),
            created_at: UnixMillis(0),
            files,
        }
    }

    fn stored(content: &[u8], fill: u8) -> FilePart {
        FilePart {
            content: content.to_vec(),
            offset: 0,
            size: content.len() as u64,
            sha256: [fill; 32],
            text: true,
        }
    }

    /// Base revision 1 holds `a.txt` and `b.txt`; the save writes `b.txt`.
    fn base() -> RevisionRecord {
        record(1, vec![entry("a.txt", 1, 1), entry("b.txt", 2, 2)])
    }

    fn saved() -> RevisionRecord {
        record(2, vec![entry("a.txt", 1, 1), entry("b.txt", 3, 9)])
    }

    fn verdict(
        made: u64,
        after: &RevisionRecord,
        part: &FilePart,
    ) -> Result<(String, u64, u64, String), Mismatch> {
        judge(
            SourceRevision(1),
            SourceRevision(made),
            &path("b.txt"),
            b"new",
            [&base(), after],
            part,
        )
        .map(|receipt| (receipt.path, receipt.revision, receipt.size, receipt.sha256))
    }

    #[test]
    fn a_revision_that_holds_the_bytes_and_leaves_the_rest_is_accepted() {
        assert_eq!(
            verdict(2, &saved(), &stored(b"new", 9)),
            Ok(("b.txt".into(), 2, 3, "09".repeat(32)))
        );
    }

    #[test]
    fn a_revision_that_differs_from_the_request_is_not_accepted() {
        let moved = |files: Vec<FileEntry>| record(2, files);
        let cases = [
            (
                "a revision that is not the next one",
                verdict(3, &record(3, saved().files), &stored(b"new", 9)),
                Mismatch::Number,
            ),
            (
                "a listing of another revision than the one made",
                verdict(2, &record(5, saved().files), &stored(b"new", 9)),
                Mismatch::Number,
            ),
            (
                "another file changed",
                verdict(
                    2,
                    &moved(vec![entry("a.txt", 1, 7), entry("b.txt", 3, 9)]),
                    &stored(b"new", 9),
                ),
                Mismatch::OtherFiles,
            ),
            (
                "another file was dropped",
                verdict(2, &moved(vec![entry("b.txt", 3, 9)]), &stored(b"new", 9)),
                Mismatch::OtherFiles,
            ),
            (
                "another file was added",
                verdict(
                    2,
                    &moved(vec![
                        entry("a.txt", 1, 1),
                        entry("b.txt", 3, 9),
                        entry("c.txt", 1, 1),
                    ]),
                    &stored(b"new", 9),
                ),
                Mismatch::OtherFiles,
            ),
            (
                "the written file is not listed",
                verdict(2, &moved(vec![entry("a.txt", 1, 1)]), &stored(b"new", 9)),
                Mismatch::Listing,
            ),
            (
                "the written file is listed with another size",
                verdict(
                    2,
                    &moved(vec![entry("a.txt", 1, 1), entry("b.txt", 4, 9)]),
                    &stored(b"new", 9),
                ),
                Mismatch::Listing,
            ),
            (
                "the stored bytes differ from the bytes sent, at the same length",
                verdict(2, &saved(), &stored(b"old", 9)),
                Mismatch::Bytes,
            ),
            (
                "the stored file's digest is not the listed one",
                verdict(2, &saved(), &stored(b"new", 8)),
                Mismatch::Bytes,
            ),
            (
                "the stored file states another size",
                verdict(
                    2,
                    &saved(),
                    &FilePart {
                        size: 4,
                        ..stored(b"new", 9)
                    },
                ),
                Mismatch::Bytes,
            ),
        ];
        for (name, got, want) in cases {
            assert_eq!(got, Err(want), "{name}");
        }
    }

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
