import {
  constants,
  lstat,
  open,
  readdir,
  readFile,
  rm,
} from 'node:fs/promises';
import { basename, join } from 'node:path';

/** A local file mapped to a package path. */
export interface LocalFile {
  readonly path: string;
  readonly bytes: Uint8Array;
}

/**
 * Reads every regular file under `directory` as package paths with `/`
 * separators. Entries whose name starts with `.` are skipped because package
 * paths cannot contain hidden segments. Symbolic links are rejected so that a
 * package never includes files from outside the directory. The server
 * validates paths, sizes, and counts again.
 */
export async function readDirectory(directory: string): Promise<LocalFile[]> {
  const files: LocalFile[] = [];
  async function visit(absolute: string, prefix: string): Promise<void> {
    const entries = await readdir(absolute, { withFileTypes: true });
    // Code-unit order, independent of the locale, keeps output stable.
    entries.sort((left, right) =>
      left.name < right.name ? -1 : left.name > right.name ? 1 : 0,
    );
    for (const entry of entries) {
      if (entry.name.startsWith('.')) continue;
      const path = prefix === '' ? entry.name : `${prefix}/${entry.name}`;
      const location = join(absolute, entry.name);
      if (entry.isSymbolicLink())
        throw new Error(`Refusing to follow a symbolic link: ${location}`);
      if (entry.isDirectory()) await visit(location, path);
      else if (entry.isFile())
        files.push({ path, bytes: await readFile(location) });
      else throw new Error(`Not a regular file: ${location}`);
    }
  }
  await visit(directory, '');
  if (files.length === 0) throw new Error(`No files found in ${directory}.`);
  return files;
}

/** Reads one regular file and names it by its base name. */
export async function readNamedFile(location: string): Promise<LocalFile> {
  return { path: basename(location), bytes: await readRegularFile(location) };
}

/** Reads a regular file, refusing symbolic links and other file types. */
export async function readRegularFile(location: string): Promise<Uint8Array> {
  const status = await lstat(location);
  if (!status.isFile()) throw new Error(`Not a regular file: ${location}`);
  return readFile(location);
}

/**
 * Reads the regular file at `location` if it is no larger than `maxBytes`.
 * Symbolic links and other non-regular files are refused, as is a file that
 * is replaced while it is opened or changes size or modification time while
 * it is read (a best-effort check: the file system's timestamp granularity
 * bounds what it can see). An oversized file is refused before any of its
 * content is read. Refusals throw an `Error` that names `location`.
 * `afterRead`, when given, runs once the bytes are read and before the final
 * check; tests use it to change the file at a fixed point instead of racing.
 */
export async function readBoundedRegularFile(
  location: string,
  maxBytes: number,
  afterRead?: () => Promise<void>,
): Promise<Uint8Array> {
  if (!Number.isSafeInteger(maxBytes) || maxBytes < 0)
    throw new RangeError(
      `maxBytes must be a non-negative safe integer, got ${maxBytes}.`,
    );
  // Inode numbers can exceed 2^53, so file identity is compared as bigint.
  const status = await lstat(location, { bigint: true });
  if (status.isSymbolicLink())
    throw new Error(`Refusing to follow a symbolic link: ${location}`);
  if (!status.isFile()) throw new Error(`Not a regular file: ${location}`);
  // Windows has no O_NOFOLLOW; there the dev/ino check catches a swapped link.
  // O_NONBLOCK keeps a regular file swapped for a FIFO from blocking this open;
  // the fstat below then refuses it (a regular file ignores the flag).
  const handle = await open(
    location,
    constants.O_RDONLY |
      (constants.O_NOFOLLOW ?? 0) |
      (constants.O_NONBLOCK ?? 0),
  );
  try {
    const opened = await handle.stat({ bigint: true });
    if (
      !opened.isFile() ||
      opened.dev !== status.dev ||
      opened.ino !== status.ino
    )
      throw new Error(
        `${location} changed while it was opened; nothing was sent.`,
      );
    const size = Number(opened.size);
    if (size > maxBytes) {
      // A whole number of MiB reads better than bytes alone; any other limit prints bytes.
      const limit =
        maxBytes % 1048576 === 0
          ? `${maxBytes / 1048576} MiB (${maxBytes} bytes)`
          : `${maxBytes} bytes`;
      throw new Error(`${location} is larger than ${limit}; nothing was sent.`);
    }
    // One spare byte shows whether the file grew after it was measured.
    const buffer = new Uint8Array(size + 1);
    let total = 0;
    while (total < buffer.length) {
      const { bytesRead } = await handle.read(
        buffer,
        total,
        buffer.length - total,
        null,
      );
      if (bytesRead === 0) break;
      total += bytesRead;
    }
    await afterRead?.();
    const after = await handle.stat({ bigint: true });
    if (
      total !== size ||
      after.size !== opened.size ||
      after.mtimeNs !== opened.mtimeNs ||
      after.ctimeNs !== opened.ctimeNs
    )
      throw new Error(
        `${location} changed while it was read; nothing was sent.`,
      );
    // slice copies, so the spare byte is not reachable through `.buffer`.
    return buffer.slice(0, size);
  } finally {
    await handle.close();
  }
}

/**
 * Writes `content` to `location`, which must not exist yet, so that an
 * existing file is never replaced. A write that fails part way removes the
 * file it created.
 */
export async function writeNewFile(
  location: string,
  content: Uint8Array,
  mode?: number,
): Promise<void> {
  let handle;
  try {
    handle = await open(location, 'wx', mode);
  } catch (error: unknown) {
    if (error instanceof Error && 'code' in error && error.code === 'EEXIST')
      throw new Error(`${location} already exists; nothing was written.`, {
        cause: error,
      });
    throw error;
  }
  try {
    await handle.writeFile(content);
    await handle.close();
  } catch (error: unknown) {
    await handle.close().catch(() => undefined);
    await rm(location, { force: true });
    throw error;
  }
}
