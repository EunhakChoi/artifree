import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFile, stat } from 'node:fs/promises';
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

// The asset commands end to end, over one real server and one artifact, in
// order. Expected values are literals, or come from the input bytes through
// node:crypto. The asset ID is the one value only the server chooses, so the
// later steps reuse the ID and metadata from the upload receipt.

const sha256 = (bytes) => createHash('sha256').update(bytes).digest('hex');
const LIMITS = { max_assets: 64, max_bytes: 26_214_400 };
const FIVE_MIB = 5_242_880;

let server;
let scratch;
let artifactId;

before(async () => {
  scratch = await makeScratch('artifree-asset-lifecycle-');
  server = await startTestServer(scratch);
  artifactId = await createArtifact(server, scratch);
});

after(() => cleanUp({ server, scratch }));

test('The asset lifecycle over a real server: list, upload, read, replay, delete, and exactly 5 MiB', async () => {
  const list = async () =>
    parseCliSuccess(await runCli(server, ['asset', 'list', artifactId]));

  // 1. A new artifact has an empty asset space at version 0.
  assert.deepEqual(await list(), {
    asset_version: 0,
    assets: [],
    bytes: 0,
    ...LIMITS,
  });

  // 2. Upload a CSV at base 0. The receipt and the listing name the asset.
  const csv = Buffer.from('name,score\nada,10\ngrace,12\n');
  const csvPath = await writeTestFile(scratch, 'scores.csv', csv);
  const upload = [
    'asset',
    'upload',
    artifactId,
    '--file',
    csvPath,
    '--content-type',
    'text/csv',
    '--base-asset-version',
    '0',
    '--idempotency-key',
    'asset-lifecycle-upload-1',
  ];
  const first = parseCliSuccess(await runCli(server, upload));
  const { asset } = first;
  assert.equal(first.asset_version, 1);
  assert.equal(first.replayed, false);
  assert.match(asset.id, /^[0-9a-f]{64}$/);
  assert.equal(asset.content_type, 'text/csv');
  assert.equal(asset.size_bytes, csv.byteLength);
  assert.equal(asset.sha256, sha256(csv));
  assert.equal(typeof asset.created_at_ms, 'number');
  assert.deepEqual(await list(), {
    asset_version: 1,
    assets: [asset],
    bytes: csv.byteLength,
    ...LIMITS,
  });

  // 3. Read it back: stdout names the result, and the file is the input.
  const readBack = join(scratch, 'read-back.csv');
  const read = parseCliSuccess(
    await runCli(server, [
      'asset',
      'read',
      artifactId,
      asset.id,
      '--output',
      readBack,
    ]),
  );
  assert.deepEqual(read, {
    artifact_id: artifactId,
    asset_version: 1,
    asset,
    output: readBack,
  });
  assert.ok(
    (await readFile(readBack)).equals(csv),
    'the file read back differs from the uploaded bytes',
  );

  // 4. The identical request replays the original receipt; nothing new is stored.
  assert.deepEqual(parseCliSuccess(await runCli(server, upload)), {
    asset,
    asset_version: 1,
    replayed: true,
  });
  assert.deepEqual(await list(), {
    asset_version: 1,
    assets: [asset],
    bytes: csv.byteLength,
    ...LIMITS,
  });

  // 5. Delete at base 1. The same request replays. A new key at base 2 finds
  // nothing to delete, so the version stays at 2.
  const remove = [
    'asset',
    'delete',
    artifactId,
    asset.id,
    '--base-asset-version',
    '1',
    '--idempotency-key',
    'asset-lifecycle-delete-1',
  ];
  assert.deepEqual(parseCliSuccess(await runCli(server, remove)), {
    asset_id: asset.id,
    asset_version: 2,
    deleted: true,
    replayed: false,
  });
  assert.deepEqual(parseCliSuccess(await runCli(server, remove)), {
    asset_id: asset.id,
    asset_version: 2,
    deleted: true,
    replayed: true,
  });
  assert.deepEqual(
    parseCliSuccess(
      await runCli(server, [
        'asset',
        'delete',
        artifactId,
        asset.id,
        '--base-asset-version',
        '2',
        '--idempotency-key',
        'asset-lifecycle-delete-2',
      ]),
    ),
    { asset_id: asset.id, asset_version: 2, deleted: false, replayed: false },
  );

  // 6. The original upload still replays after the delete, and the asset is
  // not back: its read fails and writes nothing, and the space is empty.
  assert.deepEqual(parseCliSuccess(await runCli(server, upload)), {
    asset,
    asset_version: 1,
    replayed: true,
  });
  const gone = join(scratch, 'gone.csv');
  const failed = await runCli(server, [
    'asset',
    'read',
    artifactId,
    asset.id,
    '--output',
    gone,
  ]);
  assert.equal(failed.status, 1);
  assert.equal(failed.stdout, '');
  assert.match(failed.stderr, /^error\[not_found\]: /);
  await assert.rejects(stat(gone), { code: 'ENOENT' });
  assert.deepEqual(await list(), {
    asset_version: 2,
    assets: [],
    bytes: 0,
    ...LIMITS,
  });

  // 7. A file of exactly 5 MiB is accepted at the current base from list, and
  // its bytes read back unchanged.
  const { asset_version: base } = await list();
  const big = Buffer.from(('a'.repeat(127) + '\n').repeat(40_960));
  assert.equal(big.byteLength, FIVE_MIB, 'the fixture must be exactly 5 MiB');
  const bigPath = await writeTestFile(scratch, 'five-mib.txt', big);
  const bigUpload = parseCliSuccess(
    await runCli(server, [
      'asset',
      'upload',
      artifactId,
      '--file',
      bigPath,
      '--content-type',
      'text/plain',
      '--base-asset-version',
      String(base),
      '--idempotency-key',
      'asset-lifecycle-five-mib-1',
    ]),
  );
  assert.equal(bigUpload.asset_version, base + 1);
  assert.equal(bigUpload.replayed, false);
  assert.equal(bigUpload.asset.content_type, 'text/plain');
  assert.equal(bigUpload.asset.size_bytes, FIVE_MIB);
  assert.equal(bigUpload.asset.sha256, sha256(big));
  const bigOut = join(scratch, 'five-mib-back.txt');
  assert.deepEqual(
    parseCliSuccess(
      await runCli(server, [
        'asset',
        'read',
        artifactId,
        bigUpload.asset.id,
        '--output',
        bigOut,
      ]),
    ),
    {
      artifact_id: artifactId,
      asset_version: base + 1,
      asset: bigUpload.asset,
      output: bigOut,
    },
  );
  assert.ok(
    (await readFile(bigOut)).equals(big),
    'the 5 MiB file read back differs from the uploaded bytes',
  );
});
