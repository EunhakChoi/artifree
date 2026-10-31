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

  const dummyToken = '0'.repeat(64);
  const tokenFile = join(tempDir, 'test-token');
  await writeFile(tokenFile, dummyToken);

  // 1. Shorthand profile config
  const shorthandConfig = join(tempDir, 'shorthand.json');
  await writeFile(
    shorthandConfig,
    JSON.stringify({
      server: 'http://192.168.1.50:7410',
      token_file: 'test-token',
    }),
  );

  const res1 = await resolveConnection({ config: shorthandConfig });
  assert.equal(res1.baseUrl, 'http://192.168.1.50:7410');
  assert.equal(res1.accessToken, dummyToken);

  // 2. Multi-profile config with default_profile
  const multiConfig = join(tempDir, 'profiles.json');
  await writeFile(
    multiConfig,
    JSON.stringify({
      default_profile: 'lan',
      profiles: {
        local: {
          server: 'http://127.0.0.1:7410',
          token: '1'.repeat(64),
        },
        lan: {
          server: 'http://192.168.1.100:7410',
          token_file: 'test-token',
        },
      },
    }),
  );

  // Default profile used
  const res2 = await resolveConnection({ config: multiConfig });
  assert.equal(res2.baseUrl, 'http://192.168.1.100:7410');
  assert.equal(res2.accessToken, dummyToken);

  // Explicit profile requested via flag
  const res3 = await resolveConnection({
    config: multiConfig,
    profile: 'local',
  });
  assert.equal(res3.baseUrl, 'http://127.0.0.1:7410');
  assert.equal(res3.accessToken, '1'.repeat(64));

  // 3. Flags override profile values
  const res4 = await resolveConnection({
    config: multiConfig,
    profile: 'local',
    server: 'http://custom.local:8080',
    token: '2'.repeat(64),
  });
  assert.equal(res4.baseUrl, 'http://custom.local:8080');
  assert.equal(res4.accessToken, '2'.repeat(64));

  // 4. Missing profile in loaded config throws UsageError
  await assert.rejects(
    async () => {
      await resolveConnection({
        config: multiConfig,
        profile: 'nonexistent',
      });
    },
    {
      name: 'UsageError',
      message: /Profile "nonexistent" not found/i,
    },
  );

  // 5. Missing token throws UsageError
  const noTokenConfig = join(tempDir, 'no-token.json');
  await writeFile(
    noTokenConfig,
    JSON.stringify({ server: 'http://localhost' }),
  );
  const origToken = process.env.ARTIFREE_TOKEN;
  const origTokenFile = process.env.ARTIFREE_TOKEN_FILE;
  delete process.env.ARTIFREE_TOKEN;
  delete process.env.ARTIFREE_TOKEN_FILE;
  try {
    await assert.rejects(
      async () => {
        await resolveConnection({ config: noTokenConfig });
      },
      {
        name: 'UsageError',
        message: /An access token is required/i,
      },
    );
  } finally {
    if (origToken !== undefined) process.env.ARTIFREE_TOKEN = origToken;
    if (origTokenFile !== undefined)
      process.env.ARTIFREE_TOKEN_FILE = origTokenFile;
  }

  // 6. Non-existent config file passed via --config throws UsageError
  await assert.rejects(
    async () => {
      await resolveConnection({ config: join(tempDir, 'does-not-exist.json') });
    },
    {
      name: 'UsageError',
      message: /Configuration file not found/i,
    },
  );

  // 7. Requesting a profile when no config file exists throws UsageError
  {
    const originalEnv = process.env.ARTIFREE_CONFIG_FILE;
    delete process.env.ARTIFREE_CONFIG_FILE;
    try {
      await assert.rejects(
        async () => {
          await resolveConnection({
            profile: 'nonexistent-environment-profile',
          });
        },
        {
          name: 'UsageError',
          message: /no configuration file was found/i,
        },
      );
    } finally {
      if (originalEnv !== undefined) {
        process.env.ARTIFREE_CONFIG_FILE = originalEnv;
      }
    }
  }

  // 8. Runtime validation of malformed / incorrectly shaped profile configs
  {
    // 8a. Config is not an object (e.g. null or array or string)
    const nullConfig = join(tempDir, 'null.json');
    await writeFile(nullConfig, 'null');
    await assert.rejects(
      async () => {
        await resolveConnection({ config: nullConfig });
      },
      {
        name: 'UsageError',
        message: /expected JSON object/i,
      },
    );

    const arrayConfig = join(tempDir, 'array.json');
    await writeFile(arrayConfig, '[]');
    await assert.rejects(
      async () => {
        await resolveConnection({ config: arrayConfig });
      },
      {
        name: 'UsageError',
        message: /expected JSON object/i,
      },
    );

    // 8b. Non-string token in profile
    const badTokenConfig = join(tempDir, 'bad-token.json');
    await writeFile(
      badTokenConfig,
      JSON.stringify({
        profiles: {
          test: {
            token: 12345,
          },
        },
      }),
    );
    await assert.rejects(
      async () => {
        await resolveConnection({ config: badTokenConfig, profile: 'test' });
      },
      {
        name: 'UsageError',
        message: /"token" must be a string/i,
      },
    );

    // 8c. Non-string server in profile
    const badServerConfig = join(tempDir, 'bad-server.json');
    await writeFile(
      badServerConfig,
      JSON.stringify({
        profiles: {
          test: {
            server: true,
          },
        },
      }),
    );
    await assert.rejects(
      async () => {
        await resolveConnection({ config: badServerConfig, profile: 'test' });
      },
      {
        name: 'UsageError',
        message: /"server" must be a string/i,
      },
    );

    // 8d. Non-object profile inside profiles map
    const badProfileEntry = join(tempDir, 'bad-profile-entry.json');
    await writeFile(
      badProfileEntry,
      JSON.stringify({
        profiles: {
          test: 'not-an-object',
        },
      }),
    );
    await assert.rejects(
      async () => {
        await resolveConnection({ config: badProfileEntry, profile: 'test' });
      },
      {
        name: 'UsageError',
        message: /expected object/i,
      },
    );
  }

  // 9. --token-file flag overrides ARTIFREE_TOKEN env var
  {
    const flagTokenFile = join(tempDir, 'flag-token');
    await writeFile(flagTokenFile, 'f'.repeat(64));
    const origToken = process.env.ARTIFREE_TOKEN;
    process.env.ARTIFREE_TOKEN = 'e'.repeat(64);
    try {
      const res = await resolveConnection({
        'token-file': flagTokenFile,
      });
      assert.equal(res.accessToken, 'f'.repeat(64));
    } finally {
      if (origToken !== undefined) process.env.ARTIFREE_TOKEN = origToken;
      else delete process.env.ARTIFREE_TOKEN;
    }
  }

  // 10. Explicit profile requested with a shorthand config (no profiles map) throws UsageError
  {
    await assert.rejects(
      async () => {
        await resolveConnection({
          config: shorthandConfig,
          profile: 'custom-profile',
        });
      },
      {
        name: 'UsageError',
        message: /Profile "custom-profile" not found/i,
      },
    );
  }

  // 11. Prototype property names (toString, constructor) are not treated as profiles
  {
    await assert.rejects(
      async () => {
        await resolveConnection({
          config: multiConfig,
          profile: 'toString',
        });
      },
      {
        name: 'UsageError',
        message: /Profile "toString" not found/i,
      },
    );

    await assert.rejects(
      async () => {
        await resolveConnection({
          config: multiConfig,
          profile: 'constructor',
        });
      },
      {
        name: 'UsageError',
        message: /Profile "constructor" not found/i,
      },
    );
  }
});
