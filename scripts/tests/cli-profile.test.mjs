import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import {
  resolveConnection,
  validateProfilesFile,
} from '../../apps/cli/dist/profiles.js';
import { UsageError } from '../../apps/cli/dist/errors.js';

const root = fileURLToPath(new URL('../../', import.meta.url));

test('validateProfilesFile accepts valid structure and rejects invalid shapes', () => {
  const valid = {
    default_profile: 'local',
    profiles: {
      local: {
        server: 'http://127.0.0.1:7410',
        token: 'valid-token',
      },
    },
  };
  assert.deepEqual(validateProfilesFile(valid, 'test.json'), valid);

  // Non-object root
  assert.throws(
    () => validateProfilesFile('not an object', 'test.json'),
    (err) =>
      err instanceof UsageError && err.message.includes('expected root object'),
  );

  // default_profile not a string
  assert.throws(
    () => validateProfilesFile({ default_profile: 123 }, 'test.json'),
    (err) =>
      err instanceof UsageError &&
      err.message.includes('default_profile must be a string'),
  );

  // profiles not an object
  assert.throws(
    () => validateProfilesFile({ profiles: [] }, 'test.json'),
    (err) =>
      err instanceof UsageError &&
      err.message.includes('profiles must be an object'),
  );

  // profile not an object
  assert.throws(
    () => validateProfilesFile({ profiles: { bad: 'string' } }, 'test.json'),
    (err) =>
      err instanceof UsageError && err.message.includes('expected object'),
  );

  // invalid field types
  assert.throws(
    () =>
      validateProfilesFile({ profiles: { p: { server: 123 } } }, 'test.json'),
    (err) =>
      err instanceof UsageError && err.message.includes('invalid server'),
  );
  assert.throws(
    () =>
      validateProfilesFile({ profiles: { p: { token: 123 } } }, 'test.json'),
    (err) => err instanceof UsageError && err.message.includes('invalid token'),
  );
  assert.throws(
    () =>
      validateProfilesFile(
        { profiles: { p: { token_file: 123 } } },
        'test.json',
      ),
    (err) =>
      err instanceof UsageError && err.message.includes('invalid token_file'),
  );
});

test('checkout artifree-profiles.json is NEVER loaded without explicit --profile-file or ARTIFREE_PROFILES_FILE', async () => {
  const tempDir = await mkdtemp(join(tmpdir(), 'artifree-profile-test-'));
  const origCwd = process.cwd();
  const origEnv = { ...process.env };

  try {
    delete process.env.ARTIFREE_SERVER;
    delete process.env.ARTIFREE_URL;
    delete process.env.ARTIFREE_TOKEN;
    delete process.env.ARTIFREE_TOKEN_FILE;
    delete process.env.ARTIFREE_PROFILES_FILE;

    process.chdir(tempDir);

    // Create an untrusted checkout profile
    const untrustedConfig = {
      default_profile: 'attacker',
      profiles: {
        attacker: {
          server: 'http://malicious.example:9999',
          token: 'exfiltrated-token',
        },
      },
    };
    await writeFile(
      join(tempDir, 'artifree-profiles.json'),
      JSON.stringify(untrustedConfig),
      'utf8',
    );

    // 1. Without profile arguments: auto-activation MUST NOT read working-tree file!
    await assert.rejects(
      async () => {
        await resolveConnection({});
      },
      (err) =>
        err instanceof UsageError &&
        err.message.includes('authentication required'),
    );

    // 2. With --profile attacker BUT WITHOUT --profile-file: MUST NOT read working-tree file!
    await assert.rejects(
      async () => {
        await resolveConnection({ profile: 'attacker' });
      },
      (err) =>
        err instanceof UsageError &&
        err.message.includes('profile configuration file not found'),
    );

    // 3. With explicit --profile-file: loads checkout profile with explicit consent
    const resolved = await resolveConnection({
      profile: 'attacker',
      profileFile: './artifree-profiles.json',
    });
    assert.equal(resolved.server, 'http://malicious.example:9999');
    assert.equal(resolved.token, 'exfiltrated-token');

    // 4. With explicit --profile-file without --profile: loads default_profile
    const resolvedDefault = await resolveConnection({
      profileFile: './artifree-profiles.json',
    });
    assert.equal(resolvedDefault.server, 'http://malicious.example:9999');
    assert.equal(resolvedDefault.token, 'exfiltrated-token');
  } finally {
    process.chdir(origCwd);
    for (const key of Object.keys(process.env)) {
      if (!(key in origEnv)) delete process.env[key];
    }
    Object.assign(process.env, origEnv);
    await rm(tempDir, { recursive: true, force: true });
  }
});

test('resolveConnection follows strict precedence: CLI > Env > Profile > Default', async () => {
  const tempDir = await mkdtemp(join(tmpdir(), 'artifree-prec-test-'));
  const origEnv = { ...process.env };

  try {
    const profileFile = join(tempDir, 'profiles.json');
    const profileConfig = {
      default_profile: 'dev',
      profiles: {
        dev: {
          server: 'http://profile.local:7410',
          token: 'profile-token',
        },
      },
    };
    await writeFile(profileFile, JSON.stringify(profileConfig), 'utf8');
    process.env.ARTIFREE_PROFILES_FILE = profileFile;

    // 1. Profile default values
    const fromProfile = await resolveConnection({});
    assert.equal(fromProfile.server, 'http://profile.local:7410');
    assert.equal(fromProfile.token, 'profile-token');

    // 2. Env overrides profile
    process.env.ARTIFREE_SERVER = 'http://env.local:7410';
    process.env.ARTIFREE_TOKEN = 'env-token';
    const fromEnv = await resolveConnection({});
    assert.equal(fromEnv.server, 'http://env.local:7410');
    assert.equal(fromEnv.token, 'env-token');

    // 3. CLI options override Env
    const fromCli = await resolveConnection({
      server: 'http://cli.local:7410',
      token: 'cli-token',
    });
    assert.equal(fromCli.server, 'http://cli.local:7410');
    assert.equal(fromCli.token, 'cli-token');
  } finally {
    for (const key of Object.keys(process.env)) {
      if (!(key in origEnv)) delete process.env[key];
    }
    Object.assign(process.env, origEnv);
    await rm(tempDir, { recursive: true, force: true });
  }
});

test('CLI help documents connection options including --profile and --profile-file', () => {
  const result = spawnSync(
    process.execPath,
    ['apps/cli/dist/main.js', 'artifact', '--help'],
    { cwd: root, encoding: 'utf8', timeout: 10_000 },
  );
  assert.equal(result.status, 0);
  assert.match(result.stdout, /--token <val>/);
  assert.match(result.stdout, /--profile <name>/);
  assert.match(result.stdout, /--profile-file <path>/);
  assert.match(result.stdout, /--server <url>/);
  assert.match(result.stdout, /--token-file <path>/);
});
