/* Generated from artifree-contracts. Run npm run generate:contracts; do not edit. */

/**
 * Any JSON request or response body of HTTP API v1.
 */
export type ApiBody =
  | CreateArtifactRequest
  | SaveRevisionRequest
  | CommitResponse
  | Artifact
  | ArtifactList
  | Revision
  | RevisionList
  | Preview
  | ErrorResponse;
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
 * Machine-readable error category.
 */
export type ErrorCode =
  | 'invalid_request'
  | 'unauthenticated'
  | 'not_found'
  | 'revision_conflict'
  | 'request_key_reused'
  | 'payload_too_large'
  | 'unavailable'
  | 'internal';

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
   * Latest committed revision.
   */
  current_revision: number;
  /**
   * Opaque identifier.
   */
  id: string;
  kind: ArtifactKind;
  /**
   * Display title.
   */
  title: string;
  /**
   * Time of the latest revision in Unix milliseconds.
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
 * Response of `GET /v1/artifacts`.
 */
export interface ArtifactList {
  /**
   * Accessible artifacts, most recently updated first.
   */
  artifacts: Artifact[];
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
   * The current revision, present for `revision_conflict`.
   */
  current_revision?: number | null;
  /**
   * Human-readable explanation; not a stable contract.
   */
  message: string;
}
