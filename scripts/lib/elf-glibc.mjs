import assert from 'node:assert/strict';

/**
 * Reads the glibc symbol versions an ELF64 little-endian executable requires
 * (decision 0037), from its `.gnu.version_r` section, so that the supported
 * glibc floor is checked without binutils. Imports nothing, because the
 * verifier runs without `npm install`.
 */

const SHT_GNU_VERNEED = 0x6ffffffe;
const GLIBC_VERSION = /^GLIBC_(\d+)\.(\d+)(?:\.(\d+))?$/;

function number(value) {
  assert.ok(Number.isSafeInteger(Number(value)), 'ELF offset is out of range.');
  return Number(value);
}

function cString(bytes, offset) {
  assert.ok(offset < bytes.length, 'ELF string offset is out of range.');
  const end = bytes.indexOf(0, offset);
  assert.ok(end !== -1, 'ELF string is not terminated.');
  return bytes.toString('latin1', offset, end);
}

/** `[major, minor, patch]` of a `2.34`-style version string. */
export function parseVersion(text) {
  const match = /^(\d+)\.(\d+)(?:\.(\d+))?$/.exec(text);
  assert.ok(match, `Not a glibc version: ${text}`);
  return [Number(match[1]), Number(match[2]), Number(match[3] ?? 0)];
}

/** Negative, zero, or positive as version `a` is below, equal to, or above `b`. */
export function compareVersions(a, b) {
  for (let i = 0; i < 3; i += 1) if (a[i] !== b[i]) return a[i] - b[i];
  return 0;
}

/**
 * The distinct `GLIBC_*` versions the executable requires, ascending, as
 * `2.34`-style strings. Throws when `bytes` is not an ELF64 little-endian
 * file or its version-needs table is malformed. An empty list means the file
 * requires no versioned glibc symbol.
 */
export function requiredGlibcVersions(bytes) {
  assert.ok(
    bytes.length > 0x40 &&
      bytes.readUInt32BE(0) === 0x7f454c46 &&
      bytes[4] === 2 &&
      bytes[5] === 1,
    'Not a 64-bit little-endian ELF file.',
  );
  const tableOffset = number(bytes.readBigUInt64LE(0x28));
  const entrySize = bytes.readUInt16LE(0x3a);
  const count = bytes.readUInt16LE(0x3c);
  assert.ok(
    entrySize >= 0x40 && tableOffset + count * entrySize <= bytes.length,
    'ELF section table is out of range.',
  );
  const section = (index) => {
    const at = tableOffset + index * entrySize;
    return {
      type: bytes.readUInt32LE(at + 4),
      offset: number(bytes.readBigUInt64LE(at + 0x18)),
      size: number(bytes.readBigUInt64LE(at + 0x20)),
      link: bytes.readUInt32LE(at + 0x28),
      info: bytes.readUInt32LE(at + 0x2c),
    };
  };
  const versions = new Map();
  for (let i = 0; i < count; i += 1) {
    const needs = section(i);
    if (needs.type !== SHT_GNU_VERNEED) continue;
    assert.ok(needs.link < count, 'ELF version table has no string table.');
    const strings = section(needs.link);
    assert.ok(
      needs.offset + needs.size <= bytes.length &&
        strings.offset + strings.size <= bytes.length,
      'ELF version table is out of range.',
    );
    const stringBytes = bytes.subarray(
      strings.offset,
      strings.offset + strings.size,
    );
    let need = needs.offset;
    for (let n = 0; n < needs.info; n += 1) {
      const auxCount = bytes.readUInt16LE(need + 2);
      let aux = need + bytes.readUInt32LE(need + 8);
      for (let a = 0; a < auxCount; a += 1) {
        const name = cString(stringBytes, bytes.readUInt32LE(aux + 8));
        const match = GLIBC_VERSION.exec(name);
        if (match)
          versions.set(
            `${match[1]}.${match[2]}${match[3] === undefined ? '' : `.${match[3]}`}`,
            [Number(match[1]), Number(match[2]), Number(match[3] ?? 0)],
          );
        aux += bytes.readUInt32LE(aux + 12);
      }
      need += bytes.readUInt32LE(need + 12);
    }
  }
  return [...versions.entries()]
    .sort((a, b) => compareVersions(a[1], b[1]))
    .map(([text]) => text);
}

/**
 * Throws unless the executable requires at least one versioned glibc symbol
 * (a file that needs none would pass any floor trivially) and every required
 * version is at most `floor`. Returns the highest required version.
 */
export function assertGlibcFloor(bytes, floor) {
  const versions = requiredGlibcVersions(bytes);
  assert.ok(
    versions.length > 0,
    'The binary requires no versioned glibc symbol; it is not a glibc executable.',
  );
  const above = versions.filter(
    (version) =>
      compareVersions(parseVersion(version), parseVersion(floor)) > 0,
  );
  assert.deepEqual(
    above,
    [],
    `The binary requires glibc ${above.join(', ')}, above the supported floor ${floor}.`,
  );
  return versions[versions.length - 1];
}

/**
 * The glibc version named on the first line of `ldd --version` output, as in
 * `ldd (Debian GLIBC 2.36-9+deb12u10) 2.36`. Throws when no version ends it.
 */
export function glibcFromLddOutput(output) {
  const first = output.split('\n', 1)[0].trim();
  const match = /\s(\d+\.\d+)$/.exec(first);
  assert.ok(match, `Cannot read the glibc version from: ${first}`);
  return match[1];
}
