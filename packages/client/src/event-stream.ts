import { ArtifreeApiError } from './errors.js';

/**
 * A byte-level reader of `text/event-stream` bodies for the data stream
 * (decision 0040), written against the WHATWG HTML standard with these
 * differences, all of them refusals:
 *
 * - every size is counted in UTF-8 bytes of the stream, after a leading byte
 *   order mark was removed, never in string length;
 * - text is decoded line by line with a fatal decoder, so a byte sequence that
 *   is not UTF-8 is `invalid_response` wherever it is, also in the last line
 *   when the stream ends in the middle of it (which is never dispatched);
 * - a line, or the data of one event, over the limits is `invalid_response`.
 *
 * A byte order mark is recognised on the bytes, however the chunks split it.
 * Lines end in LF, CRLF, or CR. `id` and `retry` are ignored.
 */

/** Most bytes of the data of one event, with its line ends between lines. */
export const MAX_EVENT_BYTES = 144 * 1024;
/** Most bytes of one line, its field name and colon included. */
export const MAX_LINE_BYTES = MAX_EVENT_BYTES + 64;

const LF = 0x0a;
const CR = 0x0d;
const COLON = 0x3a;
const SPACE = 0x20;
const BOM = [0xef, 0xbb, 0xbf] as const;

/** One dispatched event: its type (`message` when it names none) and data. */
export interface ServerSentEvent {
  readonly type: string;
  readonly data: string;
}

function invalid(message: string): ArtifreeApiError {
  return new ArtifreeApiError(message, {
    status: 200,
    code: 'invalid_response',
  });
}

/** Reads one stream; feed it chunks and finish it at the end of the body. */
export class EventStreamParser {
  private line = new Uint8Array(256);
  private lineLength = 0;
  /** Bytes of the byte order mark matched so far; 3 once it is decided. */
  private bomMatched = 0;
  private bomDecided = false;
  private skipLineFeed = false;
  private eventType = '';
  private dataLines: string[] = [];
  private dataBytes = 0;
  private readonly decoder = new TextDecoder('utf-8', {
    fatal: true,
    ignoreBOM: true,
  });

  /** The events a chunk completes, in order. */
  push(chunk: Uint8Array): ServerSentEvent[] {
    const events: ServerSentEvent[] = [];
    for (const byte of chunk) this.accept(byte, events);
    return events;
  }

  /**
   * The body ended. A last line the stream broke off is checked and then
   * dropped, like the event it belongs to.
   */
  finish(): void {
    const events: ServerSentEvent[] = [];
    // A stream that ends inside what might have been a mark has data after
    // all.
    if (!this.bomDecided) {
      this.bomDecided = true;
      for (const byte of BOM.slice(0, this.bomMatched)) this.feed(byte, events);
    }
    if (this.lineLength > 0)
      this.decode(this.line.subarray(0, this.lineLength));
  }

  private accept(byte: number, events: ServerSentEvent[]): void {
    if (!this.bomDecided) {
      if (byte === BOM[this.bomMatched]) {
        this.bomMatched += 1;
        if (this.bomMatched === BOM.length) this.bomDecided = true;
        return;
      }
      // Not a mark: the bytes held back are the start of the first line.
      this.bomDecided = true;
      for (const held of BOM.slice(0, this.bomMatched)) this.feed(held, events);
    }
    this.feed(byte, events);
  }

  private feed(byte: number, events: ServerSentEvent[]): void {
    if (this.skipLineFeed) {
      this.skipLineFeed = false;
      if (byte === LF) return;
    }
    if (byte === LF || byte === CR) {
      this.skipLineFeed = byte === CR;
      this.endLine(events);
      return;
    }
    if (this.lineLength === MAX_LINE_BYTES)
      throw invalid(
        `The data stream sent a line over ${MAX_LINE_BYTES} bytes.`,
      );
    if (this.lineLength === this.line.length) {
      const grown = new Uint8Array(
        Math.min(MAX_LINE_BYTES, this.line.length * 2),
      );
      grown.set(this.line);
      this.line = grown;
    }
    this.line[this.lineLength] = byte;
    this.lineLength += 1;
  }

  private decode(bytes: Uint8Array): string {
    try {
      return this.decoder.decode(bytes);
    } catch {
      throw invalid('The data stream sent bytes that are not UTF-8.');
    }
  }

  private endLine(events: ServerSentEvent[]): void {
    const bytes = this.line.subarray(0, this.lineLength);
    this.lineLength = 0;
    // Every line is checked, comments and unknown fields included.
    const text = this.decode(bytes);
    if (bytes.length === 0) {
      if (this.dataLines.length > 0)
        events.push({
          type: this.eventType === '' ? 'message' : this.eventType,
          data: this.dataLines.join('\n'),
        });
      this.eventType = '';
      this.dataLines = [];
      this.dataBytes = 0;
      return;
    }
    if (bytes[0] === COLON) return;
    // A colon is one byte of UTF-8 and never part of another character, so its
    // place in the bytes and in the text are the same line to split.
    const colon = bytes.indexOf(COLON);
    const field = colon === -1 ? text : text.slice(0, text.indexOf(':'));
    const spaced = colon !== -1 && bytes[colon + 1] === SPACE;
    const value =
      colon === -1 ? '' : text.slice(text.indexOf(':') + 1).replace(/^ /, '');
    if (field === 'event') {
      this.eventType = value;
    } else if (field === 'data') {
      const valueBytes =
        colon === -1 ? 0 : bytes.length - colon - 1 - (spaced ? 1 : 0);
      this.dataBytes += valueBytes + (this.dataLines.length > 0 ? 1 : 0);
      if (this.dataBytes > MAX_EVENT_BYTES)
        throw invalid(
          `The data stream sent an event over ${MAX_EVENT_BYTES} bytes.`,
        );
      this.dataLines.push(value);
    }
  }
}
