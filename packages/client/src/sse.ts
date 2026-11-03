import { ArtifreeApiError } from './errors.js';
import { breach } from './data-grammar.js';

/** One dispatched server-sent event. */
export interface ServerSentEvent {
  readonly type: string;
  readonly data: string;
}

/** Limits on a stream, in bytes of the UTF-8 stream itself. */
export interface SseLimits {
  /** The longest line, not counting its terminator. */
  readonly maxLineBytes: number;
  /** The most bytes the `data` lines of one event add up to (LF included). */
  readonly maxEventBytes: number;
}

const LF = 0x0a;
const CR = 0x0d;

/** The bytes of `parts` as one array. */
function join(parts: Uint8Array[], total: number): Uint8Array {
  if (parts.length === 1 && parts[0] !== undefined) return parts[0];
  const joined = new Uint8Array(total);
  let offset = 0;
  for (const part of parts) {
    joined.set(part, offset);
    offset += part.length;
  }
  return joined;
}

/**
 * The events of a `text/event-stream` body, read from its bytes as the WHATWG
 * HTML standard specifies: lines end with CRLF, LF, or CR (also when the pair
 * is split across chunks); a byte order mark at the start is dropped; comment
 * lines, `id`, `retry`, and unknown fields are ignored; one space after the
 * colon is removed; `data` lines are joined with LF; an event is dispatched at
 * a blank line only when it has data; an event the body ends before is
 * dropped.
 *
 * Every limit is in bytes of the stream, not characters. New bytes are
 * scanned once, for line ends only, so a long line arriving in tiny chunks
 * costs time linear in its length. Bytes that are not UTF-8 are an
 * `invalid_response`, as is a line or an event over its limit.
 */
export async function* serverSentEvents(
  chunks: AsyncIterable<Uint8Array>,
  limits: SseLimits,
): AsyncGenerator<ServerSentEvent> {
  const decoder = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true });
  let parts: Uint8Array[] = [];
  let lineBytes = 0;
  let skipLf = false;
  let atStart = true;
  let type = '';
  let data: string[] = [];
  let dataBytes = 0;

  // Handles one complete line; returns an event when it ends one.
  const finish = (bytes: Uint8Array): ServerSentEvent | undefined => {
    let text: string;
    try {
      text = decoder.decode(bytes);
    } catch {
      throw breach('The event stream is not valid UTF-8.');
    }
    if (atStart) {
      atStart = false;
      if (text.startsWith('﻿')) text = text.slice(1);
    }
    if (text === '') {
      const event =
        data.length > 0
          ? { type: type === '' ? 'message' : type, data: data.join('\n') }
          : undefined;
      type = '';
      data = [];
      dataBytes = 0;
      return event;
    }
    if (text.startsWith(':')) return undefined;
    const colon = text.indexOf(':');
    const field = colon === -1 ? text : text.slice(0, colon);
    let value = colon === -1 ? '' : text.slice(colon + 1);
    if (value.startsWith(' ')) value = value.slice(1);
    if (field === 'event') {
      type = value;
    } else if (field === 'data') {
      // "data:" and an optional space are ASCII, so the value's bytes are
      // the line's bytes less that prefix.
      const prefix = colon === -1 ? bytes.length : colon + 1;
      const spaced = colon !== -1 && text.charAt(colon + 1) === ' ' ? 1 : 0;
      dataBytes += bytes.length - prefix - spaced + 1;
      if (dataBytes > limits.maxEventBytes)
        throw breach(
          `An event of the stream has more than ${limits.maxEventBytes} bytes of data.`,
        );
      data.push(value);
    }
    return undefined;
  };

  for await (const chunk of chunks) {
    let start = 0;
    // An empty chunk says nothing about the LF a CR may be followed by.
    if (skipLf && chunk.length > 0) {
      skipLf = false;
      if (chunk[0] === LF) start = 1;
    }
    for (let index = start; index < chunk.length; index += 1) {
      const byte = chunk[index];
      if (byte !== LF && byte !== CR) continue;
      const piece = chunk.subarray(start, index);
      if (lineBytes + piece.length > limits.maxLineBytes)
        throw breach(
          `A line of the stream has more than ${limits.maxLineBytes} bytes.`,
        );
      parts.push(piece);
      const event = finish(join(parts, lineBytes + piece.length));
      parts = [];
      lineBytes = 0;
      if (byte === CR) {
        if (index + 1 < chunk.length) {
          if (chunk[index + 1] === LF) index += 1;
        } else {
          skipLf = true;
        }
      }
      start = index + 1;
      if (event !== undefined) yield event;
    }
    const rest = chunk.subarray(start);
    if (rest.length > 0) {
      if (lineBytes + rest.length > limits.maxLineBytes)
        throw breach(
          `A line of the stream has more than ${limits.maxLineBytes} bytes.`,
        );
      // A copy: the chunk may be large or reused by its producer.
      parts.push(rest.slice());
      lineBytes += rest.length;
    }
  }
}

/**
 * The raw chunks of a streamed body. A failure while reading, such as a lost
 * connection, is a `network_error`. Stopping early or aborting `signal`
 * cancels the body, which closes its connection, also with a `fetch` that
 * ignores the signal once the response has arrived.
 */
export async function* bodyChunks(
  body: ReadableStream<Uint8Array>,
  signal: AbortSignal,
): AsyncGenerator<Uint8Array> {
  const reader = body.getReader();
  const cancel = (): void => {
    reader.cancel().catch(() => undefined);
  };
  signal.addEventListener('abort', cancel);
  // Reading starts on the first iteration, possibly after an abort.
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
