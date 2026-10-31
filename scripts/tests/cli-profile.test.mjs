import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import {
  resolveConnection,
  validateProfilesFile,
} from '../../apps/cli/dist/profiles.js';
import { UsageError } from '../../apps/cli/dist/errors.js';

test('validateProfilesFile validates root and profile structure', () => {
  assert.throws(
    () => validateProfilesFile(null, 'test.json'),
    (err) =>
      err instanceof UsageError && err.message.includes('expected root object'),
  );
  assert.throws(
    () => validateProfilesFile('invalid', 'test.json'),
    (err) =>
      err instanceof UsageError && err.message.includes('expected root object'),
  );
  assert.throws(
    () => validateProfilesFile({ default_profile: 123 }, 'test.json'),
    (err) =>
      err instanceof UsageError &&
      err.message.includes('default_profile must be a string'),
  );
  assert.throws(
    () => validateProfilesFile({ profiles: 'not-an-object' }, 'test.json'),
    (err) =>
      err instanceof UsageError &&
      err.message.includes('profiles must be an object'),
  );
  assert.throws(
    () =>
      validateProfilesFile({ profiles: { p1: { server: 123 } } }, 'test.json'),
    (err) =>
      err instanceof UsageError &&
      err.message.includes('invalid server in profile "p1"'),
  );
  assert.throws(
    () =>
      validateProfilesFile({ profiles: { p1: { token: 123 } } }, 'test.json'),
    (err) =>
      err instanceof UsageError &&
      err.message.includes('invalid token in profile "p1"'),
  );
  assert.throws(
    () =>
      validateProfilesFile(
        { profiles: { p1: { token_file: 123 } } },
        'test.json',
      ),
    (err) =>
      err instanceof UsageError &&
      err.message.includes('invalid token_file in profile "p1"'),
  );
});

test('resolveConnection strictly follows precedence and rejects prototype pollution', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'artifree-cli-profile-'));
  const profileFile = join(dir, 'profiles.json');
  const tokenFile1 = join(dir, 'token1.txt');
  const tokenFile2 = join(dir, 'token2.txt');

  await writeFile(tokenFile1, 'tok-file-1\n');
  await writeFile(tokenFile2, 'tok-file-2\n');

  const config = {
    default_profile: 'lan',
    profiles: {
      lan: {
        server: 'http://192.168.1.10:7410',
        token: 'lan-tok',
      },
      file_prof: {
        server: 'http://localhost:7410',
        token_file: tokenFile2,
      },
    },
  };
  await writeFile(profileFile, JSON.stringify(config));

  const origEnv = { ...process.env };
  try {
    process.env.ARTIFREE_PROFILES_FILE = profileFile;
    delete process.env.ARTIFREE_SERVER;
    delete process.env.ARTIFREE_URL;
    delete process.env.ARTIFREE_TOKEN;
    delete process.env.ARTIFREE_TOKEN_FILE;
    delete process.env.ARTIFREE_PROFILE;

    // 1. Default profile from file
    const defRes = await resolveConnection({});
    assert.equal(defRes.server, 'http://192.168.1.10:7410');
    assert.equal(defRes.token, 'lan-tok');

    // 2. Profile with token_file
    const fileRes = await resolveConnection({ profile: 'file_prof' });
    assert.equal(fileRes.token, 'tok-file-2');

    // 3. ENV overrides profile
    process.env.ARTIFREE_SERVER = 'http://env-server:7410';
    process.env.ARTIFREE_TOKEN = 'env-tok';
    const envRes = await resolveConnection({});
    assert.equal(envRes.server, 'http://env-server:7410');
    assert.equal(envRes.token, 'env-tok');

    // 4. CLI flags override ENV
    const cliRes = await resolveConnection({
      server: 'http://cli-server:7410',
      token: 'cli-tok',
    });
    assert.equal(cliRes.server, 'http://cli-server:7410');
    assert.equal(cliRes.token, 'cli-tok');

    // 5. CLI token-file overrides ENV token
    const cliFileRes = await resolveConnection({
      tokenFile: tokenFile1,
    });
    assert.equal(cliFileRes.token, 'tok-file-1');

    // 6. Object.prototype properties (e.g. toString) do not match as profiles
    await assert.rejects(
      () => resolveConnection({ profile: 'toString' }),
      (err) =>
        err instanceof UsageError &&
        err.message.includes('profile "toString" not found'),
    );
    await assert.rejects(
      () => resolveConnection({ profile: 'constructor' }),
      (err) =>
        err instanceof UsageError &&
        err.message.includes('profile "constructor" not found'),
    );
  } finally {
    process.env = origEnv;
    await rm(dir, { recursive: true, force: true });
  }
});

test('resolveConnection reports missing profile file when profile is explicitly requested', async () => {
  const origEnv = { ...process.env };
  try {
    process.env.ARTIFREE_PROFILES_FILE = join(
      tmpdir(),
      'nonexistent-profiles-file.json',
    );
    await assert.rejects(
      () => resolveConnection({ profile: 'dev' }),
      (err) => err instanceof UsageError,
    );
  } finally {
    process.env = origEnv;
  }
});
