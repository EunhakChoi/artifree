/**
 * Capability names. `metadata.read`, `source.read`, `source.download` and
 * `source.write` are supported (decisions 0020, 0032 and 0033); the others
 * are proposed. A name is not a grant: the user grants each one separately in
 * the workspace.
 */
export type RuntimeCapability =
  | 'metadata.read'
  | 'source.read'
  | 'source.download'
  | 'source.write'
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
 * Why a runtime call failed. The set is closed and carries no server text.
 * `denied`: the capability is not granted. `stale_view`: the artifact has a
 * newer revision than this view; the workspace starts a new view, and the
 * user consents again. `not_found`: the view, the artifact or the file is
 * gone. `declined`: the user did not accept a download, or it was withdrawn.
 * `expired`: this view ended.
 */
export type RuntimeFailureCode =
  | 'invalid_request'
  | 'denied'
  | 'stale_view'
  | 'not_found'
  | 'rate_limited'
  | 'declined'
  | 'unavailable'
  | 'expired';

/** The error a runtime call rejects with. */
export interface RuntimeRequestFailure extends Error {
  readonly code: RuntimeFailureCode;
}

/** One file of the revision a view is pinned to. */
export interface SourceFile {
  readonly path: string;
  readonly size: number;
  /** SHA-256 of the whole file, 64 lowercase hex digits. */
  readonly sha256: string;
  readonly mediaType: string;
}

/** The files of the pinned revision, each path once. */
export interface SourceListing {
  readonly revision: number;
  readonly files: readonly SourceFile[];
}

/** A part of a file: `length` is from 1 to 1,048,576 bytes. */
export interface SourceReadRequest {
  readonly path: string;
  readonly offset: number;
  readonly length: number;
}

/**
 * Bytes of a file from `offset`. The bytes number `nextOffset - offset`, or
 * `size - offset` when `nextOffset` is null, which means they reach the end
 * of the file. Reading on from `nextOffset` always progresses.
 */
export interface SourcePart {
  readonly path: string;
  readonly offset: number;
  readonly nextOffset: number | null;
  readonly size: number;
  readonly sha256: string;
  readonly bytes: Uint8Array;
}

/**
 * The user confirmed and the workspace handed the bytes to the browser to
 * save. `started` does not mean the file was written to disk: the browser or
 * the user can still cancel the save.
 */
export interface DownloadReceipt {
  readonly started: true;
  readonly kind: 'file' | 'archive';
  /** The bytes the workspace received and handed over. */
  readonly bytes: number;
}

/**
 * One file to save. `content` is text (saved as UTF-8) or bytes, at most
 * 1,048,576 bytes. `retryId` is 8 to 36 ASCII letters, digits, `-` or `_`:
 * choose a new one for each save, and keep it to retry the same save.
 */
export interface SourceWriteRequest {
  readonly path: string;
  readonly content: string | Uint8Array;
  readonly retryId: string;
}

/**
 * The revision a save made. The frame has checked it against what it sent:
 * `path` is the one named, `size` the number of bytes sent and `sha256` their
 * digest. `revision` is the revision after the one this view is pinned to.
 */
export interface WriteReceipt {
  readonly path: string;
  readonly revision: number;
  readonly size: number;
  readonly sha256: string;
}

/** Operations on the pinned revision's source. */
export interface SourceRuntime {
  /** Lists the files; needs the `source.read` grant. */
  list(): Promise<SourceListing>;
  /** Reads one part of a file; needs the `source.read` grant. */
  read(request: SourceReadRequest): Promise<SourcePart>;
  /**
   * Asks the workspace to offer the stored file at `path`, or without
   * `path` the source export of the revision, for the user to save. Needs
   * the `source.download` grant and the user's confirmation each time.
   */
  requestDownload(request?: {
    readonly path: string;
  }): Promise<DownloadReceipt>;
  /**
   * Saves one file as a new revision; needs the `source.write` grant, which
   * lets the app change this artifact, and asks the user nothing more.
   * A view saves once: the new revision makes this view stale, so the next
   * save needs a new view and the user's consent again (`stale_view`).
   * Retrying with the same `retryId` and bytes after an unanswered attempt
   * is answered with the first outcome, while this view lives, and never
   * saves twice.
   */
  write(request: SourceWriteRequest): Promise<WriteReceipt>;
}

/** What an opted-in frame can do, each operation subject to its own grant. */
export interface ArtifactRuntime {
  /** Reads current metadata through the workspace's bound runtime view. */
  currentMetadata(): Promise<RuntimeMetadata>;
  readonly source: SourceRuntime;
}

declare global {
  interface Window {
    /** Present only after the artifact opts in to the runtime script. */
    artifree?: ArtifactRuntime;
  }
}
