/* Generated from artifree-contracts. Run npm run generate:contracts; do not edit. */

/**
 * Any JSON request or response body of HTTP API v1, the data of a change feed event, or the manifest of a source export.
 */
export type ApiBody =
  | CreateArtifactRequest
  | SaveRevisionRequest
  | RestoreRevisionRequest
  | UpdateArtifactRequest
  | CommitResponse
  | DuplicateArtifactResponse
  | UpdateArtifactResponse
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
  | ErrorResponse
  | ArtifactChange
  | FeedPosition
  | SourceExportManifest
  | CreateTokenRequest
  | CreateTokenResponse
  | RotateTokenResponse
  | TokenList
  | TokenInfo;
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
 * Source model family of an artifact. Only `app` can be created by this
 * version; the other families are reserved for planned source models.
 */
export type ArtifactKind = 'app' | 'document' | 'slides' | 'dashboard' | 'design' | 'motion';
/**
 * A part of an artifact that a duplicate copies.
 */
export type CopiedPart = 'files';
/**
 * Delivery state; acknowledgement is receipt, not model completion.
 */
export type HandoffState = 'staged' | 'queued' | 'offered' | 'blocked' | 'acknowledged' | 'cancelled' | 'expired';
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
  | 'metadata_conflict'
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
export type ChangeKind = 'created' | 'revised' | 'metadata_updated' | 'deleted' | 'recovered';
/**
 * Version of a source export manifest.
 */
export type SourceExportFormat = 'artifree.source-export.v1' | 'artifree.source-export.v2';
/**
 * What exported files need from a host to run as they do in Artifree.
 */
export type ExportRuntime = 'none' | 'metadata';

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
   * What the copy received. This version has no application data or
   * uploaded assets, so it copies files only.
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
   * change (`metadata_updated`, `deleted`, or `recovered`).
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
 * Body of `POST /v1/artifacts/{id}/revisions/{revision}/duplicate`:
 * creates an artifact from the files of that revision.
 */
export interface DuplicateArtifactRequest {
  /**
   * Display title of the copy, 1-200 characters. Without it the copy is
   * named after the source's current title followed by " (Copy)".
   */
  title?: string | null;
}
