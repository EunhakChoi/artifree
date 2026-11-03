import assert from 'node:assert/strict';
import { execFile, spawnSync } from 'node:child_process';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import {
  ArtifreeApiError,
  ArtifreeClient,
  createUser,
  listUsers,
  setUserDisabled,
} from '../../packages/client/dist/index.js';

const root = fileURLToPath(new URL('../../', import.meta.url));
const alice = {
  id: `u-${'a'.repeat(32)}`,
  label: 'Alice',
  created_at: '2026-11-03T00:00:00Z',
  is_owner: false,
};
const owner = {
  id: 'local',
  label: 'Local owner',
  created_at: '2026-11-01T00:00:00Z',
  is_owner: true,
};

function clientFor(fetch) {
  return new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:7410',
    accessToken: 'owner-token',
    fetch,
  });
}

test('the client sends user management calls and checks their answers', async () => {
  const calls = [];
  const client = clientFor(async (url, init) => {
    const { pathname } = new URL(url);
    calls.push([init.method, pathname, init.body]);
    assert.equal(init.headers.authorization, 'Bearer owner-token');
    if (init.method === 'GET') return Response.json({ users: [owner, alice] });
    if (pathname === '/v1/users') return Response.json(alice, { status: 201 });
    return Response.json({ ...alice, disabled_at: '2026-11-03T01:00:00Z' });
  });
  assert.deepEqual((await listUsers(client)).users, [owner, alice]);
  assert.deepEqual(await createUser(client, { label: 'Alice' }), alice);
  const disabled = await setUserDisabled(client, alice.id, true);
  assert.equal(disabled.disabled_at, '2026-11-03T01:00:00Z');
  await setUserDisabled(client, alice.id, false);
  assert.deepEqual(
    calls.map(([method, path]) => `${method} ${path}`),
    [
      'GET /v1/users',
      'POST /v1/users',
      `POST /v1/users/${alice.id}/disable`,
      `POST /v1/users/${alice.id}/enable`,
    ],
  );
  assert.deepEqual(JSON.parse(calls[1][2]), { label: 'Alice' });
});

test('the client refuses an answer that is not a user', async () => {
  const incomplete = {
    id: alice.id,
    label: alice.label,
    created_at: alice.created_at,
  };
  const client = clientFor(async () => Response.json(incomplete));
  await assert.rejects(setUserDisabled(client, alice.id, true));
  const list = clientFor(async () => Response.json({ users: [incomplete] }));
  await assert.rejects(listUsers(list));
});

test('a user token is refused by the server, and the client says so', async () => {
  const client = clientFor(async () =>
    Response.json(
      {
        error: {
          code: 'forbidden',
          message: 'administrative privileges required',
        },
      },
      { status: 403 },
    ),
  );
  await assert.rejects(listUsers(client), (error) => {
    assert.ok(error instanceof ArtifreeApiError);
    assert.equal(error.status, 403);
    return true;
  });
});

test('the CLI user and token commands send what the registry needs', async () => {
  const requests = [];
  const server = createServer((request, response) => {
    let body = '';
    request.on('data', (chunk) => {
      body += chunk;
    });
    request.on('end', () => {
      requests.push([
        request.method,
        request.url,
        request.headers.authorization,
        body === '' ? undefined : JSON.parse(body),
      ]);
      response.setHeader('content-type', 'application/json');
      if (request.url === '/v1/tokens') {
        response.statusCode = 201;
        response.end(
          JSON.stringify({
            token: {
              id: `tok_${'1'.repeat(32)}`,
              user_id: JSON.parse(body).user_id ?? 'local',
              name: JSON.parse(body).name,
              prefix: 'art_0123...',
              created_at: '2026-11-03T00:00:00Z',
              is_admin: false,
            },
            secret: 'art_secret',
          }),
        );
      } else if (request.url === '/v1/users' && request.method === 'GET') {
        response.end(JSON.stringify({ users: [owner, alice] }));
      } else {
        response.statusCode = request.url === '/v1/users' ? 201 : 200;
        response.end(JSON.stringify(alice));
      }
    });
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
    const created = await run('user', 'create', 'Alice', '--json');
    assert.deepEqual(JSON.parse(created.stdout), alice);
    const listed = await run('user', 'list');
    assert.match(listed.stdout, /^local\s+Local owner \[owner\]$/m);
    assert.match(listed.stdout, new RegExp(`^${alice.id}\\s+Alice$`, 'm'));
    await run('user', 'disable', alice.id);
    await run('user', 'enable', alice.id);
    await run('token', 'create', 'agent', '--user', alice.id, '--json');
    const plain = await run('token', 'create', 'agent', '--json');
    assert.equal(JSON.parse(plain.stdout).token.user_id, 'local');
    assert.deepEqual(
      requests.map(([method, url, authorization, body]) => [
        method,
        url,
        authorization,
        body,
      ]),
      [
        ['POST', '/v1/users', `Bearer ${'f'.repeat(64)}`, { label: 'Alice' }],
        ['GET', '/v1/users', `Bearer ${'f'.repeat(64)}`, undefined],
        [
          'POST',
          `/v1/users/${alice.id}/disable`,
          `Bearer ${'f'.repeat(64)}`,
          undefined,
        ],
        [
          'POST',
          `/v1/users/${alice.id}/enable`,
          `Bearer ${'f'.repeat(64)}`,
          undefined,
        ],
        [
          'POST',
          '/v1/tokens',
          `Bearer ${'f'.repeat(64)}`,
          { name: 'agent', user_id: alice.id },
        ],
        ['POST', '/v1/tokens', `Bearer ${'f'.repeat(64)}`, { name: 'agent' }],
      ],
    );
  } finally {
    server.close();
  }
});

test('the CLI user command rejects a missing argument before any request', () => {
  for (const args of [['create'], ['disable'], ['enable'], ['remove', 'x']]) {
    const result = spawnSync(
      process.execPath,
      ['apps/cli/dist/main.js', 'user', ...args],
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
      /requires|Unknown user command/,
      args.join(' '),
    );
    assert.equal(result.stdout, '');
  }
});

test('CLI user --help documents who may manage users', () => {
  const result = spawnSync(
    process.execPath,
    ['apps/cli/dist/main.js', 'user', '--help'],
    { cwd: root, encoding: 'utf8', timeout: 10_000 },
  );
  assert.equal(result.status, 0);
  assert.match(result.stdout, /artifree user create <label>/);
  assert.match(result.stdout, /bootstrap token/);
  assert.match(result.stdout, /no roles and no sharing/);
});
