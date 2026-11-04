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

/**
 * Whether `value` is JSON the server could have stored: plain objects and
 * arrays, strings, booleans, `null`, and finite numbers of magnitude at most
 * 2^53 - 1, at every depth, with containers nested at most `maxNesting`
 * deep (a scalar is 0, a container is 1 more than its deepest child). A
 * `__proto__` key is an ordinary own member here: nothing is assigned.
 * Checked without recursion, so a hostile depth cannot exhaust the stack.
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
    for (const child of Object.values(node))
      stack.push({ node: child, level: depth });
  }
  return true;
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
