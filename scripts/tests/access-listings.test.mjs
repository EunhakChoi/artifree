import assert from 'node:assert/strict';
import { execFile, spawnSync } from 'node:child_process';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { ArtifreeClient } from '../../packages/client/dist/index.js';

// Decision 0060 rules 6 and 7: an artifact carries the caller's role, and a
// listed one its owner; the CLI's --owned and --shared select the list.

const root = fileURLToPath(new URL('../../', import.meta.url));
const listed = {
  id: 'a'.repeat(26),
  kind: 'app',
  type: null,
  title: 'Shared',
  pinned: false,
  current_revision: 1,
  metadata_revision: 1,
  deleted_at_ms: null,
  created_at_ms: 1,
  updated_at_ms: 2,
  role: 'viewer',
  owner: { id: `u-${'b'.repeat(32)}`, label: 'Ana' },
};

function clientFor(answer) {
  const urls = [];
  const client = new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:7410',
    accessToken: 'token',
    fetch: async (url) => {
      urls.push(new URL(url));
      return Response.json(answer);
    },
  });
  return { client, urls };
}

test('the client keeps the role and the owner, and sends the filter', async () => {
  const { client, urls } = clientFor({ artifacts: [listed] });
  assert.deepEqual(await client.listArtifacts({ access: 'shared' }), {
    artifacts: [listed],
  });
  assert.equal(urls[0].searchParams.get('access'), 'shared');
  const { role: _role, owner: _owner, ...plain } = listed;
  const { client: written } = clientFor({ artifacts: [plain] });
  assert.deepEqual(await written.listArtifacts(), { artifacts: [plain] });
  const { client: read } = clientFor({ ...plain, role: 'owner' });
  assert.equal((await read.getArtifact(listed.id)).role, 'owner');
});

test('the client refuses a malformed role or owner', async () => {
  for (const wrong of [
    { role: 'admin' },
    { role: null },
    { owner: { id: 'x' } },
    { owner: { id: 1, label: 'Ana' } },
    { owner: 'Ana' },
  ]) {
    const { client } = clientFor({ artifacts: [{ ...listed, ...wrong }] });
    await assert.rejects(client.listArtifacts(), JSON.stringify(wrong));
  }
});

test('the CLI list sends --shared and --owned as the filter', async () => {
  const queries = [];
  const server = createServer((request, response) => {
    queries.push(new URL(request.url, 'http://x').searchParams.get('access'));
    response.setHeader('content-type', 'application/json');
    response.end(JSON.stringify({ artifacts: [listed] }));
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
    for (const flag of ['--shared', '--owned']) {
      const { stdout } = await promisify(execFile)(
        process.execPath,
        ['apps/cli/dist/main.js', 'artifact', 'list', flag],
        { cwd: root, env, timeout: 10_000 },
      );
      assert.deepEqual(JSON.parse(stdout), { artifacts: [listed] });
    }
    assert.deepEqual(queries, ['shared', 'owned']);
  } finally {
    server.close();
  }
});

test('the CLI list refuses --owned with --shared before any request', () => {
  const result = spawnSync(
    process.execPath,
    ['apps/cli/dist/main.js', 'artifact', 'list', '--owned', '--shared'],
    {
      cwd: root,
      encoding: 'utf8',
      env: {
        ...process.env,
        ARTIFREE_URL: 'http://127.0.0.1:9',
        ARTIFREE_TOKEN: 'f'.repeat(64),
      },
      timeout: 10_000,
    },
  );
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /Use --owned or --shared, not both\./);
  assert.equal(result.stdout, '');
});
