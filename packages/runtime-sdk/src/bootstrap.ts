import type {
  DownloadReceipt,
  RuntimeFailureCode,
  RuntimeMetadata,
  SourceFile,
  SourceListing,
  SourcePart,
  SourceReadRequest,
} from './index.js';
import { isPackagePath } from './package-path.js';

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
/** Calls waiting for an answer at once. */
const MAX_PENDING = 64;
/** A read waits this long; a download waits for a person, so far longer. */
const READ_TIMEOUT_MS = 10_000;
const DOWNLOAD_TIMEOUT_MS = 120_000;

const FAILURE_CODES: Record<RuntimeFailureCode, string> = {
  invalid_request: 'The runtime request was not valid.',
  denied: 'The runtime request was not permitted.',
  stale_view: 'The runtime view is out of date.',
  not_found: 'The requested source was not found.',
  rate_limited: 'Too many runtime requests.',
  declined: 'The user declined the request.',
  unavailable: 'The runtime request is unavailable.',
  expired: 'The runtime view has expired.',
};

/**
 * The only way a runtime call fails. The message and code come from a closed
 * set; nothing the server or the workspace said is passed on.
 */
class RuntimeRequestFailure extends Error {
  readonly code: RuntimeFailureCode;

  constructor(code: RuntimeFailureCode, message?: string) {
    super(message ?? FAILURE_CODES[code]);
    this.name = 'RuntimeRequestFailure';
    this.code = code;
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

/**
 * A part, checked against the request it answers and against the bytes
 * actually received: no number the workspace states is believed. The bytes
 * number `nextOffset - offset`, or `size - offset` for the final part; the
 * next offset lies strictly after the offset and strictly before the end,
 * so a read loop always progresses.
 */
function part(
  value: unknown,
  asked: SourceReadRequest,
): SourcePart | undefined {
  if (!record(value)) return undefined;
  const { path, offset, nextOffset, size, sha256: digest, bytes } = value;
  if (
    path !== asked.path ||
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
    received > MAX_PART_BYTES ||
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
    path,
    offset,
    nextOffset,
    size,
    sha256: digest,
    bytes: new Uint8Array(bytes),
  };
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
      pending.reject(new RuntimeRequestFailure(code, pending.refusal));
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
    options: { readonly timeout: number; readonly refusal?: string },
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
        channel.port1.postMessage({ ...message, id });
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

  window.artifree = Object.freeze({
    currentMetadata(): Promise<RuntimeMetadata> {
      return call({ type: 'metadata' }, (reply) => metadata(reply.metadata), {
        timeout: READ_TIMEOUT_MS,
        refusal: 'The runtime metadata request was refused.',
      });
    },
    source,
  });
}
