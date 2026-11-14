/**
 * The package path rules, for the frame script (decisions 0017 and 0032). A
 * path the server's domain would refuse is refused here before anything is
 * sent, so a bad path fails at once and holds no waiting slot. The frame
 * script is a self-contained bundle, so the rules are written out here, not
 * imported from the generated contracts, whose inventory would be bundled
 * whole. `contracts/fixtures/package-paths.json` lists paths that the domain,
 * the workspace and this function must judge alike, and a test compares these
 * constants with the build inventory, so they cannot drift quietly. The server
 * checks every request regardless.
 */

const MAX_PATH_BYTES = 240;
const MAX_PATH_SEGMENTS = 8;
const MAX_SEGMENT_BYTES = 100;

/** The extensions the server serves; any case. */
const EXTENSION_LIST: readonly string[] = [
  'avif',
  'css',
  'csv',
  'gif',
  'htm',
  'html',
  'ico',
  'jpeg',
  'jpg',
  'js',
  'json',
  'jsx',
  'map',
  'md',
  'mjs',
  'otf',
  'png',
  'svg',
  'ts',
  'tsx',
  'ttf',
  'txt',
  'wasm',
  'webp',
  'woff',
  'woff2',
  'xml',
];
const EXTENSIONS: ReadonlySet<string> = new Set(EXTENSION_LIST);

/** The rules, for the test that compares them with the build inventory. */
export const FRAME_PATH_RULES = {
  maxPathBytes: MAX_PATH_BYTES,
  maxPathSegments: MAX_PATH_SEGMENTS,
  maxSegmentBytes: MAX_SEGMENT_BYTES,
  extensions: EXTENSION_LIST,
};

/** Whether a name is a reserved device name such as `con` or `com1`. */
function isReservedDevice(segment: string): boolean {
  const stem = (segment.split('.', 1)[0] ?? '').toLowerCase();
  return /^(?:con|prn|aux|nul|com[0-9]|lpt[0-9])$/u.test(stem);
}

/**
 * Whether `path` is a package path the server would accept: 1 to 240 bytes of
 * ASCII letters, digits, `-`, `_`, `.` and `/`, at most 8 parts of 1 to 100
 * characters, no part starting or ending with a dot or naming a reserved
 * device, and an extension the server serves.
 */
export function isPackagePath(path: string): boolean {
  if (path.length === 0 || path.length > MAX_PATH_BYTES) return false;
  // ASCII only, so a length is a size in bytes.
  if (!/^[A-Za-z0-9._/-]+$/u.test(path)) return false;
  const segments = path.split('/');
  if (segments.length > MAX_PATH_SEGMENTS) return false;
  for (const segment of segments) {
    if (segment.length === 0 || segment.length > MAX_SEGMENT_BYTES)
      return false;
    if (segment.startsWith('.') || segment.endsWith('.')) return false;
    if (isReservedDevice(segment)) return false;
  }
  const name = segments.at(-1) ?? '';
  const dot = name.lastIndexOf('.');
  return dot >= 0 && EXTENSIONS.has(name.slice(dot + 1).toLowerCase());
}
