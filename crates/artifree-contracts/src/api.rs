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
    /// Latest committed revision when this metadata was produced; see
    /// [`CommitResponse::artifact`] for replays.
    pub current_revision: u64,
    /// Creation time in Unix milliseconds.
    pub created_at_ms: i64,
    /// Time of the latest revision in Unix milliseconds.
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

/// Response of `GET /v1/artifacts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArtifactList {
    /// Accessible artifacts, most recently updated first.
    pub artifacts: Vec<Artifact>,
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

/// What a committed change did to an artifact. Clients should treat a kind
/// they do not know as a reason to read the artifact again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    /// The artifact was created with revision 1.
    Created,
    /// A new source revision was committed.
    Revised,
}

/// Data of a `change` event on `GET /v1/changes`: one commit that wrote a
/// revision. Replayed retries and failed commits produce no event. The event
/// says what to read again; it carries no content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArtifactChange {
    /// Feed position of this change, also sent as the event `id`.
    pub cursor: u64,
    /// The changed artifact.
    pub artifact_id: String,
    /// What changed.
    pub change: ChangeKind,
    /// The artifact's revision after the change.
    pub revision: u64,
    /// Commit time in Unix milliseconds.
    pub committed_at_ms: i64,
}

/// Data of the `ready` and `reset` events on `GET /v1/changes`. `ready`
/// starts a stream opened without a cursor; `reset` replaces a cursor the
/// server cannot continue from, after which the client must read its state
/// again. Either way, later events follow this cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FeedPosition {
    /// The feed position to resume from, also sent as the event `id`.
    pub cursor: u64,
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
    /// The idempotency key was already used for a different request.
    RequestKeyReused,
    /// The request body exceeds the size limit.
    PayloadTooLarge,
    /// Too many requests from this caller. This request was not processed;
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
}

/// Body of every non-2xx JSON response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ErrorResponse {
    /// Error details.
    pub error: ErrorBody,
}
