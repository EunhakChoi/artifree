import { ArtifreeApiError } from './errors.js';
import {
  type FeedChange,
  parseArtifactChange,
  parseFeedPosition,
} from './responses.js';

export type { FeedChange };

/**
 * One event of `GET /v1/changes`. `ready` starts a stream opened without a
 * cursor. `reset` means the server cannot continue from the client's cursor
 * (for example, it serves another or a restored database): read all state
 * again. A `change` names a committed artifact revision to read again.
 * Every event carries the cursor to resume from.
 */
export type ChangeFeedEvent =
  | { readonly type: 'ready'; readonly cursor: number }
  | { readonly type: 'reset'; readonly cursor: number }
  | { readonly type: 'change'; readonly change: FeedChange };

/** The cursor an event leaves the feed at. */
export function cursorOf(event: ChangeFeedEvent): number {
  return event.type === 'change' ? event.change.cursor : event.cursor;
}

/**
 * Reads server-sent events from a change feed response. Comments and event
 * types this client does not know are skipped; known events must match the
 * contract.
 */
export async function* readChangeEvents(
  body: ReadableStream<Uint8Array>,
): AsyncGenerator<ChangeFeedEvent> {
  const decoder = new TextDecoder();
  const reader = body.getReader();
  const lines = new LineSplitter();
  let name = '';
  let data: string[] = [];
  try {
    for (;;) {
      let chunk: ReadableStreamReadResult<Uint8Array>;
      try {
        chunk = await reader.read();
      } catch (error: unknown) {
        throw new ArtifreeApiError('The change feed connection ended.', {
          status: 0,
          code: 'network_error',
          cause: error,
        });
      }
      const complete = chunk.done
        ? lines.end()
        : lines.push(decoder.decode(chunk.value, { stream: true }));
      for (const line of complete) {
        if (line === '') {
          const event = dispatch(name, data);
          name = '';
          data = [];
          if (event !== undefined) yield event;
        } else if (!line.startsWith(':')) {
          const separator = line.indexOf(':');
          const field = separator < 0 ? line : line.slice(0, separator);
          const value =
            separator < 0 ? '' : line.slice(separator + 1).replace(/^ /, '');
          if (field === 'event') name = value;
          else if (field === 'data') data.push(value);
        }
      }
      // An event without its closing blank line is dropped, as the
      // server-sent events format requires.
      if (chunk.done) return;
    }
  } finally {
    reader.releaseLock();
    // Ends the connection when the caller stops early.
    await body.cancel().catch(() => undefined);
  }
}

/**
 * Splits text into lines ended by CRLF, LF, or CR, across chunk boundaries.
 * A CR at the end of a chunk may be the first half of a CRLF, so it ends a
 * line only once the next chunk, or the end of the stream, shows it.
 */
class LineSplitter {
  private rest = '';

  /** The lines that `text` completes. */
  push(text: string): string[] {
    const pending = `${this.rest}${text}`;
    const held = pending.endsWith('\r') ? 1 : 0;
    const lines = pending.slice(0, pending.length - held).split(/\r\n|\r|\n/);
    this.rest = `${lines.pop() ?? ''}${pending.slice(pending.length - held)}`;
    return lines;
  }

  /** The last line, when the stream ended right after its CR. */
  end(): string[] {
    return this.rest.endsWith('\r') ? [this.rest.slice(0, -1)] : [];
  }
}

function dispatch(name: string, data: string[]): ChangeFeedEvent | undefined {
  if (name !== 'ready' && name !== 'reset' && name !== 'change')
    return undefined;
  let value: unknown;
  try {
    value = JSON.parse(data.join('\n'));
  } catch {
    value = undefined;
  }
  if (name === 'change') {
    const change = parseArtifactChange(value);
    if (change !== undefined) return { type: 'change', change };
  } else {
    const cursor = parseFeedPosition(value);
    if (cursor !== undefined) return { type: name, cursor };
  }
  throw new ArtifreeApiError(
    `A ${name} event of the change feed does not match the contract.`,
    { status: 200, code: 'invalid_response' },
  );
}

/** Options of `ArtifreeClient.followChanges`. */
export interface FollowOptions {
  /** Resume after this cursor; omit to start at the current head. */
  readonly after?: number;
  /** Stops following; the returned promise then resolves. */
  readonly signal: AbortSignal;
  /** Receives every event in order, across reconnections. */
  readonly onEvent: (event: ChangeFeedEvent) => void;
  /**
   * Called when the connection ends or cannot be opened, before waiting
   * `retryInMs` and reconnecting from the last cursor.
   */
  readonly onDisconnect?: (
    error: ArtifreeApiError | undefined,
    retryInMs: number,
  ) => void;
  /** Reconnection delays: doubling from `min` up to `max` milliseconds. */
  readonly retryDelayMs?: { readonly min: number; readonly max: number };
}

// Failures that reconnecting cannot fix, unless the status is a 5xx: a
// proxy in front of a restarting server answers with a body that is not an
// API error, and the server is back later.
const FATAL: ReadonlySet<string> = new Set([
  'unauthenticated',
  'invalid_request',
  'not_found',
  'invalid_response',
]);

const retryable = (error: ArtifreeApiError): boolean =>
  error.status >= 500 || !FATAL.has(error.code);

/**
 * Follows the feed through `open`, reconnecting after each ended or failed
 * connection from the last cursor received, so no committed change is
 * skipped. Waits `Retry-After` when the server gives one. Rejects on
 * failures a retry cannot fix.
 */
export async function follow(
  open: (
    after: number | undefined,
    signal: AbortSignal,
  ) => AsyncIterable<ChangeFeedEvent>,
  options: FollowOptions,
): Promise<void> {
  const { signal, onEvent, onDisconnect } = options;
  const { min, max } = options.retryDelayMs ?? { min: 500, max: 30_000 };
  let cursor = options.after;
  let failures = 0;
  while (!signal.aborted) {
    let failure: ArtifreeApiError | undefined;
    try {
      for await (const event of open(cursor, signal)) {
        cursor = cursorOf(event);
        failures = 0;
        onEvent(event);
      }
    } catch (error: unknown) {
      if (signal.aborted) return;
      if (!(error instanceof ArtifreeApiError) || !retryable(error))
        throw error;
      failure = error;
    }
    if (signal.aborted) return;
    const retryInMs =
      failure?.retryAfterSeconds === undefined
        ? Math.min(max, min * 2 ** failures)
        : failure.retryAfterSeconds * 1000;
    failures += 1;
    onDisconnect?.(failure, retryInMs);
    await sleep(retryInMs, signal);
  }
}

function sleep(milliseconds: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    // An abort that already happened, for example in onDisconnect, fires no
    // more events.
    if (signal.aborted) {
      resolve();
      return;
    }
    const timer = setTimeout(done, milliseconds);
    signal.addEventListener('abort', done, { once: true });
    function done(): void {
      clearTimeout(timer);
      signal.removeEventListener('abort', done);
      resolve();
    }
  });
}
