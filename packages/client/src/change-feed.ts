import type { ArtifactChange, ChangeKind } from '@artifree/contracts';
import { ArtifreeApiError } from './errors.js';
import { parseFeedChange, parseFeedPosition } from './responses.js';

/**
 * A committed change. `change` is `'unknown'` for a kind added to the
 * contract after this client; the artifact changed either way.
 */
export type FeedChange = Omit<ArtifactChange, 'change'> & {
  readonly change: ChangeKind | 'unknown';
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
interface ServerSentEvent {
  readonly type: string;
  readonly data: string;
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
 */
async function* serverSentEvents(
  chunks: AsyncIterable<string>,
): AsyncGenerator<ServerSentEvent> {
  let pending = '';
  let type = '';
  let data: string[] = [];
  for await (const chunk of chunks) {
    pending += chunk;
    // A final CR may be the first half of a CRLF in the next chunk.
    const lines = pending.split(/\r\n|\r(?!$)|\n/);
    pending = lines.pop() ?? '';
    for (const line of lines) {
      if (line === '') {
        if (data.length > 0)
          yield { type: type || 'message', data: data.join('\n') };
        type = '';
        data = [];
        continue;
      }
      if (line.startsWith(':')) continue;
      const colon = line.indexOf(':');
      const field = colon === -1 ? line : line.slice(0, colon);
      const value = colon === -1 ? '' : line.slice(colon + 1).replace(/^ /, '');
      if (field === 'event') type = value;
      else if (field === 'data') data.push(value);
    }
  }
  // The body ended: a pending CR ends its line, but an event without its
  // blank line is incomplete and dropped.
  if (pending === '\r' && data.length > 0)
    yield { type: type || 'message', data: data.join('\n') };
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
   * Reconnection delays: from `min`, doubling after each failed attempt up
   * to `max`, in milliseconds. A `Retry-After` from the server is used
   * instead when given.
   */
  readonly retryDelayMs?: { readonly min: number; readonly max: number };
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
 * not {@link retryable}, or with what `onEvent` throws.
 */
export async function follow(
  open: OpenStream,
  options: FollowOptions,
): Promise<void> {
  const { signal, onEvent, onConnect, onDisconnect } = options;
  const { min, max } = options.retryDelayMs ?? { min: 500, max: 30_000 };
  let cursor = options.after;
  let attempt = 0;
  while (!signal.aborted) {
    let failure: ArtifreeApiError | undefined;
    // What onEvent threw: it ends following and is never retried.
    let rejected: { readonly error: unknown } | undefined;
    try {
      const events = await open(cursor, signal);
      attempt = 0;
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
    }
    if (rejected !== undefined) throw rejected.error;
    if (signal.aborted) return;
    const retryInMs =
      failure?.retryAfterSeconds === undefined
        ? Math.min(max, min * 2 ** attempt)
        : failure.retryAfterSeconds * 1000;
    attempt += 1;
    onDisconnect?.(failure, retryInMs);
    await sleep(retryInMs, signal);
  }
}

/** Waits `milliseconds`, or less when `signal` aborts or has aborted. */
function sleep(milliseconds: number, signal: AbortSignal): Promise<void> {
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
