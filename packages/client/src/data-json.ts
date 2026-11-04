import type { JsonValue } from '@artifree/contracts';

/**
 * The grammar and bounds of application data that the client checks before
 * sending and after receiving (decisions 0031 and 0040). A test compares each
 * with the Rust constant it mirrors.
 */
export const DATA_LIMITS = {
  /** Segments in a path. */
  maxPathDepth: 16,
  /** Bytes in one path segment. */
  maxSegmentBytes: 64,
  /** Levels of nesting in a data space; the root object is level 1. */
  maxNesting: 32,
  /** Bytes in a member name. */
  maxMemberNameBytes: 256,
  /** Most members one page may be asked for. */
  maxMembersLimit: 100,
  /** Members per page when no limit is given. */
  defaultMembersLimit: 50,
  /** Canonical value text of one member in a page, in bytes. */
  memberMaxBytes: 32 * 1024,
  /** Names and values in one page, in bytes. */
  pageMaxBytes: 256 * 1024,
  /** Value text a subscription sends, in bytes. */
  eventMaxBytes: 128 * 1024,
} as const;

const SEGMENT = /^[A-Za-z0-9_-]+$/;

/** Whether `path` follows the data path grammar (decision 0031). */
export function isDataPath(path: string): boolean {
  if (path === '') return true;
  const segments = path.split('/');
  return (
    segments.length <= DATA_LIMITS.maxPathDepth &&
    segments.every(
      (segment) =>
        segment.length >= 1 &&
        segment.length <= DATA_LIMITS.maxSegmentBytes &&
        SEGMENT.test(segment),
    )
  );
}

/** The number of segments in a valid path. */
export function pathDepth(path: string): number {
  return path === '' ? 0 : path.split('/').length;
}

/**
 * The deepest nesting a value may have at a path with `depth` segments: the
 * space's levels are the segments plus the value's own containers.
 */
export function nestingAllowedAt(depth: number): number {
  return DATA_LIMITS.maxNesting - depth;
}

/** Whether `name` is a member name the server could have stored. */
function isMemberName(name: string): boolean {
  // A UTF-16 unit takes at least one and at most three bytes in UTF-8.
  if (name.length === 0 || name.length > DATA_LIMITS.maxMemberNameBytes)
    return false;
  if (name.length * 3 <= DATA_LIMITS.maxMemberNameBytes) return true;
  return utf8Length(name) <= DATA_LIMITS.maxMemberNameBytes;
}

/**
 * Whether `value` is JSON the server could have stored: plain objects and
 * arrays, strings, booleans, `null`, and finite numbers of magnitude at most
 * 2^53 - 1, at every depth, with containers nested at most `maxNesting`
 * deep (a scalar is 0, a container is 1 more than its deepest child), and
 * with every member name, at every depth, of 1 to 256 UTF-8 bytes (the bound
 * the domain enforces). A `__proto__` key is an ordinary own member here:
 * nothing is assigned. Checked without recursion, so a hostile depth cannot
 * exhaust the stack.
 */
export function isStoredJson(
  value: unknown,
  maxNesting: number,
): value is JsonValue {
  const stack: Array<{ node: unknown; level: number }> = [
    { node: value, level: 0 },
  ];
  for (let item = stack.pop(); item !== undefined; item = stack.pop()) {
    const { node, level } = item;
    if (node === null || typeof node === 'string' || typeof node === 'boolean')
      continue;
    if (typeof node === 'number') {
      if (!Number.isFinite(node) || Math.abs(node) > Number.MAX_SAFE_INTEGER)
        return false;
      continue;
    }
    if (typeof node !== 'object') return false;
    const depth = level + 1;
    if (depth > maxNesting) return false;
    if (Array.isArray(node)) {
      for (const child of node as unknown[])
        stack.push({ node: child, level: depth });
      continue;
    }
    const prototype: unknown = Object.getPrototypeOf(node);
    if (prototype !== Object.prototype && prototype !== null) return false;
    for (const [name, child] of Object.entries(node)) {
      if (!isMemberName(name)) return false;
      stack.push({ node: child, level: depth });
    }
  }
  return true;
}

/** The bytes a string takes in canonical JSON text, quotes included. */
function stringBytes(text: string): number {
  let bytes = 2;
  for (let index = 0; index < text.length; index += 1) {
    const unit = text.charCodeAt(index);
    if (unit < 0x20) {
      // \b \f \n \r \t are two bytes; the other controls are \u00xx.
      bytes +=
        unit === 8 || unit === 9 || unit === 10 || unit === 12 || unit === 13
          ? 2
          : 6;
    } else if (unit === 0x22 || unit === 0x5c) {
      bytes += 2;
    } else if (unit < 0x80) {
      bytes += 1;
    } else if (unit < 0x800) {
      bytes += 2;
    } else if (unit >= 0xd800 && unit <= 0xdbff) {
      const next = text.charCodeAt(index + 1);
      if (next >= 0xdc00 && next <= 0xdfff) {
        bytes += 4;
        index += 1;
      } else {
        // A lone surrogate: UTF-8 encoders write U+FFFD.
        bytes += 3;
      }
    } else {
      bytes += 3;
    }
  }
  return bytes;
}

/**
 * The bytes a number takes in canonical JSON text. A whole number is its
 * digits. Any other is the shortest text that reads back as the same
 * double, which the server writes the way Rust prints a double: plain digits
 * down to 1e-4, then the exponent form (`1.5e-7`, `1e-5`), where JavaScript
 * keeps plain digits down to 1e-6. A test pins this against the Rust side.
 */
function numberBytes(value: number): number {
  if (Number.isInteger(value) || Math.abs(value) >= 1e-4)
    return String(value).length;
  const [mantissa = '', exponent = ''] = value.toExponential().split('e');
  return mantissa.length + 1 + exponent.length;
}

/**
 * The length in UTF-8 bytes of the canonical JSON text of `value`, the
 * measure every server bound uses (decision 0040): no whitespace, strings
 * escaped only where JSON requires it (`"`, `\\`, and controls; everything
 * else, `/` and non-ASCII included, verbatim), numbers as
 * {@link numberBytes} says. Member order does not change the length, so none
 * is imposed. `value` must pass {@link isStoredJson}. Iterative, like it.
 */
export function canonicalBytes(value: JsonValue): number {
  let total = 0;
  const stack: unknown[] = [value];
  for (let node = stack.pop(); node !== undefined; node = stack.pop()) {
    if (node === null) total += 4;
    else if (typeof node === 'boolean') total += node ? 4 : 5;
    else if (typeof node === 'number') total += numberBytes(node);
    else if (typeof node === 'string') total += stringBytes(node);
    else if (Array.isArray(node)) {
      const items = node as unknown[];
      total += 2 + Math.max(0, items.length - 1);
      for (const item of items) stack.push(item);
    } else if (typeof node === 'object') {
      const members = Object.entries(node);
      total += 2 + Math.max(0, members.length - 1);
      for (const [name, child] of members) {
        total += stringBytes(name) + 1;
        stack.push(child);
      }
    }
  }
  return total;
}

/** UTF-8 byte order comparison of two strings: negative, zero, or positive. */
export function compareUtf8(left: string, right: string): number {
  const encoder = new TextEncoder();
  const a = encoder.encode(left);
  const b = encoder.encode(right);
  const common = Math.min(a.length, b.length);
  for (let index = 0; index < common; index += 1) {
    const difference = (a[index] ?? 0) - (b[index] ?? 0);
    if (difference !== 0) return difference;
  }
  return a.length - b.length;
}

/** The length of `text` in UTF-8 bytes. */
export function utf8Length(text: string): number {
  return new TextEncoder().encode(text).length;
}
