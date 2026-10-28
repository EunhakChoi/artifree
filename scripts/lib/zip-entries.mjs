import { crc32 } from 'node:zlib';

const LOCAL_HEADER = 0x04034b50;
const CENTRAL_HEADER = 0x02014b50;
const END_OF_DIRECTORY = 0x06054b50;

/**
 * The entries of a ZIP archive whose files are stored without compression,
 * as a source export writes them (decision 0015), in directory order. It
 * reads the central directory, checks that each local header agrees with
 * it, and checks every CRC-32 with Node's own implementation, so a test
 * does not trust the server's encoder to read its output. It throws on
 * anything else: compression, ZIP64, an archive comment, or a mismatch.
 */
export function storedZipEntries(archive) {
  const bytes = Buffer.from(archive);
  const end = bytes.length - 22;
  check(end >= 0 && bytes.readUInt32LE(end) === END_OF_DIRECTORY, 'end record');
  check(bytes.readUInt16LE(end + 20) === 0, 'no archive comment');
  const count = bytes.readUInt16LE(end + 10);
  const directorySize = bytes.readUInt32LE(end + 12);
  let at = bytes.readUInt32LE(end + 16);
  check(at + directorySize === end, 'directory ends at the end record');
  const entries = [];
  for (let index = 0; index < count; index += 1) {
    check(bytes.readUInt32LE(at) === CENTRAL_HEADER, `central header ${index}`);
    const method = bytes.readUInt16LE(at + 10);
    const time = bytes.readUInt16LE(at + 12);
    const date = bytes.readUInt16LE(at + 14);
    const crc = bytes.readUInt32LE(at + 16);
    const size = bytes.readUInt32LE(at + 20);
    const nameLength = bytes.readUInt16LE(at + 28);
    const extraLength = bytes.readUInt16LE(at + 30);
    const commentLength = bytes.readUInt16LE(at + 32);
    const mode = bytes.readUInt32LE(at + 38) >>> 16;
    const offset = bytes.readUInt32LE(at + 42);
    const name = bytes.toString('utf8', at + 46, at + 46 + nameLength);
    check(method === 0, `${name} is stored`);
    check(bytes.readUInt32LE(at + 24) === size, `${name} sizes agree`);

    check(bytes.readUInt32LE(offset) === LOCAL_HEADER, `${name} local header`);
    const localName = bytes.readUInt16LE(offset + 26);
    const localExtra = bytes.readUInt16LE(offset + 28);
    check(
      bytes.toString('utf8', offset + 30, offset + 30 + localName) === name &&
        bytes.readUInt16LE(offset + 8) === method &&
        bytes.readUInt32LE(offset + 14) === crc &&
        bytes.readUInt32LE(offset + 22) === size,
      `${name} local header matches the directory`,
    );
    const start = offset + 30 + localName + localExtra;
    const content = bytes.subarray(start, start + size);
    check(content.length === size, `${name} is complete`);
    check(crc32(content) === crc, `${name} CRC-32`);
    entries.push({
      name,
      content,
      mode,
      modified: {
        year: 1980 + (date >> 9),
        month: (date >> 5) & 0xf,
        day: date & 0x1f,
        hour: time >> 11,
        minute: (time >> 5) & 0x3f,
        second: (time & 0x1f) * 2,
      },
    });
    at += 46 + nameLength + extraLength + commentLength;
  }
  return entries;
}

function check(condition, what) {
  if (!condition) throw new Error(`Not a stored ZIP archive: ${what}.`);
}
