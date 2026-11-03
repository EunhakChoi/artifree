import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { readdir, readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { sha256 } from './lib/release-layout.mjs';

/**
 * Release workflow helpers (decision 0037). They hold every rule the release
 * workflow applies before it publishes, so the rules are tested here and the
 * workflow only calls them. Node built-ins only.
 *
 *   check-tag <tag>        the tag is vX.Y.Z[-pre] and equals the repository version
 *   check-ci <sha>         the commit's push run of ci.yml on main succeeded
 *   sums <dir>             <dir> holds exactly the archives and their .sha256
 *                          files; write SHA256SUMS
 *   notes <dir> <version>  write RELEASE_NOTES.md (image reference from IMAGE_REF)
 *   check-set <dir>        <dir> holds exactly the release assets, and SHA256SUMS
 *                          matches them
 */

const TAG = /^v(\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)$/;
const ARCHIVE_SUFFIXES = ['linux-x64.tar.gz', 'windows-x64.zip'];

/** The version named by `tag`; throws unless it equals both repository versions. */
export function versionFromTag(tag, { packageVersion, cargoVersion }) {
  const match = TAG.exec(tag);
  assert.ok(match, `The tag ${tag} is not of the form vX.Y.Z or vX.Y.Z-pre.`);
  assert.equal(
    match[1],
    packageVersion,
    `The tag ${tag} does not match package.json version ${packageVersion}.`,
  );
  assert.equal(
    match[1],
    cargoVersion,
    `The tag ${tag} does not match the Cargo workspace version ${cargoVersion}.`,
  );
  return match[1];
}

/**
 * The `[workspace.package]` version of a Cargo.toml text: the first
 * `version = "..."` line in that table, which ends at the next table header
 * (a line that is only `[name]` and an optional comment). Arrays and comments
 * that contain `[` do not end it.
 */
export function cargoWorkspaceVersion(toml) {
  let inTable = false;
  for (const line of toml.split(/\r?\n/)) {
    if (/^\s*\[[^\]]+\]\s*(#.*)?$/.test(line)) {
      inTable = line.trim().startsWith('[workspace.package]');
      continue;
    }
    const match = inTable ? /^version\s*=\s*"([^"]+)"/.exec(line) : null;
    if (match) return match[1];
  }
  return assert.fail('No [workspace.package] version in Cargo.toml.');
}

/** The workflow file whose push run on `main` is the quality evidence. */
const CI_WORKFLOW_PATH = '.github/workflows/ci.yml';

/**
 * Requires the newest meaningful CI run of commit `sha` to have succeeded.
 * `runs` are Actions workflow runs (`path`, `event`, `head_branch`,
 * `head_sha`, `status`, `conclusion`, `created_at`). Only completed push runs
 * of `ci.yml` on `main` for that commit count: a pull-request run of the same
 * commit or another workflow does not. Cancelled and skipped runs (CI cancels
 * superseded runs) are ignored. The newest remaining run must have succeeded.
 */
export function assertQualityGate(runs, sha) {
  const relevant = runs
    .filter(
      (run) =>
        run.path === CI_WORKFLOW_PATH &&
        run.event === 'push' &&
        run.head_branch === 'main' &&
        run.head_sha === sha &&
        run.status === 'completed' &&
        run.conclusion !== 'cancelled' &&
        run.conclusion !== 'skipped',
    )
    .sort((a, b) => (a.created_at < b.created_at ? -1 : 1));
  assert.ok(
    relevant.length > 0,
    `No completed push run of ci.yml on main for ${sha}.`,
  );
  const latest = relevant[relevant.length - 1];
  assert.equal(
    latest.conclusion,
    'success',
    `The latest CI run on main for this commit is ${latest.conclusion}.`,
  );
}

/**
 * Verifies that `files` (`{ name, bytes, checksum }`) hold exactly one archive
 * per release platform for one version, that each `.sha256` file matches its
 * archive, and returns the combined `SHA256SUMS` text (sorted by name).
 */
export function buildSums(files) {
  const archives = files.filter((file) => /\.(tar\.gz|zip)$/.test(file.name));
  const versions = new Set();
  for (const suffix of ARCHIVE_SUFFIXES) {
    const found = archives.filter((a) => a.name.endsWith(`-${suffix}`));
    assert.equal(found.length, 1, `Expected exactly one *-${suffix} archive.`);
    versions.add(found[0].name.slice('artifree-'.length, -suffix.length - 1));
  }
  assert.equal(archives.length, ARCHIVE_SUFFIXES.length, 'Unexpected archive.');
  assert.equal(versions.size, 1, 'The archives are of different versions.');
  const lines = [];
  for (const archive of archives) {
    const digest = sha256(archive.bytes);
    assert.equal(
      archive.checksum,
      `${digest}  ${archive.name}\n`,
      `The checksum file of ${archive.name} does not match the archive.`,
    );
    lines.push({ name: archive.name, line: `${digest}  ${archive.name}\n` });
  }
  return lines
    .sort((a, b) => (a.name < b.name ? -1 : 1))
    .map((entry) => entry.line)
    .join('');
}

/**
 * Requires the downloaded build outputs (`{ name, bytes }`) to be archives and
 * the `.sha256` file of each of them, and nothing else. Returns the archives.
 */
export function assertDownloadedSet(entries) {
  const archives = entries.filter((entry) =>
    /\.(tar\.gz|zip)$/.test(entry.name),
  );
  assert.deepEqual(
    entries.map((entry) => entry.name).sort(),
    archives.flatMap((a) => [a.name, `${a.name}.sha256`]).sort(),
    'The release directory holds files other than archives and their .sha256.',
  );
  return archives;
}

/**
 * Requires `entries` (`{ name, bytes }`, the files of the release directory) to
 * be exactly the release assets: both archives of one version, their `.sha256`
 * files, `SHA256SUMS`, and `RELEASE_NOTES.md`, nothing else, with `SHA256SUMS`
 * listing exactly the archives at their real hashes.
 */
export function assertReleaseSet(entries) {
  const byName = new Map(entries.map((entry) => [entry.name, entry.bytes]));
  assert.equal(byName.size, entries.length, 'Duplicate file name.');
  const archives = entries.filter((entry) =>
    /^artifree-.+\.(tar\.gz|zip)$/.test(entry.name),
  );
  const versions = new Set(
    archives.map(
      (entry) => /^artifree-(.+)-(?:linux|windows)-x64\./.exec(entry.name)?.[1],
    ),
  );
  assert.equal(versions.size, 1, 'The archives are of different versions.');
  const [version] = versions;
  assert.ok(version, 'An archive name does not carry a version.');
  const archiveNames = ARCHIVE_SUFFIXES.map(
    (suffix) => `artifree-${version}-${suffix}`,
  );
  const expected = [
    ...archiveNames,
    ...archiveNames.map((name) => `${name}.sha256`),
    'RELEASE_NOTES.md',
    'SHA256SUMS',
  ].sort();
  assert.deepEqual(
    [...byName.keys()].sort(),
    expected,
    'The release directory is not exactly the expected assets.',
  );
  const sums = buildSums(
    archiveNames.map((name) => ({
      name,
      bytes: byName.get(name),
      checksum: byName.get(`${name}.sha256`).toString('utf8'),
    })),
  );
  assert.equal(
    byName.get('SHA256SUMS').toString('utf8'),
    sums,
    'SHA256SUMS does not list exactly the archives at their hashes.',
  );
}

/**
 * The draft release description. It makes no verification claim. `image` is
 * the image reference (name and tag) the later `publish-image` job pushes.
 */
export function releaseNotes({ version, image }) {
  return [
    `Artifree ${version}`,
    '',
    'This is a DRAFT created by the release workflow. Review it, verify the',
    'assets as described in docs/release-packages.md, and publish it by hand.',
    '',
    '- Archives: Linux x64 (.tar.gz) and Windows x64 (.zip), with `.sha256` files',
    '  and one `SHA256SUMS`.',
    image
      ? `- Container image: \`${image}\`, pushed by the publish-image job AFTER this draft was created. Publish this draft only if that job succeeded.`
      : '- Container image: none.',
    '- Provenance: GitHub artifact attestations for the archives and the image.',
    '',
    'Nothing here states that a signature was checked; see the verification',
    'section of docs/release-packages.md.',
    '',
  ].join('\n');
}

function ghWorkflowRuns(sha) {
  const repository = process.env.GITHUB_REPOSITORY;
  assert.match(
    repository ?? '',
    /^[\w.-]+\/[\w.-]+$/,
    'GITHUB_REPOSITORY is not set.',
  );
  assert.match(sha, /^[0-9a-f]{40}$/, 'Not a commit SHA.');
  const output = execFileSync(
    'gh',
    [
      'api',
      '--paginate',
      `repos/${repository}/actions/runs?head_sha=${sha}&event=push&branch=main&per_page=100`,
      '--jq',
      '.workflow_runs[] | {path, event, head_branch, head_sha, status, conclusion, created_at}',
    ],
    { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 },
  );
  return output
    .split('\n')
    .filter((line) => line !== '')
    .map((line) => JSON.parse(line));
}

/** Regular files of `dir` as `{ name, bytes }`; any other entry is an error. */
async function releaseFiles(dir) {
  const files = [];
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    assert.ok(entry.isFile(), `Not a regular file: ${entry.name}`);
    files.push({
      name: entry.name,
      bytes: await readFile(join(dir, entry.name)),
    });
  }
  return files;
}

async function main() {
  const root = fileURLToPath(new URL('../', import.meta.url));
  const [command, first, second, ...extra] = process.argv.slice(2);
  assert.ok(
    extra.length === 0 && first,
    'Usage: release-assets.mjs <command> <argument>',
  );
  if (command === 'check-tag') {
    const version = versionFromTag(first, {
      packageVersion: JSON.parse(
        await readFile(join(root, 'package.json'), 'utf8'),
      ).version,
      cargoVersion: cargoWorkspaceVersion(
        await readFile(join(root, 'Cargo.toml'), 'utf8'),
      ),
    });
    console.log(version);
  } else if (command === 'check-ci') {
    assertQualityGate(ghWorkflowRuns(first), first);
    console.log('The CI run on main succeeded for this commit.');
  } else if (command === 'sums') {
    const files = (await releaseFiles(first)).filter(
      (file) => file.name !== 'SHA256SUMS',
    );
    const archives = assertDownloadedSet(files);
    const text = (name) =>
      files.find((file) => file.name === name).bytes.toString('utf8');
    await writeFile(
      join(first, 'SHA256SUMS'),
      buildSums(
        archives.map((a) => ({ ...a, checksum: text(`${a.name}.sha256`) })),
      ),
    );
  } else if (command === 'check-set') {
    assertReleaseSet(await releaseFiles(first));
    console.log('The release directory is exactly the expected assets.');
  } else if (command === 'notes') {
    assert.ok(second, 'notes needs a version.');
    await writeFile(
      join(first, 'RELEASE_NOTES.md'),
      releaseNotes({ version: second, image: process.env.IMAGE_REF }),
    );
  } else assert.fail(`Unknown command: ${command}`);
}

if (process.argv[1] === fileURLToPath(import.meta.url)) await main();
