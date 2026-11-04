import { ArtifreeApiError } from './errors.js';

/** One dispatched server-sent event. */
export interface ServerSentEvent {
  readonly type: string;
  readonly data: string;
}

/** Byte limits of the parser; every one counts UTF-8 bytes. */
export interface SseLimits {
  /** One line, without its terminator. */
  readonly lineBytes: number;
  /** The `data` of one event: its lines joined by LF. */
  readonly dataBytes: number;
  /** What is held for an event that is not complete: its line so far plus its data so far. */
  readonly pendingBytes: number;
}

const BOM = [0xef, 0xbb, 0xbf] as const;
const LF = 0x0a;
const CR = 0x0d;
const COLON = 0x3a;
const SPACE = 0x20;

function broken(message: string): ArtifreeApiError {
  return new ArtifreeApiError(message, {
    status: 200,
    code: 'invalid_response',
  });
}

/** A byte buffer that doubles when it is full, so appends cost O(1) amortized. */
class Bytes {
  private bytes = new Uint8Array(256);
  length = 0;

  append(source: Uint8Array, start: number, end: number): void {
    const added = end - start;
    if (this.length + added > this.bytes.length) {
      const grown = new Uint8Array(
        Math.max(this.bytes.length * 2, this.length + added),
      );
      grown.set(this.bytes.subarray(0, this.length));
      this.bytes = grown;
    }
    this.bytes.set(source.subarray(start, end), this.length);
    this.length += added;
  }

  view(): Uint8Array {
    return this.bytes.subarray(0, this.length);
  }

  clear(): void {
    this.length = 0;
  }
}

/**
 * The events of a `text/event-stream` body as the WHATWG HTML standard
 * specifies them, read byte by byte so that every limit counts UTF-8 bytes
 * and nothing is rescanned: lines end at CRLF, LF, or CR wherever the chunks
 * split them; a leading byte order mark is removed before anything is
 * counted; comments and unknown fields are ignored; one space after the colon
 * is removed; `data` lines join with LF; an event is dispatched at a blank
 * line only if it has data. UTF-8 is decoded strictly, so an invalid or
 * truncated sequence anywhere, in the unfinished line at the end included,
 * rejects with `invalid_response`; an event that the body ended before its
 * blank line is validated but never dispatched.
 */
export async function* serverSentEvents(
  chunks: AsyncIterable<Uint8Array>,
  limits: SseLimits,
): AsyncGenerator<ServerSentEvent> {
  const decoder = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true });
  const decode = (bytes: Uint8Array): string => {
    try {
      return decoder.decode(bytes);
    } catch {
      throw broken('The event stream is not valid UTF-8.');
    }
  };
  const line = new Bytes();
  let bomSeen = 0;
  let bomDone = false;
  let afterCr = false;
  let type = '';
  const data: string[] = [];
  let dataBytes = 0;

  const dispatchable = (): ServerSentEvent | undefined => {
    const event =
      data.length > 0
        ? { type: type === '' ? 'message' : type, data: data.join('\n') }
        : undefined;
    type = '';
    data.length = 0;
    dataBytes = 0;
    return event;
  };

  const finishLine = (): ServerSentEvent | undefined => {
    const bytes = line.view();
    if (bytes.length === 0) {
      line.clear();
      return dispatchable();
    }
    // Every line is validated, whatever field it is.
    decode(bytes);
    if (bytes[0] !== COLON) {
      const colon = bytes.indexOf(COLON);
      const nameEnd = colon === -1 ? bytes.length : colon;
      let valueStart = colon === -1 ? bytes.length : colon + 1;
      if (bytes[valueStart] === SPACE) valueStart += 1;
      const field = decode(bytes.subarray(0, nameEnd));
      if (field === 'event') {
        type = decode(bytes.subarray(valueStart));
      } else if (field === 'data') {
        const valueBytes = bytes.length - valueStart;
        dataBytes += valueBytes + (data.length > 0 ? 1 : 0);
        if (dataBytes > limits.dataBytes)
          throw broken('An event of the stream is too large.');
        data.push(decode(bytes.subarray(valueStart)));
      }
    }
    line.clear();
    return undefined;
  };

  const checkPending = (): void => {
    if (line.length > limits.lineBytes)
      throw broken('A line of the event stream is too long.');
    if (line.length + dataBytes > limits.pendingBytes)
      throw broken('The event stream holds too much of an unfinished event.');
  };

  for await (const chunk of chunks) {
    let start = 0;
    for (let index = 0; index < chunk.length; index += 1) {
      const byte = chunk[index] ?? 0;
      if (!bomDone) {
        // The first three bytes are the byte order mark or data.
        if (byte === BOM[bomSeen]) {
          bomSeen += 1;
          start = index + 1;
          if (bomSeen === BOM.length) {
            bomDone = true;
            bomSeen = 0;
          }
          continue;
        }
        // Not a mark after all: the bytes held back are data.
        if (bomSeen > 0)
          line.append(Uint8Array.from(BOM.slice(0, bomSeen)), 0, bomSeen);
        bomDone = true;
        bomSeen = 0;
      }
      // Stop reading a line the moment it cannot fit, not at its end: nothing
      // of a chunk is copied before this has seen the bytes that would be.
      if (line.length + (index - start) > limits.lineBytes)
        throw broken('A line of the event stream is too long.');
      if (line.length + (index - start) + dataBytes > limits.pendingBytes)
        throw broken('The event stream holds too much of an unfinished event.');
      if (afterCr) {
        afterCr = false;
        if (byte === LF) {
          start = index + 1;
          continue;
        }
      }
      if (byte === LF || byte === CR) {
        line.append(chunk, start, index);
        start = index + 1;
        afterCr = byte === CR;
        checkPending();
        const event = finishLine();
        if (event !== undefined) yield event;
      }
    }
    if (start < chunk.length) line.append(chunk, start, chunk.length);
    // Bytes of a mark still being matched are not counted yet.
    if (!bomDone) start = chunk.length;
    checkPending();
  }
  // The body ended: a mark that never completed is data, an unfinished line
  // is validated, and an event without its blank line is dropped.
  if (!bomDone && bomSeen > 0)
    line.append(Uint8Array.from(BOM.slice(0, bomSeen)), 0, bomSeen);
  if (line.length > 0) {
    checkPending();
    decode(line.view());
  }
}
