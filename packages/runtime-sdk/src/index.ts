/**
 * Capability names. `metadata.read`, `source.read`, `source.download`,
 * `source.write`, `assets.read`, `assets.write`, `assets.download`,
 * `data.read` and `data.write` are supported (decisions 0020, 0032, 0033,
 * 0045 and 0053); the others are proposed. A name is not a grant: the user
 * grants each one separately in the workspace.
 */
export type RuntimeCapability =
  | 'metadata.read'
  | 'source.read'
  | 'source.download'
  | 'source.write'
  | 'assets.read'
  | 'assets.write'
  | 'assets.download'
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
 * user consents again. `conflict`: the uploaded assets or the application
 * data changed since the call named their version; `currentAssetVersion`
 * (assets) or `currentDataVersion` (data) on the failure names the current
 * version when the workspace could tell. `not_found`: the view, the artifact or
 * the file is gone. `declined`: the user did not accept a download. A download
 * the user withdraws gets no answer: its call fails when its timeout ends.
 * `expired`: this view ended.
 */
export type RuntimeFailureCode =
  | 'invalid_request'
  | 'denied'
  | 'stale_view'
  | 'conflict'
  | 'not_found'
  | 'rate_limited'
  | 'declined'
  | 'unavailable'
  | 'expired';

/** The error a runtime call rejects with. */
export interface RuntimeRequestFailure extends Error {
  readonly code: RuntimeFailureCode;
  /**
   * On an asset `conflict`: the version the artifact has now, if it was named.
   */
  readonly currentAssetVersion?: number;
  /** On a data write's `conflict`: the data version now, if it was named. */
  readonly currentDataVersion?: number;
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

/** The media types an uploaded asset may have (decision 0045). */
export type AssetContentType =
  'text/plain' | 'text/csv' | 'image/png' | 'image/jpeg';

/** One uploaded asset of the artifact, as the workspace lists it. */
export interface AssetInfo {
  /** The asset ID: 64 lowercase hexadecimal digits. */
  readonly id: string;
  readonly contentType: AssetContentType;
  /** Bytes in the asset: from 1 to 5 MiB. */
  readonly size: number;
  /** SHA-256 of the whole asset, 64 lowercase hex digits. */
  readonly sha256: string;
  /** When the asset was uploaded, in milliseconds since the Unix epoch. */
  readonly createdAtMs: number;
}

/**
 * The assets of the artifact and the version of its asset space, which
 * increases with each upload or deletion.
 */
export interface AssetListing {
  readonly assetVersion: number;
  /** At most 64 assets, in ascending ID order. */
  readonly assets: readonly AssetInfo[];
}

/** The whole content of one asset, verified against its size and digest. */
export interface AssetContent {
  readonly assetId: string;
  readonly contentType: AssetContentType;
  readonly size: number;
  readonly sha256: string;
  readonly bytes: Uint8Array;
}

/**
 * One asset to add. `content` is text (saved as UTF-8) or bytes, from 1 to
 * 1,048,576 bytes. `baseAssetVersion` is the asset version the frame last saw,
 * from `list`, a receipt or a `conflict`. `retryId` is 8 to 36 ASCII letters,
 * digits, `-` or `_`: choose a new one for each change, and keep it only to
 * retry the identical call.
 */
export interface AssetUploadRequest {
  readonly contentType: AssetContentType;
  readonly content: string | Uint8Array;
  readonly baseAssetVersion: number;
  readonly retryId: string;
}

/**
 * The asset the upload stored, checked against the bytes the frame sent.
 * `assetVersion` is the version the upload produced: a retry that gets the
 * first outcome back reports that same version, even if the artifact has moved
 * on since.
 */
export interface AssetUploadReceipt {
  readonly asset: AssetInfo;
  readonly assetVersion: number;
}

/**
 * One asset to delete, named by its ID. `baseAssetVersion` and `retryId` follow
 * `AssetUploadRequest`.
 */
export interface AssetDeleteRequest {
  readonly assetId: string;
  readonly baseAssetVersion: number;
  readonly retryId: string;
}

/**
 * The outcome of a deletion. `deleted` is false when the artifact had no such
 * asset; the version then does not move.
 */
export interface AssetDeleteReceipt {
  readonly assetId: string;
  readonly assetVersion: number;
  readonly deleted: boolean;
}

/**
 * The user confirmed and the workspace handed the asset's bytes to the browser
 * to save. As for `DownloadReceipt`, `started` does not mean the file reached
 * disk: the browser or the user can still cancel the save.
 */
export interface AssetDownloadReceipt {
  readonly started: true;
  /** The asset asked for: 64 lowercase hexadecimal digits. */
  readonly assetId: string;
  /** The bytes the workspace received and handed over. */
  readonly bytes: number;
}

/** Operations on the uploaded assets of the artifact (decision 0045). */
export interface AssetsRuntime {
  /** Lists the assets; needs the `assets.read` grant. */
  list(): Promise<AssetListing>;
  /**
   * Reads all the bytes of one asset in parts of at most 256 KiB. It resolves
   * only when the assembled bytes match the asset's size and SHA-256. A
   * mismatch, or a part that breaks the read rules, rejects with
   * `unavailable`; a refusal from the workspace keeps its own code. Needs the
   * `assets.read` grant.
   */
  read(request: { readonly assetId: string }): Promise<AssetContent>;
  /**
   * Adds one asset; needs the `assets.write` grant, which does not include
   * `assets.read`. A new call based on a version other than the current one is
   * refused with `conflict`, and `currentAssetVersion` names the current one. A
   * source save since this view was pinned is `stale_view`. The SDK never
   * retries a write by itself. A retry of the identical call, with the same
   * `retryId`, gets the first outcome back while this view lives: use it after
   * an attempt that ended `unavailable`, which may have been applied.
   */
  upload(request: AssetUploadRequest): Promise<AssetUploadReceipt>;
  /**
   * Deletes one asset, with the rules of `upload`; the stored bytes cannot be
   * restored. `deleted` is false when the artifact had no such asset.
   */
  delete(request: AssetDeleteRequest): Promise<AssetDeleteReceipt>;
  /**
   * Asks the workspace to offer one uploaded asset, named by its ID, for the
   * user to save. Needs the `assets.download` grant and the user's
   * confirmation each time. The SDK never retries it: each call asks again.
   */
  requestDownload(request: {
    readonly assetId: string;
  }): Promise<AssetDownloadReceipt>;
}

/**
 * The value at one path of the application data saved for this artifact
 * (decision 0053).
 */
export interface DataValue {
  /** The path read, as sent; the empty path is the root. */
  readonly path: string;
  /** The version of the data space. */
  readonly dataVersion: number;
  /** Whether anything is at the path. The root always exists. */
  readonly exists: boolean;
  /**
   * The parsed value; `undefined` when nothing is at the path. A stored `null`
   * is `null`.
   */
  readonly value: unknown;
}

/**
 * The outcome of a data write. `path` is the one named. `dataVersion` is the
 * version of the data space after the write: one more than the base when
 * `changed` is true, and the base when it is false. A write that changes
 * nothing keeps the version, so a `set` of a value already stored answers
 * `changed: false`.
 */
export interface DataWriteReceipt {
  readonly path: string;
  readonly dataVersion: number;
  readonly changed: boolean;
}

/**
 * One value to store at `path`, in the data path grammar (decision 0046); the
 * empty path is the root. `baseDataVersion` is the data version the frame last
 * saw, from `get`, a receipt or a `conflict`. `retryId` is 8 to 36 ASCII
 * letters, digits, `-` or `_`: choose a new one for each change, and keep it
 * only to retry the identical call.
 */
export interface DataSetRequest {
  readonly path: string;
  readonly value: unknown;
  readonly baseDataVersion: number;
  readonly retryId: string;
}

/**
 * The JSON Merge Patch (RFC 7396) to apply at `path`, with the rules of
 * `DataSetRequest`.
 */
export interface DataUpdateRequest {
  readonly path: string;
  readonly patch: unknown;
  readonly baseDataVersion: number;
  readonly retryId: string;
}

/** The value at `path` to remove, with the rules of `DataSetRequest`. */
export interface DataRemoveRequest {
  readonly path: string;
  readonly baseDataVersion: number;
  readonly retryId: string;
}

/** What a data change listener is told (decision 0053, rule 16). */
export interface DataChange {
  /** The version a write left the data space at; read the value with `get`. */
  readonly dataVersion: number;
}

/** A listener registered with `data.onChange`. */
export type DataChangeListener = (change: DataChange) => void;

/**
 * The application data the user saved for this artifact (decision 0053). A
 * write that changes nothing keeps the data version. Writes are not retried
 * automatically: to retry one that went unanswered, send the identical call
 * with the same `retryId`, which gets the first receipt back when that write
 * was applied.
 */
export interface DataRuntime {
  /**
   * Reads the value at `path`; needs the `data.read` grant. A path the server
   * refuses is `invalid_request`. An answer that is not for `path`, or whose
   * value is not JSON text within 1 MiB, rejects with `unavailable`.
   */
  get(request: { readonly path: string }): Promise<DataValue>;
  /**
   * Replaces the value at `path` with `value`; needs the `data.write` grant.
   * `set` can store `null`. A value that `JSON.stringify` would change silently
   * is refused with `invalid_request` before anything is sent. A stale base is
   * `conflict` with `currentDataVersion`.
   */
  set(request: DataSetRequest): Promise<DataWriteReceipt>;
  /**
   * Merges `patch` into the value at `path` as a JSON Merge Patch; needs the
   * `data.write` grant. A patch that `JSON.stringify` would change silently is
   * refused with `invalid_request`, as for `set`. A stale base is `conflict`
   * with `currentDataVersion`.
   */
  update(request: DataUpdateRequest): Promise<DataWriteReceipt>;
  /**
   * Removes the value at `path`; needs the `data.write` grant. `remove` of a
   * missing path changes nothing. A stale base is `conflict` with
   * `currentDataVersion`.
   */
  remove(request: DataRemoveRequest): Promise<DataWriteReceipt>;
  /**
   * Registers `listener` to be told when this artifact's data changes. The
   * workspace sends this hint only to a frame that holds the `data.read`
   * grant; registering sends nothing and needs no grant. Each registration is
   * called with one argument, `{ dataVersion }`, and the same function
   * registered twice is called twice. A listener that throws does not stop the
   * others. The hint can be missed (a reload, a lost connection) or name a
   * version the frame already has, so its absence does not mean that nothing
   * changed. Returns a function that removes this registration.
   */
  onChange(listener: DataChangeListener): () => void;
}

/** Why the workspace adopted this frame's view (decision 0054, rule 10). */
export type ViewAdoptionReason = 'permissions' | 'saved' | 'head' | 'lease';

/** What a view adoption listener is told (decision 0054, rule 10). */
export interface ViewAdoption {
  readonly reason: ViewAdoptionReason;
}

/** A listener registered with `view.onAdopt`. */
export type ViewAdoptionListener = (adoption: ViewAdoption) => void;

/**
 * The frame's view (decision 0054). A page whose manifest says
 * `"lifecycle": "adopt"` may keep running when the workspace replaces its
 * view. No workspace adopts a frame yet (decision 0054, R10c), so today a
 * listener is never called.
 */
export interface ViewRuntime {
  /**
   * Registers `listener` to be told when the workspace adopted this frame's
   * view: the page keeps its document and its state, and its calls go on
   * under the new view, whose lease starts again. Each registration is called
   * with one argument, `{ reason }`; the same function registered twice is
   * called twice, and a listener that throws does not stop the others.
   * Registering sends nothing and needs no grant. The frame is not told which
   * permissions it holds; it learns that by calling. Returns a function that
   * removes this registration.
   */
  onAdopt(listener: ViewAdoptionListener): () => void;
}

/** What an opted-in frame can do, each operation subject to its own grant. */
export interface ArtifactRuntime {
  /** Reads current metadata through the workspace's bound runtime view. */
  currentMetadata(): Promise<RuntimeMetadata>;
  readonly source: SourceRuntime;
  /** Uploaded assets of this artifact (decision 0045). */
  readonly assets: AssetsRuntime;
  /** Application data saved for this artifact (decision 0053). */
  readonly data: DataRuntime;
  /** The frame's view and its adoption (decision 0054). */
  readonly view: ViewRuntime;
}

declare global {
  interface Window {
    /** Present only after the artifact opts in to the runtime script. */
    artifree?: ArtifactRuntime;
  }
}
