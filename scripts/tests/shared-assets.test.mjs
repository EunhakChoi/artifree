import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

// Decision 0060 rule 10, slice S02f2: `artifree asset ... --shared` names the
// artifact's shared asset space with `space=shared`; without it, a command
// names the caller's own space. The client's routes are pinned in
// asset-mutation-client.test.mjs.

const root = fileURLToPath(new URL('../../', import.meta.url));
const artifact = `art_${'a'.repeat(32)}`;
const asset = 'b'.repeat(64);
const listing = {
  asset_version: 1,
  assets: [],
  bytes: 0,
  max_assets: 64,
  max_bytes: 25 * 1024 * 1024,
};
const removed = {
  asset_id: asset,
  asset_version: 2,
  deleted: true,
  replayed: false,
};

test('the CLI sends --shared as the space of an asset read and write', async () => {
  const seen = [];
  const server = createServer((request, response) => {
    seen.push(`${request.method} ${new URL(request.url, 'http://x').search}`);
    response.setHeader('content-type', 'application/json');
    response.end(JSON.stringify(request.method === 'GET' ? listing : removed));
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  try {
    const address = server.address();
    assert.ok(address !== null && typeof address === 'object');
    const env = {
      ...process.env,
      ARTIFREE_URL: `http://127.0.0.1:${address.port}`,
      ARTIFREE_TOKEN: 'f'.repeat(64),
    };
    delete env.ARTIFREE_TOKEN_FILE;
    const cli = (args) =>
      promisify(execFile)(
        process.execPath,
        ['apps/cli/dist/main.js', 'asset', ...args],
        { cwd: root, env, timeout: 10_000 },
      );
    await cli(['list', artifact, '--shared']);
    await cli([
      'delete',
      artifact,
      asset,
      '--base-asset-version',
      '1',
      '--idempotency-key',
      'cli-shared-delete',
      '--shared',
    ]);
    await cli(['list', artifact]);
    assert.deepEqual(seen, [
      'GET ?space=shared',
      'DELETE ?space=shared',
      'GET ',
    ]);
  } finally {
    server.close();
  }
});
