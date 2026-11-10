import { dataText } from './data-text.js';
import type {
  AssetContent,
  AssetContentType,
  AssetDeleteReceipt,
  AssetDeleteRequest,
  AssetDownloadReceipt,
  AssetInfo,
  AssetListing,
  AssetUploadReceipt,
  AssetUploadRequest,
  DataRemoveRequest,
  DataSetRequest,
  DataUpdateRequest,
  DataValue,
  DataWriteReceipt,
  DownloadReceipt,
  RuntimeFailureCode,
  RuntimeMetadata,
  SourceFile,
  SourceListing,
  SourcePart,
  SourceReadRequest,
  SourceWriteRequest,
  WriteReceipt,
} from './index.js';
import { isPackagePath } from './package-path.js';
import { sha256Hex } from './sha256.js';

/** Server-issued initialization of a single frame and view generation. */
export interface RuntimeInitialization {
  readonly view_id: string;
  readonly generation: number;
  readonly initialization: string;
  readonly lease_ms: number;
}

/** The largest part a frame may ask for, as the server's contract states. */
const MAX_PART_BYTES = 1_048_576;
const MAX_PATH_CHARS = 1024;
const MAX_LISTED_FILES = 256;
/** The most bytes of one file a frame may save, as the server's contract states. */
const MAX_WRITE_BYTES = 1_048_576;
/** The retry identity a save carries: the server derives its key from it. */
const RETRY_ID = /^[A-Za-z0-9_-]{8,36}$/;
/** What an asset ID looks like: 64 lowercase hexadecimal digits. */
const ASSET_ID = /^[0-9a-f]{64}$/;
/** The most bytes of an asset a frame may ask for in one part: 256 KiB. */
const MAX_ASSET_PART_BYTES = 262_144;
/** The largest asset, 5 MiB, in bytes (decision 0045, rule 4). */
const MAX_ASSET_BYTES = 5_242_880;
/** The most assets in one space (decision 0045, rule 4). */
const MAX_LISTED_ASSETS = 64;
/** The most bytes of JSON text one data read returns, 1 MiB (decision 0053, rule 15). */
const MAX_DATA_READ_BYTES = 1_048_576;
/** The most bytes of JSON text one data write carries, 256 KiB (decision 0053, rule 15). */
const MAX_DATA_WRITE_BYTES = 262_144;
/** The media types an asset may have (decision 0045, rule 3). */
const ASSET_TYPES: Record<AssetContentType, true> = {
  'text/plain': true,
  'text/csv': true,
  'image/png': true,
  'image/jpeg': true,
};
/** Calls waiting for an answer at once. */
const MAX_PENDING = 64;
/** A read waits this long; a download waits for a person, so far longer. */
const READ_TIMEOUT_MS = 10_000;
const DOWNLOAD_TIMEOUT_MS = 120_000;
/** A save waits for the commit and its read-back. */
const WRITE_TIMEOUT_MS = 30_000;
/**
 * The waits before the retries of an asset part the workspace refused as
 * rate-limited, in milliseconds: four retries, so five attempts in all.
 */
const RATE_LIMIT_BACKOFF_MS: readonly number[] = [250, 500, 1_000, 2_000];

const FAILURE_CODES: Record<RuntimeFailureCode, string> = {
  invalid_request: 'The runtime request was not valid.',
  denied: 'The runtime request was not permitted.',
  stale_view: 'The runtime view is out of date.',
  conflict: 'The uploaded assets are not at the version the call named.',
  not_found: 'The requested source was not found.',
  rate_limited: 'Too many runtime requests.',
  declined: 'The user declined the request.',
  unavailable: 'The runtime request is unavailable.',
  expired: 'The runtime view has expired.',
};

/** What a data write's `conflict` says; the version it names is the data space's. */
const DATA_CONFLICT_MESSAGE =
  'The application data is not at the version the call named.';

/**
 * The only way a runtime call fails. The message and code come from a closed
 * set; nothing the server or the workspace said is passed on.
 */
class RuntimeRequestFailure extends Error {
  readonly code: RuntimeFailureCode;
  /** On a `conflict`: the version the artifact has now, if it was named. */
  readonly currentAssetVersion?: number;
  /** On a data write's `conflict`: the data version now, if it was named. */
  readonly currentDataVersion?: number;

  constructor(
    code: RuntimeFailureCode,
    message?: string,
    currentAssetVersion?: number,
    currentDataVersion?: number,
  ) {
    super(message ?? FAILURE_CODES[code]);
    this.name = 'RuntimeRequestFailure';
    this.code = code;
    if (currentAssetVersion !== undefined)
      this.currentAssetVersion = currentAssetVersion;
    if (currentDataVersion !== undefined)
      this.currentDataVersion = currentDataVersion;
  }
}

function isFailureCode(value: unknown): value is RuntimeFailureCode {
  return typeof value === 'string' && Object.hasOwn(FAILURE_CODES, value);
}

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function count(value: unknown): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0;
}

function sha256(value: unknown): value is string {
  return typeof value === 'string' && /^[0-9a-f]{64}$/.test(value);
}

/** An asset's size: from 1 byte to 5 MiB. */
function isAssetSize(value: unknown): value is number {
  return count(value) && value >= 1 && value <= MAX_ASSET_BYTES;
}

function isAssetId(value: unknown): value is string {
  return typeof value === 'string' && ASSET_ID.test(value);
}

function isAssetType(value: unknown): value is AssetContentType {
  return typeof value === 'string' && Object.hasOwn(ASSET_TYPES, value);
}

/**
 * A base version a write may name: a count that one more write still leaves a
 * safe integer.
 */
function isBaseVersion(value: unknown): value is number {
  return count(value) && value < Number.MAX_SAFE_INTEGER;
}

/**
 * Copies the own data properties of an argument a page passed, or returns
 * undefined. Page script can pass anything: a primitive, an array, a getter,
 * a proxy, a revoked proxy. Each property is read once, so what is validated
 * is what is sent. Nothing here throws.
 */
function argument(
  value: unknown,
  allowed: readonly string[],
): Record<string, unknown> | undefined {
  try {
    if (typeof value !== 'object' || value === null || Array.isArray(value))
      return undefined;
    const prototype: unknown = Object.getPrototypeOf(value);
    if (prototype !== null && Object.getPrototypeOf(prototype) !== null)
      return undefined;
    const copy: Record<string, unknown> = Object.create(null);
    for (const key of Reflect.ownKeys(value)) {
      if (typeof key !== 'string' || !allowed.includes(key)) return undefined;
      const descriptor = Object.getOwnPropertyDescriptor(value, key);
      if (
        descriptor === undefined ||
        !('value' in descriptor) ||
        descriptor.enumerable !== true
      )
        return undefined;
      copy[key] = descriptor.value;
    }
    return copy;
  } catch {
    return undefined;
  }
}

function metadata(value: unknown): RuntimeMetadata | undefined {
  if (!record(value)) return undefined;
  const { artifactId, title, currentRevision, metadataRevision } = value;
  if (
    typeof artifactId !== 'string' ||
    typeof title !== 'string' ||
    !count(currentRevision) ||
    !count(metadataRevision) ||
    currentRevision < 1 ||
    metadataRevision < 1
  )
    return undefined;
  return { artifactId, title, currentRevision, metadataRevision };
}

function sourceFile(value: unknown): SourceFile | undefined {
  if (!record(value)) return undefined;
  const { path, size, sha256: digest, mediaType } = value;
  if (
    typeof path !== 'string' ||
    path.length === 0 ||
    path.length > MAX_PATH_CHARS ||
    !count(size) ||
    !sha256(digest) ||
    typeof mediaType !== 'string'
  )
    return undefined;
  return { path, size, sha256: digest, mediaType };
}

function listing(value: unknown): SourceListing | undefined {
  if (!record(value)) return undefined;
  const { revision, files } = value;
  if (
    !count(revision) ||
    revision < 1 ||
    !Array.isArray(files) ||
    files.length > MAX_LISTED_FILES
  )
    return undefined;
  const parsed: SourceFile[] = [];
  const seen = new Set<string>();
  for (const file of files as unknown[]) {
    const item = sourceFile(file);
    if (item === undefined || seen.has(item.path)) return undefined;
    seen.add(item.path);
    parsed.push(item);
  }
  return { revision, files: parsed };
}

function assetInfo(value: unknown): AssetInfo | undefined {
  if (!record(value)) return undefined;
  const { id, contentType, size, sha256: digest, createdAtMs } = value;
  if (
    !isAssetId(id) ||
    !isAssetType(contentType) ||
    !isAssetSize(size) ||
    !sha256(digest) ||
    !count(createdAtMs)
  )
    return undefined;
  return { id, contentType, size, sha256: digest, createdAtMs };
}

/**
 * An asset listing: at most 64 assets in strictly ascending ID order, so no ID
 * repeats. A space that lists assets has been written to, so its version is at
 * least 1.
 */
function assetListing(value: unknown): AssetListing | undefined {
  if (!record(value)) return undefined;
  const { assetVersion, assets } = value;
  if (
    !count(assetVersion) ||
    !Array.isArray(assets) ||
    assets.length > MAX_LISTED_ASSETS ||
    (assets.length > 0 && assetVersion === 0)
  )
    return undefined;
  const parsed: AssetInfo[] = [];
  let previous: string | undefined;
  for (const asset of assets as unknown[]) {
    const item = assetInfo(asset);
    if (item === undefined || (previous !== undefined && item.id <= previous))
      return undefined;
    previous = item.id;
    parsed.push(item);
  }
  return { assetVersion, assets: parsed };
}

/** The fields every part carries, as `partFields` checks them. */
interface PartFields {
  readonly offset: number;
  readonly nextOffset: number | null;
  readonly size: number;
  readonly sha256: string;
  readonly bytes: Uint8Array;
}

/**
 * The fields of a part, checked against the request it answers and against the
 * bytes actually received: no number the workspace states is believed. The
 * bytes number `nextOffset - offset`, or `size - offset` for the final part;
 * the next offset lies strictly after the offset and strictly before the end,
 * so a read loop always progresses. `bound` is the most bytes one part may
 * carry.
 */
function partFields(
  value: Record<string, unknown>,
  asked: { readonly offset: number; readonly length: number },
  bound: number,
): PartFields | undefined {
  const { offset, nextOffset, size, sha256: digest, bytes } = value;
  if (
    offset !== asked.offset ||
    !count(size) ||
    !sha256(digest) ||
    !(bytes instanceof ArrayBuffer) ||
    !(nextOffset === null || count(nextOffset))
  )
    return undefined;
  const received = bytes.byteLength;
  if (
    offset > size ||
    received > bound ||
    received > asked.length ||
    (nextOffset === null
      ? received !== size - offset
      : nextOffset <= offset ||
        nextOffset >= size ||
        received !== nextOffset - offset ||
        received !== asked.length)
  )
    return undefined;
  return {
    offset,
    nextOffset,
    size,
    sha256: digest,
    bytes: new Uint8Array(bytes),
  };
}

/**
 * A source part, checked against the request it answers: it must name the path
 * asked for, and `partFields` checks the rest.
 */
function part(
  value: unknown,
  asked: SourceReadRequest,
): SourcePart | undefined {
  if (!record(value)) return undefined;
  const { path } = value;
  if (path !== asked.path) return undefined;
  const fields = partFields(value, asked, MAX_PART_BYTES);
  return fields === undefined ? undefined : { path, ...fields };
}

/** What the first part of an asset states; every later part repeats it. */
interface AssetHead {
  readonly contentType: AssetContentType;
  readonly size: number;
  readonly sha256: string;
}

/** What one asset part request asks for. */
interface AssetPartRequest {
  readonly assetId: string;
  readonly offset: number;
  readonly length: number;
}

/** An asset part, checked by `assetPart`. */
interface AssetPart extends PartFields {
  readonly assetId: string;
  readonly contentType: AssetContentType;
}

/**
 * An asset part, checked like a source part against the request it answers. It
 * must echo the asset ID and name a type from the closed set. Once the first
 * part has arrived (`head`), it must repeat that part's type, size and digest.
 */
function assetPart(
  value: unknown,
  asked: AssetPartRequest,
  head: AssetHead | undefined,
): AssetPart | undefined {
  if (!record(value)) return undefined;
  const { assetId, contentType } = value;
  if (assetId !== asked.assetId || !isAssetType(contentType)) return undefined;
  const fields = partFields(value, asked, MAX_ASSET_PART_BYTES);
  if (fields === undefined || !isAssetSize(fields.size)) return undefined;
  if (
    head !== undefined &&
    (contentType !== head.contentType ||
      fields.size !== head.size ||
      fields.sha256 !== head.sha256)
  )
    return undefined;
  return { assetId, contentType, ...fields };
}

function receipt(value: unknown): DownloadReceipt | undefined {
  if (!record(value)) return undefined;
  const { started, kind, bytes } = value;
  if (
    started !== true ||
    (kind !== 'file' && kind !== 'archive') ||
    !count(bytes)
  )
    return undefined;
  return { started, kind, bytes };
}

/** The bytes of a file as a private copy, or undefined if they are not usable. */
function fileBytes(content: unknown): Uint8Array<ArrayBuffer> | undefined {
  try {
    let source: Uint8Array;
    if (typeof content === 'string') {
      // A string of n UTF-16 units encodes to at least n bytes.
      if (content.length > MAX_WRITE_BYTES) return undefined;
      source = new TextEncoder().encode(content);
    } else if (content instanceof Uint8Array) {
      source = content;
    } else {
      return undefined;
    }
    const length = source.length;
    if (length > MAX_WRITE_BYTES) return undefined;
    const copy = new Uint8Array(length);
    copy.set(source);
    return copy;
  } catch {
    return undefined;
  }
}

/** What a save receipt must say, from the bytes the frame sent. */
interface Saved {
  readonly path: string;
  readonly size: number;
  readonly sha256: string;
}

/**
 * A save receipt, checked against the bytes the frame sent: the path is the
 * one it named, the size is the number of bytes sent, the digest is the
 * SHA-256 of those bytes, and the revision is a later one than the first.
 */
function written(value: unknown, sent: Saved): WriteReceipt | undefined {
  if (!record(value)) return undefined;
  const { path, revision, size, sha256: digest } = value;
  if (
    path !== sent.path ||
    !count(revision) ||
    revision < 2 ||
    size !== sent.size ||
    !sha256(digest) ||
    digest !== sent.sha256
  )
    return undefined;
  return { path, revision, size, sha256: digest };
}

/** What an upload must answer, from the call the frame made. */
interface SentAsset {
  readonly contentType: AssetContentType;
  readonly size: number;
  readonly sha256: string;
  readonly baseAssetVersion: number;
}

/**
 * An upload receipt, checked against the call: the asset has the type, size and
 * digest of the bytes sent, and the version is the one after the base version.
 */
function uploaded(
  value: unknown,
  sent: SentAsset,
): AssetUploadReceipt | undefined {
  if (!record(value)) return undefined;
  const { asset, assetVersion } = value;
  const info = assetInfo(asset);
  if (
    info === undefined ||
    info.contentType !== sent.contentType ||
    info.size !== sent.size ||
    info.sha256 !== sent.sha256 ||
    !count(assetVersion) ||
    assetVersion !== sent.baseAssetVersion + 1
  )
    return undefined;
  return { asset: info, assetVersion };
}

/**
 * A deletion receipt, checked against the call: it names the asset asked for,
 * and its version is the base version when nothing was deleted, and the one
 * after it when something was.
 */
function removed(
  value: unknown,
  asked: { readonly assetId: string; readonly baseAssetVersion: number },
): AssetDeleteReceipt | undefined {
  if (!record(value)) return undefined;
  const { assetId, assetVersion, deleted } = value;
  if (
    assetId !== asked.assetId ||
    typeof deleted !== 'boolean' ||
    !count(assetVersion) ||
    assetVersion !==
      (deleted ? asked.baseAssetVersion + 1 : asked.baseAssetVersion)
  )
    return undefined;
  return { assetId, assetVersion, deleted };
}

/**
 * A download receipt, checked against the request it answers: the download
 * started, it names the asset asked for, and its byte count is no more than
 * the largest asset may be.
 */
function downloaded(
  value: unknown,
  assetId: string,
): AssetDownloadReceipt | undefined {
  if (!record(value)) return undefined;
  const { started, assetId: named, bytes } = value;
  if (
    started !== true ||
    named !== assetId ||
    !count(bytes) ||
    bytes > MAX_ASSET_BYTES
  )
    return undefined;
  return { started, assetId, bytes };
}

/**
 * A data read's answer for `path`: the path echoed, a safe-integer version, and
 * for a value that exists, JSON text within the read bound, parsed here; the
 * root must exist and be an object (decision 0053, rules 5 and 13).
 */
function dataValue(value: unknown, path: string): DataValue | undefined {
  if (!record(value)) return undefined;
  const { path: echoed, dataVersion, exists, value: text } = value;
  if (echoed !== path || !count(dataVersion) || typeof exists !== 'boolean')
    return undefined;
  if (!exists)
    return path === '' || text !== null
      ? undefined
      : { path, dataVersion, exists, value: undefined };
  // Each UTF-16 unit encodes to at least one byte, so a longer text is refused
  // before it is encoded to count its bytes.
  if (
    typeof text !== 'string' ||
    text.length > MAX_DATA_READ_BYTES ||
    new TextEncoder().encode(text).byteLength > MAX_DATA_READ_BYTES
  )
    return undefined;
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    return undefined;
  }
  if (path === '' && !record(parsed)) return undefined;
  return { path, dataVersion, exists, value: parsed };
}

/**
 * The JSON text of a value a write sends, or undefined when the SDK refuses it:
 * a value that JSON.stringify would change, or text over the write bound
 * (decision 0053, rules 5 and 15).
 */
function dataWriteText(value: unknown): string | undefined {
  const text = dataText(value);
  // Each UTF-16 unit encodes to at least one byte, so a longer text is refused
  // before it is encoded to count its bytes.
  if (
    text === undefined ||
    text.length > MAX_DATA_WRITE_BYTES ||
    new TextEncoder().encode(text).byteLength > MAX_DATA_WRITE_BYTES
  )
    return undefined;
  return text;
}

/**
 * A data write's receipt, checked against the call: the path is the one named,
 * the version is the base when nothing changed and one more than the base when
 * something did (decision 0053, rule 13).
 */
function dataWritten(
  value: unknown,
  path: string,
  base: number,
): DataWriteReceipt | undefined {
  if (!record(value)) return undefined;
  const { path: echoed, dataVersion, changed } = value;
  if (
    echoed !== path ||
    !count(dataVersion) ||
    typeof changed !== 'boolean' ||
    dataVersion !== (changed ? base + 1 : base)
  )
    return undefined;
  return { path, dataVersion, changed };
}

interface Waiting {
  readonly parse: (reply: Record<string, unknown>) => unknown;
  readonly resolve: (value: never) => void;
  readonly reject: (reason: Error) => void;
  readonly timeout: number;
  /** Message of a refusal; metadata keeps its first wording. */
  readonly refusal: string | undefined;
}

/** Installs the runtime for the frame that received this initialization. */
export function start(config: RuntimeInitialization): void {
  if (window.artifree !== undefined)
    throw new Error('The Artifree runtime was already installed.');
  const channel = new MessageChannel();
  const deadline = performance.now() + config.lease_ms;
  const waiting = new Map<number, Waiting>();
  let next = 0;
  let closed = false;

  const settle = (id: number): Waiting | undefined => {
    const pending = waiting.get(id);
    if (pending === undefined) return undefined;
    waiting.delete(id);
    clearTimeout(pending.timeout);
    return pending;
  };

  channel.port1.onmessage = (event: MessageEvent<unknown>) => {
    if (!record(event.data) || !count(event.data.id)) return;
    const pending = settle(event.data.id);
    if (pending === undefined) return;
    if (event.data.ok !== true) {
      const code = isFailureCode(event.data.code) ? event.data.code : 'denied';
      // Only a conflict names a version, and only a count is believed.
      const dataVersion = event.data.currentDataVersion;
      if (code === 'conflict' && count(dataVersion)) {
        pending.reject(
          new RuntimeRequestFailure(
            code,
            DATA_CONFLICT_MESSAGE,
            undefined,
            dataVersion,
          ),
        );
        return;
      }
      const assetVersion = event.data.currentAssetVersion;
      pending.reject(
        new RuntimeRequestFailure(
          code,
          pending.refusal,
          code === 'conflict' && count(assetVersion) ? assetVersion : undefined,
        ),
      );
      return;
    }
    const value = pending.parse(event.data);
    if (value === undefined)
      pending.reject(new RuntimeRequestFailure('unavailable'));
    else pending.resolve(value as never);
  };
  window.addEventListener(
    'pagehide',
    () => {
      closed = true;
      channel.port1.close();
      for (const pending of waiting.values()) {
        clearTimeout(pending.timeout);
        pending.reject(new RuntimeRequestFailure('expired'));
      }
      waiting.clear();
    },
    { once: true },
  );
  window.parent.postMessage(
    {
      type: 'artifree-runtime-connect',
      viewId: config.view_id,
      generation: config.generation,
      initialization: config.initialization,
    },
    '*',
    [channel.port2],
  );

  /**
   * Sends one call and waits for its answer. It never throws: a refusal, a
   * timeout, a closed view and an unusable port are all rejections.
   */
  function call<T>(
    message: Record<string, unknown>,
    parse: (reply: Record<string, unknown>) => T | undefined,
    options: {
      readonly timeout: number;
      readonly refusal?: string;
      readonly transfer?: Transferable[];
    },
  ): Promise<T> {
    if (closed || performance.now() >= deadline)
      return Promise.reject(new RuntimeRequestFailure('expired'));
    if (waiting.size >= MAX_PENDING)
      return Promise.reject(new RuntimeRequestFailure('rate_limited'));
    if (next === Number.MAX_SAFE_INTEGER)
      return Promise.reject(new RuntimeRequestFailure('unavailable'));
    const id = ++next;
    return new Promise<T>((resolve, reject) => {
      const timeout = window.setTimeout(
        () => {
          if (settle(id) === undefined) return;
          try {
            channel.port1.postMessage({ type: 'cancel', id });
          } catch {
            // The view is already unusable; the rejection says so.
          }
          reject(
            new RuntimeRequestFailure(
              performance.now() >= deadline ? 'expired' : 'unavailable',
            ),
          );
        },
        Math.min(options.timeout, Math.max(0, deadline - performance.now())),
      );
      waiting.set(id, {
        parse,
        resolve: resolve as (value: never) => void,
        reject,
        timeout,
        refusal: options.refusal,
      });
      try {
        channel.port1.postMessage({ ...message, id }, options.transfer ?? []);
      } catch {
        settle(id);
        reject(new RuntimeRequestFailure('unavailable'));
      }
    });
  }

  const invalid = (): Promise<never> =>
    Promise.reject(new RuntimeRequestFailure('invalid_request'));

  /** Methods are closures, not `this` users: a detached call works. */
  const source = Object.freeze({
    list(): Promise<SourceListing> {
      return call({ type: 'source.list' }, (reply) => listing(reply.listing), {
        timeout: READ_TIMEOUT_MS,
      });
    },
    read(request: SourceReadRequest): Promise<SourcePart> {
      const fields = argument(request, ['path', 'offset', 'length']);
      if (fields === undefined) return invalid();
      const { path, offset, length } = fields;
      if (
        typeof path !== 'string' ||
        !isPackagePath(path) ||
        !count(offset) ||
        !count(length) ||
        length < 1 ||
        length > MAX_PART_BYTES
      )
        return invalid();
      const asked: SourceReadRequest = { path, offset, length };
      return call(
        { type: 'source.read', path, offset, length },
        (reply) => part(reply.part, asked),
        { timeout: READ_TIMEOUT_MS },
      );
    },
    write(request: SourceWriteRequest): Promise<WriteReceipt> {
      const fields = argument(request, ['path', 'content', 'retryId']);
      if (fields === undefined) return invalid();
      const { path, content, retryId } = fields;
      if (
        typeof path !== 'string' ||
        !isPackagePath(path) ||
        typeof retryId !== 'string' ||
        !RETRY_ID.test(retryId)
      )
        return invalid();
      const bytes = fileBytes(content);
      if (bytes === undefined) return invalid();
      const sent: Saved = {
        path,
        size: bytes.length,
        sha256: sha256Hex(bytes),
      };
      return call(
        { type: 'source.write', retryId, path, bytes: bytes.buffer },
        (reply) => written(reply.written, sent),
        { timeout: WRITE_TIMEOUT_MS, transfer: [bytes.buffer] },
      );
    },
    requestDownload(request?: {
      readonly path: string;
    }): Promise<DownloadReceipt> {
      let path: string | null = null;
      if (request !== undefined) {
        const fields = argument(request, ['path']);
        const named = fields?.path;
        if (
          fields === undefined ||
          typeof named !== 'string' ||
          !isPackagePath(named)
        )
          return invalid();
        path = named;
      }
      return call(
        {
          type: 'source.download',
          kind: path === null ? 'archive' : 'file',
          path,
        },
        (reply) => receipt(reply.receipt),
        { timeout: DOWNLOAD_TIMEOUT_MS },
      );
    },
  });

  /**
   * One part of an asset, from `offset`. `first` is the first part's facts,
   * which every later part must repeat.
   */
  function requestAssetPart(
    assetId: string,
    offset: number,
    first: AssetHead | undefined,
  ): Promise<AssetPart> {
    const asked: AssetPartRequest = {
      assetId,
      offset,
      length: MAX_ASSET_PART_BYTES,
    };
    return call(
      { type: 'assets.read', assetId, offset, length: MAX_ASSET_PART_BYTES },
      (reply) => assetPart(reply.part, asked, first),
      { timeout: READ_TIMEOUT_MS },
    );
  }

  /** Resolves after `ms`, on the timer the calls use for their timeouts. */
  function pause(ms: number): Promise<void> {
    return new Promise((resolve) => {
      window.setTimeout(resolve, ms);
    });
  }

  /**
   * One part of an asset, asked again at the same offset while the workspace
   * refuses it as rate-limited. Each wait in RATE_LIMIT_BACKOFF_MS comes before
   * a retry; when the waits run out, the last refusal is rethrown, and any other
   * failure is rethrown at once. Each attempt goes through `call`, so the lease
   * and a closed view end the read as they end any other call.
   */
  async function requestAssetPartWithBackoff(
    assetId: string,
    offset: number,
    first: AssetHead | undefined,
  ): Promise<AssetPart> {
    for (let attempt = 0; ; attempt += 1) {
      try {
        return await requestAssetPart(assetId, offset, first);
      } catch (error: unknown) {
        const wait = RATE_LIMIT_BACKOFF_MS[attempt];
        if (
          wait === undefined ||
          !(error instanceof RuntimeRequestFailure) ||
          error.code !== 'rate_limited'
        )
          throw error;
        await pause(wait);
      }
    }
  }

  const assets = Object.freeze({
    list(): Promise<AssetListing> {
      return call(
        { type: 'assets.list' },
        (reply) => assetListing(reply.listing),
        { timeout: READ_TIMEOUT_MS },
      );
    },
    async read(request: { readonly assetId: string }): Promise<AssetContent> {
      const fields = argument(request, ['assetId']);
      if (fields === undefined) return invalid();
      const { assetId } = fields;
      if (!isAssetId(assetId)) return invalid();
      // The first part states the asset's type, size and digest. Every part
      // repeats them, and nothing is returned before the bytes match.
      let current = await requestAssetPartWithBackoff(assetId, 0, undefined);
      const head: AssetHead = {
        contentType: current.contentType,
        size: current.size,
        sha256: current.sha256,
      };
      const assembled = new Uint8Array(head.size);
      let filled = 0;
      for (;;) {
        // The part rules keep each part inside the asset. Checking it here
        // keeps a broken rule from surfacing as a RangeError to the page.
        if (current.offset + current.bytes.length > head.size)
          throw new RuntimeRequestFailure('unavailable');
        assembled.set(current.bytes, current.offset);
        filled += current.bytes.length;
        if (current.nextOffset === null) break;
        current = await requestAssetPartWithBackoff(
          assetId,
          current.nextOffset,
          head,
        );
      }
      if (filled !== head.size || sha256Hex(assembled) !== head.sha256)
        throw new RuntimeRequestFailure('unavailable');
      return {
        assetId,
        contentType: head.contentType,
        size: head.size,
        sha256: head.sha256,
        bytes: assembled,
      };
    },
    upload(request: AssetUploadRequest): Promise<AssetUploadReceipt> {
      const fields = argument(request, [
        'contentType',
        'content',
        'baseAssetVersion',
        'retryId',
      ]);
      if (fields === undefined) return invalid();
      const { contentType, content, baseAssetVersion, retryId } = fields;
      if (
        !isAssetType(contentType) ||
        !isBaseVersion(baseAssetVersion) ||
        typeof retryId !== 'string' ||
        !RETRY_ID.test(retryId)
      )
        return invalid();
      const bytes = fileBytes(content);
      if (bytes === undefined) return invalid();
      const sent: SentAsset = {
        contentType,
        size: bytes.length,
        sha256: sha256Hex(bytes),
        baseAssetVersion,
      };
      // One attempt. A write is never retried here, since it may already be
      // stored; the frame retries it with the same retryId.
      return call(
        {
          type: 'assets.upload',
          retryId,
          baseAssetVersion,
          contentType,
          bytes: bytes.buffer,
        },
        (reply) => uploaded(reply.uploaded, sent),
        { timeout: WRITE_TIMEOUT_MS, transfer: [bytes.buffer] },
      );
    },
    delete(request: AssetDeleteRequest): Promise<AssetDeleteReceipt> {
      const fields = argument(request, [
        'assetId',
        'baseAssetVersion',
        'retryId',
      ]);
      if (fields === undefined) return invalid();
      const { assetId, baseAssetVersion, retryId } = fields;
      if (
        !isAssetId(assetId) ||
        !isBaseVersion(baseAssetVersion) ||
        typeof retryId !== 'string' ||
        !RETRY_ID.test(retryId)
      )
        return invalid();
      const asked = { assetId, baseAssetVersion };
      // One attempt, as for upload.
      return call(
        { type: 'assets.delete', retryId, baseAssetVersion, assetId },
        (reply) => removed(reply.removed, asked),
        { timeout: WRITE_TIMEOUT_MS },
      );
    },
    requestDownload(request: {
      readonly assetId: string;
    }): Promise<AssetDownloadReceipt> {
      const fields = argument(request, ['assetId']);
      if (fields === undefined) return invalid();
      const { assetId } = fields;
      if (!isAssetId(assetId)) return invalid();
      // One attempt. A repeat asks the user again, so the SDK never makes one.
      return call(
        { type: 'assets.download', assetId },
        (reply) => downloaded(reply.receipt, assetId),
        { timeout: DOWNLOAD_TIMEOUT_MS },
      );
    },
  });

  const data = Object.freeze({
    get(request: { readonly path: string }): Promise<DataValue> {
      const fields = argument(request, ['path']);
      if (fields === undefined) return invalid();
      const { path } = fields;
      if (typeof path !== 'string') return invalid();
      return call(
        { type: 'data.get', path },
        (reply) => dataValue(reply.data, path),
        { timeout: READ_TIMEOUT_MS },
      );
    },
    set(request: DataSetRequest): Promise<DataWriteReceipt> {
      const fields = argument(request, [
        'path',
        'value',
        'baseDataVersion',
        'retryId',
      ]);
      if (fields === undefined) return invalid();
      const { path, value, baseDataVersion, retryId } = fields;
      if (
        typeof path !== 'string' ||
        !isBaseVersion(baseDataVersion) ||
        typeof retryId !== 'string' ||
        !RETRY_ID.test(retryId)
      )
        return invalid();
      const text = dataWriteText(value);
      if (text === undefined) return invalid();
      // One attempt. Writes are not retried automatically: the frame retries
      // with the same retryId.
      return call(
        { type: 'data.set', path, value: text, baseDataVersion, retryId },
        (reply) => dataWritten(reply.written, path, baseDataVersion),
        { timeout: WRITE_TIMEOUT_MS },
      );
    },
    update(request: DataUpdateRequest): Promise<DataWriteReceipt> {
      const fields = argument(request, [
        'path',
        'patch',
        'baseDataVersion',
        'retryId',
      ]);
      if (fields === undefined) return invalid();
      const { path, patch, baseDataVersion, retryId } = fields;
      if (
        typeof path !== 'string' ||
        !isBaseVersion(baseDataVersion) ||
        typeof retryId !== 'string' ||
        !RETRY_ID.test(retryId)
      )
        return invalid();
      const text = dataWriteText(patch);
      if (text === undefined) return invalid();
      // One attempt, as for set.
      return call(
        { type: 'data.update', path, patch: text, baseDataVersion, retryId },
        (reply) => dataWritten(reply.written, path, baseDataVersion),
        { timeout: WRITE_TIMEOUT_MS },
      );
    },
    remove(request: DataRemoveRequest): Promise<DataWriteReceipt> {
      const fields = argument(request, ['path', 'baseDataVersion', 'retryId']);
      if (fields === undefined) return invalid();
      const { path, baseDataVersion, retryId } = fields;
      if (
        typeof path !== 'string' ||
        !isBaseVersion(baseDataVersion) ||
        typeof retryId !== 'string' ||
        !RETRY_ID.test(retryId)
      )
        return invalid();
      // One attempt, as for set.
      return call(
        { type: 'data.remove', path, baseDataVersion, retryId },
        (reply) => dataWritten(reply.written, path, baseDataVersion),
        { timeout: WRITE_TIMEOUT_MS },
      );
    },
  });

  window.artifree = Object.freeze({
    currentMetadata(): Promise<RuntimeMetadata> {
      return call({ type: 'metadata' }, (reply) => metadata(reply.metadata), {
        timeout: READ_TIMEOUT_MS,
        refusal: 'The runtime metadata request was refused.',
      });
    },
    source,
    assets,
    data,
  });
}
