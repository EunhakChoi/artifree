import type { AppData, DataStreamTooLarge } from '@artifree/contracts';
import type { ClientConfiguration } from './client.js';
import {
  DATA_LIMITS,
  canonicalBytes,
  isDataPath,
  hasRepeatedMember,
  isStoredJson,
  nestingAllowedAt,
  nodesAllowedAt,
  pathDepth,
} from './data-json.js';
import { checkArtifactId, getResponse, retryAfterMs } from './data-http.js';
import { ArtifreeApiError } from './errors.js';
import { isCount, isRecord, parseError } from './responses.js';
import { serverSentEvents } from './sse.js';
import type { SseLimits } from './sse.js';

/** An event of a data subscription, validated. */
export type DataEvent =
  | { readonly type: 'snapshot' | 'change'; readonly data: AppData }
  | { readonly type: 'too_large'; readonly data: DataStreamTooLarge };

/** What to watch. */
export interface FollowDataRequest {
  readonly artifactId: string;
  /** The one path to watch; the root when absent or empty. */
  readonly path?: string;
}

/** Retry timing of {@link followData}, in milliseconds. */
export interface FollowDataRetry {
  /** The first delay; doubled after each failed attempt. */
  readonly minMs?: number;
  /** The largest delay. */
  readonly maxMs?: number;
  /**
   * How long a connection must stay open, and deliver at least one event,
   * before the next attempt starts from `minMs` again.
   */
  readonly stableMs?: number;
}

/** Options of {@link followData}. */
export interface FollowDataOptions {
  /** Stops following; the returned promise then resolves with `aborted`. */
  readonly signal: AbortSignal;
  /**
   * Receives every event in order. Within one connection versions only grow;
   * a `snapshot` after a reconnect replaces what the caller has. It may
   * abort the signal; nothing is delivered after that.
   */
  readonly onEvent: (event: DataEvent) => void;
  /** Called each time the server accepts a stream. May abort the signal. */
  readonly onConnect?: () => void;
  /**
   * Called when a stream ends or cannot be opened and following continues,
   * before waiting `retryInMs`. May abort the signal.
   */
  readonly onDisconnect?: (
    failure: ArtifreeApiError | undefined,
    retryInMs: number,
  ) => void;
  readonly retry?: FollowDataRetry;
  /** A monotonic clock in milliseconds; `performance.now` by default. */
  readonly clock?: () => number;
  /** Waits `ms`, or less when `signal` aborts; real timers by default. */
  readonly sleep?: (ms: number, signal: AbortSignal) => Promise<void>;
}

/**
 * How following ended: `aborted` when the signal aborted, `too_large` when
 * the server said the watched value is over the event bound. The second is
 * terminal: it is never retried.
 */
export type FollowDataEnd = 'aborted' | 'too_large';

const LIMITS: SseLimits = {
  lineBytes: DATA_LIMITS.eventMaxBytes + 4096,
  dataBytes: DATA_LIMITS.eventMaxBytes + 4096,
  pendingBytes: 2 * (DATA_LIMITS.eventMaxBytes + 4096),
};

const DEFAULTS = { minMs: 500, maxMs: 30_000, stableMs: 10_000 } as const;

function broken(message: string): ArtifreeApiError {
  return new ArtifreeApiError(message, {
    status: 200,
    code: 'invalid_response',
  });
}

const APP_DATA_KEYS = new Set([
  'artifact_id',
  'path',
  'data_version',
  'exists',
  'value',
]);
const TOO_LARGE_KEYS = new Set([
  'artifact_id',
  'path',
  'data_version',
  'limit_bytes',
]);

function onlyKeys(
  record: Record<string, unknown>,
  allowed: Set<string>,
): boolean {
  return Object.keys(record).every((key) => allowed.has(key));
}

/** What the connection has seen, for the ordering rules. */
interface Seen {
  version: number | undefined;
  ended: boolean;
}

function jsonOf(text: string): unknown {
  try {
    return JSON.parse(text) as unknown;
  } catch {
    return undefined;
  }
}

/**
 * Validates one event against the request and the connection so far: the
 * right artifact and path, exact fields, a version strictly newer than the
 * last of this connection, a snapshot first and only first, nothing after a
 * `too_large`, and a value that is JSON the server could have stored and
 * whose canonical text is within the event bound.
 */
function validate(
  type: string,
  text: string,
  artifactId: string,
  path: string,
  seen: Seen,
): DataEvent | ArtifreeApiError {
  if (seen.ended) return broken('The stream sent an event after it ended.');
  const data = jsonOf(text);
  // JSON.parse keeps the last of two equal names; the domain stores neither.
  if (data !== undefined && hasRepeatedMember(text))
    return broken(`The stream sent a ${type} event that repeats a name.`);
  if (type === 'error') {
    const failure = parseError(data);
    if (failure === undefined)
      return broken('The stream sent a malformed error event.');
    return new ArtifreeApiError(failure.message, {
      status: 200,
      code: failure.code,
      body: data,
    });
  }
  if (!isRecord(data))
    return broken(`The stream sent a ${type} event that is not an object.`);
  const bad = (): ArtifreeApiError =>
    broken(`The stream sent a ${type} event that does not match the contract.`);
  if (
    data.artifact_id !== artifactId ||
    data.path !== path ||
    !isCount(data.data_version)
  )
    return bad();
  const version = data.data_version;
  if (seen.version !== undefined && version <= seen.version) return bad();
  if (type === 'too_large') {
    if (
      !onlyKeys(data, TOO_LARGE_KEYS) ||
      data.limit_bytes !== DATA_LIMITS.eventMaxBytes
    )
      return bad();
    seen.version = version;
    seen.ended = true;
    return {
      type,
      data: {
        artifact_id: artifactId,
        path,
        data_version: version,
        limit_bytes: DATA_LIMITS.eventMaxBytes,
      },
    };
  }
  if (type !== 'snapshot' && type !== 'change') return bad();
  if ((type === 'snapshot') !== (seen.version === undefined)) return bad();
  if (!onlyKeys(data, APP_DATA_KEYS) || typeof data.exists !== 'boolean')
    return bad();
  const hasValue = Object.hasOwn(data, 'value');
  if (hasValue !== data.exists) return bad();
  const event: AppData = {
    artifact_id: artifactId,
    path,
    data_version: version,
    exists: data.exists,
  };
  if (hasValue) {
    const found: unknown = data.value;
    // The value alone is held to the event bound, in canonical bytes; the
    // framing allowance of the line limits never widens it.
    const depth = pathDepth(path);
    if (
      !isStoredJson(found, nestingAllowedAt(depth), nodesAllowedAt(depth)) ||
      (path === '' && !isRecord(found)) ||
      canonicalBytes(found) > DATA_LIMITS.eventMaxBytes
    )
      return bad();
    event.value = found;
  }
  seen.version = version;
  return { type, data: event };
}

/** The event types a data stream may carry; others are skipped. */
const KNOWN = new Set(['snapshot', 'change', 'too_large', 'error']);

async function* bodyChunks(
  body: ReadableStream<Uint8Array>,
  signal: AbortSignal,
): AsyncGenerator<Uint8Array> {
  const reader = body.getReader();
  const cancel = (): void => {
    reader.cancel().catch(() => undefined);
  };
  signal.addEventListener('abort', cancel);
  if (signal.aborted) cancel();
  try {
    for (;;) {
      let chunk: ReadableStreamReadResult<Uint8Array>;
      try {
        chunk = await reader.read();
      } catch (error: unknown) {
        throw new ArtifreeApiError('The data stream connection was lost.', {
          status: 0,
          code: 'network_error',
          cause: error,
        });
      }
      if (chunk.done) return;
      yield chunk.value;
    }
  } finally {
    signal.removeEventListener('abort', cancel);
    await reader.cancel().catch(() => undefined);
  }
}

/**
 * Whether reconnecting can help, by rule: the connection failed
 * (`network_error`), the server asked for a wait (429), or the server or a
 * proxy before it is unavailable (502, 503, 504, and the typed
 * `unavailable`, which an `error` event of an accepted stream carries as
 * status 200). A fault of the stored data (`internal`, as a 500 before the
 * stream is accepted or as an `error` event after it), `not_found`, every 4xx
 * other than 429, and a response that breaks the contract need a change on
 * the client's side.
 */
export function isRetryable(error: ArtifreeApiError): boolean {
  return (
    error.code === 'network_error' ||
    error.status === 429 ||
    error.status === 502 ||
    error.status === 503 ||
    error.status === 504 ||
    (error.status === 200 && error.code === 'unavailable')
  );
}

function defaultSleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    if (signal.aborted) {
      resolve();
      return;
    }
    const done = (): void => {
      clearTimeout(timer);
      signal.removeEventListener('abort', done);
      resolve();
    };
    const timer = setTimeout(done, ms);
    signal.addEventListener('abort', done);
  });
}

/** The outcome of calling a callback: it may have aborted, or thrown. */
type Called =
  { readonly aborted: true } | { readonly threw: unknown } | undefined;

/**
 * Calls `callback`, then looks at the signal: after an abort nothing the
 * callback threw matters and following ends; otherwise what it threw ends
 * following with that error.
 */
function call(signal: AbortSignal, callback: () => void): Called {
  let threw: { readonly error: unknown } | undefined;
  try {
    callback();
  } catch (error: unknown) {
    threw = { error };
  }
  if (signal.aborted) return { aborted: true };
  return threw === undefined ? undefined : { threw: threw.error };
}

/**
 * Follows one path of the caller's application data (decision 0040): a
 * `snapshot`, then a `change` with the new full value whenever the data
 * version moves. There is no replay; after every reconnect the server sends
 * a fresh authoritative `snapshot`, so `onEvent` receives one per connection
 * and the caller replaces what it has. The request is checked before it is
 * sent. Reconnection doubles its delay from `minMs` up to `maxMs`, honours a
 * `Retry-After` between 500 ms and 1 h, and starts over from `minMs` only
 * after a connection that opened, delivered an event, and stayed up for
 * `stableMs`. Resolves with `aborted` or `too_large` ({@link FollowDataEnd});
 * rejects with a failure that retrying cannot fix ({@link isRetryable}), or
 * with what a callback threw while the signal was not aborted.
 */
export async function followData(
  configuration: ClientConfiguration,
  request: FollowDataRequest,
  options: FollowDataOptions,
): Promise<FollowDataEnd> {
  const { artifactId } = request;
  const path = request.path ?? '';
  checkArtifactId(artifactId);
  if (!isDataPath(path))
    throw new ArtifreeApiError('The path breaks the data path grammar.', {
      status: 0,
      code: 'invalid_request',
    });
  const { signal, onEvent, onConnect, onDisconnect } = options;
  const minMs = options.retry?.minMs ?? DEFAULTS.minMs;
  const maxMs = options.retry?.maxMs ?? DEFAULTS.maxMs;
  const stableMs = options.retry?.stableMs ?? DEFAULTS.stableMs;
  const clock = options.clock ?? (() => performance.now());
  const sleep = options.sleep ?? defaultSleep;
  const uri = `/v1/artifacts/${artifactId}/data/events${
    path === '' ? '' : `?path=${encodeURIComponent(path)}`
  }`;
  let attempt = 0;
  while (!signal.aborted) {
    let failure: ArtifreeApiError | undefined;
    // What a callback threw. It is checked before anything is classified as
    // a transport failure, so a retryable-looking error from a callback is
    // never retried.
    let callbackFailure: { readonly error: unknown } | undefined;
    let opened = false;
    let openedAt = 0;
    let delivered = 0;
    try {
      const response = await getResponse(
        configuration,
        uri,
        'text/event-stream',
        signal,
      );
      const type = response.headers.get('content-type') ?? '';
      if (
        response.body === null ||
        !type.toLowerCase().startsWith('text/event-stream')
      ) {
        await response.body?.cancel().catch(() => undefined);
        throw broken('The data stream is not an event stream.');
      }
      if (signal.aborted) {
        await response.body.cancel().catch(() => undefined);
        return 'aborted';
      }
      opened = true;
      openedAt = clock();
      if (onConnect !== undefined) {
        const called = call(signal, onConnect);
        if (called !== undefined) {
          await response.body.cancel().catch(() => undefined);
          if ('aborted' in called) return 'aborted';
          callbackFailure = { error: called.threw };
          throw called.threw;
        }
      }
      const seen: Seen = { version: undefined, ended: false };
      for await (const { type: eventType, data } of serverSentEvents(
        bodyChunks(response.body, signal),
        LIMITS,
      )) {
        if (signal.aborted) return 'aborted';
        if (!KNOWN.has(eventType)) continue;
        const checked = validate(eventType, data, artifactId, path, seen);
        if (checked instanceof ArtifreeApiError) throw checked;
        delivered += 1;
        const called = call(signal, () => {
          onEvent(checked);
        });
        if (called !== undefined && 'aborted' in called) return 'aborted';
        // Terminal the moment it is delivered, whatever the connection does.
        if (called !== undefined) {
          callbackFailure = { error: called.threw };
          throw called.threw;
        }
        if (checked.type === 'too_large') return 'too_large';
      }
    } catch (error: unknown) {
      if (callbackFailure !== undefined) throw callbackFailure.error;
      if (signal.aborted) return 'aborted';
      if (!(error instanceof ArtifreeApiError) || !isRetryable(error))
        throw error;
      failure = error;
    }
    if (signal.aborted) return 'aborted';
    if (opened && delivered >= 1 && clock() - openedAt >= stableMs) attempt = 0;
    const asked = failure === undefined ? undefined : retryAfterMs(failure);
    const retryInMs = asked ?? Math.min(maxMs, minMs * 2 ** attempt);
    attempt += 1;
    if (onDisconnect !== undefined) {
      const called = call(signal, () => {
        onDisconnect(failure, retryInMs);
      });
      if (called !== undefined) {
        if ('aborted' in called) return 'aborted';
        throw called.threw;
      }
    }
    await sleep(retryInMs, signal);
  }
  return 'aborted';
}
