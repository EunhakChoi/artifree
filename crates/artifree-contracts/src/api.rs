//! HTTP API v1 request and response bodies.
//!
//! These DTOs describe shape only. The server validates every value again,
//! converts it into domain types, and checks authorization; a body that
//! matches the schema can still be rejected.
//!
//! Mutating requests also require an `Idempotency-Key` header. Retrying with
//! the same key and body returns the original result with `replayed: true`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Byte encoding of a file's `content` string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContentEncoding {
    /// The string's UTF-8 bytes are the file content.
    Utf8,
    /// Standard padded base64 of the file content.
    Base64,
}

/// One file of a new artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileUpload {
    /// Portable relative path such as `index.html` or `assets/app.js`.
    pub path: String,
    /// How `content` encodes the bytes.
    pub encoding: ContentEncoding,
    /// Complete file content.
    pub content: String,
}

/// Body of `POST /v1/artifacts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateArtifactRequest {
    /// Display title, 1-200 characters.
    pub title: String,
    /// Initial files; `index.html` is required.
    pub files: Vec<FileUpload>,
}

/// One change in a revision commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum FileChange {
    /// Create or replace a file.
    Put {
        /// Portable relative path.
        path: String,
        /// How `content` encodes the bytes.
        encoding: ContentEncoding,
        /// Complete new content.
        content: String,
    },
    /// Remove an existing file.
    Delete {
        /// Portable relative path.
        path: String,
    },
}

/// Body of `POST /v1/artifacts/{id}/revisions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveRevisionRequest {
    /// The revision these changes were prepared against. The commit fails
    /// with `revision_conflict` unless it is still current.
    pub base_revision: u64,
    /// Changes applied to the base revision; unchanged files carry over. At
    /// most 512 changes, each to a different path.
    pub changes: Vec<FileChange>,
}

/// Body of `POST /v1/artifacts/{id}/revisions/{revision}/restore`: commits
/// the files of that earlier revision as the next revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RestoreRevisionRequest {
    /// The revision the restore was prepared against. The commit fails with
    /// `revision_conflict` unless it is still current.
    pub base_revision: u64,
}

/// Body of `POST /v1/artifacts/{id}/revisions/{revision}/duplicate`:
/// creates an artifact from the files of that revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DuplicateArtifactRequest {
    /// Display title of the copy, 1-200 characters. Without it the copy is
    /// named after the source's current title followed by " (Copy)".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// Body of `PATCH /v1/artifacts/{id}`: a new title, pin, or trash state, or
/// several of them. Source revisions are not touched. An artifact in the
/// trash accepts only a body with `deleted: false`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateArtifactRequest {
    /// The metadata revision this change was prepared against. The update
    /// fails with `metadata_conflict` unless it is still current.
    pub base_metadata_revision: u64,
    /// New display title, 1-200 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Whether the artifact is pinned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<bool>,
    /// `true` moves the artifact to the trash, where it cannot be read or
    /// changed until `false` takes it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted: Option<bool>,
}

/// Source model family of an artifact. Only `app` can be created by this
/// version; the other families are reserved for planned source models.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    /// A file package with an `index.html` entry document.
    App,
    /// A structured text document.
    Document,
    /// An ordered slide presentation.
    Slides,
    /// A data dashboard.
    Dashboard,
    /// A visual design document.
    Design,
    /// A timeline or motion composition.
    Motion,
}

/// Artifact metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    /// Opaque identifier.
    pub id: String,
    /// Source model family.
    pub kind: ArtifactKind,
    /// Display title.
    pub title: String,
    /// Whether the owner pinned the artifact.
    pub pinned: bool,
    /// Latest committed revision when this metadata was produced; see
    /// [`CommitResponse::artifact`] for replays.
    pub current_revision: u64,
    /// Version of the title, pin, and trash state, starting at 1. It changes
    /// only when they change, independently of `current_revision`.
    pub metadata_revision: u64,
    /// When the artifact was moved to the trash, in Unix milliseconds, or
    /// `null` outside it. Always present.
    // Schemars leaves an `Option` out of `required`, and drops its `null`
    // when told it is required; this field is both.
    #[schemars(required, extend("type" = ["integer", "null"]))]
    pub deleted_at_ms: Option<i64>,
    /// Creation time in Unix milliseconds.
    pub created_at_ms: i64,
    /// Time of the latest source revision in Unix milliseconds; metadata
    /// updates do not change it.
    pub updated_at_ms: i64,
}

/// A file in a revision manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RevisionFile {
    /// Package path.
    pub path: String,
    /// Size in bytes.
    pub size: u64,
    /// SHA-256 of the file's bytes, as 64 lowercase hex digits. Files with
    /// equal digests have equal bytes.
    pub sha256: String,
    /// `Content-Type` used when the file is served.
    pub media_type: String,
}

/// An immutable source revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Revision {
    /// Revision number, starting at 1.
    pub revision: u64,
    /// Actor that committed it.
    pub created_by: String,
    /// Commit time in Unix milliseconds.
    pub created_at_ms: i64,
    /// Files in path order.
    pub files: Vec<RevisionFile>,
}

/// Response of a create or revision commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommitResponse {
    /// Artifact metadata as of the commit this request identifies. On a
    /// replay it can be older than the artifact's current state; read the
    /// artifact for that before choosing the next base revision.
    pub artifact: Artifact,
    /// The revision the request committed.
    pub revision: Revision,
    /// True when an earlier request with the same key already committed it.
    pub replayed: bool,
}

/// A part of an artifact that a duplicate copies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CopiedPart {
    /// The files of the chosen revision, byte for byte.
    Files,
}

/// Response of `POST /v1/artifacts/{id}/revisions/{revision}/duplicate`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DuplicateArtifactResponse {
    /// The copy as of the duplicate this request identifies, unpinned and
    /// with its own history. On a replay it can be older than the copy's
    /// current state.
    pub artifact: Artifact,
    /// Revision 1 of the copy.
    pub revision: Revision,
    /// What the copy received. This version has no application data or
    /// uploaded assets, so it copies files only.
    pub copied: Vec<CopiedPart>,
    /// True when an earlier request with the same key already created it.
    pub replayed: bool,
}

/// Response of `PATCH /v1/artifacts/{id}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateArtifactResponse {
    /// Artifact metadata as of the update this request identifies. On a
    /// replay it can be older than the artifact's current state. An update
    /// to the values already stored returns them unchanged.
    pub artifact: Artifact,
    /// True when an earlier request with the same key already applied it.
    pub replayed: bool,
}

/// Order of `GET /v1/artifacts`. Ties are broken by identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ListSort {
    /// The most recently updated source first, the default.
    Updated,
    /// The most recently created first.
    Created,
}

/// Query parameters of `GET /v1/artifacts`, all optional. Without any, the
/// response lists up to 500 artifacts, most recently updated first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListArtifactsQuery {
    /// Only artifacts whose title contains this text, 1-200 characters.
    /// ASCII letters match regardless of case; other characters exactly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub q: Option<String>,
    /// The order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<ListSort>,
    /// Only pinned (`true`) or only unpinned (`false`) artifacts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<bool>,
    /// `true` lists the artifacts in the trash instead of the others.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted: Option<bool>,
    /// Page size, 1-500; 500 when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    /// `next_page_token` of the previous page, with the same `q`, `sort`,
    /// `pinned`, and `deleted`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_token: Option<String>,
}

/// Response of `GET /v1/artifacts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArtifactList {
    /// Accessible artifacts that match the query, in its order.
    pub artifacts: Vec<Artifact>,
    /// Pass as `page_token` to read the next page; absent on the last page.
    /// Artifacts updated while a caller pages by update time can appear
    /// twice or be missed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_page_token: Option<String>,
}

/// Response of `GET /v1/artifacts/{id}/revisions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RevisionList {
    /// Revisions, newest first.
    pub revisions: Vec<Revision>,
}

/// A short-lived link that renders one pinned revision on the content origin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Preview {
    /// URL of the revision's `index.html`; relative links resolve inside it.
    pub url: String,
    /// The pinned revision.
    pub revision: u64,
    /// Expiry time in Unix milliseconds. Access is also rechecked per request.
    pub expires_at_ms: i64,
}

/// What a commit did to an artifact. A kind that a client does not know
/// still means the artifact changed: read it again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    /// The artifact was created at revision 1.
    Created,
    /// A revision was committed.
    Revised,
    /// The title or pin changed; no source revision was committed.
    MetadataUpdated,
    /// The artifact was moved to the trash; reading it fails until it is
    /// recovered.
    Deleted,
    /// The artifact was taken out of the trash.
    Recovered,
}

/// Data of a `change` event of `GET /v1/changes`: one commit that wrote a
/// revision or changed metadata. Retries that replay a commit, failed
/// commits, and updates that changed nothing have none.
/// It names what to read again and carries no content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArtifactChange {
    /// Opaque feed position of this change, also the event's `id`.
    pub cursor: String,
    /// The changed artifact.
    pub artifact_id: String,
    /// What the commit did.
    pub change: ChangeKind,
    /// The revision the commit created, or the current one for
    /// `metadata_updated`.
    pub revision: u64,
    /// The metadata revision after the commit.
    pub metadata_revision: u64,
    /// Commit time in Unix milliseconds.
    pub committed_at_ms: i64,
}

/// Data of the `ready` and `reset` events of `GET /v1/changes`. `ready`
/// starts a stream opened without a cursor. `reset` answers a cursor this
/// feed cannot continue from: changes may have been missed, so read the
/// state again. Later events follow this position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FeedPosition {
    /// Opaque feed position, also the event's `id`.
    pub cursor: String,
}

/// Machine-readable error category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Malformed body, header, path, or value, or a method the route does not
    /// support.
    InvalidRequest,
    /// Missing or unknown access token.
    Unauthenticated,
    /// The resource does not exist or is not accessible.
    NotFound,
    /// The base revision is not current; reload and reapply the change.
    RevisionConflict,
    /// The base metadata revision is not current; reload and reapply the
    /// change.
    MetadataConflict,
    /// The idempotency key was already used for a different request.
    RequestKeyReused,
    /// The request body exceeds the size limit.
    PayloadTooLarge,
    /// Too many requests from this caller. This attempt was not processed;
    /// retry after the `Retry-After` delay with the same idempotency key,
    /// which returns the original result if an earlier attempt committed.
    RateLimited,
    /// Storage or capacity is temporarily unavailable; retry with the same key.
    Unavailable,
    /// Unexpected server failure. A mutating request may have been committed;
    /// retry with the same key to learn its outcome.
    Internal,
}

/// Error details.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ErrorBody {
    /// Category for programmatic handling.
    pub code: ErrorCode,
    /// Human-readable explanation; not a stable contract.
    pub message: String,
    /// The current revision, present for `revision_conflict`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_revision: Option<u64>,
    /// The current metadata revision, present for `metadata_conflict`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_metadata_revision: Option<u64>,
}

/// Body of every non-2xx JSON response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ErrorResponse {
    /// Error details.
    pub error: ErrorBody,
}
