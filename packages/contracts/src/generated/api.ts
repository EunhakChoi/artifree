/* Generated from artifree-contracts. Run npm run generate:contracts; do not edit. */

/**
 * Any JSON request or response body of HTTP API v1, the data of a change feed event, or the manifest of a source export.
 */
export type ApiBody =
  | CreateArtifactRequest
  | SaveRevisionRequest
  | EditFilesRequest
  | RestoreRevisionRequest
  | UpdateArtifactRequest
  | CommitResponse
  | DuplicateArtifactResponse
  | UpdateArtifactResponse
  | PurgeArtifactRequest
  | PurgeArtifactResponse
  | UnreferencedContent
  | RemoveUnreferencedContentRequest
  | RemoveUnreferencedContentResponse
  | Artifact
  | ArtifactList
  | Revision
  | RevisionList
  | Preview
  | RuntimeConnection
  | AgentSessionList
  | StageHandoffRequest
  | HandoffActionRequest
  | Handoff
  | CreateRuntimeViewRequest
  | RuntimeView
  | SetRuntimeGrantRequest
  | RuntimeGrant
  | RuntimeMetadataRequest
  | RuntimeMetadata
  | RuntimeSourceListRequest
  | RuntimeSourceList
  | RuntimeSourceReadRequest
  | RuntimeSourcePart
  | RuntimeDownloadRequest
  | RuntimeDownloadOffer
  | RuntimeSourceWriteRequest
  | RuntimeSourceWriteReceipt
  | RoomJoinRequest
  | RoomHeartbeatRequest
  | RoomEventRequest
  | RoomEventReceipt
  | RoomSnapshot
  | ErrorResponse
  | ArtifactChange
  | FeedPosition
  | SourceExportManifest
  | CreateTokenRequest
  | CreateTokenResponse
  | RotateTokenResponse
  | TokenList
  | TokenInfo
  | CreateUserRequest
  | UserInfo
  | UserList
  | EnqueueJobRequest
  | EnqueueJobResponse
  | CancelJobResponse
  | Job
  | JobList
  | AppData
  | SetDataRequest
  | UpdateDataRequest
  | DeleteDataRequest
  | DataWriteResponse
  | DataMembers
  | DataSnapshotEvent
  | DataChangeEvent
  | DataPosition;
/**
 * Byte encoding of a file's `content` string.
 */
export type ContentEncoding = 'utf8' | 'base64';
/**
 * One change in a revision commit.
 */
export type FileChange =
  | {
      /**
       * Complete new content.
       */
      content: string;
      encoding: ContentEncoding;
      op: 'put';
      /**
       * Portable relative path.
       */
      path: string;
    }
  | {
      op: 'delete';
      /**
       * Portable relative path.
       */
      path: string;
    };
/**
 * One edit of an edit request: a change to one file and the condition on
 * that file alone (decision 0035).
 */
export type FileEdit =
  | {
      /**
       * Complete new content.
       */
      content: string;
      encoding: ContentEncoding;
      /**
       * What the file must be in the current revision: `absent` when the
       * path must not be a file (a create), or the lowercase hexadecimal
       * SHA-256 digest of the bytes the file must have (a replace).
       */
      expect: string;
      op: 'put';
      /**
       * Portable relative path.
       */
      path: string;
    }
  | {
      /**
       * The lowercase hexadecimal SHA-256 digest of the bytes the file
       * must have in the current revision. `absent` is refused: deleting
       * what is not there is no edit.
       */
      expect: string;
      op: 'delete';
      /**
       * Portable relative path.
       */
      path: string;
    };
/**
 * Source model family of an artifact. Only `app` can be created by this
 * version; the other families are reserved for planned source models.
 */
export type ArtifactKind = 'app' | 'document' | 'slides' | 'dashboard' | 'design' | 'motion';
/**
 * A part of an artifact that a duplicate copies.
 */
export type CopiedPart = 'files' | 'data';
/**
 * Delivery state; acknowledgement is receipt, not model completion.
 */
export type HandoffState = 'staged' | 'queued' | 'offered' | 'blocked' | 'acknowledged' | 'cancelled' | 'expired';
/**
 * What a runtime download offers the user.
 */
export type RuntimeDownloadKind = 'file' | 'archive';
/**
 * Machine-readable error category.
 */
export type ErrorCode =
  | 'invalid_request'
  | 'unauthenticated'
  | 'forbidden'
  | 'not_found'
  | 'runtime_session_expired'
  | 'revision_conflict'
  | 'file_conflict'
  | 'metadata_conflict'
  | 'not_in_trash'
  | 'content_conflict'
  | 'data_conflict'
  | 'data_path_conflict'
  | 'data_too_large'
  | 'job_ended'
  | 'too_many_jobs'
  | 'request_key_reused'
  | 'payload_too_large'
  | 'range_not_satisfiable'
  | 'rate_limited'
  | 'unavailable'
  | 'internal';
/**
 * What a commit did to an artifact. A kind that a client does not know
 * still means the artifact changed: read it again.
 */
export type ChangeKind = 'created' | 'revised' | 'metadata_updated' | 'deleted' | 'recovered' | 'purged';
/**
 * Version of a source export manifest.
 */
export type SourceExportFormat = 'artifree.source-export.v1' | 'artifree.source-export.v2';
/**
 * What exported files need from a host to run as they do in Artifree.
 */
export type ExportRuntime = 'none' | 'metadata';
/**
 * What a job does (decision 0030). Each kind is trusted in-repository code
 * that runs in the Artifree worker process. This is not a way to run
 * untrusted code, to build sources, or to call model providers; none of
 * those exists yet.
 */
export type JobKind = 'verify_revision';
/**
 * Why a job ended as `failed`. A fixed vocabulary, never free text.
 */
export type JobFailure = 'revision_unavailable' | 'storage_unavailable' | 'lease_expired' | 'internal';
/**
 * What is wrong with one file of a verified revision.
 */
export type JobProblem = 'digest_mismatch' | 'size_mismatch' | 'content_missing';
/**
 * The outcome of a verification that ran to the end.
 */
export type JobVerdict = 'ok' | 'mismatch';
/**
 * Where a job is in its life. `succeeded`, `failed`, and `cancelled` are
 * terminal: a job there never changes again.
 */
export type JobState = 'queued' | 'running' | 'retry_scheduled' | 'succeeded' | 'failed' | 'cancelled';
/**
 * Any JSON value. Objects have unique member names, and every number has a magnitude of at most 2^53 - 1 and is finite, in whatever spelling (1e16 is refused like 10000000000000000). Whole numbers such as 1.0 and 1e2 are stored as integers, and a number too small for a double (0.1e-400) is stored as 0; 1e999 is refused as not finite.
 */
export type JsonValue =
  | {
      [k: string]: unknown;
    }
  | unknown[]
  | string
  | number
  | boolean
  | null;
/**
 * What a page of `GET /v1/artifacts/{id}/data/members` holds of each member
 * (decision 0038).
 */
export type DataMemberSelect = 'names' | 'values';

/**
 * Body of `POST /v1/artifacts`.
 */
export interface CreateArtifactRequest {
  /**
   * Initial files; `index.html` is required.
   */
  files: FileUpload[];
  /**
   * Display title, 1-200 characters.
   */
  title: string;
}
/**
 * One file of a new artifact.
 */
export interface FileUpload {
  /**
   * Complete file content.
   */
  content: string;
  encoding: ContentEncoding;
  /**
   * Portable relative path such as `index.html` or `assets/app.js`.
   */
  path: string;
}
/**
 * Body of `POST /v1/artifacts/{id}/revisions`.
 */
export interface SaveRevisionRequest {
  /**
   * The revision these changes were prepared against. The commit fails
   * with `revision_conflict` unless it is still current.
   */
  base_revision: number;
  /**
   * Changes applied to the base revision; unchanged files carry over. At
   * most 512 changes, each to a different path.
   */
  changes: FileChange[];
}
/**
 * Body of `POST /v1/artifacts/{id}/edits`: commits changes to some files as
 * the next revision, each applied only if its own file is as expected in the
 * **current** revision. One file that is not rejects all of them with
 * `file_conflict`.
 */
export interface EditFilesRequest {
  /**
   * The revision the caller read the expected digests from. It is not a
   * condition: files that no edit names may have changed since. It must be
   * a revision the artifact has.
   */
  base_revision: number;
  /**
   * At least one edit and at most 512, each to a different path.
   *
   * @maxItems 512
   */
  edits: FileEdit[];
}
/**
 * Body of `POST /v1/artifacts/{id}/revisions/{revision}/restore`: commits
 * the files of that earlier revision as the next revision.
 */
export interface RestoreRevisionRequest {
  /**
   * The revision the restore was prepared against. The commit fails with
   * `revision_conflict` unless it is still current.
   */
  base_revision: number;
}
/**
 * Body of `PATCH /v1/artifacts/{id}`: a new title, pin, trash state, or
 * several of them. Source revisions are not touched.
 */
export interface UpdateArtifactRequest {
  /**
   * The metadata revision this change was prepared against. The update
   * fails with `metadata_conflict` unless it is still current.
   */
  base_metadata_revision: number;
  /**
   * `true` moves the artifact to the trash, where every other request
   * treats it as missing; `false` takes it out, the only update an
   * artifact in the trash accepts.
   */
  deleted?: boolean | null;
  /**
   * Whether the artifact is pinned.
   */
  pinned?: boolean | null;
  /**
   * New display title, 1-200 characters.
   */
  title?: string | null;
}
/**
 * Response of a create or revision commit.
 */
export interface CommitResponse {
  artifact: Artifact;
  /**
   * True when an earlier request with the same key already committed it.
   */
  replayed: boolean;
  revision: Revision;
}
/**
 * Artifact metadata.
 */
export interface Artifact {
  /**
   * Creation time in Unix milliseconds.
   */
  created_at_ms: number;
  /**
   * Latest committed revision when this metadata was produced; see
   * [`CommitResponse::artifact`] for replays.
   */
  current_revision: number;
  /**
   * When the artifact was moved to the trash, in Unix milliseconds, or
   * `null` outside it. Always present.
   */
  deleted_at_ms: number | null;
  /**
   * Opaque identifier.
   */
  id: string;
  kind: ArtifactKind;
  /**
   * Version of the title, pin, and trash state, starting at 1. It changes
   * only when they change, independently of `current_revision`.
   */
  metadata_revision: number;
  /**
   * Whether the owner pinned the artifact.
   */
  pinned: boolean;
  /**
   * Display title.
   */
  title: string;
  /**
   * Time of the latest source revision in Unix milliseconds; metadata
   * updates do not change it.
   */
  updated_at_ms: number;
}
/**
 * An immutable source revision.
 */
export interface Revision {
  /**
   * Commit time in Unix milliseconds.
   */
  created_at_ms: number;
  /**
   * Actor that committed it.
   */
  created_by: string;
  /**
   * Files in path order.
   */
  files: RevisionFile[];
  /**
   * Revision number, starting at 1.
   */
  revision: number;
}
/**
 * A file in a revision manifest.
 */
export interface RevisionFile {
  /**
   * `Content-Type` used when the file is served.
   */
  media_type: string;
  /**
   * Package path.
   */
  path: string;
  /**
   * SHA-256 of the file's bytes, as 64 lowercase hex digits. Files with
   * equal digests have equal bytes.
   */
  sha256: string;
  /**
   * Size in bytes.
   */
  size: number;
}
/**
 * Response of `POST /v1/artifacts/{id}/revisions/{revision}/duplicate`.
 */
export interface DuplicateArtifactResponse {
  artifact: Artifact;
  /**
   * What the copy received: `files`, and `data` when the request asked for
   * it. Uploaded assets do not exist yet and are never copied.
   */
  copied: CopiedPart[];
  /**
   * True when an earlier request with the same key already created it.
   */
  replayed: boolean;
  revision: Revision;
}
/**
 * Response of `PATCH /v1/artifacts/{id}`.
 */
export interface UpdateArtifactResponse {
  artifact: Artifact;
  /**
   * True when an earlier request with the same key already applied it.
   */
  replayed: boolean;
}
/**
 * Body of `POST /v1/artifacts/{id}/purge`: removes an artifact that is in
 * the trash for good (decision 0029). It cannot be undone.
 */
export interface PurgeArtifactRequest {
  /**
   * The metadata revision of the artifact in the trash that the caller
   * saw. The purge fails with `metadata_conflict` unless it is still
   * current, so an artifact recovered and deleted again since is not
   * removed by a purge prepared against its earlier trash state.
   */
  base_metadata_revision: number;
}
/**
 * Response of `POST /v1/artifacts/{id}/purge`. After a purge the artifact is
 * `not_found` to every request, also the retry of an earlier create, save,
 * restore, duplicate, or update that named it; only a retry of the purge
 * itself answers, with `replayed: true`. The identifier is never used for
 * another artifact. A copy made from the artifact is not affected.
 */
export interface PurgeArtifactResponse {
  /**
   * The removed artifact.
   */
  artifact_id: string;
  /**
   * The metadata revision the purge recorded, which the change feed's
   * `purged` change also carries.
   */
  metadata_revision: number;
  /**
   * When the artifact was removed, in Unix milliseconds.
   */
  purged_at_ms: number;
  /**
   * True when an earlier request with the same key already purged it.
   */
  replayed: boolean;
}
/**
 * Response of `GET /v1/content/unreferenced`, the dry run: file content that
 * no revision file of any artifact refers to, in the trash or not. Nothing is
 * removed.
 */
export interface UnreferencedContent {
  /**
   * How many distinct contents.
   */
  blobs: number;
  /**
   * Their total size in bytes. The database file does not shrink; the
   * space is reused by later writes.
   */
  bytes: number;
  /**
   * The identity of this set: 64 lowercase hexadecimal digits (SHA-256 over
   * the sorted digests of its contents). Send it as `base_digest` to delete
   * exactly this set.
   */
  digest: string;
}
/**
 * Body of `DELETE /v1/content/unreferenced`, with an `Idempotency-Key`
 * header: the cleanup of the set a dry run reported (decision 0029).
 */
export interface RemoveUnreferencedContentRequest {
  /**
   * The `digest` of the dry run's set. The cleanup fails with
   * `content_conflict` and deletes nothing unless the unreferenced content
   * is still exactly that set.
   */
  base_digest: string;
}
/**
 * Response of `DELETE /v1/content/unreferenced`: what the first run under the
 * key deleted. A retry with the same key and `base_digest` returns the same
 * numbers with `replayed: true`, also when the set is empty by then.
 */
export interface RemoveUnreferencedContentResponse {
  /**
   * How many distinct contents were deleted.
   */
  blobs: number;
  /**
   * Their total size in bytes.
   */
  bytes: number;
  /**
   * True when an earlier request with the same key did the deleting.
   */
  replayed: boolean;
}
/**
 * Response of `GET /v1/artifacts`.
 */
export interface ArtifactList {
  /**
   * Accessible artifacts that match the query, in its order.
   */
  artifacts: Artifact[];
  /**
   * Pass as `page_token` to read the next page; absent on the last page.
   * Artifacts updated while a caller pages by update time can appear
   * twice or be missed.
   */
  next_page_token?: string | null;
}
/**
 * Response of `GET /v1/artifacts/{id}/revisions`.
 */
export interface RevisionList {
  /**
   * Revisions, newest first.
   */
  revisions: Revision[];
}
/**
 * A short-lived link that renders one pinned revision on the content origin.
 */
export interface Preview {
  /**
   * Expiry time in Unix milliseconds. Access is also rechecked per request.
   */
  expires_at_ms: number;
  /**
   * The pinned revision.
   */
  revision: number;
  /**
   * URL of the revision's `index.html`; relative links resolve inside it.
   */
  url: string;
}
/**
 * A server-issued ephemeral runtime connection.
 */
export interface RuntimeConnection {
  /**
   * Random 32-digit lowercase hexadecimal connection identifier.
   */
  session_id: string;
}
/**
 * Registered agent inboxes for the current actor.
 */
export interface AgentSessionList {
  /**
   * Registered sessions; registration alone does not imply a model is running.
   */
  sessions: AgentSession[];
}
/**
 * A registered agent inbox visible to the workspace, without its secret.
 */
export interface AgentSession {
  /**
   * Server-issued session identity.
   */
  id: string;
  /**
   * Label supplied by the agent when registering.
   */
  name: string;
}
/**
 * Prepares a source selection for explicit user review.
 */
export interface StageHandoffRequest {
  /**
   * Agent inbox selected by the user.
   */
  agent_session_id: string;
  /**
   * Artifact whose saved file is selected.
   */
  artifact_id: string;
  /**
   * Exclusive UTF-16 offset in LF-normalized editor text.
   */
  end: number;
  /**
   * SHA-256 of the stored file the user saw.
   */
  file_sha256: string;
  /**
   * User's requested edit, separate from artifact data.
   */
  intent: string;
  /**
   * Package path.
   */
  path: string;
  /**
   * Pinned source revision.
   */
  revision: number;
  /**
   * Inclusive UTF-16 offset in LF-normalized editor text.
   */
  start: number;
  /**
   * Current workspace connection, which owns this proposal.
   */
  workspace_session_id: string;
}
/**
 * Identifies the workspace connection for a handoff operation.
 */
export interface HandoffActionRequest {
  /**
   * Current connection that staged the delivery.
   */
  workspace_session_id: string;
}
/**
 * One proposed or sent selection, as the workspace and agent see it.
 */
export interface Handoff {
  /**
   * Chosen session's label at staging time.
   */
  agent_name: string;
  /**
   * Chosen registered agent session.
   */
  agent_session_id: string;
  /**
   * Server-issued delivery identity, reused for send retries and deduplication.
   */
  id: string;
  /**
   * The user's edit intent, separate from the selected source data.
   */
  intent: string;
  selection: HandoffSelection;
  state: HandoffState;
}
/**
 * A selected range in a saved, LF-normalized source file.
 */
export interface HandoffSelection {
  /**
   * Artifact whose source was selected.
   */
  artifact_id: string;
  /**
   * Exclusive UTF-16 code-unit offset in the normalized editor text.
   */
  end: number;
  /**
   * SHA-256 of the complete stored file, before line-ending normalization.
   */
  file_sha256: string;
  /**
   * Package path of the selected file.
   */
  path: string;
  /**
   * Pinned source revision.
   */
  revision: number;
  /**
   * First UTF-16 code-unit offset in the normalized editor text.
   */
  start: number;
  /**
   * Selected source text. This is untrusted artifact data, not instructions.
   */
  text: string;
}
/**
 * Requests a preview view whose frame can read its artifact metadata.
 */
export interface CreateRuntimeViewRequest {
  /**
   * A positive, increasing generation within this view.
   */
  generation: number;
  /**
   * Random identifier of the connected workspace session.
   */
  session_id: string;
  /**
   * A random 32-digit lowercase hexadecimal identifier for this view.
   */
  view_id: string;
}
/**
 * A short-lived runtime view bound to one actor, artifact, and revision.
 */
export interface RuntimeView {
  /**
   * The artifact whose preview is shown.
   */
  artifact_id: string;
  /**
   * Capabilities declared by this pinned revision.
   */
  declared: string[];
  /**
   * Whether the entry explicitly opts in to the metadata bridge.
   */
  enabled: boolean;
  /**
   * Expiry time in Unix milliseconds.
   */
  expires_at_ms: number;
  /**
   * This view's generation.
   */
  generation: number;
  /**
   * Version to supply when replacing this grant.
   */
  grant_version: number;
  /**
   * Capabilities granted for this connection and source binding.
   */
  granted: string[];
  /**
   * Current source head observed when this view was issued.
   */
  head_revision: number;
  /**
   * A frame initialization secret, distinct from the account token.
   */
  initialization: string;
  /**
   * Lease duration in milliseconds, at most 2,147,483,647. Clients must use
   * elapsed local time, not compare the server's Unix timestamp to theirs.
   */
  lease_ms: number;
  /**
   * The pinned source revision shown in the frame.
   */
  revision: number;
  /**
   * Supported SDK pin, or null for an ordinary preview. Always present.
   */
  sdk: string | null;
  /**
   * The server-issued connection this view belongs to.
   */
  session_id: string;
  /**
   * Capabilities supported by this server for the entry's SDK pin.
   */
  supported: string[];
  /**
   * URL of the entry document on the content origin. An opted-in entry
   * receives a runtime bootstrap; the stored source stays unchanged.
   */
  url: string;
  /**
   * The view identifier supplied when it was created.
   */
  view_id: string;
}
/**
 * Replaces the grant only if the user observed the same source and grant.
 */
export interface SetRuntimeGrantRequest {
  /**
   * Complete supported subset; an empty list revokes every capability.
   */
  capabilities: string[];
  /**
   * Version observed before the change.
   */
  expected_version: number;
  /**
   * The source head observed when choosing the grant.
   */
  head_revision: number;
}
/**
 * The new version of a connection-scoped grant.
 */
export interface RuntimeGrant {
  /**
   * The grant version, used to guard the next change.
   */
  version: number;
}
/**
 * Proves that the parent is requesting metadata for its initialized view.
 */
export interface RuntimeMetadataRequest {
  /**
   * Initialization secret issued with that view, never an account token.
   */
  initialization: string;
}
/**
 * Current metadata that a runtime frame may read for its own artifact.
 */
export interface RuntimeMetadata {
  /**
   * The bound artifact's identifier.
   */
  artifact_id: string;
  /**
   * Its current source revision.
   */
  current_revision: number;
  /**
   * Its current metadata revision.
   */
  metadata_revision: number;
  /**
   * Its current title.
   */
  title: string;
}
/**
 * Proves that the parent is listing the source of its initialized view.
 */
export interface RuntimeSourceListRequest {
  /**
   * Initialization secret issued with that view, never an account token.
   */
  initialization: string;
}
/**
 * The files of the revision a runtime view is pinned to. The listing is
 * that of the pinned revision, never of a newer one.
 */
export interface RuntimeSourceList {
  /**
   * Its files in path order, each path once.
   */
  files: RevisionFile[];
  /**
   * The pinned revision.
   */
  revision: number;
}
/**
 * Reads one part of one file of a runtime view's pinned revision.
 */
export interface RuntimeSourceReadRequest {
  /**
   * Initialization secret issued with that view, never an account token.
   */
  initialization: string;
  /**
   * How many bytes at most, from 1 to 1,048,576. A part may be shorter at
   * the end of the file. Parts are cut at bytes, not characters.
   */
  length: number;
  /**
   * The first byte to read, counted from 0. An offset past the end of
   * the file is refused; an offset at the end reads an empty final part.
   */
  offset: number;
  /**
   * Package path of the file.
   */
  path: string;
}
/**
 * Exactly the bytes of one part of a file, with what identifies the whole
 * file. The bytes number `next_offset - offset`, or `size - offset` for the
 * final part. A client checks that before it relies on the other numbers.
 */
export interface RuntimeSourcePart {
  /**
   * The bytes of the part, as base64.
   */
  content: string;
  /**
   * Where the next part starts: greater than `offset` and less than
   * `size`. Null once `content` reaches the end of the file.
   */
  next_offset: number | null;
  /**
   * Where `content` starts in the file, as requested.
   */
  offset: number;
  /**
   * Package path of the file, as requested.
   */
  path: string;
  /**
   * The pinned revision.
   */
  revision: number;
  /**
   * The whole file's SHA-256 digest, as 64 lowercase hex digits.
   */
  sha256: string;
  /**
   * The whole file's size in bytes.
   */
  size: number;
}
/**
 * Asks whether, or asks for, a download of the pinned revision. The same
 * body is used to check that an offer is allowed and, after the user
 * confirmed, to fetch the bytes; the server decides afresh each time.
 */
export interface RuntimeDownloadRequest {
  /**
   * Initialization secret issued with that view, never an account token.
   */
  initialization: string;
  kind: RuntimeDownloadKind;
  /**
   * Package path of the file for `file`; null for `archive`.
   */
  path?: string | null;
}
/**
 * What the server would hand over, without any of it. The workspace shows
 * it to the user before they confirm.
 */
export interface RuntimeDownloadOffer {
  /**
   * The name to save under: at most 100 characters of
   * `A-Z a-z 0-9 . _ -`, not starting with a dot.
   */
  file_name: string;
  kind: RuntimeDownloadKind;
  /**
   * Package path of the file for `file`; null for `archive`.
   */
  path: string | null;
  /**
   * The file's SHA-256 digest for `file`; null for `archive`.
   */
  sha256: string | null;
  /**
   * The file's size in bytes for `file`; null for `archive`.
   */
  size: number | null;
}
/**
 * Saves one file as the next revision of the artifact a runtime view is
 * pinned to, on behalf of the frame (needs the `source.write` grant). The
 * base is the view's own revision: a view that is not the artifact's
 * current revision is refused with the current one.
 */
export interface RuntimeSourceWriteRequest {
  /**
   * The file's bytes as base64, at most 1,048,576 bytes.
   */
  content: string;
  /**
   * Initialization secret issued with that view, never an account token.
   */
  initialization: string;
  /**
   * Package path of the file; a new path adds a file.
   */
  path: string;
  /**
   * The frame's retry identity for this save: 8 to 36 ASCII letters,
   * digits, `-` or `_`. The server derives the idempotency key from it,
   * the artifact, the view and its generation, so a retry with the same
   * identity and file is answered with the first outcome, and the same
   * identity with another file is refused.
   */
  retry_id: string;
}
/**
 * What a save made, which the client checks against the bytes it sent. The
 * server read the new revision back before it answered: the file holds
 * exactly the bytes sent and every other file is as it was.
 */
export interface RuntimeSourceWriteReceipt {
  /**
   * Package path of the file, as requested.
   */
  path: string;
  /**
   * The revision the save made: the view's revision plus one.
   */
  revision: number;
  /**
   * The file's SHA-256 digest, as 64 lowercase hex digits.
   */
  sha256: string;
  /**
   * The file's size in bytes: the number of bytes sent.
   */
  size: number;
}
/**
 * Joins the room of an artifact as one participant of the caller's tab, or
 * resumes the participant that tab already has in `slot` (decision 0034).
 * The tab is the runtime connection named in the route, never a field.
 */
export interface RoomJoinRequest {
  /**
   * The tab's own name for this participant, such as `page`: 1 to 32
   * ASCII letters, digits, `.`, `_` or `-`. Joining again with the same
   * slot resumes the same participant instead of adding another. A slot
   * means nothing outside the tab that chose it and other participants
   * never see it.
   */
  slot: string;
  /**
   * What the participant tells the others about itself, at most 256 bytes
   * of UTF-8; the server counts bytes. Omitted means empty.
   */
  state?: string | null;
}
/**
 * Renews a participant's lease and reads the room (decision 0034). The
 * participant and its epoch are in the route.
 */
export interface RoomHeartbeatRequest {
  /**
   * The newest event position the caller has read, from the `head` of its
   * last response. Events at or before it are not returned again, and
   * events from before the participant joined are never returned
   * whatever this says. A position past the room's newest event is
   * refused.
   */
  after: number;
  /**
   * A new state of at most 256 bytes; omitted keeps the current one.
   */
  state?: string | null;
}
/**
 * Sends one ephemeral event to the room (decision 0034). It is delivered to
 * the participants present who poll before the room drops it, and never
 * kept for anyone else: there is no outbox, no acknowledgement and no
 * retry that makes it arrive.
 */
export interface RoomEventRequest {
  /**
   * The event's text, at most 512 bytes of UTF-8; the server counts
   * bytes. The server does not read it.
   */
  data: string;
  /**
   * What the event is about: 1 to 32 ASCII letters, digits, `.`, `_` or
   * `-`.
   */
  kind: string;
}
/**
 * What a sent event was given.
 */
export interface RoomEventReceipt {
  /**
   * The event's position in the room. It says the room accepted the
   * event, not that anyone read it.
   */
  seq: number;
}
/**
 * The viewer's identity in a room, who is present, and the events it has
 * not read. A join and a heartbeat answer the same way.
 */
export interface RoomSnapshot {
  /**
   * The viewer's connection generation, to send with every later call. A
   * join that resumes the participant returns a newer one, and the
   * connection that held the older one can no longer act.
   */
  epoch: number;
  /**
   * Events after the requested position, oldest first. A join that makes
   * a new participant returns none. A join that resumes a participant
   * returns the events after the newest position that participant
   * reported in a heartbeat (its join position if it reported none); they
   * may repeat events an earlier connection of the participant handled, so
   * tell them apart by `seq`.
   */
  events: RoomEvent[];
  /**
   * The newest event's position, 0 if the room never had one. Send it as
   * `after` once its events were read.
   */
  head: number;
  /**
   * How long the lease lasts from this response, in milliseconds. Renew
   * well before it ends; the server measures it, not the client.
   */
  lease_ms: number;
  /**
   * Everyone present, the viewer included, by participant.
   */
  members: RoomMember[];
  /**
   * Whether the room dropped events the caller had not read, including, in
   * a resuming join, events that came while the participant was away.
   * Ephemeral events can be lost; this says when it happened to this
   * caller.
   */
  missed: boolean;
  /**
   * The viewer's participant. A join that resumes a participant returns
   * the same number as before; one after the lease ran out returns a new
   * one.
   */
  participant: number;
  /**
   * The viewer's tab as this room labels it.
   */
  tab: number;
}
/**
 * One ephemeral event, as the viewer of a [`RoomSnapshot`] sees it.
 */
export interface RoomEvent {
  /**
   * The sender's text.
   */
  data: string;
  /**
   * Whether the viewer's own participant sent it.
   */
  is_me: boolean;
  /**
   * What the sender says the event is about.
   */
  kind: string;
  /**
   * The sender's participant.
   */
  participant: number;
  /**
   * Whether it came from the viewer's tab.
   */
  same_tab: boolean;
  /**
   * The event's position in the room, from 1. It is not a change feed
   * cursor and not a revision.
   */
  seq: number;
  /**
   * The sender's tab, labelled as in [`RoomMember::tab`].
   */
  tab: number;
}
/**
 * One participant in a room, as the viewer of a [`RoomSnapshot`] sees it.
 * `is_me` and `same_tab` are computed by the server from the connection
 * that asked; the participant sends neither.
 */
export interface RoomMember {
  /**
   * The account the member acts as. Every member of a room acts as the
   * artifact's owner today.
   */
  actor: string;
  /**
   * Whether this member is the viewer's own participant.
   */
  is_me: boolean;
  /**
   * The participant, a number the server assigned. It is a label, not a
   * credential.
   */
  participant: number;
  /**
   * Whether this member is in the viewer's tab. The viewer's own
   * participant is in its own tab.
   */
  same_tab: boolean;
  /**
   * What the member says about itself.
   */
  state: string;
  /**
   * The member's tab as this room labels tabs; equal numbers mean the
   * same tab while the tab has a participant in the room.
   */
  tab: number;
}
/**
 * Body of every non-2xx JSON response.
 */
export interface ErrorResponse {
  error: ErrorBody;
}
/**
 * Error details.
 */
export interface ErrorBody {
  code: ErrorCode;
  /**
   * The first files, in path order, whose edits did not hold; present for
   * `file_conflict`, together with `current_revision`. At most 32.
   *
   * @maxItems 32
   */
  conflicting_files?: ConflictingFile[] | null;
  /**
   * How many edits did not hold, at least the length of `conflicting_files`;
   * present for `file_conflict`.
   */
  conflicting_files_total?: number | null;
  /**
   * The current data version, present for `data_conflict`.
   */
  current_data_version?: number | null;
  /**
   * The current metadata revision, present for `metadata_conflict`.
   */
  current_metadata_revision?: number | null;
  /**
   * The current revision, present for `revision_conflict`.
   */
  current_revision?: number | null;
  /**
   * Human-readable explanation; not a stable contract.
   */
  message: string;
}
/**
 * A file named by an edit that is not as expected, with what it is now.
 */
export interface ConflictingFile {
  /**
   * Lowercase hexadecimal SHA-256 digest of the file in the current
   * revision, or null when the path is not a file of it. Always present.
   */
  current_sha256: string | null;
  /**
   * The edited path.
   */
  path: string;
}
/**
 * Data of a `change` event of `GET /v1/changes`: one commit that wrote a
 * revision or changed metadata. Retries that replay a commit, failed
 * commits, and updates that changed nothing have none.
 * It names what to read again and carries no content.
 */
export interface ArtifactChange {
  /**
   * The changed artifact.
   */
  artifact_id: string;
  change: ChangeKind;
  /**
   * Commit time in Unix milliseconds.
   */
  committed_at_ms: number;
  /**
   * Opaque feed position of this change, also the event's `id`.
   */
  cursor: string;
  /**
   * The metadata revision after the commit.
   */
  metadata_revision: number;
  /**
   * The revision the commit created, or the current one for a metadata
   * change (`metadata_updated`, `deleted`, `recovered`, or `purged`).
   */
  revision: number;
}
/**
 * Data of the `ready` and `reset` events of `GET /v1/changes`. `ready`
 * starts a stream opened without a cursor. `reset` answers a cursor this
 * feed cannot continue from: changes may have been missed, so read the
 * state again. Later events follow this position.
 */
export interface FeedPosition {
  /**
   * Opaque feed position, also the event's `id`.
   */
  cursor: string;
}
/**
 * The manifest of a source export: the file `.artifree-export.json`, first
 * in the ZIP archive that `GET /v1/artifacts/{id}/revisions/{revision}/export`
 * answers. Package paths never start with a dot, so no file of the revision
 * has this name. Every other entry is one file of the revision, at its
 * package path, with its bytes unchanged.
 */
export interface SourceExportManifest {
  /**
   * The exported artifact.
   */
  artifact_id: string;
  /**
   * When the revision was committed, in Unix milliseconds; also the
   * modification time of every entry.
   */
  created_at_ms: number;
  /**
   * The file a preview opens: `index.html`.
   */
  entry: string;
  /**
   * The revision's files in path order, as the revision lists them.
   */
  files: RevisionFile[];
  format: SourceExportFormat;
  /**
   * The exported revision.
   */
  revision: number;
  runtime: ExportRuntime;
  /**
   * The artifact's title when it was exported. Titles are not part of a
   * revision.
   */
  title: string;
}
/**
 * Request to issue a new agent access token.
 */
export interface CreateTokenRequest {
  /**
   * Human-readable name or label (1-64 characters).
   */
  name: string;
  /**
   * The user the token authenticates as. Omitted: the owner profile, as
   * before users existed. A user that is not registered is `not_found`
   * and a disabled one is a conflict.
   */
  user_id?: string | null;
}
/**
 * Response returned when a new token is created.
 * The `secret` is displayed only once.
 */
export interface CreateTokenResponse {
  /**
   * The secret bearer token plaintext. Displayed only once.
   */
  secret: string;
  token: TokenInfo;
}
/**
 * Summary information for an access token.
 * Secret plaintext is never exposed here.
 */
export interface TokenInfo {
  /**
   * ISO 8601 creation timestamp.
   */
  created_at: string;
  /**
   * Opaque token identifier.
   */
  id: string;
  /**
   * Whether this is an administrative owner token.
   */
  is_admin: boolean;
  /**
   * ISO 8601 last-used timestamp, if used.
   */
  last_used_at?: string | null;
  /**
   * Human-readable label for this token.
   */
  name: string;
  /**
   * Masked prefix for identification.
   */
  prefix: string;
  /**
   * ISO 8601 revocation timestamp, if revoked.
   */
  revoked_at?: string | null;
  /**
   * The user this token authenticates as, for its whole life: `local` for
   * the owner profile (the bootstrap token and every token issued before
   * users existed), otherwise a `u-` identifier from `POST /v1/users`.
   */
  user_id: string;
}
/**
 * Response returned when a token is rotated.
 * The new `secret` is displayed only once.
 */
export interface RotateTokenResponse {
  /**
   * The new secret bearer token plaintext. Displayed only once.
   */
  secret: string;
  token: TokenInfo;
}
/**
 * List of tokens.
 */
export interface TokenList {
  /**
   * The tokens.
   */
  tokens: TokenInfo[];
}
/**
 * Request to register a user.
 */
export interface CreateUserRequest {
  /**
   * Display label (1-64 characters, no control characters, no surrounding
   * whitespace) that is not another user's.
   */
  label: string;
}
/**
 * A registered user (decision 0036). It names an actor that owns artifacts;
 * it carries no credential and no role.
 */
export interface UserInfo {
  /**
   * ISO 8601 creation timestamp.
   */
  created_at: string;
  /**
   * ISO 8601 timestamp of the disabling, present while the user is
   * disabled: its tokens are refused and its open streams, connections,
   * links, and rooms are closed.
   */
  disabled_at?: string | null;
  /**
   * The user's identifier, which is its actor: `local` for the owner
   * profile, otherwise `u-` and 32 lowercase hexadecimal digits.
   */
  id: string;
  /**
   * Whether this is the local owner profile, which cannot be disabled and
   * holds the only administrative credential.
   */
  is_owner: boolean;
  /**
   * The label the owner gave (1-64 characters).
   */
  label: string;
}
/**
 * The registered users, the owner first.
 */
export interface UserList {
  /**
   * The users.
   */
  users: UserInfo[];
}
/**
 * Body of `POST /v1/artifacts/{id}/jobs`, with an `Idempotency-Key` header:
 * enqueues a job for one revision of the artifact.
 */
export interface EnqueueJobRequest {
  kind: JobKind;
  /**
   * The source revision to pin. It must exist.
   */
  revision: number;
}
/**
 * Response of `POST /v1/artifacts/{id}/jobs`.
 */
export interface EnqueueJobResponse {
  job: Job;
  /**
   * True when an earlier request with the same key already enqueued it.
   */
  replayed: boolean;
}
/**
 * A persistent job. It pins the actor that enqueued it, one artifact, one
 * source revision, and a kind; none of these changes. Time fields are Unix
 * milliseconds. A client learns of progress by reading the job again: there
 * is no push for jobs, and the change feed does not carry them.
 */
export interface Job {
  /**
   * The artifact whose revision the job verifies.
   */
  artifact_id: string;
  /**
   * How many times a worker claimed the job.
   */
  attempt: number;
  /**
   * True when a cancel was requested while the job was running. It then
   * ends as `cancelled` unless it already ended; the request stops work
   * and rolls nothing back.
   */
  cancel_requested: boolean;
  /**
   * When the job was enqueued.
   */
  created_at_ms: number;
  /**
   * Why the job failed, once it is `failed`.
   */
  failure?: JobFailure | null;
  /**
   * When it reached a terminal state.
   */
  finished_at_ms?: number | null;
  /**
   * Job identifier, `job_` followed by 32 lowercase hex digits.
   */
  id: string;
  kind: JobKind;
  /**
   * How many claims it gets at most.
   */
  max_attempts: number;
  /**
   * The result, once the job is `succeeded`.
   */
  result?: JobResult | null;
  /**
   * While `retry_scheduled`, the earliest time of the next attempt.
   */
  retry_at_ms?: number | null;
  /**
   * The pinned source revision.
   */
  revision: number;
  /**
   * When a worker first claimed it.
   */
  started_at_ms?: number | null;
  state: JobState;
}
/**
 * The result of a `verify_revision` job that ran to the end. It holds counts
 * and package paths of the pinned revision, never file content.
 */
export interface JobResult {
  /**
   * The sum of the sizes of the content that was read.
   */
  bytes_checked: number;
  /**
   * How many files of the revision were read.
   */
  files_checked: number;
  /**
   * How many files mismatched, counted without the bound of `mismatches`.
   */
  mismatch_count: number;
  /**
   * The first mismatches in path order, at most 50.
   */
  mismatches: JobMismatch[];
  /**
   * True when `mismatches` lists fewer than `mismatch_count`.
   */
  truncated: boolean;
  verdict: JobVerdict;
}
/**
 * One file that does not match its listing.
 */
export interface JobMismatch {
  /**
   * The file's package path, relative to the artifact.
   */
  path: string;
  problem: JobProblem;
}
/**
 * Response of `POST /v1/jobs/{id}/cancel`. A job that had not started is
 * `cancelled`. A running job is still `running` with `cancel_requested`: its
 * worker stops at its next checkpoint, and the job ends `cancelled` unless it
 * ended first. A job that already ended answers `job_ended`.
 */
export interface CancelJobResponse {
  job: Job;
  /**
   * True when an earlier request with the same key already asked.
   */
  replayed: boolean;
}
/**
 * Response of `GET /v1/jobs` and `GET /v1/artifacts/{id}/jobs`.
 */
export interface JobList {
  /**
   * Jobs of artifacts that are not in the trash, newest first.
   */
  jobs: Job[];
  /**
   * Pass as `page_token` to read the next page; absent on the last page.
   */
  next_page_token?: string | null;
}
/**
 * Response of `GET /v1/artifacts/{id}/data`: the value at a path of the
 * caller's data space of the artifact, and the version of the space. A path
 * with nothing at it is a successful read with `exists` false.
 */
export interface AppData {
  /**
   * The artifact.
   */
  artifact_id: string;
  /**
   * The version of the data space: the `base_data_version` of the next
   * write. 0 for a space nobody wrote. It is unrelated to the source
   * revision and to the metadata revision.
   */
  data_version: number;
  /**
   * Whether anything is at the path. The root always exists.
   */
  exists: boolean;
  /**
   * The path that was read, as parsed.
   */
  path: string;
  /**
   * The value at the path, present when `exists` is true. It can be
   * `null`: a member set to `null`, which is present as `null`, unlike a
   * value that is not there, which has no `value` member.
   */
  value?: JsonValue | null;
}
/**
 * Body of `PUT /v1/artifacts/{id}/data`, with an `Idempotency-Key` header:
 * replaces the value at a path.
 */
export interface SetDataRequest {
  /**
   * The version of the data space this change was prepared against. The
   * write fails with `data_conflict` unless it is still current.
   */
  base_data_version: number;
  /**
   * Where; the empty string is the root, whose value must be an object and
   * replaces the whole space. Objects above the path that are missing are
   * made; a value in the way that is not an object is `data_path_conflict`.
   */
  path: string;
  value: JsonValue;
}
/**
 * Body of `PATCH /v1/artifacts/{id}/data`, with an `Idempotency-Key` header:
 * applies a JSON Merge Patch (RFC 7396) at a path. Objects merge member by
 * member, a `null` member removes that member, and arrays and scalars replace
 * what is there wholesale. A merge patch cannot store `null` in an object;
 * use a set for that.
 */
export interface UpdateDataRequest {
  /**
   * The version of the data space this change was prepared against.
   */
  base_data_version: number;
  patch: JsonValue;
  /**
   * Where; the empty string is the root, whose patch must be an object.
   * Objects above the path that are missing are made, unless the patch is
   * `null`; a value in the way that is not an object is
   * `data_path_conflict`.
   */
  path: string;
}
/**
 * Body of `DELETE /v1/artifacts/{id}/data`, with an `Idempotency-Key`
 * header: removes the value at a path.
 */
export interface DeleteDataRequest {
  /**
   * The version of the data space this change was prepared against.
   */
  base_data_version: number;
  /**
   * Where; the empty string is the root and empties the space. Removing
   * what is not there succeeds and changes nothing.
   */
  path: string;
}
/**
 * Response of the three writes of application data.
 */
export interface DataWriteResponse {
  /**
   * The artifact.
   */
  artifact_id: string;
  /**
   * Whether the request altered the data space.
   */
  changed: boolean;
  /**
   * The version of the data space after the request. A change that altered
   * nothing leaves the version where it was. On a replay it is the version
   * the original request left, not the current one.
   */
  data_version: number;
  /**
   * The path that was written, as parsed.
   */
  path: string;
  /**
   * True when an earlier request with the same key already did it.
   */
  replayed: boolean;
}
/**
 * Response of `GET /v1/artifacts/{id}/data/members`.
 */
export interface DataMembers {
  /**
   * The artifact.
   */
  artifact_id: string;
  canonical: CanonicalMembers;
  query: DataMembersQuery;
}
/**
 * The committed state a page reports. Nothing in it is pending: the server
 * holds no write that was not committed, and a client that queues writes
 * keeps them beside this object, never inside it (decision 0038).
 */
export interface CanonicalMembers {
  /**
   * The version of the data space the page was read at. Pages of one
   * listing are not tied to one version: compare this between pages.
   */
  data_version: number;
  /**
   * Whether an object is at the path. False where nothing is and where the
   * value is not an object; `members` is then empty.
   */
  exists: boolean;
  /**
   * The page, by the UTF-8 bytes of the names.
   */
  members: DataMember[];
  /**
   * Pass as `after` for the next page; present exactly when a further
   * matching member exists. A member that existed throughout the reads of a
   * listing appears once; one added or removed meanwhile may not appear.
   */
  next_after?: string | null;
}
/**
 * One member of a page.
 */
export interface DataMember {
  /**
   * The member's name.
   */
  name: string;
  /**
   * True when the query selected `values` but this member's name and value
   * together exceed the 512 KiB bound of a page, so `value` is left out;
   * read it with `GET data` at its path. Absent otherwise.
   */
  too_large?: boolean | null;
  /**
   * Its value, when the query selected `values`. A stored `null` is present
   * as `null`.
   */
  value?: JsonValue | null;
}
/**
 * The query of a page as the server parsed and completed it: two requests
 * that mean the same thing have the same `DataMembersQuery`.
 */
export interface DataMembersQuery {
  /**
   * The name the page starts after, when the query had one.
   */
  after?: string | null;
  /**
   * The page size that applied.
   */
  limit: number;
  /**
   * The collection's path, as parsed.
   */
  path: string;
  /**
   * The name prefix, when the query had one.
   */
  prefix?: string | null;
  select: DataMemberSelect;
}
/**
 * Data of the `snapshot` and `reset` events of `GET
 * /v1/artifacts/{id}/data/events`. `snapshot` opens a stream that has no
 * `Last-Event-ID`. `reset` answers a cursor that cannot be continued: the
 * window of retained writes no longer holds every write after it, the cursor
 * is ahead of the data or from another database, or the window was lost.
 * Discard what is held for the path and take this value; later events follow
 * it.
 */
export interface DataSnapshotEvent {
  /**
   * Data cursor of this position, also the event's `id`. Opaque.
   */
  cursor: string;
  /**
   * The version of the data space the value was read at.
   */
  data_version: number;
  /**
   * Whether anything is at the path.
   */
  exists: boolean;
  /**
   * The watched path.
   */
  path: string;
  /**
   * The value at the path, present when `exists` is true. A stored `null`
   * is present as `null`.
   */
  value?: JsonValue | null;
}
/**
 * Data of a `change` event of `GET /v1/artifacts/{id}/data/events`: after the
 * write that left the space at `data_version`, the value at `path` is
 * `value`, or is gone. `path` is the watched path when the write replaced it
 * or something above it, and the written path when the write was below it.
 * Applying each event in order to the value of the `snapshot` or `reset` gives
 * the value that a read reports at the same version. Writes that did not touch
 * the watched path are not sent.
 */
export interface DataChangeEvent {
  /**
   * Data cursor after this write, also the event's `id`. Opaque.
   */
  cursor: string;
  /**
   * The version of the data space the write left.
   */
  data_version: number;
  /**
   * Whether anything is at the path afterwards.
   */
  exists: boolean;
  /**
   * The path the new state is for, within the watched path.
   */
  path: string;
  /**
   * The value at the path afterwards, present when `exists` is true.
   */
  value?: JsonValue | null;
}
/**
 * Data of the `position` event of `GET /v1/artifacts/{id}/data/events`:
 * every write up to `data_version` has been sent or does not touch the
 * watched path. It follows a resumption and any run of writes that no sent
 * `change` covers, so the stored cursor keeps up.
 */
export interface DataPosition {
  /**
   * Data cursor of this position, also the event's `id`. Opaque.
   */
  cursor: string;
  /**
   * The data version reached.
   */
  data_version: number;
}

/**
 * Order of `GET /v1/artifacts`. Ties are broken by identifier.
 */
export type ListSort = 'updated' | 'created';

/**
 * Query parameters of `GET /v1/artifacts`, all optional. Without any, the
 * response lists up to 500 artifacts, most recently updated first.
 */
export interface ListArtifactsQuery {
  /**
   * `true` lists the trash instead of the artifacts outside it.
   */
  deleted?: boolean | null;
  /**
   * Page size, 1-500; 500 when absent.
   */
  limit?: number | null;
  /**
   * `next_page_token` of the previous page, with the same `q`, `sort`,
   * `pinned`, and `deleted`.
   */
  page_token?: string | null;
  /**
   * Only pinned (`true`) or only unpinned (`false`) artifacts.
   */
  pinned?: boolean | null;
  /**
   * Only artifacts whose title contains this text, 1-200 characters.
   * ASCII letters match regardless of case; other characters exactly.
   */
  q?: string | null;
  /**
   * The order.
   */
  sort?: ListSort | null;
}

/**
 * Query parameters of `GET /v1/jobs` and `GET /v1/artifacts/{id}/jobs`, all
 * optional.
 */
export interface ListJobsQuery {
  /**
   * Page size, 1-100; 50 when absent.
   */
  limit?: number | null;
  /**
   * `next_page_token` of the previous page of the same list.
   */
  page_token?: string | null;
}

/**
 * Body of `POST /v1/jobs/{id}/cancel`, with an `Idempotency-Key` header. It
 * has no fields.
 */
export interface CancelJobRequest {}

/**
 * Body of `POST /v1/artifacts/{id}/revisions/{revision}/duplicate`:
 * creates an artifact from the files of that revision.
 */
export interface DuplicateArtifactRequest {
  /**
   * Whether the copy also receives a copy of the source's application
   * data, as its own independent data space. By default `false`: the copy
   * receives files only. The data copied is the source's data when the copy
   * commits, whichever revision the files come from.
   */
  copy_data?: boolean | null;
  /**
   * Display title of the copy, 1-200 characters. Without it the copy is
   * named after the source's current title followed by " (Copy)".
   */
  title?: string | null;
}
