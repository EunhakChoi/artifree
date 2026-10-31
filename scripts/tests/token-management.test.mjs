import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import {
  ArtifreeApiError,
  ArtifreeClient,
} from '../../packages/client/dist/index.js';

const root = fileURLToPath(new URL('../../', import.meta.url));

function cli(args, env = {}) {
  const result = spawnSync(
    process.execPath,
    ['apps/cli/dist/main.js', 'token', ...args],
    {
      cwd: root,
      encoding: 'utf8',
      env: { ...process.env, ...env },
      timeout: 10_000,
    },
  );
  if (result.error) throw result.error;
  return result;
}

test('CLI token --help outputs usage text', () => {
  const result = cli(['--help']);
  assert.equal(result.status, 0);
  assert.match(result.stdout, /Usage:\s+artifree token list/);
  assert.match(result.stdout, /artifree token create <name>/);
  assert.match(result.stdout, /artifree token rotate <id>/);
  assert.match(result.stdout, /artifree token revoke <id>/);
});

test('ArtifreeClient token methods handle responses and validation correctly', async () => {
  const mockToken = {
    id: 'tok_0123456789abcdef0123456789abcdef',
    name: 'Test Agent',
    prefix: 'art_01234567...',
    created_at: '2026-11-01T00:00:00Z',
    is_admin: false,
    last_used_at: null,
    revoked_at: null,
  };

  // 1. listTokens
  {
    const client = new ArtifreeClient({
      baseUrl: 'http://127.0.0.1:7410',
      accessToken: 'test-token',
      fetch: async (url, init) => {
        assert.equal(init.method, 'GET');
        assert.equal(new URL(url).pathname, '/v1/tokens');
        assert.equal(init.headers.authorization, 'Bearer test-token');
        return Response.json({ tokens: [mockToken] });
      },
    });
    const list = await client.listTokens();
    assert.equal(list.tokens.length, 1);
    assert.deepEqual(list.tokens[0], mockToken);
  }

  // 2. createToken
  {
    const client = new ArtifreeClient({
      baseUrl: 'http://127.0.0.1:7410',
      accessToken: 'test-token',
      fetch: async (url, init) => {
        assert.equal(init.method, 'POST');
        assert.equal(new URL(url).pathname, '/v1/tokens');
        assert.equal(JSON.parse(init.body).name, 'Test Agent');
        return Response.json(
          {
            token: mockToken,
            secret: 'art_secret_value_123',
          },
          { status: 201 },
        );
      },
    });
    const result = await client.createToken({ name: 'Test Agent' });
    assert.equal(result.secret, 'art_secret_value_123');
    assert.equal(result.token.id, mockToken.id);
  }

  // 3. rotateToken
  {
    const client = new ArtifreeClient({
      baseUrl: 'http://127.0.0.1:7410',
      accessToken: 'test-token',
      fetch: async (url, init) => {
        assert.equal(init.method, 'POST');
        assert.equal(
          new URL(url).pathname,
          `/v1/tokens/${mockToken.id}/rotate`,
        );
        return Response.json({
          token: mockToken,
          secret: 'art_new_secret_456',
        });
      },
    });
    const result = await client.rotateToken(mockToken.id);
    assert.equal(result.secret, 'art_new_secret_456');
    assert.equal(result.token.id, mockToken.id);
  }

  // 4. revokeToken
  {
    const client = new ArtifreeClient({
      baseUrl: 'http://127.0.0.1:7410',
      accessToken: 'test-token',
      fetch: async (url, init) => {
        assert.equal(init.method, 'DELETE');
        assert.equal(new URL(url).pathname, `/v1/tokens/${mockToken.id}`);
        return new Response(null, { status: 204 });
      },
    });
    await client.revokeToken(mockToken.id);
  }

  // 5. 403 Forbidden maps to ArtifreeApiError with forbidden code
  {
    const client = new ArtifreeClient({
      baseUrl: 'http://127.0.0.1:7410',
      accessToken: 'agent-token',
      fetch: async () => {
        return Response.json(
          {
            error: {
              code: 'forbidden',
              message: 'administrative privileges required',
            },
          },
          { status: 403 },
        );
      },
    });
    await assert.rejects(
      async () => client.listTokens(),
      (err) => {
        assert.ok(err instanceof ArtifreeApiError);
        assert.equal(err.status, 403);
        assert.equal(err.code, 'forbidden');
        assert.equal(err.message, 'administrative privileges required');
        return true;
      },
    );
  }
});
