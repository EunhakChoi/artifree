import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  assertQualityGate,
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

test('the workspace version is read from its own section', () => {
  assert.equal(
    cargoWorkspaceVersion(
      '[package]\nversion = "9.9.9"\n[workspace.package]\nversion = "0.1.0"\nedition = "2024"\n',
    ),
    '0.1.0',
  );
  assert.throws(
    () => cargoWorkspaceVersion('[package]\nversion = "9.9.9"\n'),
    /No \[workspace.package\]/,
  );
});

const run = (
  conclusion,
  completed_at,
  status = 'completed',
  name = 'Quality gate',
) => ({
  name,
  status,
  conclusion,
  completed_at,
});

test('a commit passes only when its latest completed Quality gate run succeeded', () => {
  assertQualityGate([run('success', '2026-01-01T00:00:00Z')]);
  assertQualityGate([
    run('failure', '2026-01-01T00:00:00Z'),
    run('success', '2026-01-02T00:00:00Z'),
  ]);
  assert.throws(
    () =>
      assertQualityGate([
        run('success', '2026-01-01T00:00:00Z'),
        run('failure', '2026-01-02T00:00:00Z'),
      ]),
    /latest Quality gate run for this commit is failure/,
  );
  assert.throws(
    () => assertQualityGate([run(null, null, 'in_progress')]),
    /No completed Quality gate/,
  );
  assert.throws(
    () =>
      assertQualityGate([
        run('success', '2026-01-01T00:00:00Z', 'completed', 'Checks (ubuntu)'),
      ]),
    /No completed Quality gate/,
  );
  assert.throws(() => assertQualityGate([]), /No completed Quality gate/);
});

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

test('the notes name the image only when one was published and claim no verification', () => {
  assert.match(
    releaseNotes({ version: '0.1.0', image: 'ghcr.io/o/a@sha256:abc' }),
    /`ghcr.io\/o\/a@sha256:abc`/,
  );
  assert.match(releaseNotes({ version: '0.1.0' }), /none was published/);
  assert.match(releaseNotes({ version: '0.1.0' }), /DRAFT/);
});
