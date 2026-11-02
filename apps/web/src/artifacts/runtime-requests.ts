import type { RuntimeErrorCode } from '@artifree/runtime-sdk';

/**
 * The workspace's side of the frame protocol (decision 0032). Everything a
 * frame sends is hostile input: it is checked here before anything is asked
 * of the server, and the server checks it again. This file has no DOM, React
 * or network access so that its rules can be tested alone.
 */

const MAX_CHANGES = 512;
const MAX_PATH_LENGTH = 240;
/** The package limits of the domain (decision 0003). */
const MAX_FILE_BYTES = 5 * 1024 * 1024;
const MAX_PACKAGE_BYTES = 25 * 1024 * 1024;
const KEY_PATTERN = /^[A-Za-z0-9._:-]{8,100}$/;
const PATH_PATTERN = /^[\x21-\x7e]+$/;

/** One change of a save, with its content as bytes. */
export type FrameChange =
  | { readonly op: 'put'; readonly path: string; readonly bytes: Uint8Array }
  | { readonly op: 'delete'; readonly path: string };

/** A request a frame sent, after validation. */
export type FrameRequest =
  | { readonly kind: 'metadata'; readonly id: number }
  | { readonly kind: 'source.list'; readonly id: number }
  | {
      readonly kind: 'source.read';
      readonly id: number;
      readonly path: string;
      readonly offset?: number;
      readonly length?: number;
    }
  | {
      readonly kind: 'source.write';
      readonly id: number;
      readonly key: string;
      readonly changes: readonly FrameChange[];
    }
  | {
      readonly kind: 'download';
      readonly id: number;
      readonly path?: string;
    };

/** The outcome of reading one message. */
export type ParsedFrameMessage =
  | { readonly ok: true; readonly request: FrameRequest }
  /** A message with an id that is refused: answer it with this code. */
  | {
      readonly ok: false;
      readonly id: number;
      readonly code: RuntimeErrorCode;
    };

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function count(value: unknown): value is number {
  return Number.isSafeInteger(value) && typeof value === 'number' && value >= 0;
}

function path(value: unknown): value is string {
  return (
    typeof value === 'string' &&
    value.length > 0 &&
    value.length <= MAX_PATH_LENGTH &&
    PATH_PATTERN.test(value)
  );
}

/** Whether `value` has exactly these own enumerable members, no others. */
function onlyMembers(
  value: Record<string, unknown>,
  allowed: readonly string[],
): boolean {
  return Object.keys(value).every((name) => allowed.includes(name));
}

/** A text's UTF-8 bytes, or undefined when it holds a lone surrogate. */
function textBytes(text: string): Uint8Array | undefined {
  const bytes = new TextEncoder().encode(text);
  // A lone surrogate encodes as U+FFFD and would be stored silently changed.
  // A leading U+FEFF is a character of the text, not a marker to drop, so the
  // round trip keeps it.
  return new TextDecoder('utf-8', { ignoreBOM: true }).decode(bytes) === text
    ? bytes
    : undefined;
}

function changes(
  value: unknown,
): readonly FrameChange[] | 'invalid' | 'too_large' {
  if (!Array.isArray(value) || value.length === 0) return 'invalid';
  if (value.length > MAX_CHANGES) return 'too_large';
  const parsed: FrameChange[] = [];
  const seen = new Set<string>();
  let total = 0;
  for (const entry of value as unknown[]) {
    if (!record(entry) || !path(entry.path) || seen.has(entry.path))
      return 'invalid';
    seen.add(entry.path);
    if (entry.op === 'delete') {
      if (!onlyMembers(entry, ['op', 'path'])) return 'invalid';
      parsed.push({ op: 'delete', path: entry.path });
      continue;
    }
    if (entry.op !== 'put' || !onlyMembers(entry, ['op', 'path', 'content']))
      return 'invalid';
    const content: unknown = entry.content;
    const bytes =
      typeof content === 'string'
        ? textBytes(content)
        : content instanceof Uint8Array
          ? content
          : undefined;
    if (bytes === undefined) return 'invalid';
    total += bytes.length;
    if (bytes.length > MAX_FILE_BYTES || total > MAX_PACKAGE_BYTES)
      return 'too_large';
    // Copy: the frame's own buffer must not change under the request.
    parsed.push({ op: 'put', path: entry.path, bytes: bytes.slice() });
  }
  return parsed;
}

/** The id of a message, or undefined when nothing could answer it. */
export function frameMessageId(message: unknown): number | undefined {
  if (!record(message)) return undefined;
  const { id } = message;
  return Number.isSafeInteger(id) && typeof id === 'number' && id >= 1
    ? id
    : undefined;
}

/**
 * How much one frame may ask of the workspace (decision 0032): 20 requests at
 * once, then 10 a second, and 8 at a time. The server has its own, smaller
 * budget for all frames of the user; this one keeps a single frame from
 * spending it, and is checked before a message is read at all.
 */
export const FRAME_BURST = 20;
export const FRAME_REFILL_MS = 100;
export const FRAME_MAX_PENDING = 8;

/** A token bucket on a clock the caller supplies. */
export class RequestBudget {
  readonly #burst: number;
  readonly #refillMs: number;
  #tokens: number;
  #at: number;

  constructor(burst: number, refillMs: number, now: number) {
    this.#burst = burst;
    this.#refillMs = refillMs;
    this.#tokens = burst;
    this.#at = now;
  }

  /** Spends one request, or says there is none left now. */
  take(now: number): boolean {
    const elapsed = Math.max(0, now - this.#at);
    this.#at = Math.max(this.#at, now);
    this.#tokens = Math.min(
      this.#burst,
      this.#tokens + elapsed / this.#refillMs,
    );
    if (this.#tokens < 1) return false;
    this.#tokens -= 1;
    return true;
  }
}

/**
 * Reads one message from a frame's port. A message that is not a request at
 * all, or has no usable id, returns undefined and is dropped silently, since
 * nothing can answer it. Any other defect is refused with a code that quotes
 * nothing the frame sent.
 */
export function parseFrameMessage(
  message: unknown,
): ParsedFrameMessage | undefined {
  if (!record(message)) return undefined;
  const { id, type } = message;
  if (!Number.isSafeInteger(id) || typeof id !== 'number' || id < 1)
    return undefined;
  const refuse = (code: RuntimeErrorCode): ParsedFrameMessage => ({
    ok: false,
    id,
    code,
  });
  switch (type) {
    case 'metadata':
    case 'source.list':
      return onlyMembers(message, ['type', 'id'])
        ? { ok: true, request: { kind: type, id } }
        : refuse('invalid_request');
    case 'source.read': {
      const { offset, length } = message;
      if (
        !onlyMembers(message, ['type', 'id', 'path', 'offset', 'length']) ||
        !path(message.path) ||
        (offset !== undefined && !count(offset)) ||
        (length !== undefined && !(count(length) && length >= 1))
      )
        return refuse('invalid_request');
      return {
        ok: true,
        request: {
          kind: 'source.read',
          id,
          path: message.path,
          ...(offset === undefined ? {} : { offset }),
          ...(length === undefined ? {} : { length }),
        },
      };
    }
    case 'source.write': {
      if (
        !onlyMembers(message, ['type', 'id', 'key', 'changes']) ||
        typeof message.key !== 'string' ||
        !KEY_PATTERN.test(message.key)
      )
        return refuse('invalid_request');
      const parsed = changes(message.changes);
      if (typeof parsed === 'string')
        return refuse(parsed === 'invalid' ? 'invalid_request' : 'too_large');
      return {
        ok: true,
        request: {
          kind: 'source.write',
          id,
          key: message.key,
          changes: parsed,
        },
      };
    }
    case 'download':
      if (
        !onlyMembers(message, ['type', 'id', 'path']) ||
        (message.path !== undefined && !path(message.path))
      )
        return refuse('invalid_request');
      return {
        ok: true,
        request: {
          kind: 'download',
          id,
          ...(message.path === undefined ? {} : { path: message.path }),
        },
      };
    default:
      return refuse('invalid_request');
  }
}

/** What the workspace does with one raw message of a frame. */
export type Admission =
  /** Nothing can answer it (no usable id): drop it. */
  | { readonly action: 'ignore' }
  /** Answer with this code and do nothing else. */
  | {
      readonly action: 'answer';
      readonly id: number;
      readonly code: RuntimeErrorCode;
    }
  /** A valid request within budget: carry it out. */
  | { readonly action: 'run'; readonly request: FrameRequest };

/**
 * Decides, for one frame, whether a message is read, answered with a refusal,
 * or carried out (decision 0032). The budget is spent first, before the
 * message is parsed, so a flood of large or malformed messages costs the
 * workspace one token check each and never reaches the server. Then the
 * message is parsed, and the caps on requests in flight apply.
 */
export class FrameAdmission {
  readonly #budget: RequestBudget;

  constructor(now: number) {
    this.#budget = new RequestBudget(FRAME_BURST, FRAME_REFILL_MS, now);
  }

  admit(
    message: unknown,
    now: number,
    inFlight: number,
    saving: boolean,
  ): Admission {
    const id = frameMessageId(message);
    if (id === undefined) return { action: 'ignore' };
    if (!this.#budget.take(now))
      return { action: 'answer', id, code: 'rate_limited' };
    const parsed = parseFrameMessage(message);
    if (parsed === undefined) return { action: 'ignore' };
    if (!parsed.ok)
      return { action: 'answer', id: parsed.id, code: parsed.code };
    const { request } = parsed;
    if (
      inFlight >= FRAME_MAX_PENDING ||
      (request.kind === 'source.write' && saving)
    )
      return { action: 'answer', id: request.id, code: 'busy' };
    return { action: 'run', request };
  }
}

/**
 * The code a frame is told for a failed server request. Only the closed set
 * of the SDK crosses to the frame, never the server's message.
 */
export function frameErrorCode(code: string): RuntimeErrorCode {
  switch (code) {
    case 'forbidden':
      return 'not_granted';
    case 'revision_conflict':
      return 'revision_conflict';
    case 'request_key_reused':
      return 'key_reused';
    case 'payload_too_large':
      return 'too_large';
    case 'range_not_satisfiable':
      return 'range';
    case 'rate_limited':
      return 'rate_limited';
    case 'runtime_session_expired':
    case 'stale_view':
      return 'view_unavailable';
    case 'not_found':
      return 'not_found';
    case 'invalid_request':
      return 'invalid_request';
    case 'network_error':
    case 'unavailable':
    case 'internal':
    case 'invalid_response':
      return 'unavailable';
    default:
      return 'refused';
  }
}

/** The workspace itself, not the server, refused a request of a frame. */
export class HostRefusal extends Error {
  readonly code: RuntimeErrorCode;

  constructor(code: RuntimeErrorCode) {
    super(`The workspace refused a runtime request (${code}).`);
    this.name = 'HostRefusal';
    this.code = code;
  }
}

/** The bytes of padded base64 text, as the server writes a file part. */
export function base64Bytes(content: string): Uint8Array {
  const binary = atob(content);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1)
    bytes[index] = binary.charCodeAt(index);
  return bytes;
}
