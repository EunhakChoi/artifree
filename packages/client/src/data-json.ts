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
  /** Values in a data space below its root: arrays, objects, and scalars. */
  maxSpaceValues: 10_000,
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
 * The most values (counting the value itself and everything in it) that a
 * value at a path with `depth` segments may hold. The space counts every
 * value below its root, the containers along the path included, so a value
 * at a deeper path has room for fewer; at the root the root object is not
 * counted.
 */
export function nodesAllowedAt(depth: number): number {
  return DATA_LIMITS.maxSpaceValues + 1 - depth;
}

const SURROGATE = /[\ud800-\udfff]/;

/**
 * Whether `text` is valid Unicode: a surrogate code unit may appear only as
 * half of a pair (the domain refuses `"\ud800"` as it refuses any other text
 * that is not Unicode). Nothing else about text is restricted.
 */
export function isWellFormedText(text: string): boolean {
  if (!SURROGATE.test(text)) return true;
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

/** Whether `name` is a member name the server could have stored. */
export function isMemberName(name: string): boolean {
  // A UTF-16 unit takes at least one and at most three bytes in UTF-8.
  if (name.length === 0 || name.length > DATA_LIMITS.maxMemberNameBytes)
    return false;
  if (!isWellFormedText(name)) return false;
  if (name.length * 3 <= DATA_LIMITS.maxMemberNameBytes) return true;
  return utf8Length(name) <= DATA_LIMITS.maxMemberNameBytes;
}

/**
 * How many values `value` holds, counting itself, if it is JSON the server
 * could have stored; `undefined` if it is not, or if it holds more than
 * `maxNodes`. Stored means: plain objects and arrays, strings of valid
 * Unicode, booleans, `null`, and finite numbers of magnitude at most
 * 2^53 - 1, at every depth; containers nested at most `maxNesting` deep (a
 * scalar is 0, a container is 1 more than its deepest child); every member
 * name, at every depth, of valid Unicode and 1 to 256 UTF-8 bytes. A
 * `__proto__` key is an ordinary own member here: nothing is assigned. The
 * walk is iterative, so a hostile depth cannot exhaust the stack, and it stops
 * as soon as a bound is passed. Repeated member names cannot be seen in a
 * parsed value; {@link hasRepeatedMember} finds them in its text.
 */
export function storedJsonNodes(
  value: unknown,
  maxNesting: number,
  maxNodes: number,
): number | undefined {
  const stack: Array<{ node: unknown; level: number }> = [
    { node: value, level: 0 },
  ];
  let nodes = 0;
  for (let item = stack.pop(); item !== undefined; item = stack.pop()) {
    const { node, level } = item;
    nodes += 1;
    if (nodes > maxNodes) return undefined;
    if (node === null || typeof node === 'boolean') continue;
    if (typeof node === 'string') {
      if (!isWellFormedText(node)) return undefined;
      continue;
    }
    if (typeof node === 'number') {
      // One comparison refuses NaN, both infinities, and anything beyond
      // 2^53 - 1.
      if (!(Math.abs(node) <= Number.MAX_SAFE_INTEGER)) return undefined;
      continue;
    }
    if (typeof node !== 'object') return undefined;
    const depth = level + 1;
    if (depth > maxNesting) return undefined;
    if (Array.isArray(node)) {
      for (const child of node as unknown[])
        stack.push({ node: child, level: depth });
      continue;
    }
    const prototype: unknown = Object.getPrototypeOf(node);
    if (prototype !== Object.prototype && prototype !== null) return undefined;
    for (const [name, child] of Object.entries(node)) {
      if (!isMemberName(name)) return undefined;
      stack.push({ node: child, level: depth });
    }
  }
  return nodes;
}

/** Whether `value` is {@link storedJsonNodes}-valid within the two bounds. */
export function isStoredJson(
  value: unknown,
  maxNesting: number,
  maxNodes: number,
): value is JsonValue {
  return storedJsonNodes(value, maxNesting, maxNodes) !== undefined;
}

/**
 * Whether the JSON `text` has an object with a member name twice (names
 * compared after unescaping, so `"a"` and `"\u0061"` are the same), which the
 * domain refuses to store and `JSON.parse` silently collapses. `text` must
 * already have parsed as JSON. Iterative, one pass.
 */
export function hasRepeatedMember(text: string): boolean {
  // One entry per open container: the names an object has so far, or
  // `undefined` for an array; `key` is whether the next string is a name.
  const open: Array<{ names: Set<string> | undefined; key: boolean }> = [];
  let index = 0;
  while (index < text.length) {
    const char = text[index];
    if (char === '"') {
      let end = index + 1;
      while (text[end] !== '"') end += text[end] === '\\' ? 2 : 1;
      const top = open.at(-1);
      if (top?.names !== undefined && top.key) {
        const token = text.slice(index, end + 1);
        const name: unknown = token.includes('\\')
          ? JSON.parse(token)
          : token.slice(1, -1);
        if (typeof name !== 'string' || top.names.has(name)) return true;
        top.names.add(name);
        top.key = false;
      }
      index = end + 1;
      continue;
    }
    if (char === '{') open.push({ names: new Set(), key: true });
    else if (char === '[') open.push({ names: undefined, key: false });
    else if (char === '}' || char === ']') open.pop();
    else if (char === ',') {
      const top = open.at(-1);
      if (top?.names !== undefined) top.key = true;
    }
    index += 1;
  }
  return false;
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
 * is imposed. `value` must pass {@link storedJsonNodes}. Iterative, like it.
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
