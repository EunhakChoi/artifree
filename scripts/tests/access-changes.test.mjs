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
  revokeAccess,
  setAccess,
} from '../../packages/client/dist/index.js';

const root = fileURLToPath(new URL('../../', import.meta.url));
const artifactId = 'a'.repeat(26);
const bob = `u-${'b'.repeat(32)}`;
const changed = (version, replayed = false) => ({
  artifact_id: artifactId,
  user_id: bob,
  access_version: version,
  replayed,
});

function clientFor(fetch) {
  return new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:7410',
    accessToken: 'owner-token',
    fetch,
  });
}

test('the client sends a grant and a revocation with their keys and bases', async () => {
  const calls = [];
  const client = clientFor(async (url, init) => {
    calls.push([
      init.method,
      new URL(url).pathname,
      init.headers['idempotency-key'],
      JSON.parse(init.body),
    ]);
    return Response.json(changed(init.method === 'PUT' ? 1 : 2));
  });
  assert.deepEqual(
    await setAccess(
      client,
      artifactId,
      bob,
      { role: 'viewer', baseAccessVersion: 0 },
      { idempotencyKey: 'grant-key-1' },
    ),
    changed(1),
  );
  assert.deepEqual(
    await revokeAccess(client, artifactId, bob, 1, {
      idempotencyKey: 'revoke-key-1',
    }),
    changed(2),
  );
  const path = `/v1/artifacts/${artifactId}/access/${bob}`;
  assert.deepEqual(calls, [
    ['PUT', path, 'grant-key-1', { role: 'viewer', base_access_version: 0 }],
    ['DELETE', path, 'revoke-key-1', { base_access_version: 1 }],
  ]);
});

test('the client refuses an answer for another change and reads a conflict', async () => {
  for (const body of [
    { ...changed(1), user_id: `u-${'c'.repeat(32)}` },
    { ...changed(1), artifact_id: 'b'.repeat(26) },
    { ...changed(1), replayed: 'no' },
    { ...changed(-1) },
  ]) {
    const client = clientFor(async () => Response.json(body));
    await assert.rejects(
      revokeAccess(client, artifactId, bob, 0, { idempotencyKey: 'key-0001' }),
      JSON.stringify(body),
    );
  }
  const conflict = clientFor(async () =>
    Response.json(
      {
        error: {
          code: 'access_conflict',
          message: 'access changed',
          current_access_version: 4,
        },
      },
      { status: 409 },
    ),
  );
  await assert.rejects(
    setAccess(
      conflict,
      artifactId,
      bob,
      { role: 'editor', baseAccessVersion: 1 },
      { idempotencyKey: 'key-0002' },
    ),
    (error) => {
      assert.ok(error instanceof ArtifreeApiError);
      assert.equal(error.code, 'access_conflict');
      assert.equal(error.currentAccessVersion, 4);
      return true;
    },
  );
});

function serve(answer) {
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
        request.headers['idempotency-key'],
        JSON.parse(body),
      ]);
      const [status, json] = answer(request);
      response.statusCode = status;
      response.setHeader('content-type', 'application/json');
      response.end(JSON.stringify(json));
    });
  });
  server.listen(0, '127.0.0.1');
  return { server, requests, listening: once(server, 'listening') };
}

async function cli({ server, listening }, ...args) {
  await listening;
  const address = server.address();
  assert.ok(address !== null && typeof address === 'object');
  const env = {
    ...process.env,
    ARTIFREE_URL: `http://127.0.0.1:${address.port}`,
    ARTIFREE_TOKEN: 'f'.repeat(64),
  };
  delete env.ARTIFREE_TOKEN_FILE;
  return promisify(execFile)(
    process.execPath,
    ['apps/cli/dist/main.js', ...args],
    { cwd: root, env, timeout: 10_000 },
  );
}

test('the CLI grant and revoke send what the routes need', async () => {
  const served = serve((request) => [
    200,
    changed(request.method === 'PUT' ? 1 : 2),
  ]);
  try {
    const granted = await cli(
      served,
      'artifact',
      'access',
      'grant',
      artifactId,
      bob,
      '--role',
      'editor',
      '--base',
      '0',
      '--idempotency-key',
      'cli-grant-1',
    );
    assert.deepEqual(JSON.parse(granted.stdout), changed(1));
    await cli(
      served,
      'artifact',
      'access',
      'revoke',
      artifactId,
      bob,
      '--base',
      '1',
    );
    const path = `/v1/artifacts/${artifactId}/access/${bob}`;
    assert.deepEqual(served.requests[0], [
      'PUT',
      path,
      'cli-grant-1',
      { role: 'editor', base_access_version: 0 },
    ]);
    assert.deepEqual(served.requests[1].slice(0, 2), ['DELETE', path]);
    assert.ok(served.requests[1][2].length >= 8, 'a key is made');
    assert.deepEqual(served.requests[1][3], { base_access_version: 1 });
  } finally {
    served.server.close();
  }
});

test('the CLI prints the current access version of a conflict', async () => {
  const served = serve(() => [
    409,
    {
      error: {
        code: 'access_conflict',
        message: 'access changed',
        current_access_version: 4,
      },
    },
  ]);
  try {
    await assert.rejects(
      cli(
        served,
        'artifact',
        'access',
        'revoke',
        artifactId,
        bob,
        '--base',
        '1',
      ),
      (error) => {
        assert.match(error.stderr, /^error\[access_conflict\]/m);
        assert.match(error.stderr, /^current_access_version=4$/m);
        return true;
      },
    );
  } finally {
    served.server.close();
  }
});

test('the CLI grant and revoke reject bad arguments before any request', () => {
  for (const [args, message] of [
    [['grant', artifactId, bob, '--base', '0'], /grant requires --role/],
    [
      ['grant', artifactId, bob, '--role', 'owner', '--base', '0'],
      /grant requires --role/,
    ],
    [['grant', artifactId, bob, '--role', 'viewer'], /--base must be/],
    [
      ['grant', artifactId, bob, '--role', 'viewer', '--base=-1'],
      /--base must be/,
    ],
    [
      ['revoke', artifactId, '--base', '0'],
      /requires an artifact id and a user id/,
    ],
    [
      ['revoke', artifactId, bob, '--role', 'viewer', '--base', '0'],
      /revoke takes no --role/,
    ],
    [
      ['revoke', artifactId, bob, 'extra', '--base', '0'],
      /requires an artifact id and a user id/,
    ],
  ]) {
    const result = spawnSync(
      process.execPath,
      ['apps/cli/dist/main.js', 'artifact', 'access', ...args],
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
    assert.match(result.stderr, message, args.join(' '));
    assert.doesNotMatch(result.stderr, /Retry with/, args.join(' '));
    assert.equal(result.stdout, '');
  }
});
