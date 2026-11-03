import type { JsonValue } from '@artifree/contracts';
import { ArtifreeApiError } from './errors.js';

// These constants are the server's (decision 0040 and 0031). A test reads the
// Rust source and fails when one of them differs, so the two cannot drift.

/** Most segments in a data path. */
export const MAX_PATH_SEGMENTS = 16;
/** Most bytes in one segment of a data path. */
export const MAX_SEGMENT_BYTES = 64;
/** Most bytes in the name of an object member. */
export const MAX_MEMBER_NAME_BYTES = 256;
/** Members in a page when a query names no limit. */
export const MEMBERS_DEFAULT_LIMIT = 50;
/** Most members in a page. */
export const MEMBERS_MAX_LIMIT = 100;
/** Largest canonical value text an event carries. */
export const MAX_EVENT_VALUE_BYTES = 128 * 1024;
/** Deepest nesting of arrays and objects in a value (the root counts). */
export const MAX_NESTING = 32;
/** The largest integer a double holds exactly (2^53 - 1). */
export const MAX_SAFE = Number.MAX_SAFE_INTEGER;

const encoder = new TextEncoder();

/** The bytes `text` takes in UTF-8. */
export function utf8Length(text: string): number {
  return encoder.encode(text).length;
}

/** Whether `text` has no lone surrogate, so that UTF-8 can carry it. */
export function isWellFormed(text: string): boolean {
  for (let index = 0; index < text.length; index += 1) {
    const unit = text.charCodeAt(index);
    if (unit >= 0xd800 && unit <= 0xdbff) {
      const next = text.charCodeAt(index + 1);
      if (!(next >= 0xdc00 && next <= 0xdfff)) return false;
      index += 1;
    } else if (unit >= 0xdc00 && unit <= 0xdfff) {
      return false;
    }
  }
  return true;
}

/**
 * Orders two well-formed strings by their UTF-8 bytes, which is the order of
 * their code points. (JavaScript's own comparison orders by UTF-16 units and
 * puts U+FF5E after U+10000.)
 */
export function compareUtf8(a: string, b: string): number {
  const left = [...a];
  const right = [...b];
  const shared = Math.min(left.length, right.length);
  for (let index = 0; index < shared; index += 1) {
    const x = left[index]?.codePointAt(0) ?? 0;
    const y = right[index]?.codePointAt(0) ?? 0;
    if (x !== y) return x < y ? -1 : 1;
  }
  return left.length - right.length;
}

/** The rule a data path breaks, or undefined for a valid path. */
export function dataPathProblem(path: string): string | undefined {
  if (path === '') return undefined;
  const segments = path.split('/');
  if (segments.length > MAX_PATH_SEGMENTS)
    return `a path has at most ${MAX_PATH_SEGMENTS} segments`;
  for (const segment of segments) {
    if (segment.length === 0)
      return 'a path has no leading, trailing, or doubled "/"';
    if (segment.length > MAX_SEGMENT_BYTES)
      return `a segment has at most ${MAX_SEGMENT_BYTES} characters`;
    if (!/^[A-Za-z0-9_-]+$/.test(segment))
      return 'a segment has only ASCII letters, digits, "_" and "-"';
  }
  return undefined;
}

/** The rule a member-name cursor breaks, or undefined. */
export function cursorProblem(name: string): string | undefined {
  if (!isWellFormed(name)) return 'a cursor must be well-formed Unicode';
  const bytes = utf8Length(name);
  if (bytes === 0 || bytes > MAX_MEMBER_NAME_BYTES)
    return `a cursor is 1 to ${MAX_MEMBER_NAME_BYTES} bytes of UTF-8`;
  return undefined;
}

/** A request the client refuses to send. */
export function refusal(message: string): ArtifreeApiError {
  return new ArtifreeApiError(message, {
    status: 0,
    code: 'invalid_request',
  });
}

/** A response that breaks the contract. */
export function breach(message: string, status = 200): ArtifreeApiError {
  return new ArtifreeApiError(message, { status, code: 'invalid_response' });
}

/** Whether `value` is a safe, non-negative whole number. */
export function isVersion(value: unknown): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0;
}

/**
 * Checks a version field. A number above 2^53 - 1 has already lost precision
 * in the JSON parse, so it is refused by name rather than compared.
 */
export function versionField(value: unknown, field: string): number {
  if (isVersion(value)) return value;
  if (typeof value === 'number' && Number.isFinite(value) && value >= 0)
    throw breach(
      `${field} is above 2^53 - 1 or not a whole number: it cannot be compared exactly.`,
    );
  throw breach(`${field} is not a non-negative whole number.`);
}

/**
 * Checks that `value` is data the server can have stored: JSON with finite
 * numbers of magnitude at most 2^53 - 1, within the nesting limit. Stops
 * descending at the limit, so the recursion is bounded.
 */
export function checkValue(
  value: unknown,
  field: string,
  depth = 1,
): asserts value is JsonValue {
  if (value === null || typeof value === 'string' || typeof value === 'boolean')
    return;
  if (typeof value === 'number') {
    if (!Number.isFinite(value) || Math.abs(value) > MAX_SAFE)
      throw breach(`${field} holds a number beyond 2^53 - 1 or not finite.`);
    return;
  }
  if (typeof value !== 'object')
    throw breach(`${field} holds a value that is not JSON.`);
  if (depth > MAX_NESTING)
    throw breach(`${field} is nested deeper than ${MAX_NESTING} levels.`);
  const items: unknown[] = Array.isArray(value) ? value : Object.values(value);
  for (const item of items) checkValue(item, field, depth + 1);
}

/** The own keys of `record`, which must be exactly `required` and `optional`. */
export function exactKeys(
  record: Record<string, unknown>,
  required: readonly string[],
  optional: readonly string[],
  what: string,
): void {
  for (const key of required)
    if (!Object.hasOwn(record, key)) throw breach(`${what} lacks ${key}.`);
  for (const key of Object.keys(record))
    if (!required.includes(key) && !optional.includes(key))
      throw breach(`${what} has the unknown field ${key}.`);
}

/** A plain JSON object (not an array, not null). */
export function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}
