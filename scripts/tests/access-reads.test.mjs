import assert from 'node:assert/strict';
import { execFile, spawnSync } from 'node:child_process';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import {
  ArtifreeClient,
  readAccess,
  readMe,
} from '../../packages/client/dist/index.js';

const root = fileURLToPath(new URL('../../', import.meta.url));
const artifactId = 'a'.repeat(26);
const ana = { id: `u-${'a'.repeat(32)}`, label: 'Ana' };
const bob = { id: `u-${'b'.repeat(32)}`, label: 'Bob' };
const owners = {
  artifact_id: artifactId,
  owner: ana,
  members: [{ ...bob, role: 'viewer', disabled: true }],
  access_version: 3,
};

function clientFor(fetch) {
  return new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:7410',
    accessToken: 'user-token',
    fetch,
  });
}

test('the client reads the caller and an access list', async () => {
  const calls = [];
  const client = clientFor(async (url, init) => {
    const { pathname } = new URL(url);
    calls.push(`${init.method} ${pathname}`);
    return Response.json(pathname === '/v1/me' ? ana : owners);
  });
  assert.deepEqual(await readMe(client), ana);
  assert.deepEqual(await readAccess(client, artifactId), owners);
  assert.deepEqual(calls, [
    'GET /v1/me',
    `GET /v1/artifacts/${artifactId}/access`,
  ]);
});

test("a member's answer, with disabled null, reads as such", async () => {
  const members = [{ ...bob, role: 'editor', disabled: null }];
  const client = clientFor(async () => Response.json({ ...owners, members }));
  assert.deepEqual((await readAccess(client, artifactId)).members, members);
});

test('the client refuses an answer that is not an access list', async () => {
  const member = owners.members[0];
  for (const body of [
    { ...owners, artifact_id: 'b'.repeat(26) },
    { ...owners, access_version: -1 },
    { ...owners, owner: { id: ana.id } },
    { ...owners, members: [{ ...member, role: 'owner' }] },
    { ...owners, members: [{ ...member, disabled: 'no' }] },
    { ...owners, members: [{ id: bob.id, label: 'Bob', role: 'viewer' }] },
  ]) {
    const client = clientFor(async () => Response.json(body));
    await assert.rejects(readAccess(client, artifactId), JSON.stringify(body));
  }
  await assert.rejects(readMe(clientFor(async () => Response.json({ id: 1 }))));
});

test('the CLI me and access list commands print what the server answers', async () => {
  const requests = [];
  const server = createServer((request, response) => {
    requests.push(`${request.method} ${request.url}`);
    response.setHeader('content-type', 'application/json');
    response.end(JSON.stringify(request.url === '/v1/me' ? ana : owners));
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
    const run = async (...args) =>
      promisify(execFile)(
        process.execPath,
        ['apps/cli/dist/main.js', ...args],
        {
          cwd: root,
          env,
          timeout: 10_000,
        },
      );
    assert.equal((await run('me')).stdout, `${ana.id} Ana\n`);
    assert.deepEqual(JSON.parse((await run('me', '--json')).stdout), ana);
    const listed = await run('artifact', 'access', 'list', artifactId);
    assert.match(listed.stdout, new RegExp(`^${ana.id}\\s+owner\\s+Ana$`, 'm'));
    assert.match(
      listed.stdout,
      new RegExp(`^${bob.id}\\s+viewer\\s+Bob \\(disabled\\)$`, 'm'),
    );
    assert.match(listed.stdout, /^access_version=3$/m);
    const json = await run('artifact', 'access', 'list', artifactId, '--json');
    assert.deepEqual(JSON.parse(json.stdout), owners);
    assert.deepEqual(requests, [
      'GET /v1/me',
      'GET /v1/me',
      `GET /v1/artifacts/${artifactId}/access`,
      `GET /v1/artifacts/${artifactId}/access`,
    ]);
  } finally {
    server.close();
  }
});

test('the CLI access commands reject bad arguments before any request', () => {
  for (const args of [
    ['me', 'extra'],
    ['artifact', 'access', 'list'],
    ['artifact', 'access', 'list', artifactId, 'extra'],
    ['artifact', 'access', 'grant', artifactId],
  ]) {
    const result = spawnSync(
      process.execPath,
      ['apps/cli/dist/main.js', ...args],
      {
        cwd: root,
        encoding: 'utf8',
        // Nothing listens here: a request would fail with another message.
        env: {
          ...process.env,
          ARTIFREE_URL: 'http://127.0.0.1:9',
          ARTIFREE_TOKEN: 'f'.repeat(64),
        },
        timeout: 10_000,
      },
    );
    assert.notEqual(result.status, 0, args.join(' '));
    assert.match(
      result.stderr,
      /takes no|requires|Unknown access/,
      args.join(' '),
    );
    assert.equal(result.stdout, '');
  }
});

test('CLI access help is printed after the command, and only on its own', () => {
  const cli = (...args) =>
    spawnSync(process.execPath, ['apps/cli/dist/main.js', ...args], {
      cwd: root,
      encoding: 'utf8',
      env: { ...process.env, ARTIFREE_URL: 'http://127.0.0.1:9' },
      timeout: 10_000,
    });
  for (const args of [
    ['artifact', 'access', '--help'],
    ['artifact', 'access', 'list', '--help'],
  ]) {
    const result = cli(...args);
    assert.equal(result.status, 0, args.join(' '));
    assert.match(result.stdout, /artifree artifact access list <id>/);
  }
  assert.match(cli('me', '--help').stdout, /artifree me \[--json\]/);
  for (const args of [
    ['me', '--json', '--help'],
    ['artifact', 'access', 'list', artifactId, '--help'],
  ]) {
    const result = cli(...args);
    assert.notEqual(result.status, 0, args.join(' '));
    assert.match(result.stderr, /on its own/, args.join(' '));
  }
});
