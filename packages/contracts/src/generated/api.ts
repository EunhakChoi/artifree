/* Generated from artifree-contracts. Run npm run generate:contracts; do not edit. */

/**
 * Any JSON request or response body of HTTP API v1, or the data of a change feed event.
 */
export type ApiBody =
  | CreateArtifactRequest
  | SaveRevisionRequest
  | UpdateArtifactRequest
  | CommitResponse
  | UpdateArtifactResponse
  | Artifact
  | ListArtifactsQuery
  | ArtifactList
  | Revision
  | RevisionList
  | Preview
  | ErrorResponse
  | ArtifactChange
  | FeedPosition;
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
 * Order of `GET /v1/artifacts`. Ties are broken by identifier.
 */
export type ListSort = 'updated' | 'created';
/**
 * Machine-readable error category.
 */
export type ErrorCode =
  | 'invalid_request'
  | 'unauthenticated'
  | 'not_found'
  | 'revision_conflict'
  | 'metadata_conflict'
  | 'request_key_reused'
  | 'payload_too_large'
  | 'rate_limited'
  | 'unavailable'
  | 'internal';
/**
 * What a commit did to an artifact. A kind that a client does not know
 * still means the artifact changed: read it again.
 */
export type ChangeKind = 'created' | 'revised' | 'metadata_updated';

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
 * Body of `PATCH /v1/artifacts/{id}`: a new title, pin, or both. Source
 * revisions are not touched.
 */
export interface UpdateArtifactRequest {
  /**
   * The metadata revision this change was prepared against. The update
   * fails with `metadata_conflict` unless it is still current.
   */
  base_metadata_revision: number;
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
   * Opaque identifier.
   */
  id: string;
  kind: ArtifactKind;
  /**
   * Version of the title and pin, starting at 1. It changes only when
   * they change, independently of `current_revision`.
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
   * Size in bytes.
   */
  size: number;
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
 * Query parameters of `GET /v1/artifacts`, all optional. Without any, the
 * response lists up to 500 artifacts, most recently updated first.
 */
export interface ListArtifactsQuery {
  /**
   * Page size, 1-500; 500 when absent.
   */
  limit?: number | null;
  /**
   * `next_page_token` of the previous page, with the same `q`, `sort`,
   * and `pinned`.
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
   * The revision the commit created, or the current one for
   * `metadata_updated`.
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
