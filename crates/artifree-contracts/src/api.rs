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

/// One edit of an edit request: a change to one file and the condition on
/// that file alone (decision 0035).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum FileEdit {
    /// Create or replace a file.
    Put {
        /// Portable relative path.
        path: String,
        /// How `content` encodes the bytes.
        encoding: ContentEncoding,
        /// Complete new content.
        content: String,
        /// What the file must be in the current revision: `absent` when the
        /// path must not be a file (a create), or the lowercase hexadecimal
        /// SHA-256 digest of the bytes the file must have (a replace).
        #[schemars(regex(pattern = r"^(absent|[0-9a-f]{64})$"))]
        expect: String,
    },
    /// Remove an existing file.
    Delete {
        /// Portable relative path.
        path: String,
        /// The lowercase hexadecimal SHA-256 digest of the bytes the file
        /// must have in the current revision. `absent` is refused: deleting
        /// what is not there is no edit.
        #[schemars(regex(pattern = r"^[0-9a-f]{64}$"))]
        expect: String,
    },
}

/// Body of `POST /v1/artifacts/{id}/edits`: commits changes to some files as
/// the next revision, each applied only if its own file is as expected in the
/// **current** revision. One file that is not rejects all of them with
/// `file_conflict`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditFilesRequest {
    /// The revision the caller read the expected digests from. It is not a
    /// condition: files that no edit names may have changed since. It must be
    /// a revision the artifact has.
    pub base_revision: u64,
    /// At least one edit and at most 512, each to a different path.
    #[schemars(length(max = 512))]
    pub edits: Vec<FileEdit>,
}

/// A file named by an edit that is not as expected, with what it is now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConflictingFile {
    /// The edited path.
    pub path: String,
    /// Lowercase hexadecimal SHA-256 digest of the file in the current
    /// revision, or null when the path is not a file of it. Always present.
    #[schemars(required, extend("type" = ["string", "null"]))]
    pub current_sha256: Option<String>,
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
    /// Whether the copy also receives a copy of the source's application
    /// data, as its own independent data space. By default `false`: the copy
    /// receives files only. The data copied is the source's data when the copy
    /// commits, whichever revision the files come from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy_data: Option<bool>,
}

/// Body of `PATCH /v1/artifacts/{id}`: a new title, pin, trash state, or
/// several of them. Source revisions are not touched.
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
    /// `true` moves the artifact to the trash, where every other request
    /// treats it as missing; `false` takes it out, the only update an
    /// artifact in the trash accepts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted: Option<bool>,
}

/// Body of `POST /v1/artifacts/{id}/purge`: removes an artifact that is in
/// the trash for good (decision 0029). It cannot be undone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PurgeArtifactRequest {
    /// The metadata revision of the artifact in the trash that the caller
    /// saw. The purge fails with `metadata_conflict` unless it is still
    /// current, so an artifact recovered and deleted again since is not
    /// removed by a purge prepared against its earlier trash state.
    pub base_metadata_revision: u64,
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
    // Schemars leaves an `Option` out of `required`, and its `required`
    // attribute drops the `null`; the field is both required and nullable.
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

/// Version of a source export manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum SourceExportFormat {
    /// Version 1, described by [`SourceExportManifest`].
    #[serde(rename = "artifree.source-export.v1")]
    V1,
    /// Version 2 records the metadata runtime dependency when requested.
    #[serde(rename = "artifree.source-export.v2")]
    V2,
}

/// What exported files need from a host to run as they do in Artifree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExportRuntime {
    /// No runtime is requested by the entry document. Network resources are
    /// not part of the export and may be unavailable offline.
    None,
    /// The entry opts in to Artifree's metadata bridge, which is not bundled
    /// in the source export and requires an active Artifree workspace.
    Metadata,
}

/// The manifest of a source export: the file `.artifree-export.json`, first
/// in the ZIP archive that `GET /v1/artifacts/{id}/revisions/{revision}/export`
/// answers. Package paths never start with a dot, so no file of the revision
/// has this name. Every other entry is one file of the revision, at its
/// package path, with its bytes unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceExportManifest {
    /// Format and version of this manifest.
    pub format: SourceExportFormat,
    /// The exported artifact.
    pub artifact_id: String,
    /// The artifact's title when it was exported. Titles are not part of a
    /// revision.
    pub title: String,
    /// The exported revision.
    pub revision: u64,
    /// When the revision was committed, in Unix milliseconds; also the
    /// modification time of every entry.
    pub created_at_ms: i64,
    /// The file a preview opens: `index.html`.
    pub entry: String,
    /// What the files need to run.
    pub runtime: ExportRuntime,
    /// The revision's files in path order, as the revision lists them.
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
    /// The source's application data as of the copy, as the copy's own data
    /// space. Listed only when the request asked for it; a source whose data
    /// space is empty copies nothing, and the copy's data space is then empty.
    Data,
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
    /// What the copy received: `files`, and `data` when the request asked for
    /// it. Uploaded assets do not exist yet and are never copied.
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

/// Response of `POST /v1/artifacts/{id}/purge`. After a purge the artifact is
/// `not_found` to every request, also the retry of an earlier create, save,
/// restore, duplicate, or update that named it; only a retry of the purge
/// itself answers, with `replayed: true`. The identifier is never used for
/// another artifact. A copy made from the artifact is not affected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PurgeArtifactResponse {
    /// The removed artifact.
    pub artifact_id: String,
    /// The metadata revision the purge recorded, which the change feed's
    /// `purged` change also carries.
    pub metadata_revision: u64,
    /// When the artifact was removed, in Unix milliseconds.
    pub purged_at_ms: i64,
    /// True when an earlier request with the same key already purged it.
    pub replayed: bool,
}

/// Response of `GET /v1/content/unreferenced`, the dry run: file content that
/// no revision file of any artifact refers to, in the trash or not. Nothing is
/// removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UnreferencedContent {
    /// How many distinct contents.
    pub blobs: u64,
    /// Their total size in bytes. The database file does not shrink; the
    /// space is reused by later writes.
    pub bytes: u64,
    /// The identity of this set: 64 lowercase hexadecimal digits (SHA-256 over
    /// the sorted digests of its contents). Send it as `base_digest` to delete
    /// exactly this set.
    pub digest: String,
}

/// Body of `DELETE /v1/content/unreferenced`, with an `Idempotency-Key`
/// header: the cleanup of the set a dry run reported (decision 0029).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveUnreferencedContentRequest {
    /// The `digest` of the dry run's set. The cleanup fails with
    /// `content_conflict` and deletes nothing unless the unreferenced content
    /// is still exactly that set.
    pub base_digest: String,
}

/// Response of `DELETE /v1/content/unreferenced`: what the first run under the
/// key deleted. A retry with the same key and `base_digest` returns the same
/// numbers with `replayed: true`, also when the set is empty by then.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveUnreferencedContentResponse {
    /// How many distinct contents were deleted.
    pub blobs: u64,
    /// Their total size in bytes.
    pub bytes: u64,
    /// True when an earlier request with the same key did the deleting.
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
    /// `true` lists the trash instead of the artifacts outside it.
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

/// What a job does (decision 0030). Each kind is trusted in-repository code
/// that runs in the Artifree worker process. This is not a way to run
/// untrusted code, to build sources, or to call model providers; none of
/// those exists yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    /// Re-reads every file of a pinned revision from storage, recomputes its
    /// SHA-256 digest and size, and compares them with the stored listing.
    VerifyRevision,
}

/// Where a job is in its life. `succeeded`, `failed`, and `cancelled` are
/// terminal: a job there never changes again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    /// Waiting for a worker to claim it.
    Queued,
    /// A worker holds its lease. With `cancel_requested` the worker stops at
    /// its next checkpoint.
    Running,
    /// An attempt failed in a way worth repeating; the next one starts at
    /// `retry_at_ms`.
    RetryScheduled,
    /// The job ran to the end. A verification that found mismatches is still
    /// `succeeded`: read its `result.verdict`.
    Succeeded,
    /// The job could not run to the end and will not be tried again; see
    /// `failure`.
    Failed,
    /// The job was stopped before it finished. This says the work stopped. It
    /// does not say that anything the work did was undone; a verification
    /// does nothing outside its own record.
    Cancelled,
}

/// Why a job ended as `failed`. A fixed vocabulary, never free text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobFailure {
    /// The pinned revision could not be read; its artifact was purged.
    RevisionUnavailable,
    /// Storage kept failing for every attempt.
    StorageUnavailable,
    /// A worker held the final attempt and stopped renewing its lease.
    LeaseExpired,
    /// A failure the worker did not expect.
    Internal,
}

/// What is wrong with one file of a verified revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobProblem {
    /// The stored content has another SHA-256 digest than the listing.
    DigestMismatch,
    /// The stored content has another size than the listing.
    SizeMismatch,
    /// The listing names a file whose content is not stored.
    ContentMissing,
}

/// The outcome of a verification that ran to the end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobVerdict {
    /// Every file matched its listing.
    Ok,
    /// At least one file did not; see `mismatches`.
    Mismatch,
}

/// One file that does not match its listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JobMismatch {
    /// The file's package path, relative to the artifact.
    pub path: String,
    /// What is wrong with it.
    pub problem: JobProblem,
}

/// The result of a `verify_revision` job that ran to the end. It holds counts
/// and package paths of the pinned revision, never file content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JobResult {
    /// `ok` when no file mismatched.
    pub verdict: JobVerdict,
    /// How many files of the revision were read.
    pub files_checked: u64,
    /// The sum of the sizes of the content that was read.
    pub bytes_checked: u64,
    /// How many files mismatched, counted without the bound of `mismatches`.
    pub mismatch_count: u64,
    /// The first mismatches in path order, at most 50.
    pub mismatches: Vec<JobMismatch>,
    /// True when `mismatches` lists fewer than `mismatch_count`.
    pub truncated: bool,
}

/// A persistent job. It pins the actor that enqueued it, one artifact, one
/// source revision, and a kind; none of these changes. Time fields are Unix
/// milliseconds. A client learns of progress by reading the job again: there
/// is no push for jobs, and the change feed does not carry them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Job {
    /// Job identifier, `job_` followed by 32 lowercase hex digits.
    pub id: String,
    /// The artifact whose revision the job verifies.
    pub artifact_id: String,
    /// The pinned source revision.
    pub revision: u64,
    /// What the job does.
    pub kind: JobKind,
    /// Where it is in its life.
    pub state: JobState,
    /// True when a cancel was requested while the job was running. It then
    /// ends as `cancelled` unless it already ended; the request stops work
    /// and rolls nothing back.
    pub cancel_requested: bool,
    /// How many times a worker claimed the job.
    pub attempt: u64,
    /// How many claims it gets at most.
    pub max_attempts: u64,
    /// When the job was enqueued.
    pub created_at_ms: i64,
    /// When a worker first claimed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<i64>,
    /// When it reached a terminal state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<i64>,
    /// While `retry_scheduled`, the earliest time of the next attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_at_ms: Option<i64>,
    /// Why the job failed, once it is `failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<JobFailure>,
    /// The result, once the job is `succeeded`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<JobResult>,
}

/// Body of `POST /v1/artifacts/{id}/jobs`, with an `Idempotency-Key` header:
/// enqueues a job for one revision of the artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnqueueJobRequest {
    /// What to do.
    pub kind: JobKind,
    /// The source revision to pin. It must exist.
    pub revision: u64,
}

/// Response of `POST /v1/artifacts/{id}/jobs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnqueueJobResponse {
    /// The job. On a replay it is shown as it is now, not as it was.
    pub job: Job,
    /// True when an earlier request with the same key already enqueued it.
    pub replayed: bool,
}

/// Body of `POST /v1/jobs/{id}/cancel`, with an `Idempotency-Key` header. It
/// has no fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CancelJobRequest {}

/// Response of `POST /v1/jobs/{id}/cancel`. A job that had not started is
/// `cancelled`. A running job is still `running` with `cancel_requested`: its
/// worker stops at its next checkpoint, and the job ends `cancelled` unless it
/// ended first. A job that already ended answers `job_ended`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CancelJobResponse {
    /// The job. On a replay it is shown as it is now.
    pub job: Job,
    /// True when an earlier request with the same key already asked.
    pub replayed: bool,
}

/// Query parameters of `GET /v1/jobs` and `GET /v1/artifacts/{id}/jobs`, all
/// optional.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListJobsQuery {
    /// Page size, 1-100; 50 when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    /// `next_page_token` of the previous page of the same list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_token: Option<String>,
}

/// Response of `GET /v1/jobs` and `GET /v1/artifacts/{id}/jobs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JobList {
    /// Jobs of artifacts that are not in the trash, newest first.
    pub jobs: Vec<Job>,
    /// Pass as `page_token` to read the next page; absent on the last page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_page_token: Option<String>,
}

/// A JSON value of any shape, kept as the text it arrived as so that the server
/// applies the rules of application data (decision 0031) to what was sent:
/// duplicate member names, numbers beyond what a double holds exactly, and
/// the size are checked by the server, not lost to a parser.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JsonValue(Box<serde_json::value::RawValue>);

impl JsonValue {
    /// The value's JSON text.
    #[must_use]
    pub fn get(&self) -> &str {
        self.0.get()
    }

    /// A value from JSON text, or `None` when the text is not JSON.
    #[must_use]
    pub fn from_text(text: String) -> Option<Self> {
        serde_json::value::RawValue::from_string(text)
            .ok()
            .map(Self)
    }
}

impl PartialEq for JsonValue {
    fn eq(&self, other: &Self) -> bool {
        self.get() == other.get()
    }
}

impl Eq for JsonValue {}

impl JsonSchema for JsonValue {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "JsonValue".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "Any JSON value. Objects have unique member names, and every number has a magnitude of at most 2^53 - 1 and is finite, in whatever spelling (1e16 is refused like 10000000000000000). Whole numbers such as 1.0 and 1e2 are stored as integers, and a number too small for a double (0.1e-400) is stored as 0; 1e999 is refused as not finite.",
            "anyOf": [
                { "type": "object" },
                { "type": "array" },
                { "type": "string" },
                { "type": "number" },
                { "type": "boolean" },
                { "type": "null" }
            ]
        })
    }
}

/// Query of `GET /v1/artifacts/{id}/data`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadDataQuery {
    /// The path to read: at most 16 `/`-separated segments of 1-64 UTF-8
    /// bytes. ASCII letters, digits, `_`, `-` and non-ASCII Unicode 16.0.0
    /// letters, marks, numbers and symbols are allowed, excluding whitespace,
    /// default ignorables, noncharacters and slash lookalikes (0046).
    /// Exact scalar identity; no normalization. Absent or empty is the root,
    /// always an object. The query parameter is strictly decoded once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// Response of `GET /v1/artifacts/{id}/data`: the value at a path of the
/// caller's data space of the artifact, and the version of the space. A path
/// with nothing at it is a successful read with `exists` false.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AppData {
    /// The artifact.
    pub artifact_id: String,
    /// The path that was read, as parsed.
    pub path: String,
    /// The version of the data space: the `base_data_version` of the next
    /// write. 0 for a space nobody wrote. It is unrelated to the source
    /// revision and to the metadata revision.
    pub data_version: u64,
    /// Whether anything is at the path. The root always exists.
    pub exists: bool,
    /// The value at the path, present when `exists` is true. It can be
    /// `null`: a member set to `null`, which is present as `null`, unlike a
    /// value that is not there, which has no `value` member.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub value: Option<JsonValue>,
}

/// Query of `GET /v1/artifacts/{id}/data/members`. A parameter that is
/// unknown or repeated is refused.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadDataMembersQuery {
    /// The object whose member names are listed, written as for
    /// `GET /v1/artifacts/{id}/data`. Absent or empty is the root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Page size, 1-100; 50 when absent. Digits only, without sign or leading
    /// zero.
    #[serde(
        default,
        deserialize_with = "whole_number",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(range(min = 1, max = 100))]
    pub limit: Option<u64>,
    /// `next_page_token` of the previous page of the same artifact and path.
    /// Opaque: send it back as it was received.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_token: Option<String>,
    /// Answer only if the data space is at this version; any other version,
    /// older or newer, is `data_conflict` with `current_data_version`. 0
    /// matches a space nobody wrote. Digits only, without sign or leading
    /// zero, at most 2^53 - 1. Send the same value with every page to get
    /// pages that agree with each other.
    #[serde(
        default,
        deserialize_with = "whole_number",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(range(max = 9_007_199_254_740_991_u64))]
    pub at_data_version: Option<u64>,
}

/// Response of `GET /v1/artifacts/{id}/data/members`: one page of the member
/// names of the object at a path of the caller's data space, read from one
/// version of the space. Values, sizes, and types are not listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DataMembers {
    /// The artifact.
    pub artifact_id: String,
    /// The path that was listed, as parsed.
    pub path: String,
    /// The version of the data space this page was read at.
    pub data_version: u64,
    /// Whether an object is at the path. False when nothing is there or the
    /// path passes through a value that is not an object, as for
    /// `GET /v1/artifacts/{id}/data`; `names` is then empty. The root always
    /// exists.
    pub exists: bool,
    /// Member names after the page token's name, ascending by their UTF-8
    /// bytes, at most `limit`. Only names that a path can address are listed
    /// (`[A-Za-z0-9_-]{1,64}`); other names are skipped, do not count toward
    /// `limit`, and do not make `next_page_token` present.
    pub names: Vec<String>,
    /// Present exactly when more listed names follow at `data_version`. Pass
    /// it as `page_token` for the next page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_page_token: Option<String>,
}

/// Reads a whole number written as digits only, without sign or leading
/// zero, of at most 2^53 - 1: a query parameter, which Serde would otherwise
/// read as `05` or `+5`. A JSON number is read as it is.
fn whole_number<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u64>, D::Error> {
    use serde::de::{Error, Visitor};

    /// The largest integer a double holds exactly.
    const LARGEST: u64 = 9_007_199_254_740_991;

    struct Digits;

    impl Visitor<'_> for Digits {
        type Value = u64;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("digits without sign or leading zero, at most 2^53 - 1")
        }

        fn visit_u64<E: Error>(self, value: u64) -> Result<u64, E> {
            if value > LARGEST {
                return Err(E::invalid_value(
                    serde::de::Unexpected::Unsigned(value),
                    &self,
                ));
            }
            Ok(value)
        }

        fn visit_str<E: Error>(self, text: &str) -> Result<u64, E> {
            let plain = !text.is_empty()
                && text.bytes().all(|byte| byte.is_ascii_digit())
                && (text == "0" || !text.starts_with('0'));
            match text.parse::<u64>() {
                Ok(value) if plain => self.visit_u64(value),
                _ => Err(E::invalid_value(serde::de::Unexpected::Str(text), &self)),
            }
        }
    }

    deserializer.deserialize_any(Digits).map(Some)
}

/// Reads a member that is there, `null` included, as `Some`. Without it,
/// serde reads `"value": null` as `None`, the same as a member that is absent.
fn present<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<JsonValue>, D::Error> {
    JsonValue::deserialize(deserializer).map(Some)
}

/// Body of `PUT /v1/artifacts/{id}/data`, with an `Idempotency-Key` header:
/// replaces the value at a path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetDataRequest {
    /// Where; the empty string is the root, whose value must be an object and
    /// replaces the whole space. Objects above the path that are missing are
    /// made; a value in the way that is not an object is `data_path_conflict`.
    pub path: String,
    /// The version of the data space this change was prepared against. The
    /// write fails with `data_conflict` unless it is still current.
    pub base_data_version: u64,
    /// The new value. It can be `null`: unlike a patch, a set stores it.
    pub value: JsonValue,
}

/// Body of `PATCH /v1/artifacts/{id}/data`, with an `Idempotency-Key` header:
/// applies a JSON Merge Patch (RFC 7396) at a path. Objects merge member by
/// member, a `null` member removes that member, and arrays and scalars replace
/// what is there wholesale. A merge patch cannot store `null` in an object;
/// use a set for that.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateDataRequest {
    /// Where; the empty string is the root, whose patch must be an object.
    /// Objects above the path that are missing are made, unless the patch is
    /// `null`; a value in the way that is not an object is
    /// `data_path_conflict`.
    pub path: String,
    /// The version of the data space this change was prepared against.
    pub base_data_version: u64,
    /// The merge patch. A `null` patch at a path removes what is there.
    pub patch: JsonValue,
}

/// Body of `DELETE /v1/artifacts/{id}/data`, with an `Idempotency-Key`
/// header: removes the value at a path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeleteDataRequest {
    /// Where; the empty string is the root and empties the space. Removing
    /// what is not there succeeds and changes nothing.
    pub path: String,
    /// The version of the data space this change was prepared against.
    pub base_data_version: u64,
}

/// Authenticated server binding for opt-in browser data writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DataOutboxBinding {
    /// Registered actor: local or u- followed by 32 lowercase hex digits.
    #[schemars(regex(pattern = r"^(local|u-[0-9a-f]{32})$"))]
    pub actor_id: String,
    /// Canonical positive `SQLite` integer, encoded as decimal text.
    /// The 19-digit alternatives bound the string to 9223372036854775807.
    #[schemars(regex(
        pattern = r"^([1-9][0-9]{0,17}|[1-8][0-9]{18}|9[0-1][0-9]{17}|92[0-1][0-9]{16}|922[0-2][0-9]{15}|9223[0-2][0-9]{14}|92233[0-6][0-9]{13}|922337[0-1][0-9]{12}|92233720[0-2][0-9]{10}|922337203[0-5][0-9]{9}|9223372036[0-7][0-9]{8}|92233720368[0-4][0-9]{7}|922337203685[0-3][0-9]{6}|9223372036854[0-6][0-9]{5}|92233720368547[0-6][0-9]{4}|922337203685477[0-4][0-9]{3}|9223372036854775[0-7][0-9]{2}|922337203685477580[0-6]|9223372036854775807)$"
    ))]
    pub actor_generation: String,
    /// Existing feed identity, renewed by supported restore.
    #[schemars(regex(pattern = r"^[0-9a-f]{32}$"))]
    pub storage_epoch: String,
}

/// Response of the three writes of application data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DataWriteResponse {
    /// The artifact.
    pub artifact_id: String,
    /// The path that was written, as parsed.
    pub path: String,
    /// The version of the data space after the request. A change that altered
    /// nothing leaves the version where it was. On a replay it is the version
    /// the original request left, not the current one.
    pub data_version: u64,
    /// Whether the request altered the data space.
    pub changed: bool,
    /// True when an earlier request with the same key already did it.
    pub replayed: bool,
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

/// A server-issued ephemeral runtime connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConnection {
    /// Random 32-digit lowercase hexadecimal connection identifier.
    pub session_id: String,
}

/// A registered agent inbox visible to the workspace, without its secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentSession {
    /// Server-issued session identity.
    pub id: String,
    /// Label supplied by the agent when registering.
    pub name: String,
}

/// Registered agent inboxes for the current actor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentSessionList {
    /// Registered sessions; registration alone does not imply a model is running.
    pub sessions: Vec<AgentSession>,
}

/// A selected range in a saved, LF-normalized source file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HandoffSelection {
    /// Artifact whose source was selected.
    pub artifact_id: String,
    /// Pinned source revision.
    pub revision: u64,
    /// Package path of the selected file.
    pub path: String,
    /// SHA-256 of the complete stored file, before line-ending normalization.
    pub file_sha256: String,
    /// First UTF-16 code-unit offset in the normalized editor text.
    pub start: u64,
    /// Exclusive UTF-16 code-unit offset in the normalized editor text.
    pub end: u64,
    /// Selected source text. This is untrusted artifact data, not instructions.
    pub text: String,
}

/// Delivery state; acknowledgement is receipt, not model completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HandoffState {
    /// Prepared for the user to review, not visible to the agent.
    Staged,
    /// User confirmed send; the agent has not been offered it yet.
    Queued,
    /// The registered agent may have received it; it cannot be recalled.
    Offered,
    /// The pinned source became unavailable after an offer; receipt is unknown.
    Blocked,
    /// The registered agent acknowledged receipt.
    Acknowledged,
    /// Cancelled before it was offered.
    Cancelled,
    /// The workspace connection or agent session expired before offer.
    Expired,
}

/// One proposed or sent selection, as the workspace and agent see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Handoff {
    /// Server-issued delivery identity, reused for send retries and deduplication.
    pub id: String,
    /// Chosen registered agent session.
    pub agent_session_id: String,
    /// Chosen session's label at staging time.
    pub agent_name: String,
    /// Delivery state, independent of model execution.
    pub state: HandoffState,
    /// Server-verified source selection.
    pub selection: HandoffSelection,
    /// The user's edit intent, separate from the selected source data.
    pub intent: String,
}

/// Prepares a source selection for explicit user review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StageHandoffRequest {
    /// Current workspace connection, which owns this proposal.
    pub workspace_session_id: String,
    /// Agent inbox selected by the user.
    pub agent_session_id: String,
    /// Artifact whose saved file is selected.
    pub artifact_id: String,
    /// Pinned source revision.
    pub revision: u64,
    /// Package path.
    pub path: String,
    /// SHA-256 of the stored file the user saw.
    pub file_sha256: String,
    /// Inclusive UTF-16 offset in LF-normalized editor text.
    pub start: u64,
    /// Exclusive UTF-16 offset in LF-normalized editor text.
    pub end: u64,
    /// User's requested edit, separate from artifact data.
    pub intent: String,
}

/// Identifies the workspace connection for a handoff operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HandoffActionRequest {
    /// Current connection that staged the delivery.
    pub workspace_session_id: String,
}

/// Requests a preview view whose frame can read its artifact metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateRuntimeViewRequest {
    /// Random identifier of the connected workspace session.
    pub session_id: String,
    /// A random 32-digit lowercase hexadecimal identifier for this view.
    pub view_id: String,
    /// A positive, increasing generation within this view.
    pub generation: u64,
}

/// A short-lived runtime view bound to one actor, artifact, and revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RuntimeView {
    /// URL of the entry document on the content origin. An opted-in entry
    /// receives a runtime bootstrap; the stored source stays unchanged.
    pub url: String,
    /// Whether the entry explicitly opts in to the metadata bridge.
    pub enabled: bool,
    /// The server-issued connection this view belongs to.
    pub session_id: String,
    /// The artifact whose preview is shown.
    pub artifact_id: String,
    /// The pinned source revision shown in the frame.
    pub revision: u64,
    /// Current source head observed when this view was issued.
    pub head_revision: u64,
    /// Supported SDK pin, or null for an ordinary preview. Always present.
    #[schemars(required, extend("type" = ["string", "null"]))]
    pub sdk: Option<String>,
    /// Capabilities declared by this pinned revision.
    pub declared: Vec<String>,
    /// Capabilities supported by this server for the entry's SDK pin.
    pub supported: Vec<String>,
    /// Capabilities granted for this connection and source binding.
    pub granted: Vec<String>,
    /// Version to supply when replacing this grant.
    pub grant_version: u64,
    /// The view identifier supplied when it was created.
    pub view_id: String,
    /// This view's generation.
    pub generation: u64,
    /// A frame initialization secret, distinct from the account token.
    pub initialization: String,
    /// Expiry time in Unix milliseconds.
    pub expires_at_ms: i64,
    /// Lease duration in milliseconds, at most 2,147,483,647. Clients must use
    /// elapsed local time, not compare the server's Unix timestamp to theirs.
    #[schemars(range(max = 2_147_483_647))]
    pub lease_ms: u64,
}

/// Replaces the grant only if the user observed the same source and grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetRuntimeGrantRequest {
    /// The source head observed when choosing the grant.
    pub head_revision: u64,
    /// Version observed before the change.
    pub expected_version: u64,
    /// Complete supported subset; an empty list revokes every capability.
    pub capabilities: Vec<String>,
}

/// The new version of a connection-scoped grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RuntimeGrant {
    /// The grant version, used to guard the next change.
    pub version: u64,
}

/// Proves that the parent is requesting metadata for its initialized view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RuntimeMetadataRequest {
    /// Initialization secret issued with that view, never an account token.
    pub initialization: String,
}

/// Current metadata that a runtime frame may read for its own artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RuntimeMetadata {
    /// The bound artifact's identifier.
    pub artifact_id: String,
    /// Its current title.
    pub title: String,
    /// Its current source revision.
    pub current_revision: u64,
    /// Its current metadata revision.
    pub metadata_revision: u64,
}

/// Most bytes of one part a runtime frame reads from its source. A part
/// travels as base64 in a JSON body, so this bounds the response too.
pub const MAX_RUNTIME_SOURCE_PART_BYTES: u64 = 1_048_576;

/// Longest file name a runtime download offers, in ASCII characters. It is
/// the longest package path segment (100 bytes), so a file's name never
/// needs shortening.
pub const MAX_DOWNLOAD_NAME: usize = 100;

/// Proves that the parent is listing the source of its initialized view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSourceListRequest {
    /// Initialization secret issued with that view, never an account token.
    pub initialization: String,
}

/// The files of the revision a runtime view is pinned to. The listing is
/// that of the pinned revision, never of a newer one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSourceList {
    /// The pinned revision.
    pub revision: u64,
    /// Its files in path order, each path once.
    pub files: Vec<RevisionFile>,
}

/// Reads one part of one file of a runtime view's pinned revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSourceReadRequest {
    /// Initialization secret issued with that view, never an account token.
    pub initialization: String,
    /// Package path of the file.
    pub path: String,
    /// The first byte to read, counted from 0. An offset past the end of
    /// the file is refused; an offset at the end reads an empty final part.
    pub offset: u64,
    /// How many bytes at most, from 1 to 1,048,576. A part may be shorter at
    /// the end of the file. Parts are cut at bytes, not characters.
    #[schemars(range(min = 1, max = 1_048_576))]
    pub length: u64,
}

/// Exactly the bytes of one part of a file, with what identifies the whole
/// file. The bytes number `next_offset - offset`, or `size - offset` for the
/// final part. A client checks that before it relies on the other numbers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSourcePart {
    /// Package path of the file, as requested.
    pub path: String,
    /// The pinned revision.
    pub revision: u64,
    /// Where `content` starts in the file, as requested.
    pub offset: u64,
    /// Where the next part starts: greater than `offset` and less than
    /// `size`. Null once `content` reaches the end of the file.
    #[schemars(required, extend("type" = ["integer", "null"]))]
    pub next_offset: Option<u64>,
    /// The whole file's size in bytes.
    pub size: u64,
    /// The whole file's SHA-256 digest, as 64 lowercase hex digits.
    pub sha256: String,
    /// The bytes of the part, as base64.
    pub content: String,
}

/// What a runtime download offers the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeDownloadKind {
    /// One stored file of the pinned revision, byte for byte.
    File,
    /// The source export of the pinned revision (decision 0015).
    Archive,
}

/// Asks whether, or asks for, a download of the pinned revision. The same
/// body is used to check that an offer is allowed and, after the user
/// confirmed, to fetch the bytes; the server decides afresh each time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RuntimeDownloadRequest {
    /// Initialization secret issued with that view, never an account token.
    pub initialization: String,
    /// What to download.
    pub kind: RuntimeDownloadKind,
    /// Package path of the file for `file`; null for `archive`.
    #[serde(default)]
    #[schemars(required, extend("type" = ["string", "null"]))]
    pub path: Option<String>,
}

/// What the server would hand over, without any of it. The workspace shows
/// it to the user before they confirm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RuntimeDownloadOffer {
    /// What is offered.
    pub kind: RuntimeDownloadKind,
    /// Package path of the file for `file`; null for `archive`.
    #[schemars(required, extend("type" = ["string", "null"]))]
    pub path: Option<String>,
    /// The name to save under: at most 100 characters of
    /// `A-Z a-z 0-9 . _ -`, not starting with a dot.
    #[schemars(length(max = 100))]
    pub file_name: String,
    /// The file's size in bytes for `file`; null for `archive`.
    #[schemars(required, extend("type" = ["integer", "null"]))]
    pub size: Option<u64>,
    /// The file's SHA-256 digest for `file`; null for `archive`.
    #[schemars(required, extend("type" = ["string", "null"]))]
    pub sha256: Option<String>,
}

/// Most bytes of one file a runtime frame writes in one call (decision
/// 0033). It is the size of a read part: the file travels as base64 in one
/// JSON body, and the package rules still apply to the revision it makes.
pub const MAX_RUNTIME_SOURCE_WRITE_BYTES: u64 = 1_048_576;

/// The characters of the base64 text of [`MAX_RUNTIME_SOURCE_WRITE_BYTES`]
/// bytes, padding included.
pub const MAX_RUNTIME_SOURCE_WRITE_BASE64: u64 = MAX_RUNTIME_SOURCE_WRITE_BYTES.div_ceil(3) * 4;

/// Saves one file as the next revision of the artifact a runtime view is
/// pinned to, on behalf of the frame (needs the `source.write` grant). The
/// base is the view's own revision: a view that is not the artifact's
/// current revision is refused with the current one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSourceWriteRequest {
    /// Initialization secret issued with that view, never an account token.
    pub initialization: String,
    /// The frame's retry identity for this save: 8 to 36 ASCII letters,
    /// digits, `-` or `_`. The server derives the idempotency key from it,
    /// the artifact, the view and its generation, so a retry with the same
    /// identity and file is answered with the first outcome, and the same
    /// identity with another file is refused.
    #[schemars(length(min = 8, max = 36))]
    pub retry_id: String,
    /// Package path of the file; a new path adds a file.
    pub path: String,
    /// The file's bytes as base64, at most 1,048,576 bytes.
    #[schemars(length(max = 1_398_104))]
    pub content: String,
}

/// What a save made, which the client checks against the bytes it sent. The
/// server read the new revision back before it answered: the file holds
/// exactly the bytes sent and every other file is as it was.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSourceWriteReceipt {
    /// Package path of the file, as requested.
    pub path: String,
    /// The revision the save made: the view's revision plus one.
    pub revision: u64,
    /// The file's size in bytes: the number of bytes sent.
    pub size: u64,
    /// The file's SHA-256 digest, as 64 lowercase hex digits.
    pub sha256: String,
}

/// Joins the room of an artifact as one participant of the caller's tab, or
/// resumes the participant that tab already has in `slot` (decision 0034).
/// The tab is the runtime connection named in the route, never a field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RoomJoinRequest {
    /// The tab's own name for this participant, such as `page`: 1 to 32
    /// ASCII letters, digits, `.`, `_` or `-`. Joining again with the same
    /// slot resumes the same participant instead of adding another. A slot
    /// means nothing outside the tab that chose it and other participants
    /// never see it.
    #[schemars(length(min = 1, max = 32), regex(pattern = r"^[A-Za-z0-9._-]+$"))]
    pub slot: String,
    /// What the participant tells the others about itself, at most 256 bytes
    /// of UTF-8; the server counts bytes. Omitted means empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 256))]
    pub state: Option<String>,
}

/// Renews a participant's lease and reads the room (decision 0034). The
/// participant and its epoch are in the route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RoomHeartbeatRequest {
    /// The newest event position the caller has read, from the `head` of its
    /// last response. Events at or before it are not returned again, and
    /// events from before the participant joined are never returned
    /// whatever this says. A position past the room's newest event is
    /// refused.
    #[schemars(range(max = 9_007_199_254_740_991_u64))]
    pub after: u64,
    /// A new state of at most 256 bytes; omitted keeps the current one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 256))]
    pub state: Option<String>,
}

/// Sends one ephemeral event to the room (decision 0034). It is delivered to
/// the participants present who poll before the room drops it, and never
/// kept for anyone else: there is no outbox, no acknowledgement and no
/// retry that makes it arrive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RoomEventRequest {
    /// What the event is about: 1 to 32 ASCII letters, digits, `.`, `_` or
    /// `-`.
    #[schemars(length(min = 1, max = 32), regex(pattern = r"^[A-Za-z0-9._-]+$"))]
    pub kind: String,
    /// The event's text, at most 512 bytes of UTF-8; the server counts
    /// bytes. The server does not read it.
    #[schemars(length(max = 512))]
    pub data: String,
}

/// One participant in a room, as the viewer of a [`RoomSnapshot`] sees it.
/// `is_me` and `same_tab` are computed by the server from the connection
/// that asked; the participant sends neither.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RoomMember {
    /// The participant, a number the server assigned. It is a label, not a
    /// credential.
    pub participant: u64,
    /// The member's tab as this room labels tabs; equal numbers mean the
    /// same tab while the tab has a participant in the room.
    pub tab: u64,
    /// The account the member acts as. Every member of a room acts as the
    /// artifact's owner today.
    pub actor: String,
    /// Whether this member is the viewer's own participant.
    pub is_me: bool,
    /// Whether this member is in the viewer's tab. The viewer's own
    /// participant is in its own tab.
    pub same_tab: bool,
    /// What the member says about itself.
    pub state: String,
}

/// One ephemeral event, as the viewer of a [`RoomSnapshot`] sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RoomEvent {
    /// The event's position in the room, from 1. It is not a change feed
    /// cursor and not a revision.
    pub seq: u64,
    /// The sender's participant.
    pub participant: u64,
    /// The sender's tab, labelled as in [`RoomMember::tab`].
    pub tab: u64,
    /// Whether the viewer's own participant sent it.
    pub is_me: bool,
    /// Whether it came from the viewer's tab.
    pub same_tab: bool,
    /// What the sender says the event is about.
    pub kind: String,
    /// The sender's text.
    pub data: String,
}

/// The viewer's identity in a room, who is present, and the events it has
/// not read. A join and a heartbeat answer the same way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RoomSnapshot {
    /// The viewer's participant. A join that resumes a participant returns
    /// the same number as before; one after the lease ran out returns a new
    /// one.
    pub participant: u64,
    /// The viewer's connection generation, to send with every later call. A
    /// join that resumes the participant returns a newer one, and the
    /// connection that held the older one can no longer act.
    pub epoch: u64,
    /// The viewer's tab as this room labels it.
    pub tab: u64,
    /// How long the lease lasts from this response, in milliseconds. Renew
    /// well before it ends; the server measures it, not the client.
    pub lease_ms: u64,
    /// Everyone present, the viewer included, by participant.
    pub members: Vec<RoomMember>,
    /// Events after the requested position, oldest first. A join that makes
    /// a new participant returns none. A join that resumes a participant
    /// returns the events after the newest position that participant
    /// reported in a heartbeat (its join position if it reported none); they
    /// may repeat events an earlier connection of the participant handled, so
    /// tell them apart by `seq`.
    pub events: Vec<RoomEvent>,
    /// The newest event's position, 0 if the room never had one. Send it as
    /// `after` once its events were read.
    pub head: u64,
    /// Whether the room dropped events the caller had not read, including, in
    /// a resuming join, events that came while the participant was away.
    /// Ephemeral events can be lost; this says when it happened to this
    /// caller.
    pub missed: bool,
}

/// What a sent event was given.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RoomEventReceipt {
    /// The event's position in the room. It says the room accepted the
    /// event, not that anyone read it.
    pub seq: u64,
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
    /// The artifact was moved to the trash; reading it is `not_found`.
    Deleted,
    /// The artifact was taken out of the trash.
    Recovered,
    /// The artifact was removed from the trash for good; reading it is
    /// `not_found` and it never returns.
    Purged,
    /// A write changed the data space of the actor whose stream carries this
    /// change. The event names the space's new `data_version`, and no path or
    /// value; read the data again. The artifact's source and metadata did not
    /// change.
    DataChanged,
}

/// Summary information for an access token.
/// Secret plaintext is never exposed here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TokenInfo {
    /// Opaque token identifier.
    pub id: String,
    /// The user this token authenticates as, for its whole life: `local` for
    /// the owner profile (the bootstrap token and every token issued before
    /// users existed), otherwise a `u-` identifier from `POST /v1/users`.
    pub user_id: String,
    /// Human-readable label for this token.
    pub name: String,
    /// Masked prefix for identification.
    pub prefix: String,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
    /// ISO 8601 last-used timestamp, if used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<String>,
    /// ISO 8601 revocation timestamp, if revoked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<String>,
    /// Whether this is an administrative owner token.
    pub is_admin: bool,
}

/// Request to issue a new agent access token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateTokenRequest {
    /// Human-readable name or label (1-64 characters).
    pub name: String,
    /// The user the token authenticates as. Omitted: the owner profile, as
    /// before users existed. A user that is not registered is `not_found`
    /// and a disabled one is a conflict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
}

/// Response returned when a new token is created.
/// The `secret` is displayed only once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateTokenResponse {
    /// The token metadata.
    pub token: TokenInfo,
    /// The secret bearer token plaintext. Displayed only once.
    pub secret: String,
}

/// Response returned when a token is rotated.
/// The new `secret` is displayed only once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RotateTokenResponse {
    /// The updated token metadata.
    pub token: TokenInfo,
    /// The new secret bearer token plaintext. Displayed only once.
    pub secret: String,
}

/// List of tokens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TokenList {
    /// The tokens.
    pub tokens: Vec<TokenInfo>,
}

/// A registered user (decision 0036). It names an actor that owns artifacts;
/// it carries no credential and no role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UserInfo {
    /// The user's identifier, which is its actor: `local` for the owner
    /// profile, otherwise `u-` and 32 lowercase hexadecimal digits.
    pub id: String,
    /// The label the owner gave (1-64 characters).
    pub label: String,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
    /// ISO 8601 timestamp of the disabling, present while the user is
    /// disabled: its tokens are refused and its open streams, connections,
    /// links, and rooms are closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_at: Option<String>,
    /// Whether this is the local owner profile, which cannot be disabled and
    /// holds the only administrative credential.
    pub is_owner: bool,
}

/// Request to register a user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateUserRequest {
    /// Display label (1-64 characters, no control characters, no surrounding
    /// whitespace) that is not another user's.
    pub label: String,
}

/// The registered users, the owner first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UserList {
    /// The users.
    pub users: Vec<UserInfo>,
}

/// Data of a `change` event of `GET /v1/changes`: one commit that wrote a
/// revision or changed metadata, or one data write that changed the actor's
/// data space. Retries that replay a commit, failed commits, and updates that
/// changed nothing have none. It names what to read again and carries no
/// content, path, or value.
///
/// `data_version` is present exactly when `change` is `data_changed`: the
/// published schema requires it for that kind and forbids it for every other
/// kind, and deserializing refuses both violations (decision 0040).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(transform = data_version_only_for_data_changes)]
pub struct ArtifactChange {
    /// Opaque feed position of this change, also the event's `id`.
    pub cursor: String,
    /// The changed artifact.
    pub artifact_id: String,
    /// What the commit did.
    pub change: ChangeKind,
    /// The revision the commit created, or the current one for a metadata
    /// change (`metadata_updated`, `deleted`, `recovered`, or `purged`) or a
    /// data change (`data_changed`). Never a data version.
    pub revision: u64,
    /// The metadata revision after the commit, or the current one for a data
    /// change.
    pub metadata_revision: u64,
    /// The version the write left the data space at, a positive number.
    /// Present exactly for `data_changed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 9_007_199_254_740_991_u64))]
    pub data_version: Option<u64>,
    /// Commit time in Unix milliseconds.
    pub committed_at_ms: i64,
}

/// The largest whole number a JSON number states exactly.
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Adds the kind-dependent rule to the schema of [`ArtifactChange`]: a data
/// version for `data_changed`, none for any other kind. `null` is no data
/// version in either branch.
fn data_version_only_for_data_changes(schema: &mut schemars::Schema) {
    schema.insert(
        "if".to_owned(),
        serde_json::json!({
            "properties": { "change": { "const": "data_changed" } },
            "required": ["change"]
        }),
    );
    schema.insert(
        "then".to_owned(),
        serde_json::json!({
            "properties": { "data_version": { "type": "integer" } },
            "required": ["data_version"]
        }),
    );
    schema.insert(
        "else".to_owned(),
        serde_json::json!({ "not": { "required": ["data_version"] } }),
    );
}

/// The fields of an [`ArtifactChange`] as they arrive, before the rule that
/// ties `data_version` to the kind is checked. The data version keeps the
/// difference between absent and `null`, and `null` is refused like any other
/// value that is no data version.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactChangeFields {
    cursor: String,
    artifact_id: String,
    change: ChangeKind,
    revision: u64,
    metadata_revision: u64,
    #[serde(default)]
    data_version: DataVersionMember,
    committed_at_ms: i64,
}

/// A `data_version` member as it arrived: not there, `null`, or a number.
#[derive(Debug, Default, Clone, Copy)]
enum DataVersionMember {
    #[default]
    Absent,
    Null,
    Number(u64),
}

impl<'de> Deserialize<'de> for DataVersionMember {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Option::<u64>::deserialize(deserializer)
            .map(|number| number.map_or(Self::Null, Self::Number))
    }
}

// Written by hand because the derive would publish the schema of the fields
// type, which has none of the documentation or the kind-dependent rule.
impl<'de> Deserialize<'de> for ArtifactChange {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        ArtifactChangeFields::deserialize(deserializer)?
            .try_into()
            .map_err(serde::de::Error::custom)
    }
}

impl TryFrom<ArtifactChangeFields> for ArtifactChange {
    type Error = &'static str;

    fn try_from(fields: ArtifactChangeFields) -> Result<Self, Self::Error> {
        let data_version = match (fields.change, fields.data_version) {
            (ChangeKind::DataChanged, DataVersionMember::Number(version))
                if (1..=MAX_SAFE_INTEGER).contains(&version) =>
            {
                Some(version)
            }
            (ChangeKind::DataChanged, _) => {
                return Err("a data_changed change needs a data_version from 1 to 2^53 - 1");
            }
            (_, DataVersionMember::Absent) => None,
            (_, _) => {
                return Err("only a data_changed change carries a data_version");
            }
        };
        Ok(Self {
            cursor: fields.cursor,
            artifact_id: fields.artifact_id,
            change: fields.change,
            revision: fields.revision,
            metadata_revision: fields.metadata_revision,
            data_version,
            committed_at_ms: fields.committed_at_ms,
        })
    }
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
    /// Caller lacks authority to perform this operation.
    Forbidden,
    /// The resource does not exist or is not accessible.
    NotFound,
    /// The runtime connection is missing, closed, or expired; open a new one.
    RuntimeSessionExpired,
    /// The base revision is not current; reload and reapply the change.
    RevisionConflict,
    /// A file named by an edit is not as the edit expects it in the current
    /// revision (`POST /v1/artifacts/{id}/edits` only). Nothing was written,
    /// not even the other edits. The error lists the files and the current
    /// revision; read them again and edit again. The strict commands never
    /// answer this: for them a head that moved is `revision_conflict`.
    FileConflict,
    /// The base metadata revision is not current; reload and reapply the
    /// change.
    MetadataConflict,
    /// The artifact is not in the trash, so it cannot be purged. Move it to
    /// the trash first.
    NotInTrash,
    /// The unreferenced content is no longer the set the dry run reported, so
    /// nothing was deleted. Run the dry run again.
    ContentConflict,
    /// The base data version is not current; read the data again and reapply
    /// the change. Nothing was written. A listing of member names answers it
    /// too when its `at_data_version` is not the space's version, older or
    /// newer; start the listing again.
    DataConflict,
    /// The independent upload state changed.
    AssetConflict,
    /// The declared type is not currently accepted for new uploads.
    AssetTypeUnsupported,
    /// Empty or invalid asset content.
    InvalidAsset,
    /// Upload byte, count or logical quota exceeded.
    AssetLimitExceeded,
    /// The opt-in write authority changed; no key or mutation was recorded.
    OutboxBindingChanged,
    /// The path crosses a value that is not an object, so a write cannot make
    /// room below it. Nothing was written; set the value at a shorter path
    /// first, or write somewhere else. A listing of member names answers it
    /// too when the value at the path is not an object (a string, number,
    /// boolean, `null`, or array).
    DataPathConflict,
    /// The change would exceed a limit of application data: the value, the
    /// data space, the number of values in it, or how deeply they nest.
    /// Nothing was written.
    DataTooLarge,
    /// The job already succeeded, failed, or was cancelled, so it cannot be
    /// cancelled. Nothing was changed.
    JobEnded,
    /// The actor already has the largest number of jobs waiting or running.
    /// Wait for one to end or cancel one; nothing was enqueued.
    TooManyJobs,
    /// The idempotency key was already used for a different request.
    RequestKeyReused,
    /// The request body exceeds the size limit.
    PayloadTooLarge,
    /// The byte range selects no bytes of the file; nothing was read. Over
    /// HTTP it starts at or past the end of the file, and `Content-Range`
    /// gives the file's size; over MCP it starts past the end, and the
    /// message gives the size.
    RangeNotSatisfiable,
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
    /// The current data version, present for `data_conflict` (of a write, or
    /// of a listing whose `at_data_version` is not current).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_data_version: Option<u64>,
    /// Current independent state for `asset_conflict`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_asset_version: Option<u64>,
    /// The first files, in path order, whose edits did not hold; present for
    /// `file_conflict`, together with `current_revision`. At most 32.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 32))]
    pub conflicting_files: Option<Vec<ConflictingFile>>,
    /// How many edits did not hold, at least the length of `conflicting_files`;
    /// present for `file_conflict`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflicting_files_total: Option<u64>,
}

/// Body of every non-2xx JSON response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ErrorResponse {
    /// Error details.
    pub error: ErrorBody,
}

/// Immutable private upload metadata (0045).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Asset {
    /// Opaque identifier interpreted within its actor/artifact space.
    pub id: String,
    /// Stored MIME type, including historical image spellings. New uploads admit only UTF-8 plain text/CSV; PNG and JPEG are unsupported pending complete validation.
    pub content_type: String,
    /// Exact immutable byte count.
    pub size_bytes: u64,
    /// SHA-256 as lowercase hexadecimal.
    pub sha256: String,
    /// Server creation time.
    pub created_at_ms: i64,
}
/// Bounded current metadata and usage from one snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AssetList {
    /// Current independent asset version.
    pub asset_version: u64,
    /// At most 64 entries ordered by ID.
    #[schemars(length(max = 64))]
    pub assets: Vec<Asset>,
    /// Logical byte usage in this space.
    pub bytes: u64,
    /// Maximum associations per space.
    pub max_assets: u64,
    /// Maximum bytes per space.
    pub max_bytes: u64,
}
/// Current immutable metadata and space version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AssetResponse {
    /// Current version.
    pub asset_version: u64,
    /// Scoped immutable metadata.
    pub asset: Asset,
}
/// Stable bounded upload envelope. MIME syntax is separate from mutable admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UploadAssetRequest {
    /// Observed independent state version.
    pub base_asset_version: u64,
    /// Original lowercase, parameter-free MIME declaration.
    pub content_type: String,
    /// Strict standard base64, at most 5 MiB decoded bytes.
    pub content_base64: String,
}
/// Original metadata-only upload receipt; replay does not recreate deleted bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UploadAssetResponse {
    /// Original resulting version.
    pub asset_version: u64,
    /// Original metadata.
    pub asset: Asset,
    /// Whether this returns a prior committed result.
    pub replayed: bool,
}
/// Conditional association deletion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeleteAssetRequest {
    /// Observed independent state version.
    pub base_asset_version: u64,
}
/// Original association deletion receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeleteAssetResponse {
    /// Requested opaque identifier.
    pub asset_id: String,
    /// Original resulting version.
    pub asset_version: u64,
    /// Whether the original operation removed an association.
    pub deleted: bool,
    /// Whether this returns a prior committed result.
    pub replayed: bool,
}
