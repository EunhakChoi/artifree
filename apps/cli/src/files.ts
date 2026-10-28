import { lstat, open, readdir, readFile, rm } from 'node:fs/promises';
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
 * Writes `content` to `location`, which must not exist yet, so that an
 * existing file is never replaced. A write that fails part way removes the
 * file it created.
 */
export async function writeNewFile(
  location: string,
  content: Uint8Array,
): Promise<void> {
  let handle;
  try {
    handle = await open(location, 'wx');
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
