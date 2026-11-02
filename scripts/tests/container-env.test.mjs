import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, before, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { ArtifreeClient } from '../../packages/client/dist/index.js';
import { buildServer, freePort, startServer } from '../lib/server-process.mjs';

// The real binary configured the way a container is: by environment
// variables instead of arguments (decision 0028).

const root = fileURLToPath(new URL('../../', import.meta.url));
const SECRET = 'hunter2';
let binary;
let scratch;

before(async () => {
  binary = buildServer(root);
  scratch = await mkdtemp(join(tmpdir(), 'artifree-container-env-'));
});

after(async () => {
  await rm(scratch, { recursive: true, force: true });
});

/** The process environment without anything Artifree reads, plus `extra`. */
function cleanEnv(extra = {}) {
  const env = Object.fromEntries(
    Object.entries(process.env).filter(
      ([name]) => !name.startsWith('ARTIFREE_'),
    ),
  );
  return { ...env, ...extra };
}

/** Runs `serve` with arguments that must stop it before it listens. */
function refuse(args, env = {}) {
  const result = spawnSync(
    binary,
    ['serve', '--data-dir', join(scratch, 'refused'), ...args],
    { env: cleanEnv(env), encoding: 'utf8', timeout: 20_000 },
  );
  assert.equal(result.status, 1, `Expected a refusal: ${args.join(' ')}`);
  assert.equal(result.stdout, '', 'A refused start prints no readiness line.');
  assert.ok(result.stderr.length > 0, 'A refusal explains itself on stderr.');
  assert.ok(
    !`${result.stdout}${result.stderr}`.includes(SECRET),
    `The refusal leaked the secret: ${result.stderr}`,
  );
  return result.stderr;
}

async function previewPolicy(server, key) {
  const client = new ArtifreeClient({
    baseUrl: server.api_url,
    accessToken: server.token,
  });
  const created = await client.createArtifact(
    {
      title: 'Container env',
      files: [{ path: 'index.html', encoding: 'utf8', content: '<p>x</p>' }],
    },
    { idempotencyKey: `env-${key}` },
  );
  const preview = await client.createPreview(created.artifact.id, 1);
  const response = await fetch(preview.url);
  assert.equal(response.status, 200);
  return {
    url: preview.url,
    policy: response.headers.get('content-security-policy') ?? '',
  };
}

test('the preview origin and frame ancestors come from the environment alone', async () => {
  const contentPort = await freePort();
  const server = await startServer(
    binary,
    join(scratch, 'env-only'),
    ['--content-listen', `127.0.0.1:${contentPort}`],
    {
      env: cleanEnv({
        ARTIFREE_CONTENT_ORIGIN: `HTTP://127.0.0.1:${contentPort}/`,
        ARTIFREE_FRAME_ANCESTORS: "https://ui.example   'self'",
      }),
    },
  );
  try {
    assert.equal(server.content_origin, `http://127.0.0.1:${contentPort}`);
    const { url, policy } = await previewPolicy(server, contentPort);
    assert.ok(url.startsWith(`http://127.0.0.1:${contentPort}/`), url);
    assert.match(policy, /frame-ancestors https:\/\/ui\.example 'self'$/);
  } finally {
    await server.stop();
  }
});

test('flags win over the variables, and empty variables are unset', async () => {
  const contentPort = await freePort();
  const server = await startServer(
    binary,
    join(scratch, 'flag-wins'),
    [
      '--content-listen',
      `127.0.0.1:${contentPort}`,
      '--content-origin',
      `http://127.0.0.1:${contentPort}`,
      '--frame-ancestors',
      'https://flag.example',
    ],
    {
      env: cleanEnv({
        ARTIFREE_CONTENT_ORIGIN: 'https://env.example',
        ARTIFREE_FRAME_ANCESTORS: 'https://env.example',
      }),
    },
  );
  try {
    assert.equal(server.content_origin, `http://127.0.0.1:${contentPort}`);
    const { policy } = await previewPolicy(server, contentPort);
    assert.match(policy, /frame-ancestors https:\/\/flag\.example$/);
  } finally {
    await server.stop();
  }

  const emptyPort = await freePort();
  const empty = await startServer(
    binary,
    join(scratch, 'empty-vars'),
    ['--content-listen', `127.0.0.1:${emptyPort}`],
    {
      env: cleanEnv({
        ARTIFREE_CONTENT_ORIGIN: '',
        ARTIFREE_FRAME_ANCESTORS: '',
      }),
    },
  );
  try {
    // Unset, so the default loopback list applies and the origin is the bound one.
    assert.equal(empty.content_origin, `http://127.0.0.1:${emptyPort}`);
    const { policy } = await previewPolicy(empty, emptyPort);
    assert.match(policy, /frame-ancestors http:\/\/127\.0\.0\.1:\*/);
  } finally {
    await empty.stop();
  }
});

const HOSTILE = [
  'ftp://user:hunter2@host',
  'user:hunter2@host',
  'https://user:hunter2@host:65536',
  'https://user:hunter2@host:notaport',
  'https://user:hunter2@',
  'https://user:hunter2@[not-an-ip]',
  'https//user:p%40ss-hunter2@host',
];

test('a bad origin stops the server without repeating it, from every source', () => {
  for (const value of HOSTILE) {
    for (const flag of [
      '--content-origin',
      '--api-origin',
      '--allowed-origin',
    ]) {
      const stderr = refuse([flag, value]);
      assert.ok(stderr.startsWith(flag), `${flag}: ${stderr}`);
    }
    for (const name of [
      'ARTIFREE_CONTENT_ORIGIN',
      'ARTIFREE_API_ORIGIN',
      'ARTIFREE_ALLOWED_ORIGINS',
    ]) {
      const stderr = refuse([], { [name]: value });
      assert.ok(stderr.startsWith(name), `${name}: ${stderr}`);
    }
  }
});

test('an allowed origin with an invalid port stops the server', () => {
  for (const port of ['65536', 'abc', '', '0', '-1', '08080x']) {
    const stderr = refuse(['--allowed-origin', `https://ui.example:${port}`]);
    assert.match(
      stderr,
      /^--allowed-origin must be a valid HTTP or HTTPS origin/,
    );
    refuse([], { ARTIFREE_ALLOWED_ORIGINS: `https://ui.example:${port}` });
  }
});

test('frame ancestors that could end the directive stop the server', () => {
  for (const list of [
    '',
    'https://a.example; script-src *',
    'https://a.example,https://b.example',
  ]) {
    assert.match(
      refuse(['--frame-ancestors', list]),
      /^--frame-ancestors must be/,
    );
  }
  assert.match(
    refuse([], { ARTIFREE_FRAME_ANCESTORS: 'https://a.example;hunter2' }),
    /^ARTIFREE_FRAME_ANCESTORS must be/,
  );
});

test('an unrecognized option is not echoed beyond its name', () => {
  const credentialed = `https://user:${SECRET}@host`;
  for (const argument of [
    `--content-origin=${credentialed}`,
    `--content-origin ${credentialed}`,
    `--content-origin:${credentialed}`,
    `--CONTENT-ORIGIN=${credentialed}`,
    `-content-origin=${credentialed}`,
    credentialed,
    SECRET,
  ]) {
    refuse([argument]);
  }
  assert.match(
    refuse([`--content-origin=${credentialed}`]),
    /serve option --content-origin;/,
  );
});

test('other settings do not repeat a credentialed value either', () => {
  const credentialed = `https://user:${SECRET}@host`;
  for (const flag of [
    '--lan',
    '--api-listen',
    '--loopback-listen',
    '--trusted-proxy',
  ]) {
    refuse([flag, credentialed]);
  }
  for (const name of [
    'ARTIFREE_LAN',
    'ARTIFREE_LOOPBACK_LISTEN',
    'ARTIFREE_TRUSTED_PROXIES',
  ]) {
    refuse([], { [name]: credentialed });
  }
});
