import type { AppData, DataEventTooLarge } from '@artifree/contracts';
import { parseRetryAfter, type ClientConfiguration } from './client.js';
import { DATA_PATH_RULE, invalidRequest, isDataPath } from './data-query.js';
import { ArtifreeApiError } from './errors.js';
import { isCount, isRecord, parseAppData, parseError } from './responses.js';

/**
 * Following the value at one path of application data (decision 0040). A
 * function, not a method of the client class, for the reason `listDataMembers`
 * is one.
 *
 * The server sends the current state when a stream opens (`snapshot`) and the
 * newest state whenever the data version moves (`change`). There is no
 * replay: a connection that was lost is replaced by a new one that begins with
 * a new snapshot, and events never describe a difference. A reducer must
 * replace its state with each event it is given.
 */

/** Longest line, and most bytes of `data` in one event, the reader accepts. */
export const MAX_EVENT_BYTES = 256 * 1024;
/** Every wait before a new connection is at least this long (milliseconds). */
export const MIN_WAIT_MS = 500;
/** The longest wait the doubling reaches (milliseconds). */
export const MAX_BACKOFF_MS = 30_000;
/** The longest `Retry-After` honored (milliseconds): one hour. */
export const RETRY_AFTER_CEILING_MS = 3_600_000;
/** A connection with nothing received for this long is dropped (milliseconds). */
export const IDLE_TIMEOUT_MS = 45_000;
/** A connection open this long counts as healthy: the backoff starts over. */
export const HEALTHY_AFTER_MS = 30_000;

/** An event of a data stream. */
export interface DataStreamEvent {
  /** `snapshot` is the first event of a connection, `change` each later one. */
  readonly type: 'snapshot' | 'change';
  /** The state at `data.path`: the full value, or `exists: false`. */
  readonly data: AppData;
}

/** Time, injectable so that tests do not wait. */
export interface DataStreamTimers {
  /** Resolves after `milliseconds`, or at once when `signal` aborts. */
  readonly sleep: (milliseconds: number, signal: AbortSignal) => Promise<void>;
  /** A monotonic clock in milliseconds. */
  readonly now: () => number;
}

/** Options of {@link followData}. */
export interface FollowDataOptions {
  /** The watched path; absent or empty is the root. */
  readonly path?: string;
  /** Stops following; the returned promise then resolves. */
  readonly signal: AbortSignal;
  /** Receives every event in order, across connections. */
  readonly onEvent: (event: DataStreamEvent) => void;
  /** Called each time the server accepted a stream. */
  readonly onConnect?: () => void;
  /**
   * Called when a stream ended or could not be opened and following
   * continues, before waiting `retryInMs`.
   */
  readonly onDisconnect?: (
    error: ArtifreeApiError | undefined,
    retryInMs: number,
  ) => void;
  /** Replaces the clock and the sleep; the defaults are the real ones. */
  readonly timers?: DataStreamTimers;
}

function realSleep(milliseconds: number, signal: AbortSignal): Promise<void> {
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
    const timer = setTimeout(done, milliseconds);
    signal.addEventListener('abort', done);
  });
}

const REAL_TIMERS: DataStreamTimers = {
  sleep: realSleep,
  now: () => performance.now(),
};

function invalidResponse(message: string): ArtifreeApiError {
  return new ArtifreeApiError(message, {
    status: 200,
    code: 'invalid_response',
  });
}

function networkError(message: string, cause: unknown): ArtifreeApiError {
  return new ArtifreeApiError(message, {
    status: 0,
    code: 'network_error',
    cause,
  });
}

/** One dispatched server-sent event. */
interface ServerSentEvent {
  readonly type: string;
  readonly data: string;
}

/**
 * The events of a `text/event-stream` body as the WHATWG HTML standard
 * specifies, with a limit: a line, or the `data` of one event, over
 * {@link MAX_EVENT_BYTES} characters is `invalid_response`. Comment lines,
 * `id`, `retry`, and unknown fields are ignored; an event the body ends
 * before its blank line is dropped.
 */
async function* serverSentEvents(
  chunks: AsyncIterable<string>,
): AsyncGenerator<ServerSentEvent> {
  let pending = '';
  let type = '';
  let data: string[] = [];
  let dataSize = 0;
  const tooLong = (): ArtifreeApiError =>
    invalidResponse(
      `The data stream sent a line or event over ${MAX_EVENT_BYTES} characters.`,
    );
  for await (const chunk of chunks) {
    pending += chunk;
    // A final CR may be the first half of a CRLF in the next chunk.
    const lines = pending.split(/\r\n|\r(?!$)|\n/);
    pending = lines.pop() ?? '';
    if (pending.length > MAX_EVENT_BYTES) throw tooLong();
    for (const line of lines) {
      if (line.length > MAX_EVENT_BYTES) throw tooLong();
      if (line === '') {
        if (data.length > 0)
          yield { type: type || 'message', data: data.join('\n') };
        type = '';
        data = [];
        dataSize = 0;
        continue;
      }
      if (line.startsWith(':')) continue;
      const colon = line.indexOf(':');
      const field = colon === -1 ? line : line.slice(0, colon);
      const value = colon === -1 ? '' : line.slice(colon + 1).replace(/^ /, '');
      if (field === 'event') type = value;
      else if (field === 'data') {
        dataSize += value.length + 1;
        if (dataSize > MAX_EVENT_BYTES) throw tooLong();
        data.push(value);
      }
    }
  }
  if (pending === '\r' && data.length > 0)
    yield { type: type || 'message', data: data.join('\n') };
}

/** The text of a body; a read that waits too long or fails is a network error. */
async function* bodyText(
  body: ReadableStream<Uint8Array>,
  signal: AbortSignal,
  idleTimeoutMs: number,
  timers: DataStreamTimers,
): AsyncGenerator<string> {
  const reader = body.getReader();
  const cancel = (): void => {
    reader.cancel().catch(() => undefined);
  };
  signal.addEventListener('abort', cancel);
  if (signal.aborted) cancel();
  const decoder = new TextDecoder();
  try {
    for (;;) {
      const idle = new AbortController();
      const timeout = timers
        .sleep(idleTimeoutMs, idle.signal)
        .then(() => (idle.signal.aborted ? undefined : ('idle' as const)));
      let chunk: ReadableStreamReadResult<Uint8Array> | 'idle' | undefined;
      try {
        chunk = await Promise.race([reader.read(), timeout]);
      } catch (error: unknown) {
        throw networkError('The data stream connection was lost.', error);
      } finally {
        idle.abort();
      }
      if (chunk === 'idle' || chunk === undefined)
        throw networkError(
          'The data stream sent nothing for too long.',
          undefined,
        );
      if (chunk.done) {
        yield decoder.decode();
        return;
      }
      yield decoder.decode(chunk.value, { stream: true });
    }
  } finally {
    signal.removeEventListener('abort', cancel);
    await reader.cancel().catch(() => undefined);
  }
}

function parseTooLarge(value: unknown): DataEventTooLarge | undefined {
  if (!isRecord(value)) return undefined;
  const { artifact_id, path, data_version, max_bytes } = value;
  if (
    typeof artifact_id !== 'string' ||
    typeof path !== 'string' ||
    !isCount(data_version) ||
    !isCount(max_bytes)
  )
    return undefined;
  return { artifact_id, path, data_version, max_bytes };
}

/** What a connection must agree with. */
interface Watched {
  readonly artifactId: string;
  readonly path: string;
}

/**
 * Whether an event of `type` is acceptable after the one with version
 * `lastVersion` (`undefined` before any): for the watched artifact and path,
 * and with a `data_version` greater than every one delivered before. A
 * `snapshot` is the first, a `change` is never first, and a `too_large` stands
 * in for either.
 */
function inOrder(
  type: 'snapshot' | 'change' | 'too_large',
  event: { artifact_id: string; path: string; data_version: number },
  watched: Watched,
  lastVersion: number | undefined,
): boolean {
  if (event.artifact_id !== watched.artifactId || event.path !== watched.path)
    return false;
  if (type === 'snapshot' && lastVersion !== undefined) return false;
  if (type === 'change' && lastVersion === undefined) return false;
  return lastVersion === undefined || event.data_version > lastVersion;
}

/**
 * The validated events of one connection. The first is a `snapshot`, each
 * later one a `change` whose `data_version` is greater than the one before,
 * all for the watched artifact and path. A `too_large` event is held to the
 * same rules (the right artifact and path, and newer than what was delivered)
 * and then rejects with `data_too_large`; an event of a type this client does
 * not know is skipped.
 */
async function* dataEvents(
  chunks: AsyncIterable<string>,
  watched: Watched,
  signal: AbortSignal,
): AsyncGenerator<DataStreamEvent> {
  let lastVersion: number | undefined;
  for await (const { type, data } of serverSentEvents(chunks)) {
    // One chunk can hold several events; none follows an abort.
    if (signal.aborted) return;
    if (type !== 'snapshot' && type !== 'change' && type !== 'too_large')
      continue;
    let value: unknown;
    try {
      value = JSON.parse(data);
    } catch {
      value = undefined;
    }
    if (type === 'too_large') {
      const large = parseTooLarge(value);
      if (large === undefined || !inOrder(type, large, watched, lastVersion))
        throw invalidResponse(
          'The data stream sent a too_large event that does not match the contract, the watched path, or its order.',
        );
      throw new ArtifreeApiError(
        `The value at "${watched.path}" is over ${large.max_bytes} bytes and is not streamed; watch a smaller path.`,
        { status: 200, code: 'data_too_large' },
      );
    }
    const state = parseAppData(value);
    if (state === undefined || !inOrder(type, state, watched, lastVersion))
      throw invalidResponse(
        `The data stream sent a ${type} event that does not match the contract or its order.`,
      );
    lastVersion = state.data_version;
    yield { type, data: state };
  }
}

/** Whether a new connection can help. */
function retryable(error: ArtifreeApiError): boolean {
  return (
    error.code === 'network_error' ||
    error.status === 429 ||
    error.status >= 500
  );
}

/** Opens one connection; resolves once the server accepted the stream. */
async function openStream(
  configuration: ClientConfiguration,
  watched: Watched,
  signal: AbortSignal,
  timers: DataStreamTimers,
): Promise<AsyncGenerator<DataStreamEvent>> {
  const base = configuration.baseUrl.replace(/\/+$/, '');
  const query =
    watched.path === '' ? '' : `?path=${encodeURIComponent(watched.path)}`;
  const url = `${base}/v1/artifacts/${encodeURIComponent(watched.artifactId)}/data/events${query}`;
  const fetchImpl =
    configuration.fetch ?? ((input, init) => fetch(input, init));
  let response: Response;
  try {
    response = await fetchImpl(url, {
      method: 'GET',
      headers: {
        authorization: `Bearer ${configuration.accessToken}`,
        accept: 'text/event-stream',
      },
      signal,
    });
  } catch (error: unknown) {
    throw networkError('Could not reach the Artifree API.', error);
  }
  if (!response.ok) {
    const retryAfter = parseRetryAfter(response.headers.get('retry-after'));
    let answer: unknown;
    try {
      answer = JSON.parse(await response.text());
    } catch {
      // Not JSON, or the body was lost: the status still decides.
      answer = undefined;
    }
    const failure = parseError(answer);
    const limited = response.status === 429;
    throw new ArtifreeApiError(
      failure?.message ??
        `The data stream failed with HTTP ${response.status}.`,
      {
        status: response.status,
        code: limited ? 'rate_limited' : (failure?.code ?? 'invalid_response'),
        body: answer,
        ...(retryAfter === undefined ? {} : { retryAfterSeconds: retryAfter }),
      },
    );
  }
  const mediaType = response.headers
    .get('content-type')
    ?.split(';')[0]
    ?.trim()
    .toLowerCase();
  if (mediaType !== 'text/event-stream' || response.body === null) {
    await response.body?.cancel().catch(() => undefined);
    throw invalidResponse(
      `The data stream answered with ${mediaType ?? 'no content type'}, not an event stream.`,
    );
  }
  return dataEvents(
    bodyText(response.body, signal, IDLE_TIMEOUT_MS, timers),
    watched,
    signal,
  );
}

/**
 * Follows the value at `options.path` of an artifact's application data until
 * `options.signal` aborts, opening a new connection after each ended or failed
 * one. Every connection begins with a `snapshot`; if events were missed
 * meanwhile, the snapshot says where things stand now and nothing says what
 * was missed. Within a connection `data_version` strictly increases.
 *
 * Waiting before a new connection is at least {@link MIN_WAIT_MS}. After a
 * `Retry-After` it is that long, up to {@link RETRY_AFTER_CEILING_MS}; after
 * anything else it doubles from {@link MIN_WAIT_MS} to {@link MAX_BACKOFF_MS},
 * and starts over only after a connection that stayed open for
 * {@link HEALTHY_AFTER_MS}, so a server that closes streams at once is not
 * hammered. A connection that receives nothing for {@link IDLE_TIMEOUT_MS}
 * is dropped.
 *
 * Rejects at once with `invalid_request`, sending nothing, for a path that is
 * not in the form the server accepts (no leading, trailing or doubled `/`).
 *
 * Resolves when the signal aborts. Rejects with the failure when a new
 * connection cannot help (a `4xx` other than `429`, a response that is not the
 * stream, an event that breaks the contract, `data_too_large`), or with what
 * `onEvent` throws.
 */
export async function followData(
  configuration: ClientConfiguration,
  artifactId: string,
  options: FollowDataOptions,
): Promise<void> {
  const { signal, onEvent, onConnect, onDisconnect } = options;
  const timers = options.timers ?? REAL_TIMERS;
  const watched: Watched = { artifactId, path: options.path ?? '' };
  // The server answers with the path it accepted; a path it would not accept
  // (or would write differently) can never match and is refused here.
  if (!isDataPath(watched.path)) throw invalidRequest(DATA_PATH_RULE);
  let attempt = 0;
  while (!signal.aborted) {
    let failure: ArtifreeApiError | undefined;
    // What onEvent threw: it ends following and is never retried.
    let rejected: { readonly error: unknown } | undefined;
    let connectedAt: number | undefined;
    try {
      const events = await openStream(configuration, watched, signal, timers);
      connectedAt = timers.now();
      onConnect?.();
      for await (const event of events) {
        try {
          onEvent(event);
        } catch (error: unknown) {
          rejected = { error };
          break;
        }
      }
    } catch (error: unknown) {
      if (signal.aborted) return;
      if (!(error instanceof ArtifreeApiError) || !retryable(error))
        throw error;
      failure = error;
    }
    if (rejected !== undefined) throw rejected.error;
    if (signal.aborted) return;
    if (
      connectedAt !== undefined &&
      timers.now() - connectedAt >= HEALTHY_AFTER_MS
    )
      attempt = 0;
    const backoff = Math.min(MAX_BACKOFF_MS, MIN_WAIT_MS * 2 ** attempt);
    const requested =
      failure?.retryAfterSeconds === undefined
        ? backoff
        : Math.min(RETRY_AFTER_CEILING_MS, failure.retryAfterSeconds * 1000);
    const retryInMs = Math.max(MIN_WAIT_MS, requested);
    attempt += 1;
    onDisconnect?.(failure, retryInMs);
    await timers.sleep(retryInMs, signal);
  }
}
