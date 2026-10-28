import { packageRules } from '@artifree/contracts';

/**
 * Why the server would refuse a package path. The names are those of the
 * domain's `PathError`; `contracts/fixtures/package-paths.json` holds the
 * cases both sides must judge alike (decision 0017).
 */
export type PathError =
  | 'Length'
  | 'Character'
  | 'Depth'
  | 'Segment'
  | 'Dot'
  | 'Reserved'
  | 'Extension';

const MESSAGES: Record<PathError, string> = {
  Length: `A path has 1–${packageRules.max_path_bytes} characters.`,
  Character:
    'A path may contain only ASCII letters, digits, “-”, “_”, “.”, and “/”.',
  Depth: `A path has at most ${packageRules.max_path_segments} parts.`,
  Segment: `Each part of a path has 1–${packageRules.max_segment_bytes} characters.`,
  Dot: 'No part of a path may start or end with “.”.',
  Reserved:
    'No part of a path may be a reserved device name such as “con” or “nul”.',
  Extension: 'The file extension is not one Artifree serves.',
};

/** What to tell the user about `error`. */
export function describePathError(error: PathError): string {
  return MESSAGES[error];
}

/** The `Content-Type` a file at `path` is served with, if any. */
export function mediaTypeOf(path: string): string | undefined {
  const name = path.slice(path.lastIndexOf('/') + 1);
  const dot = name.lastIndexOf('.');
  if (dot < 0) return undefined;
  const extension = name.slice(dot + 1).toLowerCase();
  return Object.hasOwn(packageRules.media_types, extension)
    ? packageRules.media_types[extension]
    : undefined;
}

function isReservedDevice(segment: string): boolean {
  const dot = segment.indexOf('.');
  const stem = (dot < 0 ? segment : segment.slice(0, dot)).toLowerCase();
  return (
    ['con', 'prn', 'aux', 'nul'].includes(stem) ||
    /^(?:com|lpt)[0-9]$/u.test(stem)
  );
}

/**
 * Why the server would refuse `path` as a package path, checked in the
 * domain's order, or `undefined` when it would accept it.
 */
export function pathError(path: string): PathError | undefined {
  // Paths are ASCII once the character check passes; before it, the length
  // is counted in UTF-8 bytes, as the server counts it.
  const bytes = new TextEncoder().encode(path).length;
  if (bytes === 0 || bytes > packageRules.max_path_bytes) return 'Length';
  if (!/^[A-Za-z0-9_./-]*$/u.test(path)) return 'Character';
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
