import type {
  RuntimeDownloadRequest,
  RuntimeDownloadResult,
  RuntimeErrorCode,
  RuntimeMetadata,
  RuntimeRequestFailure,
  RuntimeSaveRequest,
  RuntimeSaveResult,
  RuntimeSourceChange,
  RuntimeSourceFileInfo,
  RuntimeSourceManifest,
  RuntimeSourcePart,
  RuntimeSourceRange,
} from './index.js';

/** Server-issued initialization of a single frame and view generation. */
export interface RuntimeInitialization {
  readonly view_id: string;
  readonly generation: number;
  readonly initialization: string;
  readonly lease_ms: number;
}

/** What the frame can ask the parent workspace for. */
type RequestKind =
  'metadata' | 'source.list' | 'source.read' | 'source.write' | 'download';

/** Longest wait per request, also bounded by the view's lease. */
const TIMEOUT_MS: Readonly<Record<RequestKind, number>> = {
  metadata: 10_000,
  'source.list': 30_000,
  'source.read': 30_000,
  'source.write': 60_000,
  // The user decides when to confirm.
  download: 120_000,
};
const MAX_PENDING = 64;
/** The most bytes one answer of a read carries. */
const MAX_READ_BYTES = 1024 * 1024;
const MAX_CHANGES = 512;
const MAX_PATH_BYTES = 240;
const KEY_PATTERN = /^[A-Za-z0-9._:-]{8,100}$/;
const DIGEST_PATTERN = /^[0-9a-f]{64}$/;

const ERROR_CODES: Readonly<Record<RuntimeErrorCode, true>> = {
  invalid_request: true,
  not_granted: true,
  view_unavailable: true,
  not_found: true,
  revision_conflict: true,
  key_reused: true,
  too_large: true,
  range: true,
  rate_limited: true,
  busy: true,
  declined: true,
  unavailable: true,
  refused: true,
};

/** A refused or failed request; the code says what the frame can do next. */
class RuntimeRequestError extends Error implements RuntimeRequestFailure {
  readonly code: RuntimeErrorCode;
  readonly currentRevision?: number | undefined;

  constructor(
    code: RuntimeErrorCode,
    message: string,
    currentRevision?: number,
  ) {
    super(message);
    this.name = 'RuntimeRequestError';
    this.code = code;
    this.currentRevision = currentRevision;
  }
}

interface Waiting {
  readonly kind: RequestKind;
  readonly resolve: (value: never) => void;
  readonly reject: (reason: Error) => void;
  readonly timeout: number;
  /** Whether the answer fits the request that was sent, or it is refused. */
  readonly fits: (value: never) => boolean;
}

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function count(value: unknown): value is number {
  return Number.isSafeInteger(value) && typeof value === 'number' && value >= 0;
}

function metadata(value: unknown): RuntimeMetadata | undefined {
  if (!record(value)) return undefined;
  const { artifactId, title, currentRevision, metadataRevision } = value;
  if (
    typeof artifactId !== 'string' ||
    typeof title !== 'string' ||
    typeof currentRevision !== 'number' ||
    typeof metadataRevision !== 'number' ||
    !Number.isSafeInteger(currentRevision) ||
    !Number.isSafeInteger(metadataRevision) ||
    currentRevision < 1 ||
    metadataRevision < 1
  )
    return undefined;
  return { artifactId, title, currentRevision, metadataRevision };
}

function fileInfo(value: unknown): RuntimeSourceFileInfo | undefined {
  if (!record(value)) return undefined;
  const { path, size, sha256, mediaType } = value;
  if (
    typeof path !== 'string' ||
    !count(size) ||
    typeof sha256 !== 'string' ||
    !DIGEST_PATTERN.test(sha256) ||
    typeof mediaType !== 'string'
  )
    return undefined;
  return { path, size, sha256, mediaType };
}

function fileInfos(value: unknown): RuntimeSourceFileInfo[] | undefined {
  if (!Array.isArray(value)) return undefined;
  const files: RuntimeSourceFileInfo[] = [];
  for (const entry of value as unknown[]) {
    const file = fileInfo(entry);
    if (file === undefined) return undefined;
    files.push(file);
  }
  // A path is listed once.
  if (new Set(files.map((file) => file.path)).size !== files.length)
    return undefined;
  return files;
}

function manifest(value: unknown): RuntimeSourceManifest | undefined {
  if (!record(value)) return undefined;
  const { artifactId, revision } = value;
  const files = fileInfos(value.files);
  if (
    typeof artifactId !== 'string' ||
    !count(revision) ||
    revision < 1 ||
    files === undefined
  )
    return undefined;
  return { artifactId, revision, files };
}

function part(value: unknown): RuntimeSourcePart | undefined {
  if (!record(value)) return undefined;
  const { path, revision, offset, size, sha256, nextOffset, bytes } = value;
  if (
    typeof path !== 'string' ||
    !count(revision) ||
    revision < 1 ||
    !count(offset) ||
    !count(size) ||
    typeof sha256 !== 'string' ||
    !DIGEST_PATTERN.test(sha256) ||
    !(nextOffset === null || count(nextOffset)) ||
    !(bytes instanceof Uint8Array) ||
    offset > size ||
    bytes.length > MAX_READ_BYTES
  )
    return undefined;
  // The numbers must agree with the bytes, or joining the parts at
  // `nextOffset` would skip, repeat, or cut short.
  const end = offset + bytes.length;
  if (
    nextOffset === null
      ? end !== size
      : nextOffset <= offset || nextOffset >= size || end !== nextOffset
  )
    return undefined;
  return { path, revision, offset, size, sha256, nextOffset, bytes };
}

function saved(value: unknown): RuntimeSaveResult | undefined {
  if (!record(value)) return undefined;
  const { revision, createdAtMs, replayed } = value;
  const files = fileInfos(value.files);
  if (
    !count(revision) ||
    revision < 2 ||
    typeof createdAtMs !== 'number' ||
    !Number.isSafeInteger(createdAtMs) ||
    typeof replayed !== 'boolean' ||
    files === undefined
  )
    return undefined;
  return { revision, createdAtMs, replayed, files };
}

function download(value: unknown): RuntimeDownloadResult | undefined {
  if (!record(value)) return undefined;
  const { kind, name, size, revision } = value;
  if (
    (kind !== 'file' && kind !== 'archive') ||
    typeof name !== 'string' ||
    !count(size) ||
    !count(revision) ||
    revision < 1
  )
    return undefined;
  return { status: 'started', kind, name, size, revision };
}

/**
 * Whether a save's receipt describes exactly the files the save wrote: each
 * `put` once, with the size of the bytes that were sent.
 */
function describesWrites(
  files: readonly RuntimeSourceFileInfo[],
  sent: readonly RuntimeSourceChange[],
): boolean {
  const sizes = new Map<string, number>();
  for (const change of sent) {
    if (change.op !== 'put') continue;
    sizes.set(
      change.path,
      typeof change.content === 'string'
        ? new TextEncoder().encode(change.content).length
        : change.content.length,
    );
  }
  return (
    files.length === sizes.size &&
    files.every((file) => sizes.get(file.path) === file.size)
  );
}

function isErrorCode(value: unknown): value is RuntimeErrorCode {
  return typeof value === 'string' && Object.hasOwn(ERROR_CODES, value);
}

function failure(kind: RequestKind, data: Record<string, unknown>): Error {
  const code = isErrorCode(data.code) ? data.code : 'refused';
  const revision = count(data.currentRevision)
    ? data.currentRevision
    : undefined;
  return new RuntimeRequestError(
    code,
    kind === 'metadata'
      ? 'The runtime metadata request was refused.'
      : `The runtime request was refused (${code}).`,
    revision,
  );
}

function rejected(code: RuntimeErrorCode, message: string): Promise<never> {
  return Promise.reject(new RuntimeRequestError(code, message));
}

function validPath(path: unknown): path is string {
  return (
    typeof path === 'string' &&
    path.length > 0 &&
    path.length <= MAX_PATH_BYTES &&
    // Package paths are printable ASCII; the server decides the rest.
    /^[\x21-\x7e]+$/.test(path)
  );
}

/**
 * A non-null object that is neither an array nor an instance of a class.
 * Artifacts are plain JavaScript, so every argument is hostile or merely
 * mistaken: it is read here, property by property and once, never trusted to
 * be what the types promise.
 */
function plain(value: unknown): value is Record<string, unknown> {
  if (typeof value !== 'object' || value === null || Array.isArray(value))
    return false;
  const prototype: unknown = Object.getPrototypeOf(value);
  return prototype === Object.prototype || prototype === null;
}

/** One change, copied out of the caller's object, or undefined. */
function change(value: unknown): RuntimeSourceChange | undefined {
  if (!plain(value)) return undefined;
  const keys = Object.keys(value);
  const { op, path, content } = value;
  if (!validPath(path)) return undefined;
  if (op === 'delete')
    return keys.length === 2 ? { op: 'delete', path } : undefined;
  if (op !== 'put' || keys.length !== 3) return undefined;
  if (typeof content === 'string') return { op: 'put', path, content };
  if (content instanceof Uint8Array)
    return { op: 'put', path, content: new Uint8Array(content) };
  return undefined;
}

/** The changes of a save, copied, or undefined when any is not valid. */
function changes(value: unknown): RuntimeSourceChange[] | undefined {
  if (!Array.isArray(value)) return undefined;
  const length = (value as unknown[]).length;
  if (length === 0 || length > MAX_CHANGES) return undefined;
  const copy: RuntimeSourceChange[] = [];
  for (let index = 0; index < length; index += 1) {
    const entry = change((value as unknown[])[index]);
    if (entry === undefined) return undefined;
    copy.push(entry);
  }
  return copy;
}

/**
 * Runs the checks of a public method. Whatever goes wrong while reading an
 * argument (a getter or a proxy that throws, a detached buffer) is the
 * caller's invalid request, answered as a rejected promise like any other:
 * a public method never throws synchronously.
 */
function guarded<T>(
  message: string,
  work: () => Promise<T> | undefined,
): Promise<T> {
  try {
    return work() ?? rejected('invalid_request', message);
  } catch {
    return rejected('invalid_request', message);
  }
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
  channel.port1.onmessage = (event: MessageEvent<unknown>) => {
    if (!record(event.data) || typeof event.data.id !== 'number') return;
    const pending = waiting.get(event.data.id);
    if (pending === undefined) return;
    waiting.delete(event.data.id);
    clearTimeout(pending.timeout);
    if (event.data.ok !== true) {
      pending.reject(failure(pending.kind, event.data));
      return;
    }
    const parsed =
      pending.kind === 'metadata'
        ? metadata(event.data.metadata)
        : pending.kind === 'source.list'
          ? manifest(event.data.result)
          : pending.kind === 'source.read'
            ? part(event.data.result)
            : pending.kind === 'source.write'
              ? saved(event.data.result)
              : download(event.data.result);
    if (parsed === undefined || !pending.fits(parsed as never))
      pending.reject(failure(pending.kind, {}));
    else pending.resolve(parsed as never);
  };
  window.addEventListener(
    'pagehide',
    () => {
      closed = true;
      channel.port1.close();
      for (const pending of waiting.values()) {
        clearTimeout(pending.timeout);
        pending.reject(
          new RuntimeRequestError(
            'view_unavailable',
            'The runtime view closed.',
          ),
        );
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

  function call<T>(
    kind: RequestKind,
    fields: Record<string, unknown> = {},
    fits: (value: T) => boolean = () => true,
  ): Promise<T> {
    if (closed || performance.now() >= deadline)
      return rejected('view_unavailable', 'The runtime view has expired.');
    if (waiting.size >= MAX_PENDING)
      return rejected('busy', 'Too many runtime requests are pending.');
    if (next === Number.MAX_SAFE_INTEGER)
      return rejected('unavailable', 'The runtime request limit was reached.');
    const id = ++next;
    return new Promise<T>((resolve, reject) => {
      const timeout = window.setTimeout(
        () => {
          waiting.delete(id);
          reject(
            new RuntimeRequestError(
              'unavailable',
              kind === 'metadata'
                ? 'The runtime metadata request timed out.'
                : 'The runtime request timed out; its outcome is unknown.',
            ),
          );
        },
        Math.min(TIMEOUT_MS[kind], Math.max(0, deadline - performance.now())),
      );
      waiting.set(id, {
        kind,
        resolve: resolve as (value: never) => void,
        reject,
        timeout,
        fits: fits as (value: never) => boolean,
      });
      try {
        channel.port1.postMessage({ type: kind, id, ...fields });
      } catch {
        waiting.delete(id);
        clearTimeout(timeout);
        reject(
          new RuntimeRequestError(
            'view_unavailable',
            'The runtime view is unavailable.',
          ),
        );
      }
    });
  }

  window.artifree = Object.freeze({
    currentMetadata(): Promise<RuntimeMetadata> {
      return guarded('The metadata request is not valid.', () =>
        call('metadata'),
      );
    },
    listSource(): Promise<RuntimeSourceManifest> {
      return guarded('The source listing is not valid.', () =>
        call('source.list'),
      );
    },
    readSource(
      path: string,
      range: RuntimeSourceRange = {},
    ): Promise<RuntimeSourcePart> {
      return guarded('The source read is not valid.', () => {
        // `undefined` means no range; `null` and everything else that is not
        // an object of numbers is a mistake.
        const limits: unknown = range;
        if (!validPath(path) || !plain(limits)) return undefined;
        const { offset, length } = limits;
        if (
          (offset !== undefined && !count(offset)) ||
          (length !== undefined && !(count(length) && length >= 1))
        )
          return undefined;
        return call<RuntimeSourcePart>(
          'source.read',
          {
            path,
            ...(offset === undefined ? {} : { offset }),
            ...(length === undefined ? {} : { length }),
          },
          (answer) =>
            answer.path === path &&
            answer.offset === (offset ?? 0) &&
            answer.bytes.length <= (length ?? MAX_READ_BYTES),
        );
      });
    },
    saveSource(request: RuntimeSaveRequest): Promise<RuntimeSaveResult> {
      return guarded('The source save is not valid.', () => {
        const candidate: unknown = request;
        if (!plain(candidate)) return undefined;
        const { key } = candidate;
        const copy = changes(candidate.changes);
        if (
          typeof key !== 'string' ||
          !KEY_PATTERN.test(key) ||
          copy === undefined
        )
          return undefined;
        return call<RuntimeSaveResult>(
          'source.write',
          { key, changes: copy },
          (answer) => describesWrites(answer.files, copy),
        );
      });
    },
    requestDownload(
      request: RuntimeDownloadRequest = {},
    ): Promise<RuntimeDownloadResult> {
      return guarded('The download is not valid.', () => {
        const candidate: unknown = request;
        if (!plain(candidate)) return undefined;
        const { path } = candidate;
        if (path !== undefined && !validPath(path)) return undefined;
        return call<RuntimeDownloadResult>(
          'download',
          path === undefined ? {} : { path },
          (answer) => answer.kind === (path === undefined ? 'archive' : 'file'),
        );
      });
    },
  });
}
