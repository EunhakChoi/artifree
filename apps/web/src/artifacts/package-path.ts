import { scaffoldDescription } from '@artifree/contracts';

/** The package rules of the server build this page was built with. */
export const rules = scaffoldDescription.package_rules;

/**
 * Why the server would refuse a path, named as the domain's `PathError`.
 * `contracts/fixtures/package-paths.json` lists paths that the domain and
 * `pathError` must judge alike (decision 0017).
 */
export type PathError =
  | 'Length'
  | 'Character'
  | 'Depth'
  | 'Segment'
  | 'Dot'
  | 'Reserved'
  | 'Extension';

const encoder = new TextEncoder();

/**
 * Why the server would refuse `path`, checked in the domain's order, or
 * `undefined` if it would accept it. The server checks every request
 * regardless.
 */
export function pathError(path: string): PathError | undefined {
  const bytes = encoder.encode(path).length;
  if (bytes === 0 || bytes > rules.max_path_bytes) return 'Length';
  if (!/^[A-Za-z0-9._/-]+$/u.test(path)) return 'Character';
  const segments = path.split('/');
  if (segments.length > rules.max_path_segments) return 'Depth';
  for (const segment of segments) {
    // Only ASCII is left, so a segment's length is its size in bytes.
    if (segment.length === 0 || segment.length > rules.max_segment_bytes)
      return 'Segment';
    if (segment.startsWith('.') || segment.endsWith('.')) return 'Dot';
    if (isReservedDevice(segment)) return 'Reserved';
  }
  return mediaTypeOf(path) === undefined ? 'Extension' : undefined;
}

function isReservedDevice(segment: string): boolean {
  const stem = (segment.split('.', 1)[0] ?? '').toLowerCase();
  return /^(?:con|prn|aux|nul|com[0-9]|lpt[0-9])$/u.test(stem);
}

/** The `Content-Type` the server serves `path` with, if it accepts it. */
export function mediaTypeOf(path: string): string | undefined {
  const name = path.slice(path.lastIndexOf('/') + 1);
  const dot = name.lastIndexOf('.');
  if (dot < 0) return undefined;
  const extension = name.slice(dot + 1).toLowerCase();
  // A plain object: names it inherits are not extensions.
  return Object.hasOwn(rules.media_types, extension)
    ? rules.media_types[extension]
    : undefined;
}

/** A sentence that tells the user what `error` asks of a path. */
export function explainPathError(error: PathError): string {
  switch (error) {
    case 'Length':
      return `A path has 1 to ${rules.max_path_bytes} bytes.`;
    case 'Character':
      return 'A path uses only ASCII letters, digits, “-”, “_”, “.”, and “/”.';
    case 'Depth':
      return `A path has at most ${rules.max_path_segments} parts.`;
    case 'Segment':
      return `Each part of a path has 1 to ${rules.max_segment_bytes} characters.`;
    case 'Dot':
      return 'No part of a path starts or ends with “.”.';
    case 'Reserved':
      return 'No part of a path is a reserved device name such as “con” or “nul”.';
    case 'Extension':
      return 'Artifree does not serve files with this extension.';
  }
}
