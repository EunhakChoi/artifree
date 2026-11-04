import type { AppData, DataTooLarge } from '@artifree/contracts';
import type { ArtifreeClient } from './client.js';
import { checkArtifactId, checkDataPath } from './data-queries.js';
import { ArtifreeApiError } from './errors.js';
import { EventStreamParser } from './event-stream.js';
import type { ServerSentEvent } from './event-stream.js';
import { isCount, isRecord, parseAppData } from './responses.js';

/**
 * Follows one path of application data (decision 0040). This module is loaded
 * on demand and is not part of the client class. It reads the stream as bytes,
 * checks every event against the question and against the events before it,
 * and reconnects by the retry table of the decision.
 */

/** An event of the data stream, after it passed every check. */
export type DataEvent =
  | { readonly type: 'snapshot' | 'change'; readonly data: AppData }
  | { readonly type: 'too_large'; readonly data: DataTooLarge };

/**
 * The value at the followed path is over 128 KiB, so the server cannot send it
 * and ended the stream. Following stops: reconnecting would get the same
 * answer. The `too_large` event was delivered before this rejection.
 */
export class DataTooLargeError extends Error {
  /** The artifact. */
  readonly artifactId: string;
  /** The followed path. */
  readonly path: string;
  /** The data version the value was read at. */
  readonly dataVersion: number;

  constructor(details: DataTooLarge) {
    super(
      `The value at "${details.path}" is too large to follow; read it with getData.`,
    );
    this.name = 'DataTooLargeError';
    this.artifactId = details.artifact_id;
    this.path = details.path;
    this.dataVersion = details.data_version;
  }
}

function invalid(message: string): ArtifreeApiError {
  return new ArtifreeApiError(message, {
    status: 200,
    code: 'invalid_response',
  });
}

const APP_DATA_FIELDS = new Set([
  'artifact_id',
  'path',
  'data_version',
  'exists',
  'value',
]);
const TOO_LARGE_FIELDS = new Set(['artifact_id', 'path', 'data_version']);

function hasOnly(
  value: Record<string, unknown>,
  allowed: Set<string>,
): boolean {
  return Object.keys(value).every((key) => allowed.has(key));
}

function parseTooLarge(value: unknown): DataTooLarge | undefined {
  if (!isRecord(value) || !hasOnly(value, TOO_LARGE_FIELDS)) return undefined;
  const { artifact_id, path, data_version } = value;
  if (
    typeof artifact_id !== 'string' ||
    typeof path !== 'string' ||
    !isCount(data_version)
  )
    return undefined;
  return { artifact_id, path, data_version };
}

/**
 * Checks the events of one connection, in order, against the artifact and path
 * that were asked for and against the versions already delivered on it. The data
 * of an event names the data space only: an object with a field of any other
 * kind, such as a source revision, is refused.
 */
export class DataEventChecker {
  private readonly artifactId: string;
  private readonly path: string;
  private last: number | undefined;
  private ended = false;

  constructor(artifactId: string, path: string) {
    this.artifactId = artifactId;
    this.path = path;
  }

  /** The event for `event`, or `invalid_response` when it breaks a rule. */
  check(event: ServerSentEvent): DataEvent {
    const { type } = event;
    if (type !== 'snapshot' && type !== 'change' && type !== 'too_large')
      throw invalid(`The data stream sent an event named "${type}".`);
    if (this.ended)
      throw invalid(`The data stream sent a ${type} event after too_large.`);
    let value: unknown;
    try {
      value = JSON.parse(event.data);
    } catch {
      throw invalid(`The data stream sent a ${type} event that is not JSON.`);
    }
    const first = this.last === undefined;
    if (type === 'snapshot' && !first)
      throw invalid('The data stream sent a snapshot that was not first.');
    if (type === 'change' && first)
      throw invalid('The data stream sent a change before its snapshot.');
    if (type === 'too_large') {
      const data = parseTooLarge(value);
      if (data === undefined) throw this.mismatch(type);
      this.accept(type, data);
      this.ended = true;
      return { type, data };
    }
    const data =
      isRecord(value) && hasOnly(value, APP_DATA_FIELDS)
        ? parseAppData(value)
        : undefined;
    // The root is an object, so it always exists.
    if (data === undefined || (data.path === '' && !data.exists))
      throw this.mismatch(type);
    this.accept(type, data);
    return { type, data };
  }

  private mismatch(type: string): ArtifreeApiError {
    return invalid(
      `The data stream sent a ${type} event that does not match the contract.`,
    );
  }

  /** Checks the artifact, the path, and the version; then remembers it. */
  private accept(
    type: string,
    data: { artifact_id: string; path: string; data_version: number },
  ): void {
    if (data.artifact_id !== this.artifactId || data.path !== this.path)
      throw invalid(
        `The data stream sent a ${type} event about another artifact or path.`,
      );
    const version = data.data_version;
    // The snapshot that opens a connection is authoritative whatever version
    // it carries (the space may have been deleted and created again, or
    // restored); within a connection every version has to be newer.
    if (this.last !== undefined && version <= this.last)
      throw invalid(
        `The data stream sent a ${type} event with an old version.`,
      );
    this.last = version;
  }
}

/** Options of {@link followData}. */
export interface FollowDataOptions {
  /** The artifact. */
  readonly artifactId: string;
  /** The path to follow; the root when absent. */
  readonly path?: string;
  /** Stops following; the returned promise then resolves. */
  readonly signal: AbortSignal;
  /**
   * Receives every event in order, across reconnections. A `snapshot` is the
   * state to show; a `change` carries the new full value. What this throws
   * ends following and rejects it.
   */
  readonly onEvent: (event: DataEvent) => void;
  /** Called each time the server accepted a stream. */
  readonly onConnect?: () => void;
  /**
   * Called when a stream ends or cannot be opened and following continues,
   * before waiting `retryInMs` to reconnect.
   */
  readonly onDisconnect?: (
    error: ArtifreeApiError | undefined,
    retryInMs: number,
  ) => void;
  /** Delays between attempts, in milliseconds. Default 500 and 30 000. */
  readonly retryDelayMs?: { readonly min: number; readonly max: number };
  /**
   * How long an accepted connection must last, with at least one event
   * delivered, to count as stable and restart the delays at `min`. Default
   * 10 000 ms.
   */
  readonly stableMs?: number;
  /** The time in milliseconds; replaceable in tests. */
  readonly now?: () => number;
  /** Waits; replaceable in tests. It resolves early when `signal` aborts. */
  readonly sleep?: (milliseconds: number, signal: AbortSignal) => Promise<void>;
}

/** The shortest and longest wait a `Retry-After` of the server can cause. */
const RETRY_AFTER_FLOOR_MS = 500;
const RETRY_AFTER_CEILING_MS = 3_600_000;

/**
 * Whether reconnecting can help: the connection failed, or the server or a
 * proxy before it asked for a wait or failed to answer. Everything else needs
 * a change on the client's side, including a successful response that is not
 * this stream and a server fault (500) that a retry would repeat.
 */
export function isRetryable(error: ArtifreeApiError): boolean {
  return (
    error.code === 'network_error' ||
    error.status === 429 ||
    error.status === 502 ||
    error.status === 503 ||
    error.status === 504
  );
}

/** The wait before attempt number `attempt` (from 0) after a failure. */
export function backoffMs(
  attempt: number,
  min: number,
  max: number,
  retryAfterSeconds: number | undefined,
): number {
  if (retryAfterSeconds !== undefined)
    return Math.min(
      RETRY_AFTER_CEILING_MS,
      Math.max(RETRY_AFTER_FLOOR_MS, retryAfterSeconds * 1000),
    );
  return Math.min(max, min * 2 ** Math.min(attempt, 30));
}

function defaultSleep(
  milliseconds: number,
  signal: AbortSignal,
): Promise<void> {
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

function networkError(): ArtifreeApiError {
  return new ArtifreeApiError('The data stream connection was lost.', {
    status: 0,
    code: 'network_error',
  });
}

/** What a connection did, for the decision to reset the delays. */
interface Outcome {
  readonly acceptedAt: number;
  readonly failure: ArtifreeApiError | undefined;
}

/**
 * Follows the data at `options.path` of an artifact until `options.signal`
 * aborts: delivers a `snapshot`, then a `change` with the new full value each
 * time the data version moved, and reconnects after a lost connection, 429,
 * 502, 503, 504, or a stream the server ended, with a fresh `snapshot`. There
 * is no replay: a state in between may not be delivered.
 *
 * Resolves when the signal aborts, at any point; nothing is delivered after
 * it. Rejects with the failure when it is not retried (every other status,
 * `invalid_response`, a handler that threw), and with
 * {@link DataTooLargeError} after the `too_large` event.
 */
export async function followData(
  client: ArtifreeClient,
  options: FollowDataOptions,
): Promise<void> {
  const { artifactId, signal, onEvent, onConnect, onDisconnect } = options;
  const path = options.path ?? '';
  checkArtifactId(artifactId);
  checkDataPath(path);
  const { min, max } = options.retryDelayMs ?? { min: 500, max: 30_000 };
  const stableMs = options.stableMs ?? 10_000;
  if (
    !Number.isFinite(min) ||
    !Number.isFinite(max) ||
    min <= 0 ||
    max < min ||
    !Number.isFinite(stableMs) ||
    stableMs < 0
  )
    throw new ArtifreeApiError(
      'retryDelayMs needs 0 < min <= max, and stableMs cannot be negative.',
      { status: 0, code: 'invalid_request' },
    );
  const now = options.now ?? ((): number => performance.now());
  const sleep = options.sleep ?? defaultSleep;
  const uri = `/v1/artifacts/${artifactId}/data/events?${new URLSearchParams({ path }).toString()}`;
  let attempt = 0;
  while (!signal.aborted) {
    const checker = new DataEventChecker(artifactId, path);
    let acceptedAt: number | undefined;
    let delivered = 0;
    let failure: ArtifreeApiError | undefined;
    let response: Response | undefined;
    try {
      response = await client.send('GET', uri, {
        headers: { accept: 'text/event-stream' },
        signal,
      });
    } catch (error: unknown) {
      if (signal.aborted) return;
      if (!(error instanceof ArtifreeApiError) || !isRetryable(error))
        throw error;
      failure = error;
    }
    if (response !== undefined) {
      const outcome = await readConnection(
        response,
        signal,
        checker,
        (event) => {
          delivered += 1;
          onEvent(event);
        },
        onConnect,
        now,
      );
      acceptedAt = outcome.acceptedAt;
      failure = outcome.failure;
      if (signal.aborted) return;
    }
    // Only an accepted connection that delivered and lasted counts as stable;
    // opening a stream alone never restarts the delays.
    if (
      acceptedAt !== undefined &&
      delivered > 0 &&
      now() - acceptedAt >= stableMs
    )
      attempt = 0;
    const retryInMs = backoffMs(attempt, min, max, failure?.retryAfterSeconds);
    attempt += 1;
    onDisconnect?.(failure, retryInMs);
    await sleep(retryInMs, signal);
  }
}

/**
 * Reads one accepted response to its end. Returns what happened; throws what
 * must end following: a handler's error, `invalid_response`, and
 * {@link DataTooLargeError}. The body is cancelled on every exit.
 */
async function readConnection(
  response: Response,
  signal: AbortSignal,
  checker: DataEventChecker,
  deliver: (event: DataEvent) => void,
  onConnect: (() => void) | undefined,
  now: () => number,
): Promise<Outcome> {
  const { body } = response;
  const mediaType = response.headers
    .get('content-type')
    ?.split(';')[0]
    ?.trim()
    .toLowerCase();
  if (mediaType !== 'text/event-stream' || body === null) {
    await body?.cancel().catch(() => undefined);
    throw new ArtifreeApiError(
      `The data stream answered HTTP ${response.status} with ${mediaType ?? 'no content type'}, not an event stream.`,
      { status: response.status, code: 'invalid_response' },
    );
  }
  const acceptedAt = now();
  const reader = body.getReader();
  const cancel = (): void => {
    reader.cancel().catch(() => undefined);
  };
  signal.addEventListener('abort', cancel);
  if (signal.aborted) cancel();
  const parser = new EventStreamParser();
  try {
    onConnect?.();
    for (;;) {
      let chunk: ReadableStreamReadResult<Uint8Array>;
      try {
        chunk = await reader.read();
      } catch {
        if (signal.aborted) return { acceptedAt, failure: undefined };
        return { acceptedAt, failure: networkError() };
      }
      if (signal.aborted) return { acceptedAt, failure: undefined };
      if (chunk.done) {
        parser.finish();
        break;
      }
      for (const raw of parser.push(chunk.value)) {
        // One chunk can hold several events; none follows an abort.
        if (signal.aborted) return { acceptedAt, failure: undefined };
        const event = checker.check(raw);
        deliver(event);
        // Reconnecting would get the same answer, however the body ends.
        if (event.type === 'too_large') throw new DataTooLargeError(event.data);
      }
    }
  } finally {
    signal.removeEventListener('abort', cancel);
    await reader.cancel().catch(() => undefined);
  }
  // The server ended the stream: reconnect for a fresh snapshot.
  return { acceptedAt, failure: undefined };
}
