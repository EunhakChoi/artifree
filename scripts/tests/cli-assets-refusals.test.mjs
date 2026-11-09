import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { stat } from 'node:fs/promises';
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

// How the CLI reports the server's refusals. One server serves the file. Each
// test makes its own artifact, so each starts with an empty asset space at
// asset_version 0 and passes alone. A refusal exits 1, prints nothing on stdout,
// and starts stderr with error[<code>]. After each refusal, `asset list` must
// show that nothing changed. Expected values are literals, or come from the
// input bytes through node:crypto.

const sha256 = (bytes) => createHash('sha256').update(bytes).digest('hex');
const LIMITS = { max_assets: 64, max_bytes: 26_214_400 };
const EMPTY_SPACE = { asset_version: 0, assets: [], bytes: 0, ...LIMITS };
const NO_SUCH_ASSET = '0'.repeat(64);

let server;
let scratch;

before(async () => {
  scratch = await makeScratch('artifree-asset-refusals-');
  server = await startTestServer(scratch);
});

after(() => cleanUp({ server, scratch }));

/** `asset list` for an artifact, parsed from stdout. */
async function listSpace(artifactId) {
  return parseCliSuccess(await runCli(server, ['asset', 'list', artifactId]));
}

/** The arguments of `asset upload`, with every part the server compares. */
function uploadArgs(artifactId, file, { contentType, base, key }) {
  return [
    'asset',
    'upload',
    artifactId,
    '--file',
    file,
    '--content-type',
    contentType,
    '--base-asset-version',
    String(base),
    '--idempotency-key',
    key,
  ];
}

/** A refusal exits 1, prints nothing on stdout, and starts stderr with the code. */
function assertRefused(result, code) {
  assert.equal(result.status, 1, result.stderr);
  assert.equal(result.stdout, '');
  assert.ok(result.stderr.startsWith(`error[${code}]: `), result.stderr);
}

/** Asserts that stderr has a line that is exactly `current_asset_version=<n>`. */
function assertCurrentVersion(result, version) {
  assert.ok(
    result.stderr.split('\n').includes(`current_asset_version=${version}`),
    result.stderr,
  );
}

test('a stale base refuses an upload and a delete with asset_conflict and the current version', async () => {
  const artifactId = await createArtifact(server, scratch, 'Stale base');
  const first = Buffer.from('first\n');
  const firstPath = await writeTestFile(scratch, 'stale-first.txt', first);
  const receipt = parseCliSuccess(
    await runCli(
      server,
      uploadArgs(artifactId, firstPath, {
        contentType: 'text/plain',
        base: 0,
        key: 'refusal-stale-first',
      }),
    ),
  );
  assert.equal(receipt.asset_version, 1);
  const { asset } = receipt;

  const second = await writeTestFile(scratch, 'stale-second.txt', 'second\n');
  const staleUpload = await runCli(
    server,
    uploadArgs(artifactId, second, {
      contentType: 'text/plain',
      base: 0,
      key: 'refusal-stale-second',
    }),
  );
  assertRefused(staleUpload, 'asset_conflict');
  assertCurrentVersion(staleUpload, 1);

  const staleDelete = await runCli(server, [
    'asset',
    'delete',
    artifactId,
    asset.id,
    '--base-asset-version',
    '0',
    '--idempotency-key',
    'refusal-stale-delete',
  ]);
  assertRefused(staleDelete, 'asset_conflict');
  assertCurrentVersion(staleDelete, 1);

  assert.deepEqual(await listSpace(artifactId), {
    asset_version: 1,
    assets: [asset],
    bytes: first.byteLength,
    ...LIMITS,
  });
});

test('image/gif is refused by the server with asset_type_unsupported and stores nothing', async () => {
  const artifactId = await createArtifact(server, scratch, 'Image type');
  // Plain text declared as a picture: the CLI sends the declared type as given.
  const file = await writeTestFile(scratch, 'picture.txt', 'plain text\n');
  const result = await runCli(
    server,
    uploadArgs(artifactId, file, {
      contentType: 'image/gif',
      base: 0,
      key: 'refusal-image-gif',
    }),
  );
  assertRefused(result, 'asset_type_unsupported');
  assert.deepEqual(await listSpace(artifactId), EMPTY_SPACE);
});

test('plain text declared as image/png is refused by the server with invalid_asset and stores nothing', async () => {
  const artifactId = await createArtifact(server, scratch, 'Image mismatch');
  const file = await writeTestFile(scratch, 'picture.txt', 'plain text\n');
  const result = await runCli(
    server,
    uploadArgs(artifactId, file, {
      contentType: 'image/png',
      base: 0,
      key: 'refusal-image-png',
    }),
  );
  assertRefused(result, 'invalid_asset');
  assert.deepEqual(await listSpace(artifactId), EMPTY_SPACE);
});

test('plain text declared as image/jpeg is refused by the server with invalid_asset and stores nothing', async () => {
  const artifactId = await createArtifact(server, scratch, 'JPEG mismatch');
  const file = await writeTestFile(scratch, 'photo.txt', 'plain text\n');
  const result = await runCli(
    server,
    uploadArgs(artifactId, file, {
      contentType: 'image/jpeg',
      base: 0,
      key: 'refusal-image-jpeg',
    }),
  );
  assertRefused(result, 'invalid_asset');
  assert.deepEqual(await listSpace(artifactId), EMPTY_SPACE);
});

test('text/html is refused by the server with asset_type_unsupported and stores nothing', async () => {
  const artifactId = await createArtifact(server, scratch, 'HTML type');
  const file = await writeTestFile(scratch, 'page.txt', 'plain text\n');
  const result = await runCli(
    server,
    uploadArgs(artifactId, file, {
      contentType: 'text/html',
      base: 0,
      key: 'refusal-text-html',
    }),
  );
  assertRefused(result, 'asset_type_unsupported');
  assert.deepEqual(await listSpace(artifactId), EMPTY_SPACE);
});

test('an empty file is refused by the server with invalid_asset and stores nothing', async () => {
  const artifactId = await createArtifact(server, scratch, 'Empty file');
  const file = await writeTestFile(scratch, 'empty.txt', Buffer.alloc(0));
  const result = await runCli(
    server,
    uploadArgs(artifactId, file, {
      contentType: 'text/plain',
      base: 0,
      key: 'refusal-empty-file',
    }),
  );
  assertRefused(result, 'invalid_asset');
  assert.deepEqual(await listSpace(artifactId), EMPTY_SPACE);
});

test('the same key with a changed file is refused with request_key_reused, and the first upload stands', async () => {
  const artifactId = await createArtifact(server, scratch, 'Key reuse');
  const first = Buffer.from('alpha\n');
  // The same length as `first`, so the only difference is the content.
  const changed = Buffer.from('bravo\n');
  const firstPath = await writeTestFile(scratch, 'key-first.txt', first);
  const changedPath = await writeTestFile(scratch, 'key-changed.txt', changed);
  const receipt = parseCliSuccess(
    await runCli(
      server,
      uploadArgs(artifactId, firstPath, {
        contentType: 'text/plain',
        base: 0,
        key: 'refusal-key-reuse',
      }),
    ),
  );
  assert.equal(receipt.asset_version, 1);
  assert.equal(receipt.asset.sha256, sha256(first));

  // Base 0 is the base of the first call, so the request differs from it only
  // in its file content.
  const reused = await runCli(
    server,
    uploadArgs(artifactId, changedPath, {
      contentType: 'text/plain',
      base: 0,
      key: 'refusal-key-reuse',
    }),
  );
  assertRefused(reused, 'request_key_reused');
  assert.deepEqual(await listSpace(artifactId), {
    asset_version: 1,
    assets: [receipt.asset],
    bytes: first.byteLength,
    ...LIMITS,
  });
});

test('reading an asset that is not in the space fails with not_found and creates no file', async () => {
  const artifactId = await createArtifact(server, scratch, 'Missing read');
  const present = Buffer.from('present\n');
  const presentPath = await writeTestFile(scratch, 'present.txt', present);
  const receipt = parseCliSuccess(
    await runCli(
      server,
      uploadArgs(artifactId, presentPath, {
        contentType: 'text/plain',
        base: 0,
        key: 'refusal-missing-read',
      }),
    ),
  );
  assert.equal(receipt.asset_version, 1);

  const output = join(scratch, 'never-created.txt');
  const result = await runCli(server, [
    'asset',
    'read',
    artifactId,
    NO_SUCH_ASSET,
    '--output',
    output,
  ]);
  assertRefused(result, 'not_found');
  await assert.rejects(stat(output), { code: 'ENOENT' });
  assert.deepEqual(await listSpace(artifactId), {
    asset_version: 1,
    assets: [receipt.asset],
    bytes: present.byteLength,
    ...LIMITS,
  });
});
