import type { JsonValue } from '@artifree/contracts';
import type { ArtifreeClient } from './client.js';
import { ArtifreeApiError } from './errors.js';
import {
  MAX_EVENT_VALUE_BYTES,
  breach,
  checkValue,
  dataPathProblem,
  exactKeys,
  isObject,
  refusal,
  versionField,
} from './data-grammar.js';
import { bodyChunks, serverSentEvents } from './sse.js';

/**
 * A value the stream delivers: the whole current value at the followed path
 * and the data space's version.
 */
export interface DataFollowEvent {
  /** `snapshot` first on each connection, then `change`. */
  readonly type: 'snapshot' | 'change';
  readonly artifactId: string;
  readonly path: string;
  /** The version of the data space; strictly increasing on a connection. */
  readonly dataVersion: number;
  /** Whether anything is at the path. */
  readonly exists: boolean;
  /** The value, present exactly when `exists` is true; it can be `null`. */
  readonly value?: JsonValue;
}

/** Options of {@link followData}. */
export interface FollowDataOptions {
  /** The path to follow; the empty path (default) is the root. */
  readonly path?: string;
  /** Stops following; the returned promise then resolves. */
  readonly signal: AbortSignal;
  /** Receives every event in order, across reconnections. */
  readonly onEvent: (event: DataFollowEvent) => void;
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
  /**
   * Waits between attempts: from `min` (at least 1), doubling after each
   * attempt to `max`, in milliseconds. Default 500 and 30,000. No wait is
   * shorter than `min` or longer than one hour.
   */
  readonly retryDelayMs?: { readonly min: number; readonly max: number };
  /**
   * How long a connection must have lasted before the next wait starts again
   * at `min`; default 10,000 ms. A server that accepts a stream and closes it
   * at once is thereby asked less and less often.
   */
  readonly stableMs?: number;
  /** Waits; tests replace it. Must resolve early when `signal` aborts. */
  readonly sleep?: (milliseconds: number, signal: AbortSignal) => Promise<void>;
  /** A monotonic clock in milliseconds; tests replace it. */
  readonly now?: () => number;
}

/** The longest wait between attempts, whatever the server asks. */
export const MAX_RETRY_MS = 60 * 60 * 1000;

/** A line or event is the value plus the JSON around it. */
const MAX_EVENT_BYTES = MAX_EVENT_VALUE_BYTES + 4096;

function wait(milliseconds: number, signal: AbortSignal): Promise<void> {
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

/**
 * Whether trying again can help: the connection failed, the server asked for
 * a wait, or the server or a proxy before it failed. Everything else, a
 * response that breaks the contract and a value that is too large included,
 * needs a change on the client's side.
 */
function retryable(error: ArtifreeApiError): boolean {
  return (
    error.code === 'network_error' ||
    error.status === 429 ||
    error.status >= 500
  );
}

/** Where a connection is in the sequence the contract allows. */
interface Session {
  readonly artifactId: string;
  readonly path: string;
  /** The highest version delivered on earlier connections. */
  readonly floor: number | undefined;
  /** The version of the last event of this connection. */
  last: number | undefined;
}

type Checked =
  | { readonly kind: 'value'; readonly event: DataFollowEvent }
  | { readonly kind: 'too_large'; readonly version: number };

/** Checks one event against the contract and the sequence; throws on a breach. */
function checkEvent(session: Session, type: string, data: string): Checked {
  if (type !== 'snapshot' && type !== 'change' && type !== 'too_large')
    throw breach(`The stream sent an event named ${JSON.stringify(type)}.`);
  let parsed: unknown;
  try {
    parsed = JSON.parse(data);
  } catch {
    throw breach(`A ${type} event is not JSON.`);
  }
  if (!isObject(parsed)) throw breach(`A ${type} event is not an object.`);
  const first = session.last === undefined;
  if (type === 'snapshot' && !first)
    throw breach('A snapshot came after the first event.');
  if (type === 'change' && first)
    throw breach('A change came before the snapshot.');
  if (type === 'too_large') {
    exactKeys(
      parsed,
      ['artifact_id', 'path', 'data_version', 'max_bytes'],
      [],
      'A too_large event',
    );
  } else {
    exactKeys(
      parsed,
      ['artifact_id', 'path', 'data_version', 'exists'],
      ['value'],
      `A ${type} event`,
    );
  }
  if (parsed.artifact_id !== session.artifactId)
    throw breach(`A ${type} event is for another artifact.`);
  if (parsed.path !== session.path)
    throw breach(`A ${type} event is for another path.`);
  const version = versionField(parsed.data_version, 'data_version');
  // Within a connection versions rise; a new connection may repeat the
  // version it last delivered but not go back.
  if (session.last !== undefined) {
    if (version <= session.last)
      throw breach(
        `data_version ${version} does not exceed the previous ${session.last}.`,
      );
  } else if (session.floor !== undefined && version < session.floor) {
    throw breach(
      `data_version ${version} is below ${session.floor}, already delivered.`,
    );
  }
  if (type === 'too_large') {
    const { max_bytes: max } = parsed;
    if (typeof max !== 'number' || !Number.isSafeInteger(max) || max < 1)
      throw breach('max_bytes is not a positive whole number.');
    return { kind: 'too_large', version };
  }
  if (typeof parsed.exists !== 'boolean')
    throw breach('exists is not a boolean.');
  const base = {
    type,
    artifactId: session.artifactId,
    path: session.path,
    dataVersion: version,
    exists: parsed.exists,
  } as const;
  if (!parsed.exists) {
    if (Object.hasOwn(parsed, 'value'))
      throw breach('A value is sent where nothing exists.');
    if (session.path === '') throw breach('The root always exists.');
    return { kind: 'value', event: base };
  }
  if (!Object.hasOwn(parsed, 'value'))
    throw breach('No value is sent where something exists.');
  const { value } = parsed;
  if (session.path === '' && !isObject(value))
    throw breach('The value at the root is not an object.');
  checkValue(value, 'A value');
  return { kind: 'value', event: { ...base, value } };
}

/** Opens the stream; resolves once the server accepted it. */
async function openStream(
  client: ArtifreeClient,
  artifactId: string,
  path: string,
  signal: AbortSignal,
): Promise<ReadableStream<Uint8Array>> {
  const query = path === '' ? '' : `?path=${encodeURIComponent(path)}`;
  const target = `/v1/artifacts/${encodeURIComponent(artifactId)}/data/events${query}`;
  const response = await client.send('GET', target, {
    headers: { accept: 'text/event-stream' },
    signal,
  });
  const mediaType = response.headers
    .get('content-type')
    ?.split(';')[0]
    ?.trim()
    .toLowerCase();
  if (mediaType !== 'text/event-stream' || response.body === null) {
    await response.body?.cancel().catch(() => undefined);
    throw breach(
      `GET ${target} answered HTTP ${response.status} with ${mediaType ?? 'no content type'}, not an event stream.`,
      response.status,
    );
  }
  return response.body;
}

function checkOptions(options: FollowDataOptions): {
  path: string;
  min: number;
  max: number;
  stableMs: number;
} {
  const path = options.path ?? '';
  const problem = dataPathProblem(path);
  if (problem !== undefined) throw refusal(`path: ${problem}`);
  const { min, max } = options.retryDelayMs ?? { min: 500, max: 30_000 };
  if (!(Number.isFinite(min) && min >= 1 && Number.isFinite(max) && max >= min))
    throw refusal('retryDelayMs: min is at least 1 and max at least min');
  const stableMs = options.stableMs ?? 10_000;
  if (!Number.isFinite(stableMs) || stableMs < 0)
    throw refusal('stableMs: a number from 0');
  return { path, min, max, stableMs };
}

/**
 * Follows the value at `path` of your application data until `signal`
 * aborts (decision 0040). Each connection delivers a `snapshot`, then a
 * `change` with the whole new value each time the data space's version
 * moves; nothing is replayed, so after a lost connection, a 429, a 5xx, or a
 * clean end (the server ends a stream after ten minutes) it reconnects and
 * delivers a fresh snapshot. Every event is checked: artifact, path, fields,
 * order (`snapshot` once and first), and a `data_version` that rises, which
 * must be a safe integer; a breach rejects with `invalid_response`. A value
 * too large for an event rejects with `data_too_large`, as does every
 * failure that trying again cannot fix, a 4xx included, and an error thrown by
 * `onEvent`. The wait between attempts is the doubling delay, or the server's
 * `Retry-After` if longer, never below `retryDelayMs.min` nor above an hour,
 * and a clean close waits too. Aborting resolves at once: before the first
 * request, during a wait, and during a read, and nothing is delivered after
 * it. Invalid parameters reject with `invalid_request` before anything is
 * sent.
 */
export async function followData(
  client: ArtifreeClient,
  artifactId: string,
  options: FollowDataOptions,
): Promise<void> {
  const { path, min, max, stableMs } = checkOptions(options);
  const { signal, onEvent } = options;
  const sleep = options.sleep ?? wait;
  const now = options.now ?? ((): number => performance.now());
  let floor: number | undefined;
  let attempt = 0;
  while (!signal.aborted) {
    const started = now();
    let failure: ArtifreeApiError | undefined;
    // What onEvent threw: it ends following and is never retried.
    let rejected: { readonly error: unknown } | undefined;
    try {
      const body = await openStream(client, artifactId, path, signal);
      options.onConnect?.();
      const session: Session = {
        artifactId,
        path,
        floor,
        last: undefined,
      };
      const events = serverSentEvents(bodyChunks(body, signal), {
        maxLineBytes: MAX_EVENT_BYTES,
        maxEventBytes: MAX_EVENT_BYTES,
      });
      for await (const { type, data } of events) {
        // One chunk can hold several events; none follows an abort.
        if (signal.aborted) break;
        const checked = checkEvent(session, type, data);
        if (checked.kind === 'too_large')
          throw new ArtifreeApiError(
            `The value at "${path}" is larger than ${MAX_EVENT_VALUE_BYTES} bytes, so it cannot be followed; follow a smaller path.`,
            { status: 200, code: 'data_too_large' },
          );
        session.last = checked.event.dataVersion;
        floor = Math.max(floor ?? 0, checked.event.dataVersion);
        try {
          onEvent(checked.event);
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
    // Only a connection that stayed up starts the waits over.
    if (now() - started >= stableMs) attempt = 0;
    const backoff = Math.min(max, min * 2 ** attempt);
    attempt += 1;
    const asked =
      failure?.retryAfterSeconds === undefined
        ? 0
        : failure.retryAfterSeconds * 1000;
    const retryInMs = Math.min(MAX_RETRY_MS, Math.max(min, backoff, asked));
    options.onDisconnect?.(failure, retryInMs);
    await sleep(retryInMs, signal);
  }
}
