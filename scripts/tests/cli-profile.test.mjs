import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

const root = fileURLToPath(new URL('../../', import.meta.url));
const execFileAsync = promisify(execFile);

async function runCli(args, env = {}) {
  const mergedEnv = {
    ...process.env,
    ...env,
  };
  try {
    const { stdout, stderr } = await execFileAsync(
      process.execPath,
      ['apps/cli/dist/main.js', ...args],
      { cwd: root, env: mergedEnv, encoding: 'utf8', timeout: 10_000 },
    );
    return { status: 0, stdout, stderr };
  } catch (error) {
    if ('code' in error && typeof error.code === 'number') {
      return {
        status: error.code,
        stdout: error.stdout ?? '',
        stderr: error.stderr ?? '',
      };
    }
    throw error;
  }
}

test('CLI profile precedence: --server and --token take highest precedence', async () => {
  const tempDir = await mkdtemp(join(tmpdir(), 'artifree-cli-profile-'));
  try {
    const configPath = join(tempDir, 'profiles.json');
    const tokenFile = join(tempDir, 'token.txt');
    await writeFile(tokenFile, 'file-token'.repeat(4));

    // Profile points to 192.0.2.1 (TEST-NET-1, non-routable dummy)
    await writeFile(
      configPath,
      JSON.stringify({
        default_profile: 'dummy',
        profiles: {
          dummy: {
            server: 'http://192.0.2.1:7410',
            token: 'profile-token'.repeat(4),
          },
        },
      }),
    );

    // Mock server to receive the request
    let receivedAuth = '';
    const server = createServer((req, res) => {
      receivedAuth = req.headers['authorization'] ?? '';
      res.setHeader('content-type', 'application/json');
      res.end(JSON.stringify({ artifacts: [] }));
    });
    server.listen(0, '127.0.0.1');
    await once(server, 'listening');

    try {
      const address = server.address();
      const serverUrl = `http://127.0.0.1:${address.port}`;
      const explicitToken = 'explicit-token-'.repeat(4);

      const result = await runCli(
        [
          'artifact',
          'list',
          '--server',
          serverUrl,
          '--token',
          explicitToken,
          '--config',
          configPath,
        ],
        {
          ARTIFREE_URL: 'http://192.0.2.2:7410',
          ARTIFREE_TOKEN: 'env-token'.repeat(4),
        },
      );

      assert.equal(result.status, 0, result.stderr);
      assert.equal(receivedAuth, `Bearer ${explicitToken}`);
    } finally {
      server.close();
    }
  } finally {
    await rm(tempDir, { recursive: true, force: true });
  }
});

test('CLI profile precedence: profile is used when CLI flags and env vars are absent', async () => {
  const tempDir = await mkdtemp(join(tmpdir(), 'artifree-cli-profile-'));
  try {
    let receivedAuth = '';
    const server = createServer((req, res) => {
      receivedAuth = req.headers['authorization'] ?? '';
      res.setHeader('content-type', 'application/json');
      res.end(JSON.stringify({ artifacts: [] }));
    });
    server.listen(0, '127.0.0.1');
    await once(server, 'listening');

    try {
      const address = server.address();
      const serverUrl = `http://127.0.0.1:${address.port}`;
      const profileToken = 'profile-token-'.repeat(4);

      const configPath = join(tempDir, 'profiles.json');
      await writeFile(
        configPath,
        JSON.stringify({
          default_profile: 'dev',
          profiles: {
            dev: {
              server: serverUrl,
              token: profileToken,
            },
            other: {
              server: 'http://192.0.2.3:7410',
              token: 'other-token'.repeat(4),
            },
          },
        }),
      );

      // Call without --server or --token, env cleared of ARTIFREE_URL/ARTIFREE_TOKEN
      const env = { ...process.env };
      delete env.ARTIFREE_URL;
      delete env.ARTIFREE_TOKEN;
      delete env.ARTIFREE_TOKEN_FILE;

      const result = await runCli(
        ['artifact', 'list', '--config', configPath],
        env,
      );

      assert.equal(result.status, 0, result.stderr);
      assert.equal(receivedAuth, `Bearer ${profileToken}`);
    } finally {
      server.close();
    }
  } finally {
    await rm(tempDir, { recursive: true, force: true });
  }
});

test('CLI profile: selecting specific profile with --profile', async () => {
  const tempDir = await mkdtemp(join(tmpdir(), 'artifree-cli-profile-'));
  try {
    let receivedAuth = '';
    const server = createServer((req, res) => {
      receivedAuth = req.headers['authorization'] ?? '';
      res.setHeader('content-type', 'application/json');
      res.end(JSON.stringify({ artifacts: [] }));
    });
    server.listen(0, '127.0.0.1');
    await once(server, 'listening');

    try {
      const address = server.address();
      const serverUrl = `http://127.0.0.1:${address.port}`;
      const lanToken = 'lan-profile-tok'.repeat(4);

      const configPath = join(tempDir, 'profiles.json');
      await writeFile(
        configPath,
        JSON.stringify({
          default_profile: 'dev',
          profiles: {
            dev: {
              server: 'http://192.0.2.4:7410',
              token: 'dev-token'.repeat(4),
            },
            lan: {
              server: serverUrl,
              token: lanToken,
            },
          },
        }),
      );

      const env = { ...process.env };
      delete env.ARTIFREE_URL;
      delete env.ARTIFREE_TOKEN;
      delete env.ARTIFREE_TOKEN_FILE;

      const result = await runCli(
        ['artifact', 'list', '--config', configPath, '--profile', 'lan'],
        env,
      );

      assert.equal(result.status, 0, result.stderr);
      assert.equal(receivedAuth, `Bearer ${lanToken}`);
    } finally {
      server.close();
    }
  } finally {
    await rm(tempDir, { recursive: true, force: true });
  }
});

test('CLI profile: token_file in profile is resolved relative to config file', async () => {
  const tempDir = await mkdtemp(join(tmpdir(), 'artifree-cli-profile-'));
  try {
    let receivedAuth = '';
    const server = createServer((req, res) => {
      receivedAuth = req.headers['authorization'] ?? '';
      res.setHeader('content-type', 'application/json');
      res.end(JSON.stringify({ artifacts: [] }));
    });
    server.listen(0, '127.0.0.1');
    await once(server, 'listening');

    try {
      const address = server.address();
      const serverUrl = `http://127.0.0.1:${address.port}`;
      const relativeToken = 'rel-file-token-'.repeat(4);

      const secretDir = join(tempDir, 'secrets');
      await mkdir(secretDir);
      await writeFile(join(secretDir, 'my.token'), `${relativeToken}\n`);

      const configPath = join(tempDir, 'profiles.json');
      await writeFile(
        configPath,
        JSON.stringify({
          profiles: {
            default: {
              server: serverUrl,
              token_file: 'secrets/my.token',
            },
          },
        }),
      );

      const env = { ...process.env };
      delete env.ARTIFREE_URL;
      delete env.ARTIFREE_TOKEN;
      delete env.ARTIFREE_TOKEN_FILE;

      const result = await runCli(
        ['artifact', 'list', '--config', configPath],
        env,
      );

      assert.equal(result.status, 0, result.stderr);
      assert.equal(receivedAuth, `Bearer ${relativeToken}`);
    } finally {
      server.close();
    }
  } finally {
    await rm(tempDir, { recursive: true, force: true });
  }
});

test('CLI profile errors: missing profile, invalid JSON, missing token, invalid server URL', async () => {
  const tempDir = await mkdtemp(join(tmpdir(), 'artifree-cli-profile-'));
  try {
    const configPath = join(tempDir, 'profiles.json');
    await writeFile(
      configPath,
      JSON.stringify({
        profiles: {
          test: { server: 'http://127.0.0.1:7410', token: 'a'.repeat(64) },
        },
      }),
    );

    const env = { ...process.env };
    delete env.ARTIFREE_URL;
    delete env.ARTIFREE_TOKEN;
    delete env.ARTIFREE_TOKEN_FILE;

    // 1. Missing profile
    let result = await runCli(
      ['artifact', 'list', '--config', configPath, '--profile', 'nonexistent'],
      env,
    );
    assert.equal(result.status, 2);
    assert.match(result.stderr, /Profile "nonexistent" not found/);

    // 2. Malformed config JSON
    const malformedPath = join(tempDir, 'malformed.json');
    await writeFile(malformedPath, '{ not valid json');
    result = await runCli(['artifact', 'list', '--config', malformedPath], env);
    assert.equal(result.status, 2);
    assert.match(result.stderr, /Failed to parse configuration file/);

    // 3. Missing token everywhere
    const noTokenConfig = join(tempDir, 'notoken.json');
    await writeFile(
      noTokenConfig,
      JSON.stringify({
        profiles: { default: { server: 'http://127.0.0.1:7410' } },
      }),
    );
    result = await runCli(['artifact', 'list', '--config', noTokenConfig], env);
    assert.equal(result.status, 2);
    assert.match(result.stderr, /An access token is required/);

    // 4. Invalid server URL
    result = await runCli(
      [
        'artifact',
        'list',
        '--server',
        'ftp://invalid.example',
        '--token',
        'a'.repeat(64),
      ],
      env,
    );
    assert.equal(result.status, 2);
    assert.match(result.stderr, /Invalid server URL/);
  } finally {
    await rm(tempDir, { recursive: true, force: true });
  }
});
