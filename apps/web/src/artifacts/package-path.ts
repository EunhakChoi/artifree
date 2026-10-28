import { packageRules } from '@artifree/contracts';

/**
 * Why the server would refuse a package path: the names of the domain's
 * `PathError`. `contracts/fixtures/package-paths.json` holds cases the
 * domain and this module must judge alike (decision 0017).
 */
export type PathError =
  | 'Length'
  | 'Character'
  | 'Depth'
  | 'Segment'
  | 'Dot'
  | 'Reserved'
  | 'Extension';

const EXPLANATIONS: Record<PathError, string> = {
  Length: `A path has 1 to ${packageRules.max_path_bytes} characters.`,
  Character:
    'A path may use only ASCII letters, digits, “-”, “_”, “.”, and “/”.',
  Depth: `A path has at most ${packageRules.max_path_segments} parts.`,
  Segment: `Each part of a path has 1 to ${packageRules.max_segment_bytes} characters.`,
  Dot: 'No part of a path may start or end with “.”.',
  Reserved:
    'No part of a path may be a reserved device name such as “con” or “nul”.',
  Extension: 'Artifree does not serve files with this extension.',
};

/** A sentence that tells the user what `error` means. */
export function explainPathError(error: PathError): string {
  return EXPLANATIONS[error];
}

/** The `Content-Type` a file at `path` is served with, if it has one. */
export function mediaTypeOf(path: string): string | undefined {
  const name = path.slice(path.lastIndexOf('/') + 1);
  const dot = name.lastIndexOf('.');
  if (dot < 0) return undefined;
  const extension = name.slice(dot + 1).toLowerCase();
  // The table is a plain object; inherited names are not extensions.
  return Object.hasOwn(packageRules.media_types, extension)
    ? packageRules.media_types[extension]
    : undefined;
}

const DEVICES = new Set(['con', 'prn', 'aux', 'nul']);

function isReservedDevice(segment: string): boolean {
  const stem = segment.split('.', 1)[0]?.toLowerCase() ?? '';
  return DEVICES.has(stem) || /^(?:com|lpt)[0-9]$/u.test(stem);
}

const encoder = new TextEncoder();

/**
 * Why the server would refuse `path`, checked in the domain's order, or
 * `undefined` when it would accept it.
 */
export function pathError(path: string): PathError | undefined {
  // The length is counted in UTF-8 bytes, before characters are checked.
  const length = encoder.encode(path).length;
  if (length === 0 || length > packageRules.max_path_bytes) return 'Length';
  if (!/^[A-Za-z0-9_./-]+$/u.test(path)) return 'Character';
  const segments = path.split('/');
  if (segments.length > packageRules.max_path_segments) return 'Depth';
  for (const segment of segments) {
    if (segment.length === 0 || segment.length > packageRules.max_segment_bytes)
      return 'Segment';
    if (segment.startsWith('.') || segment.endsWith('.')) return 'Dot';
    if (isReservedDevice(segment)) return 'Reserved';
  }
  return mediaTypeOf(path) === undefined ? 'Extension' : undefined;
}
