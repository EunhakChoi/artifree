/**
 * Capability names. `metadata.read`, `source.read`, `source.write` and
 * `source.download` can be granted today (decisions 0020 and 0032); the rest
 * are proposed and nothing grants them. A name is not a grant: a frame has a
 * capability only while the user's connection currently grants it.
 */
export type RuntimeCapability =
  | 'metadata.read'
  | 'source.read'
  | 'source.write'
  | 'source.download'
  | 'model.generate'
  | 'data.read'
  | 'data.write'
  | 'files.read'
  | 'tools.call';

/**
 * Proposed capability binding. The server uses its own connection and view
 * binding for grants; this type grants no permission.
 */
export interface RuntimeSessionBinding {
  readonly sessionId: string;
  readonly artifactId: string;
  readonly revisionId: string;
  readonly capabilities: readonly RuntimeCapability[];
}

/** Current metadata available to a frame for its own artifact. */
export interface RuntimeMetadata {
  readonly artifactId: string;
  readonly title: string;
  readonly currentRevision: number;
  readonly metadataRevision: number;
}

/**
 * Why a request failed, as the frame can act on it. It names no secret and
 * quotes nothing the frame sent.
 *
 * - `invalid_request`: the request is malformed or breaks a path, size or
 *   count rule. Nothing was done.
 * - `not_granted`: the user's connection does not currently grant the
 *   capability (never granted, revoked, or the connection ended).
 * - `view_unavailable`: this view is closed, expired, replaced by a newer
 *   generation, or the artifact has a newer revision than the one the view
 *   was started for (its permissions belonged to that revision). The
 *   workspace has to start a new view, and the user has to allow again.
 * - `not_found`: no such file, or the artifact is no longer available.
 * - `revision_conflict`: the source moved past the revision this frame is
 *   pinned to. Nothing was written; `currentRevision` is the new head.
 * - `key_reused`: the save key was used for a different set of changes.
 * - `too_large`: a size limit of the package or of a request was exceeded.
 * - `range`: the range starts past the end of the file.
 * - `rate_limited`: too many requests; retry the same save key later.
 * - `busy`: the workspace is already handling a request of this kind.
 * - `declined`: the user did not confirm the download.
 * - `unavailable`: the server or the connection failed. The outcome of a save
 *   is unknown: retry with the same key to learn it.
 * - `refused`: anything else.
 */
export type RuntimeErrorCode =
  | 'invalid_request'
  | 'not_granted'
  | 'view_unavailable'
  | 'not_found'
  | 'revision_conflict'
  | 'key_reused'
  | 'too_large'
  | 'range'
  | 'rate_limited'
  | 'busy'
  | 'declined'
  | 'unavailable'
  | 'refused';

/** The shape of the error a rejected runtime request carries. */
export interface RuntimeRequestFailure extends Error {
  readonly code: RuntimeErrorCode;
  /** For `revision_conflict`, the revision that is now the head. */
  readonly currentRevision?: number | undefined;
}

/** One file of the revision a frame is pinned to. */
export interface RuntimeSourceFileInfo {
  readonly path: string;
  /** Size in bytes. */
  readonly size: number;
  /** SHA-256 of the file's bytes, as 64 lowercase hex digits. */
  readonly sha256: string;
  readonly mediaType: string;
}

/** The files of the pinned revision (`source.read`). */
export interface RuntimeSourceManifest {
  readonly artifactId: string;
  /** The pinned revision, which is not necessarily the current head. */
  readonly revision: number;
  readonly files: readonly RuntimeSourceFileInfo[];
}

/** Which bytes of a file to read. */
export interface RuntimeSourceRange {
  /** The first byte, counted from 0; the start of the file when absent. */
  readonly offset?: number | undefined;
  /** How many bytes at most. One answer holds at most 1 MiB. */
  readonly length?: number | undefined;
}

/** Part of one file of the pinned revision, cut at bytes. */
export interface RuntimeSourcePart {
  readonly path: string;
  readonly revision: number;
  /** Where this part starts in the file. */
  readonly offset: number;
  /** The whole file's size in bytes. */
  readonly size: number;
  /** SHA-256 of the whole file. */
  readonly sha256: string;
  /** Where the next part starts, or null when this one reaches the end. */
  readonly nextOffset: number | null;
  readonly bytes: Uint8Array;
}

/** One change of a save. Text is stored as its UTF-8 bytes. */
export type RuntimeSourceChange =
  | {
      readonly op: 'put';
      readonly path: string;
      readonly content: string | Uint8Array;
    }
  | { readonly op: 'delete'; readonly path: string };

/** A conditional save on top of the pinned revision (`source.write`). */
export interface RuntimeSaveRequest {
  /**
   * Retry identity, 8-100 ASCII letters, digits, `.`, `_`, `:` or `-`. Reuse
   * the same key to retry the same changes: the first result comes back with
   * `replayed: true` and no second revision is made.
   */
  readonly key: string;
  /** At most 512 changes, each to a different path. */
  readonly changes: readonly RuntimeSourceChange[];
}

/** The revision a save committed. */
export interface RuntimeSaveResult {
  readonly revision: number;
  readonly createdAtMs: number;
  readonly replayed: boolean;
  /**
   * The files this save wrote, with their sizes and digests; not the other
   * files of the revision, which a frame may not have the right to read.
   */
  readonly files: readonly RuntimeSourceFileInfo[];
}

/** What to hand to the user: one file, or the whole revision as an archive. */
export interface RuntimeDownloadRequest {
  /** A file of the pinned revision; the source export archive when absent. */
  readonly path?: string | undefined;
}

/**
 * The user confirmed and the browser was handed the file. This is a receipt,
 * not the bytes, and it does not prove the user kept the file.
 */
export interface RuntimeDownloadResult {
  readonly status: 'started';
  readonly kind: 'file' | 'archive';
  readonly name: string;
  readonly size: number;
  /** The pinned revision the bytes belong to. */
  readonly revision: number;
}

/**
 * Runtime operations. Each rejects with a {@link RuntimeRequestFailure}. All
 * but `currentMetadata` need the capability named in their description, which
 * the user grants separately for each view.
 */
export interface ArtifactRuntime {
  /** Reads current metadata through the workspace's bound runtime view. */
  currentMetadata(): Promise<RuntimeMetadata>;
  /** `source.read`: lists the pinned revision's files with sizes and digests. */
  listSource(): Promise<RuntimeSourceManifest>;
  /** `source.read`: reads up to 1 MiB of one file of the pinned revision. */
  readSource(
    path: string,
    range?: RuntimeSourceRange,
  ): Promise<RuntimeSourcePart>;
  /**
   * `source.write`: commits the changes as the revision after the pinned one,
   * only if that is still the head. Rejects with `revision_conflict` when
   * the source moved; it never overwrites. The commit is attributed to the
   * user who is viewing the artifact. The keys the app may use are its own:
   * a key an owner or an agent used elsewhere is neither seen nor reused.
   */
  saveSource(request: RuntimeSaveRequest): Promise<RuntimeSaveResult>;
  /**
   * `source.download`: asks the workspace to offer the pinned revision's file
   * or archive to the user, who has to confirm. Rejects with `declined` when
   * the user does not.
   */
  requestDownload(
    request?: RuntimeDownloadRequest,
  ): Promise<RuntimeDownloadResult>;
}

declare global {
  interface Window {
    /** Present only after the artifact opts in to the runtime script. */
    artifree?: ArtifactRuntime;
  }
}
