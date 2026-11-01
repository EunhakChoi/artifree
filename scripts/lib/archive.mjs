import assert from 'node:assert/strict';
import { crc32, deflateRawSync, gzipSync } from 'node:zlib';

/**
 * Deterministic archive writers for release packages. Both take entries as
 * `{ path, data, executable }` with portable `/`-separated paths, write them
 * in the order given with fixed timestamps and owners, and so produce the same
 * bytes for the same input (and the same Node/zlib build). Only what a
 * release needs is supported: regular files, no links, paths under 100 bytes.
 */

const MAX_PATH_BYTES = 100;
const MAX_FILE_BYTES = 0xffff_ffff;

function checked(entries) {
  const seen = new Set();
  for (const entry of entries) {
    const bytes = Buffer.byteLength(entry.path);
    assert.ok(
      entry.path !== '' &&
        !entry.path.startsWith('/') &&
        !entry.path.includes('\\') &&
        !entry.path.split('/').some((part) => part === '' || part === '..'),
      `Unsafe archive path: ${entry.path}`,
    );
    assert.ok(bytes <= MAX_PATH_BYTES, `Archive path too long: ${entry.path}`);
    assert.ok(entry.data.length <= MAX_FILE_BYTES, `Too large: ${entry.path}`);
    assert.ok(!seen.has(entry.path), `Duplicate archive path: ${entry.path}`);
    seen.add(entry.path);
  }
}

function octal(value, width) {
  const text = value.toString(8);
  assert.ok(text.length < width, 'Tar field overflow.');
  return `${text.padStart(width - 1, '0')}\0`;
}

function tarHeader(entry) {
  const header = Buffer.alloc(512);
  header.write(entry.path, 0, 'utf8');
  header.write(octal(entry.executable ? 0o755 : 0o644, 8), 100, 'ascii');
  header.write(octal(0, 8), 108, 'ascii'); // uid
  header.write(octal(0, 8), 116, 'ascii'); // gid
  header.write(octal(entry.data.length, 12), 124, 'ascii');
  header.write(octal(0, 12), 136, 'ascii'); // mtime: the epoch
  header.write('        ', 148, 'ascii'); // checksum placeholder: spaces
  header.write('0', 156, 'ascii'); // regular file
  header.write('ustar\0', 257, 'ascii');
  header.write('00', 263, 'ascii');
  let sum = 0;
  for (const byte of header) sum += byte;
  header.write(`${sum.toString(8).padStart(6, '0')}\0 `, 148, 'ascii');
  return header;
}

/** A gzip-compressed POSIX ustar archive. */
export function tarGz(entries) {
  checked(entries);
  const parts = [];
  for (const entry of entries) {
    parts.push(tarHeader(entry), entry.data);
    const padding = (512 - (entry.data.length % 512)) % 512;
    if (padding > 0) parts.push(Buffer.alloc(padding));
  }
  parts.push(Buffer.alloc(1024)); // end of archive
  return gzipSync(Buffer.concat(parts), { level: 9 });
}

// 1980-01-01 00:00:00, the earliest MS-DOS timestamp.
const DOS_TIME = 0;
const DOS_DATE = 0x21;
// Version made by: Unix (3) with spec 2.0; Unix mode lives in the high bits.
const MADE_BY = (3 << 8) | 20;

/** A ZIP archive with deflated entries (no ZIP64, no comment). */
export function zip(entries) {
  checked(entries);
  assert.ok(entries.length < 0xffff, 'Too many ZIP entries.');
  const local = [];
  const central = [];
  let offset = 0;
  for (const entry of entries) {
    const name = Buffer.from(entry.path, 'utf8');
    const packed = deflateRawSync(entry.data, { level: 9 });
    const crc = crc32(entry.data);
    const header = Buffer.alloc(30);
    header.writeUInt32LE(0x04034b50, 0);
    header.writeUInt16LE(20, 4);
    header.writeUInt16LE(0x0800, 6); // UTF-8 names
    header.writeUInt16LE(8, 8); // deflate
    header.writeUInt16LE(DOS_TIME, 10);
    header.writeUInt16LE(DOS_DATE, 12);
    header.writeUInt32LE(crc, 14);
    header.writeUInt32LE(packed.length, 18);
    header.writeUInt32LE(entry.data.length, 22);
    header.writeUInt16LE(name.length, 26);
    local.push(header, name, packed);

    const dir = Buffer.alloc(46);
    dir.writeUInt32LE(0x02014b50, 0);
    dir.writeUInt16LE(MADE_BY, 4);
    dir.writeUInt16LE(20, 6);
    dir.writeUInt16LE(0x0800, 8);
    dir.writeUInt16LE(8, 10);
    dir.writeUInt16LE(DOS_TIME, 12);
    dir.writeUInt16LE(DOS_DATE, 14);
    dir.writeUInt32LE(crc, 16);
    dir.writeUInt32LE(packed.length, 20);
    dir.writeUInt32LE(entry.data.length, 24);
    dir.writeUInt16LE(name.length, 28);
    dir.writeUInt32LE(
      ((entry.executable ? 0o100755 : 0o100644) << 16) >>> 0,
      38,
    );
    dir.writeUInt32LE(offset, 42);
    central.push(dir, name);
    offset += header.length + name.length + packed.length;
  }
  const directory = Buffer.concat(central);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(entries.length, 8);
  end.writeUInt16LE(entries.length, 10);
  end.writeUInt32LE(directory.length, 12);
  end.writeUInt32LE(offset, 16);
  return Buffer.concat([...local, directory, end]);
}
