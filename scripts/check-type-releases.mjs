import { createHash } from 'node:crypto';
import { readdir, readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

/**
 * The release lock of the type catalog (decision 0059 rule 3). Every release
 * directory `types/<id>/<release>/` is listed in `types/releases.lock.json`
 * with the SHA-256 of each of its files, and a listed release never changes
 * and is never removed. `--check` (part of `npm run check`) refuses a locked
 * release whose files differ from the lock, a locked release that is gone,
 * and a release directory the lock does not list. `--add <id>/<release>`
 * locks one new release directory and refuses to touch a listed one; it is
 * the only way the lock grows.
 */

export const LOCK_FILE = 'releases.lock.json';

/** Every file under `dir`, as sorted `/`-separated paths relative to it. */
async function filesUnder(dir, prefix = '') {
  const found = [];
  for (const entry of await readdir(join(dir, prefix), {
    withFileTypes: true,
  })) {
    const path = prefix === '' ? entry.name : `${prefix}/${entry.name}`;
    if (entry.isDirectory()) found.push(...(await filesUnder(dir, path)));
    else found.push(path);
  }
  return found.sort();
}

/** The release directories under `typesDir`, as sorted `<id>/<release>`. */
async function releaseDirectories(typesDir) {
  const releases = [];
  for (const type of await readdir(typesDir, { withFileTypes: true })) {
    if (!type.isDirectory()) continue;
    for (const release of await readdir(join(typesDir, type.name), {
      withFileTypes: true,
    }))
      releases.push(`${type.name}/${release.name}`);
  }
  return releases.sort();
}

/** `{ path: sha256 }` of every file of one release directory. */
async function digests(typesDir, release) {
  const dir = join(typesDir, ...release.split('/'));
  const entries = {};
  for (const path of await filesUnder(dir))
    entries[path] = createHash('sha256')
      .update(await readFile(join(dir, ...path.split('/'))))
      .digest('hex');
  return entries;
}

async function readLock(typesDir) {
  const lock = JSON.parse(await readFile(join(typesDir, LOCK_FILE), 'utf8'));
  if (
    typeof lock !== 'object' ||
    lock === null ||
    typeof lock.releases !== 'object' ||
    lock.releases === null
  )
    throw new Error(`${LOCK_FILE} must be {"releases": {...}}.`);
  return lock;
}

/**
 * The problems of `typesDir` against its lock, one sentence each; empty when
 * every release matches.
 */
export async function checkReleases(typesDir) {
  const lock = await readLock(typesDir);
  const problems = [];
  const present = await releaseDirectories(typesDir);
  for (const release of present) {
    if (!/^[a-z0-9-]{1,32}\/[1-9][0-9]*$/.test(release)) {
      problems.push(`${release} is not a <type id>/<release> directory.`);
      continue;
    }
    if (!Object.hasOwn(lock.releases, release))
      problems.push(
        `${release} is not in ${LOCK_FILE}; lock it with node scripts/check-type-releases.mjs --add ${release}.`,
      );
  }
  for (const [release, locked] of Object.entries(lock.releases)) {
    if (!present.includes(release)) {
      problems.push(`${release} is locked but its directory is gone.`);
      continue;
    }
    const actual = await digests(typesDir, release);
    const paths = new Set([...Object.keys(locked), ...Object.keys(actual)]);
    for (const path of [...paths].sort()) {
      if (!Object.hasOwn(actual, path))
        problems.push(`${release}/${path} is locked but missing.`);
      else if (!Object.hasOwn(locked, path))
        problems.push(`${release}/${path} is not in the locked release.`);
      else if (locked[path] !== actual[path])
        problems.push(`${release}/${path} differs from the locked release.`);
    }
  }
  if (problems.length > 0)
    problems.push(
      'A published release never changes (decision 0059 rule 3): put a change in a new release directory.',
    );
  return problems;
}

/** Locks the unlisted release directory `release`. */
export async function addRelease(typesDir, release) {
  const lock = await readLock(typesDir);
  if (Object.hasOwn(lock.releases, release))
    throw new Error(
      `${release} is already locked; a locked release never changes.`,
    );
  if (!(await releaseDirectories(typesDir)).includes(release))
    throw new Error(`${release} is not a release directory.`);
  const releases = {
    ...lock.releases,
    [release]: await digests(typesDir, release),
  };
  const sorted = Object.fromEntries(
    Object.keys(releases)
      .sort()
      .map((key) => [key, releases[key]]),
  );
  await writeFile(
    join(typesDir, LOCK_FILE),
    `${JSON.stringify({ releases: sorted }, null, 2)}\n`,
  );
}

async function main() {
  const typesDir = fileURLToPath(new URL('../types/', import.meta.url));
  const [mode, release, ...rest] = process.argv.slice(2);
  if (mode === '--check' && release === undefined) {
    const problems = await checkReleases(typesDir);
    for (const problem of problems) console.error(problem);
    if (problems.length > 0) process.exitCode = 1;
  } else if (mode === '--add' && release !== undefined && rest.length === 0) {
    await addRelease(typesDir, release);
  } else {
    console.error(
      'Usage: node scripts/check-type-releases.mjs --check | --add <id>/<release>',
    );
    process.exitCode = 2;
  }
}

if (process.argv[1] === fileURLToPath(import.meta.url)) await main();
