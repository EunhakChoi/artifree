import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  assertDownloadedSet,
  assertQualityGate,
  assertReleaseSet,
  buildSums,
  cargoWorkspaceVersion,
  releaseNotes,
  versionFromTag,
} from '../release-assets.mjs';
import { sha256 } from '../lib/release-layout.mjs';

const versions = { packageVersion: '0.1.0', cargoVersion: '0.1.0' };

test('a tag equal to the repository version is accepted, with or without a prerelease', () => {
  assert.equal(versionFromTag('v0.1.0', versions), '0.1.0');
  const rc = { packageVersion: '0.2.0-rc.1', cargoVersion: '0.2.0-rc.1' };
  assert.equal(versionFromTag('v0.2.0-rc.1', rc), '0.2.0-rc.1');
});

for (const [tag, pattern] of [
  ['0.1.0', /not of the form/],
  ['v0.1', /not of the form/],
  ['v0.1.0; rm -rf /', /not of the form/],
  ['v0.1.1', /does not match package.json version 0.1.0/],
]) {
  test(`the tag ${tag} is refused`, () => {
    assert.throws(() => versionFromTag(tag, versions), pattern);
  });
}

test('a tag that disagrees with the Cargo version is refused', () => {
  assert.throws(
    () =>
      versionFromTag('v0.1.0', {
        packageVersion: '0.1.0',
        cargoVersion: '0.1.1',
      }),
    /does not match the Cargo workspace version 0.1.1/,
  );
});

test('the workspace version is read from its own table, past arrays and comments', () => {
  assert.equal(
    cargoWorkspaceVersion(
      '[package]\nversion = "9.9.9"\n[workspace.package]\nversion = "0.1.0"\nedition = "2024"\n',
    ),
    '0.1.0',
  );
  assert.equal(
    cargoWorkspaceVersion(
      [
        '[workspace]',
        'members = ["a"]',
        '',
        '[workspace.package] # shared',
        '# a comment with [brackets] before the version',
        'authors = ["x", "y"]',
        'categories = [',
        '  "one",',
        ']',
        'matrix = [',
        '  ["a", "b"],',
        ']',
        'version = "0.2.0-rc.1"',
        '',
        '[workspace.dependencies]',
        'version = "7.7.7"',
      ].join('\n'),
    ),
    '0.2.0-rc.1',
  );
  assert.throws(
    () => cargoWorkspaceVersion('[package]\nversion = "9.9.9"\n'),
    /No \[workspace.package\]/,
  );
  assert.throws(
    () =>
      cargoWorkspaceVersion(
        '[workspace.package]\nedition = "2024"\n[workspace.dependencies]\nversion = "7.7.7"\n',
      ),
    /No \[workspace.package\]/,
  );
});

const SHA = 'a'.repeat(40);
const ci = (overrides) => ({
  path: '.github/workflows/ci.yml',
  event: 'push',
  head_branch: 'main',
  head_sha: SHA,
  status: 'completed',
  conclusion: 'success',
  created_at: '2026-01-01T00:00:00Z',
  ...overrides,
});

test('a commit passes when its newest meaningful push run of ci.yml on main succeeded', () => {
  assertQualityGate([ci({})], SHA);
  assertQualityGate(
    [
      ci({ conclusion: 'failure', created_at: '2026-01-01T00:00:00Z' }),
      ci({ conclusion: 'success', created_at: '2026-01-02T00:00:00Z' }),
    ],
    SHA,
  );
});

test('a newer cancelled or skipped run does not hide an older success', () => {
  for (const conclusion of ['cancelled', 'skipped'])
    assertQualityGate(
      [ci({}), ci({ conclusion, created_at: '2026-01-03T00:00:00Z' })],
      SHA,
    );
});

test('a newer failure after an older success refuses the commit', () => {
  assert.throws(
    () =>
      assertQualityGate(
        [
          ci({}),
          ci({ conclusion: 'failure', created_at: '2026-01-02T00:00:00Z' }),
        ],
        SHA,
      ),
    /latest CI run on main for this commit is failure/,
  );
});

for (const [name, runs] of [
  ['a pull-request run of the same commit', [ci({ event: 'pull_request' })]],
  ['a run on another branch', [ci({ head_branch: 'feature' })]],
  ['another workflow', [ci({ path: '.github/workflows/release.yml' })]],
  ['a run of another commit', [ci({ head_sha: 'b'.repeat(40) })]],
  [
    'a run that has not completed',
    [ci({ status: 'in_progress', conclusion: null })],
  ],
  ['only cancelled runs', [ci({ conclusion: 'cancelled' })]],
  ['no run at all', []],
]) {
  test(`${name} is not evidence`, () => {
    assert.throws(
      () => assertQualityGate(runs, SHA),
      /No completed push run of ci.yml on main/,
    );
  });
}

function archive(name, content) {
  const bytes = Buffer.from(content);
  return { name, bytes, checksum: `${sha256(bytes)}  ${name}\n` };
}

const linux = () => archive('artifree-0.1.0-linux-x64.tar.gz', 'linux');
const windows = () => archive('artifree-0.1.0-windows-x64.zip', 'windows');

test('the checksum list covers both archives in name order and matches each .sha256', () => {
  const sums = buildSums([windows(), linux()]);
  assert.equal(
    sums,
    `${sha256(Buffer.from('linux'))}  artifree-0.1.0-linux-x64.tar.gz\n${sha256(Buffer.from('windows'))}  artifree-0.1.0-windows-x64.zip\n`,
  );
});

test('a missing platform, a mixed version, or a stale checksum file is refused', () => {
  assert.throws(() => buildSums([linux()]), /exactly one \*-windows-x64.zip/);
  assert.throws(
    () => buildSums([linux(), archive('artifree-0.2.0-windows-x64.zip', 'w')]),
    /different versions/,
  );
  assert.throws(
    () =>
      buildSums([
        linux(),
        {
          ...windows(),
          checksum: `${'0'.repeat(64)}  artifree-0.1.0-windows-x64.zip\n`,
        },
      ]),
    /checksum file of artifree-0.1.0-windows-x64.zip does not match/,
  );
  assert.throws(
    () =>
      buildSums([
        linux(),
        windows(),
        archive('artifree-0.1.0-windows-x64.zip.bak.zip', 'x'),
      ]),
    /Unexpected archive|exactly one/,
  );
});

test('the notes name the image only when given and claim no verification', () => {
  assert.match(
    releaseNotes({ version: '0.1.0', image: 'ghcr.io/o/a@sha256:abc' }),
    /`ghcr.io\/o\/a@sha256:abc`/,
  );
  assert.match(releaseNotes({ version: '0.1.0' }), /Container image: none/);
  assert.match(
    releaseNotes({ version: '0.1.0', image: 'ghcr.io/o/a:0.1.0' }),
    /AFTER this draft was created/,
  );
  assert.match(releaseNotes({ version: '0.1.0' }), /DRAFT/);
});

/** The full, valid release directory as entries. */
function releaseDirectory() {
  const linuxArchive = linux();
  const windowsArchive = windows();
  return [
    { name: linuxArchive.name, bytes: linuxArchive.bytes },
    {
      name: `${linuxArchive.name}.sha256`,
      bytes: Buffer.from(linuxArchive.checksum),
    },
    { name: windowsArchive.name, bytes: windowsArchive.bytes },
    {
      name: `${windowsArchive.name}.sha256`,
      bytes: Buffer.from(windowsArchive.checksum),
    },
    { name: 'RELEASE_NOTES.md', bytes: Buffer.from('notes') },
    {
      name: 'SHA256SUMS',
      bytes: Buffer.from(buildSums([linuxArchive, windowsArchive])),
    },
  ];
}

test('the exact release directory is accepted', () => {
  assertReleaseSet(releaseDirectory());
});

test('a missing, extra, or renamed release file is refused', () => {
  const without = (name) => releaseDirectory().filter((e) => e.name !== name);
  for (const name of [
    'artifree-0.1.0-linux-x64.tar.gz.sha256',
    'artifree-0.1.0-windows-x64.zip',
    'SHA256SUMS',
    'RELEASE_NOTES.md',
  ])
    assert.throws(
      () => assertReleaseSet(without(name)),
      /not exactly the expected assets/,
      name,
    );
  assert.throws(
    () =>
      assertReleaseSet([
        ...releaseDirectory(),
        { name: 'extra.txt', bytes: Buffer.from('x') },
      ]),
    /not exactly the expected assets/,
  );
});

test('a SHA256SUMS or .sha256 file that disagrees with the archives is refused', () => {
  const changed = (name, bytes) =>
    releaseDirectory().map((e) =>
      e.name === name ? { ...e, bytes: Buffer.from(bytes) } : e,
    );
  assert.throws(
    () =>
      assertReleaseSet(
        changed(
          'SHA256SUMS',
          `${'0'.repeat(64)}  artifree-0.1.0-linux-x64.tar.gz\n`,
        ),
      ),
    /SHA256SUMS does not list exactly the archives/,
  );
  assert.throws(
    () =>
      assertReleaseSet(changed('artifree-0.1.0-linux-x64.tar.gz', 'tampered')),
    /checksum file of artifree-0.1.0-linux-x64.tar.gz does not match/,
  );
  assert.throws(
    () =>
      assertReleaseSet(
        releaseDirectory().map((e) =>
          e.name === 'artifree-0.1.0-windows-x64.zip'
            ? { ...e, name: 'artifree-0.2.0-windows-x64.zip' }
            : e,
        ),
      ),
    /different versions/,
  );
});

test('the downloaded build outputs must be archives with their checksum files and nothing else', () => {
  const entries = releaseDirectory().filter(
    (e) => e.name !== 'SHA256SUMS' && e.name !== 'RELEASE_NOTES.md',
  );
  assert.equal(assertDownloadedSet(entries).length, 2);
  assert.throws(
    () =>
      assertDownloadedSet([
        ...entries,
        { name: 'extra.txt', bytes: Buffer.from('x') },
      ]),
    /other than archives and their .sha256/,
  );
  assert.throws(
    () =>
      assertDownloadedSet(
        entries.filter(
          (e) => e.name !== 'artifree-0.1.0-linux-x64.tar.gz.sha256',
        ),
      ),
    /other than archives and their .sha256/,
  );
  assert.throws(
    () =>
      assertDownloadedSet(
        entries.filter((e) => e.name !== 'artifree-0.1.0-linux-x64.tar.gz'),
      ),
    /other than archives and their .sha256/,
  );
});
