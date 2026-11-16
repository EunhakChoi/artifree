import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { ArtifreeClient } from '../../packages/client/dist/index.js';

// Decision 0060 rule 10: a request names the shared space explicitly, with
// `space=shared`; without it, it names the caller's own space.

const root = fileURLToPath(new URL('../../', import.meta.url));
const id = 'a'.repeat(26);
const read = {
  artifact_id: id,
  path: 'note',
  data_version: 1,
  exists: true,
  value: 'hi',
};
const written = {
  artifact_id: id,
  path: 'note',
  data_version: 2,
  changed: true,
  replayed: false,
};

function clientFor(answer) {
  const requests = [];
  const client = new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:7410',
    accessToken: 'token',
    fetch: async (url, init) => {
      requests.push({ url: new URL(url), method: init?.method ?? 'GET' });
      return Response.json(answer);
    },
  });
  return { client, requests };
}

test('the client names the shared space only when asked', async () => {
  const { client, requests } = clientFor(read);
  await client.getData(id, 'note', { space: 'shared' });
  await client.getData(id, 'note');
  await client.getData(id, undefined, { space: 'shared' });
  assert.deepEqual(
    requests.map(({ url }) => url.search),
    ['?space=shared&path=note', '?path=note', '?space=shared'],
  );
  const { client: writer, requests: writes } = clientFor(written);
  const base = { path: 'note', base_data_version: 1 };
  await writer.setData(
    id,
    { ...base, value: 2 },
    { idempotencyKey: 'shared-set-1', space: 'shared' },
  );
  await writer.updateData(
    id,
    { ...base, patch: 2 },
    { idempotencyKey: 'shared-update-1', space: 'shared' },
  );
  await writer.deleteData(id, base, {
    idempotencyKey: 'shared-delete-1',
    space: 'shared',
  });
  await writer.setData(
    id,
    { ...base, value: 2 },
    { idempotencyKey: 'own-set-1' },
  );
  assert.deepEqual(
    writes.map(({ url, method }) => `${method} ${url.search}`),
    [
      'PUT ?space=shared',
      'PATCH ?space=shared',
      'DELETE ?space=shared',
      'PUT ',
    ],
  );
  const { client: lister, requests: lists } = clientFor({
    artifact_id: id,
    path: '',
    data_version: 1,
    exists: true,
    names: [],
  });
  await lister.listDataMembers(id, { space: 'shared' });
  assert.equal(lists[0].url.searchParams.get('space'), 'shared');
});

test('the CLI sends --shared as the space of a read and a write', async () => {
  const seen = [];
  const server = createServer((request, response) => {
    seen.push(`${request.method} ${new URL(request.url, 'http://x').search}`);
    response.setHeader('content-type', 'application/json');
    response.end(JSON.stringify(request.method === 'GET' ? read : written));
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
        ['apps/cli/dist/main.js', 'data', ...args],
        { cwd: root, env, timeout: 10_000 },
      );
    await cli(['get', id, '--path', 'note', '--shared']);
    await cli([
      'set',
      id,
      '--path',
      'note',
      '--base-data-version',
      '1',
      '--value',
      '2',
      '--idempotency-key',
      'cli-shared-set',
      '--shared',
    ]);
    await cli(['get', id, '--path', 'note']);
    assert.deepEqual(seen, [
      'GET ?space=shared&path=note',
      'PUT ?space=shared',
      'GET ?path=note',
    ]);
  } finally {
    server.close();
  }
});
