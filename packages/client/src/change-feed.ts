import type { ArtifactChange, ChangeKind } from '@artifree/contracts';
import { ArtifreeApiError } from './errors.js';
import { parseFeedChange, parseFeedPosition } from './responses.js';
import {
  type Clock,
  RETRY_RESET_MS,
  reconnectDelayMs,
  startTimer,
  systemClock,
  wait,
} from './transport.js';

/**
 * A committed change. `change` is `'unknown'` for a kind added to the
 * contract after this client; the artifact changed either way.
 * `data_version` is the version a data write left the space at, a safe
 * integer of at least 1: present exactly when `change` is `'data_changed'`.
 */
export type FeedChange = Omit<ArtifactChange, 'change' | 'data_version'> & {
  readonly change: ChangeKind | 'unknown';
  readonly data_version?: number;
};

/**
 * An event of `GET /v1/changes`. `ready` starts a stream opened without a
 * cursor; `reset` means changes may have been missed, so read all state
 * again; `change` names an artifact and revision to read again.
 */
export type ChangeFeedEvent =
  | { readonly type: 'ready' | 'reset'; readonly cursor: string }
  | { readonly type: 'change'; readonly change: FeedChange };

/** The feed position after `event`, to resume from. */
export function cursorOf(event: ChangeFeedEvent): string {
  return event.type === 'change' ? event.change.cursor : event.cursor;
}

/** One dispatched server-sent event: its type and data. */
export interface ServerSentEvent {
  readonly type: string;
  readonly data: string;
}

/**
 * The most UTF-16 code units of an unfinished line, and of one event's type
 * and data together, before a stream is an `invalid_response`. A line is
 * measured as received, its field name included; an event as its type plus
 * its data lines joined with LF.
 */
export const EVENT_STREAM_LIMIT = 65_536;

function oversized(what: string): ArtifreeApiError {
  return new ArtifreeApiError(
    `The change feed sent ${what} of more than ${EVENT_STREAM_LIMIT} UTF-16 code units.`,
    { status: 200, code: 'invalid_response' },
  );
}

/**
 * The events of a `text/event-stream` body, interpreted as the WHATWG HTML
 * standard specifies: UTF-8 with a leading byte order mark removed; lines
 * ended by CRLF, LF, or CR, also when split across chunks; comment lines and
 * unknown fields ignored; one space after the colon removed; `data` lines
 * joined with LF; an event dispatched at a blank line only if it has data;
 * and an event not ended by a blank line discarded. `id` and `retry` are
 * ignored: feed data carries its cursor, and reconnection follows
 * {@link follow}'s own delays.
 *
 * Memory is bounded by {@link EVENT_STREAM_LIMIT}: a line or an event past
 * it throws `invalid_response`, which ends the iteration and so cancels the
 * body at once. The work for a chunk is proportional to the chunk: line ends
 * are searched for only in the new text, and a chunk that leaves an
 * unfinished line past the limit is refused before it is split.
 */
export async function* serverSentEvents(
  chunks: AsyncIterable<string>,
): AsyncGenerator<ServerSentEvent> {
  // The text of the line being received, up to its line end.
  let pending = '';
  // The previous chunk ended with a CR: an LF now starting is part of it.
  let afterCarriageReturn = false;
  let type = '';
  let data: string[] = [];
  // The length of `data` joined with LF.
  let dataUnits = 0;
  const lineEnd = /[\r\n]/g;
  for await (const chunk of chunks) {
    let from = 0;
    if (afterCarriageReturn && chunk !== '') {
      afterCarriageReturn = false;
      if (chunk.charCodeAt(0) === 10) from = 1;
    }
    const start = from;
    const lastEnd = Math.max(chunk.lastIndexOf('\n'), chunk.lastIndexOf('\r'));
    const unfinished =
      lastEnd >= start
        ? chunk.length - lastEnd - 1
        : pending.length + chunk.length - start;
    if (unfinished > EVENT_STREAM_LIMIT) throw oversized('an unfinished line');
    for (;;) {
      lineEnd.lastIndex = from;
      const found = lineEnd.exec(chunk);
      if (found === null) break;
      const end = found.index;
      if (pending.length + end - from > EVENT_STREAM_LIMIT)
        throw oversized('a line');
      const line = pending + chunk.slice(from, end);
      pending = '';
      from = end + 1;
      if (chunk.charCodeAt(end) === 13) {
        if (from === chunk.length) afterCarriageReturn = true;
        else if (chunk.charCodeAt(from) === 10) from += 1;
      }
      if (line === '') {
        if (data.length > 0)
          yield { type: type || 'message', data: data.join('\n') };
        type = '';
        data = [];
        dataUnits = 0;
        continue;
      }
      if (line.startsWith(':')) continue;
      const colon = line.indexOf(':');
      const field = colon === -1 ? line : line.slice(0, colon);
      const value = colon === -1 ? '' : line.slice(colon + 1).replace(/^ /, '');
      if (field === 'event') {
        type = value;
      } else if (field === 'data') {
        dataUnits += value.length + (data.length > 0 ? 1 : 0);
        data.push(value);
      }
      if (type.length + dataUnits > EVENT_STREAM_LIMIT)
        throw oversized('an event');
    }
    pending += chunk.slice(from);
  }
  // The body ended: a line or an event without its end is incomplete and
  // dropped.
}

/**
 * The feed events of a stream, until `signal` aborts. Event types this
 * client does not know are skipped; a known event whose data breaks the
 * contract rejects with `invalid_response`.
 */
export async function* changeFeedEvents(
  chunks: AsyncIterable<string>,
  signal: AbortSignal | undefined,
): AsyncGenerator<ChangeFeedEvent> {
  for await (const { type, data } of serverSentEvents(chunks)) {
    // One chunk can hold several events; none follows an abort.
    if (signal?.aborted === true) return;
    if (type !== 'ready' && type !== 'reset' && type !== 'change') continue;
    let value: unknown;
    try {
      value = JSON.parse(data);
    } catch {
      value = undefined;
    }
    if (type === 'change') {
      const change = parseFeedChange(value);
      if (change !== undefined) {
        yield { type, change };
        continue;
      }
    } else {
      const position = parseFeedPosition(value);
      if (position !== undefined) {
        yield { type, cursor: position.cursor };
        continue;
      }
    }
    throw new ArtifreeApiError(
      `The change feed sent a ${type} event that does not match the contract.`,
      { status: 200, code: 'invalid_response' },
    );
  }
}

/** Options of {@link follow}. */
export interface FollowOptions {
  /** Resume after this cursor; without it, start at the head with `ready`. */
  readonly after?: string;
  /** Stops following; the returned promise then resolves. */
  readonly signal: AbortSignal;
  /** Receives every event in order, across reconnections. */
  readonly onEvent: (event: ChangeFeedEvent) => void;
  /**
   * Called each time the server accepts a stream. Changes missed while
   * disconnected follow on it.
   */
  readonly onConnect?: () => void;
  /**
   * Called when a stream ends or cannot be opened and following continues,
   * before waiting `retryInMs` to reconnect from the last cursor.
   */
  readonly onDisconnect?: (
    error: ArtifreeApiError | undefined,
    retryInMs: number,
  ) => void;
  /**
   * Reconnection delays in milliseconds, 500 and 30,000 by default. The
   * delay before retry `n` is `min` doubled `n` times up to `max`, scaled by
   * `0.5 + 0.5u` for `u` from `random`, so it lies from half that to just
   * under all of it. `n` counts retries since a connection last stayed open
   * for 30 seconds after the server accepted it; a stream dropped sooner
   * never resets it. A `Retry-After` from the server is a floor, at most one
   * hour: the delay is the larger of the two.
   */
  readonly retryDelayMs?: { readonly min: number; readonly max: number };
  /** A number in [0, 1) for the jitter of each delay; `Math.random` by default. */
  readonly random?: () => number;
}

/** Opens one stream after `after`; resolves once the server accepted it. */
export type OpenStream = (
  after: string | undefined,
  signal: AbortSignal,
) => Promise<AsyncIterable<ChangeFeedEvent>>;

/**
 * Whether reconnecting can help: the connection failed, the server asked
 * for a wait, or the server or a proxy before it failed. Every other
 * failure needs a change on the client's side, including a successful
 * response that is not this feed.
 */
function retryable(error: ArtifreeApiError): boolean {
  return (
    error.code === 'network_error' ||
    error.status === 429 ||
    error.status >= 500
  );
}

/**
 * Follows the feed through `open`, reconnecting after each ended or failed
 * stream from the last cursor received, so no committed change is skipped.
 * Resolves when `options.signal` aborts. Rejects with the failure when it is
 * not {@link retryable}, or with what `onEvent` throws. Waits and stalls
 * run on `clock`.
 */
export async function follow(
  open: OpenStream,
  options: FollowOptions,
  clock: Clock = systemClock,
): Promise<void> {
  const { signal, onEvent, onConnect, onDisconnect } = options;
  const { min, max } = options.retryDelayMs ?? { min: 500, max: 30_000 };
  const random = options.random ?? Math.random;
  let cursor = options.after;
  // Retries since the last connection that stayed open long enough.
  let attempt = 0;
  while (!signal.aborted) {
    let failure: ArtifreeApiError | undefined;
    // What onEvent threw: it ends following and is never retried.
    let rejected: { readonly error: unknown } | undefined;
    // Set once this connection has been open for RETRY_RESET_MS.
    let lasted = false;
    let stopLasting: (() => void) | undefined;
    try {
      const events = await open(cursor, signal);
      stopLasting = startTimer(clock, RETRY_RESET_MS, () => {
        lasted = true;
      });
      onConnect?.();
      for await (const event of events) {
        cursor = cursorOf(event);
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
    } finally {
      stopLasting?.();
    }
    if (rejected !== undefined) throw rejected.error;
    if (signal.aborted) return;
    if (lasted) attempt = 0;
    const retryInMs = reconnectDelayMs({
      min,
      max,
      attempt,
      random,
      retryAfterSeconds: failure?.retryAfterSeconds,
    });
    attempt += 1;
    onDisconnect?.(failure, retryInMs);
    await wait(clock, retryInMs, signal);
  }
}
