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
 *   check-ci <sha>         the commit's `Quality gate` check run succeeded
 *   sums <dir>             verify each archive against its .sha256, write SHA256SUMS
 *   notes <dir> <version>  write RELEASE_NOTES.md (image reference from IMAGE_REF)
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

/** The `[workspace.package]` version of a Cargo.toml text. */
export function cargoWorkspaceVersion(toml) {
  const section = /\[workspace\.package\]([^[]*)/.exec(toml);
  const match = /^version\s*=\s*"([^"]+)"/m.exec(section?.[1] ?? '');
  assert.ok(match, 'No [workspace.package] version in Cargo.toml.');
  return match[1];
}

/**
 * Requires the most recently completed `Quality gate` check run of a commit to
 * have succeeded. `runs` are `{ name, status, conclusion, completed_at }`.
 * A commit that CI has not finished, or whose latest run failed, is refused.
 */
export function assertQualityGate(runs) {
  const gates = runs
    .filter((run) => run.name === 'Quality gate' && run.status === 'completed')
    .sort((a, b) => (a.completed_at < b.completed_at ? -1 : 1));
  assert.ok(gates.length > 0, 'No completed Quality gate run for this commit.');
  const latest = gates[gates.length - 1];
  assert.equal(
    latest.conclusion,
    'success',
    `The latest Quality gate run for this commit is ${latest.conclusion}.`,
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

/** The draft release description. It makes no verification claim. */
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
      ? `- Container image: \`${image}\``
      : '- Container image: none was published.',
    '- Provenance: GitHub artifact attestations for the archives and the image.',
    '',
    'Nothing here states that a signature was checked; see the verification',
    'section of docs/release-packages.md.',
    '',
  ].join('\n');
}

function ghCheckRuns(sha) {
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
      `repos/${repository}/commits/${sha}/check-runs?per_page=100`,
      '--jq',
      '.check_runs[] | {name, status, conclusion, completed_at}',
    ],
    { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 },
  );
  return output
    .split('\n')
    .filter((line) => line !== '')
    .map((line) => JSON.parse(line));
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
    assertQualityGate(ghCheckRuns(first));
    console.log('The Quality gate succeeded for this commit.');
  } else if (command === 'sums') {
    const names = (await readdir(first)).filter((name) =>
      /\.(tar\.gz|zip)$/.test(name),
    );
    const files = await Promise.all(
      names.map(async (name) => ({
        name,
        bytes: await readFile(join(first, name)),
        checksum: await readFile(join(first, `${name}.sha256`), 'utf8'),
      })),
    );
    await writeFile(join(first, 'SHA256SUMS'), buildSums(files));
  } else if (command === 'notes') {
    assert.ok(second, 'notes needs a version.');
    await writeFile(
      join(first, 'RELEASE_NOTES.md'),
      releaseNotes({ version: second, image: process.env.IMAGE_REF }),
    );
  } else assert.fail(`Unknown command: ${command}`);
}

if (process.argv[1] === fileURLToPath(import.meta.url)) await main();
