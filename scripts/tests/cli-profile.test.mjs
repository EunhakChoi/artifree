import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { resolveConnection } from '../../apps/cli/dist/profiles.js';

test('CLI profile and connection resolution precedence', async (t) => {
  const tempDir = await mkdtemp(join(tmpdir(), 'artifree-cli-profile-'));

  t.after(async () => {
    await rm(tempDir, { recursive: true, force: true });
  });

  const tokenPath = join(tempDir, 'explicit-token.txt');
  await writeFile(tokenPath, 'explicit-token-content\n');

  const profileTokenPath = join(tempDir, 'profile-token.txt');
  await writeFile(profileTokenPath, 'profile-token-content\n');

  const configPath = join(tempDir, 'profiles.json');
  await writeFile(
    configPath,
    JSON.stringify({
      default_profile: 'default',
      profiles: {
        default: {
          server: 'http://192.168.1.100:7410',
          token_file: 'profile-token.txt',
        },
        custom: {
          server: 'http://custom.example:7410',
          token: 'custom-inline-token',
        },
      },
    }),
  );

  // 1. Explicit CLI arguments take highest priority
  {
    const res = await resolveConnection({
      server: 'http://override.example:7410',
      token: 'cli-token',
      config: configPath,
      profile: 'default',
    });
    assert.equal(res.baseUrl, 'http://override.example:7410');
    assert.equal(res.accessToken, 'cli-token');
  }

  // 2. CLI token-file over env/profile
  {
    const res = await resolveConnection({
      'token-file': tokenPath,
      config: configPath,
    });
    assert.equal(res.baseUrl, 'http://192.168.1.100:7410');
    assert.equal(res.accessToken, 'explicit-token-content');
  }

  // 3. Profile token_file resolved relative to config file directory
  {
    const res = await resolveConnection({
      config: configPath,
      profile: 'default',
    });
    assert.equal(res.baseUrl, 'http://192.168.1.100:7410');
    assert.equal(res.accessToken, 'profile-token-content');
  }

  // 4. Custom named profile
  {
    const res = await resolveConnection({
      config: configPath,
      profile: 'custom',
    });
    assert.equal(res.baseUrl, 'http://custom.example:7410');
    assert.equal(res.accessToken, 'custom-inline-token');
  }

  // 5. Missing token throws UsageError
  {
    await assert.rejects(
      async () => {
        await resolveConnection({});
      },
      {
        name: 'UsageError',
        message: /access token is required/i,
      },
    );
  }

  // 6. Non-existent profile requested via --profile throws UsageError
  {
    await assert.rejects(
      async () => {
        await resolveConnection({
          config: configPath,
          profile: 'nonexistent',
        });
      },
      {
        name: 'UsageError',
        message: /Profile "nonexistent" not found/i,
      },
    );
  }

  // 7. Non-existent default profile throws UsageError
  {
    const brokenConfigPath = join(tempDir, 'broken-default.json');
    await writeFile(
      brokenConfigPath,
      JSON.stringify({
        default_profile: 'missing',
        profiles: {
          prod: { server: 'http://127.0.0.1:7410', token: 'prod-token' },
        },
      }),
    );
    await assert.rejects(
      async () => {
        await resolveConnection({ config: brokenConfigPath });
      },
      {
        name: 'UsageError',
        message: /Profile "missing" not found/i,
      },
    );
  }
});
