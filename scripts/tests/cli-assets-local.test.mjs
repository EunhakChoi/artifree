import assert from 'node:assert/strict';
import { mkdir, readFile, stat, symlink } from 'node:fs/promises';
import { join } from 'node:path';
import { after, before, test } from 'node:test';
import {
  cleanUp,
  createArtifact,
  makeScratch,
  parseCliSuccess,
  runCli,
  startTestServer,
  writeTestFile,
} from '../lib/cli-assets-harness.mjs';

// Local refusals of `artifree asset`: usage errors (exit 2) and file refusals
// that stop before any upload (exit 1), plus `asset read` refusing an existing
// or dangling --output (exit 1). One real server serves the file. Each test
// makes its own artifact, so every asset space starts at a known version, and
// the expected values are literals or come from an upload receipt.

const FIVE_MIB = 5_242_880;
const LIMITS = { max_assets: 64, max_bytes: 26_214_400 };
// Windows cannot create a symbolic link without extra privileges, so the
// symbolic link tests are skipped there.
const SYMLINK_TEST = {
  skip: process.platform === 'win32' && 'symbolic links need extra privileges',
};

let server;
let scratch;

before(async () => {
  scratch = await makeScratch('artifree-asset-local-');
  server = await startTestServer(scratch);
});

after(() => cleanUp({ server, scratch }));

/** The asset space of an artifact, as `asset list` prints it. */
async function listSpace(artifactId) {
  return parseCliSuccess(await runCli(server, ['asset', 'list', artifactId]));
}

/** Exit 1, nothing on stdout, and exactly `message` on stderr. */
function assertLocalRefusal(result, message) {
  assert.equal(result.status, 1);
  assert.equal(result.stdout, '');
  assert.equal(result.stderr, `${message}\n`);
}

/** Arguments for an upload of `file` as text/plain at base `base`. */
function uploadArgs(artifactId, file, base) {
  return [
    'asset',
    'upload',
    artifactId,
    '--file',
    file,
    '--content-type',
    'text/plain',
    '--base-asset-version',
    base,
  ];
}

test('usage errors exit 2 with empty stdout, name the problem, and send nothing', async (t) => {
  const artifactId = await createArtifact(server, scratch, 'Usage errors');
  const file = await writeTestFile(scratch, 'usage.txt', 'usage input\n');
  const typeOf = (value) => ['--content-type', value];
  const uploadWith = (...flags) => ['asset', 'upload', artifactId, ...flags];
  const fileFlag = ['--file', file];
  const typeFlag = typeOf('text/plain');
  const baseFlag = ['--base-asset-version', '0'];
  const placeholder = '0'.repeat(64);
  // Each row breaks one thing and keeps the rest valid, so the row's own check
  // is the one that refuses.
  const rows = [
    {
      name: 'upload without --file',
      args: uploadWith(...typeFlag, ...baseFlag),
      message: 'upload needs --file <path>',
    },
    {
      name: 'upload without --content-type',
      args: uploadWith(...fileFlag, ...baseFlag),
      message: 'upload requires --content-type <mime>',
    },
    {
      name: 'upload without --base-asset-version',
      args: uploadWith(...fileFlag, ...typeFlag),
      message: '--base-asset-version <n> is required',
    },
    {
      name: 'upload with --base-asset-version=-1',
      args: uploadWith(...fileFlag, ...typeFlag, '--base-asset-version=-1'),
      message: '--base-asset-version <n> is required',
    },
    {
      name: 'upload with --base-asset-version -1',
      args: uploadWith(...fileFlag, ...typeFlag, '--base-asset-version', '-1'),
      message: "Option '--base-asset-version' argument is ambiguous",
    },
    {
      name: 'upload with --file -',
      args: uploadWith('--file', '-', ...typeFlag, ...baseFlag),
      message: '--file - is not supported',
    },
    {
      name: 'upload with --content-type Text/Plain',
      args: uploadWith(...fileFlag, ...typeOf('Text/Plain'), ...baseFlag),
      message: '--content-type must be a lowercase MIME type',
    },
    {
      name: 'upload with --content-type carrying parameters',
      args: uploadWith(
        ...fileFlag,
        ...typeOf('text/plain; charset=utf-8'),
        ...baseFlag,
      ),
      message: '--content-type must be a lowercase MIME type',
    },
    {
      name: 'read without --output',
      args: ['asset', 'read', artifactId, placeholder],
      message: 'read needs --output <file>',
    },
    {
      name: 'list with an extra operand',
      args: ['asset', 'list', artifactId, 'extra'],
      message: 'Expected <artifact-id>',
    },
    {
      name: 'an unknown subcommand',
      args: ['asset', 'frobnicate'],
      message: 'Unknown asset command: frobnicate',
    },
    {
      name: 'delete without --base-asset-version',
      args: ['asset', 'delete', artifactId, placeholder],
      message: '--base-asset-version <n> is required',
    },
  ];
  for (const row of rows) {
    await t.test(row.name, async () => {
      const result = await runCli(server, row.args);
      assert.equal(result.status, 2);
      assert.equal(result.stdout, '');
      assert.ok(result.stderr.includes(row.message), result.stderr);
    });
  }
  // The rows changed nothing: the space is still empty at version 0.
  assert.deepEqual(await listSpace(artifactId), {
    asset_version: 0,
    assets: [],
    bytes: 0,
    ...LIMITS,
  });
});

test('local file refusals exit 1, print the refusal, and leave the asset space unchanged', async (t) => {
  const artifactId = await createArtifact(server, scratch, 'Local refusals');
  const seed = await writeTestFile(scratch, 'seed.txt', 'baseline asset\n');
  const receipt = parseCliSuccess(
    await runCli(server, uploadArgs(artifactId, seed, '0')),
  );
  // One 15-byte asset at version 1. Every refusal below must leave exactly this.
  const space = {
    asset_version: 1,
    assets: [receipt.asset],
    bytes: 15,
    ...LIMITS,
  };
  assert.deepEqual(await listSpace(artifactId), space);

  await t.test('an oversized file is refused before it is read', async () => {
    const oversized = await writeTestFile(
      scratch,
      'five-mib-plus-one.bin',
      Buffer.alloc(FIVE_MIB + 1, 0x61),
    );
    const result = await runCli(server, uploadArgs(artifactId, oversized, '1'));
    assertLocalRefusal(
      result,
      `${oversized} is larger than 5 MiB (5242880 bytes); nothing was sent.`,
    );
    assert.deepEqual(await listSpace(artifactId), space);
  });

  await t.test('a symlink is refused', SYMLINK_TEST, async () => {
    const target = await writeTestFile(scratch, 'target.txt', 'linked\n');
    const link = join(scratch, 'link-to-target.txt');
    await symlink(target, link);
    const result = await runCli(server, uploadArgs(artifactId, link, '1'));
    assertLocalRefusal(result, `Refusing to follow a symbolic link: ${link}`);
    assert.deepEqual(await listSpace(artifactId), space);
  });

  await t.test('a directory is refused', async () => {
    const directory = join(scratch, 'directory-input');
    await mkdir(directory);
    const result = await runCli(server, uploadArgs(artifactId, directory, '1'));
    assertLocalRefusal(result, `Not a regular file: ${directory}`);
    assert.deepEqual(await listSpace(artifactId), space);
  });
});

test('asset read refuses an existing or dangling --output and writes nothing', async (t) => {
  const artifactId = await createArtifact(server, scratch, 'Read outputs');
  const source = await writeTestFile(scratch, 'source.txt', 'asset bytes\n');
  const receipt = parseCliSuccess(
    await runCli(server, uploadArgs(artifactId, source, '0')),
  );
  const readInto = (output) => [
    'asset',
    'read',
    artifactId,
    receipt.asset.id,
    '--output',
    output,
  ];

  await t.test('an existing file is never replaced', async () => {
    const existing = Buffer.from('existing bytes, keep me\n');
    const output = await writeTestFile(scratch, 'existing.txt', existing);
    const result = await runCli(server, readInto(output));
    assertLocalRefusal(
      result,
      `${output} already exists; nothing was written.`,
    );
    assert.deepEqual(await readFile(output), existing);
  });

  await t.test('a dangling symlink is refused', SYMLINK_TEST, async () => {
    const target = join(scratch, 'never-created.txt');
    const link = join(scratch, 'dangling-link.txt');
    await symlink(target, link);
    const result = await runCli(server, readInto(link));
    assertLocalRefusal(result, `${link} already exists; nothing was written.`);
    await assert.rejects(stat(target), { code: 'ENOENT' });
  });
});
