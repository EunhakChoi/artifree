import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createServer } from 'node:net';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, before, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { buildServer, startServer } from '../lib/server-process.mjs';

// Decision 0028: ARTIFREE_CONTENT_ORIGIN and ARTIFREE_FRAME_ANCESTORS declare
// what --content-origin and --frame-ancestors do. The Rust unit tests cover the
// precedence rules; these tests run the real binary to show that the values
// reach the preview policy and that bad values stop the process.

const root = fileURLToPath(new URL('../../', import.meta.url));
let binary;
let scratch;

before(async () => {
  binary = buildServer(root);
  scratch = await mkdtemp(join(tmpdir(), 'artifree-container-env-test-'));
});

after(async () => {
  await rm(scratch, { recursive: true, force: true });
});

/** The environment of this process without any setting the tests control. */
function cleanEnvironment(extra = {}) {
  const env = { ...process.env };
  for (const name of Object.keys(env)) {
    if (name.startsWith('ARTIFREE_')) delete env[name];
  }
  return { ...env, ...extra };
}

async function freePort() {
  const probe = createServer();
  await new Promise((resolve) => probe.listen(0, '127.0.0.1', resolve));
  const { port } = probe.address();
  await new Promise((resolve) => probe.close(resolve));
  return port;
}

/** Creates an artifact with the CLI and returns the preview URL on the real listener. */
async function previewHeaders(server, port, name) {
  const site = join(scratch, `${name}-site`);
  await mkdir(site);
  await writeFile(join(site, 'index.html'), '<h1>env</h1>');
  const env = cleanEnvironment({
    ARTIFREE_URL: server.api_url,
    ARTIFREE_TOKEN: server.token,
  });
  const cli = (args) => {
    const result = spawnSync(
      process.execPath,
      ['apps/cli/dist/main.js', 'artifact', ...args],
      { cwd: root, encoding: 'utf8', timeout: 20_000, env },
    );
    assert.equal(result.status, 0, result.stderr);
    return JSON.parse(result.stdout);
  };
  const created = cli(['create', '--title', name, '--dir', site]);
  const preview = cli(['preview', created.artifact.id]);
  // The configured origin names a host that is not listening; the same path
  // on the real listener is what a proxy would forward.
  const url = new URL(preview.url);
  const response = await fetch(`http://127.0.0.1:${port}${url.pathname}`);
  assert.equal(response.status, 200);
  return {
    url: preview.url,
    csp: response.headers.get('content-security-policy'),
  };
}

async function serveWith(name, port, env, args = []) {
  return startServer(
    binary,
    join(scratch, `${name}-data`),
    ['--content-listen', `127.0.0.1:${port}`, ...args],
    { env: cleanEnvironment(env) },
  );
}

test('ARTIFREE_CONTENT_ORIGIN and ARTIFREE_FRAME_ANCESTORS reach the preview policy', async () => {
  const port = await freePort();
  const server = await serveWith('env', port, {
    ARTIFREE_CONTENT_ORIGIN: `http://localhost:${port}`,
    ARTIFREE_FRAME_ANCESTORS: 'https://ui.example.test',
  });
  try {
    assert.equal(server.content_origin, `http://localhost:${port}`);
    const { url, csp } = await previewHeaders(server, port, 'env');
    assert.ok(url.startsWith(`http://localhost:${port}/p/`), url);
    assert.match(csp, new RegExp(`default-src http://localhost:${port} `));
    assert.match(csp, /frame-ancestors https:\/\/ui\.example\.test$/);
  } finally {
    await server.stop();
  }
});

test('flags win over the environment variables in the running server', async () => {
  const port = await freePort();
  const server = await serveWith(
    'flag-wins',
    port,
    {
      ARTIFREE_CONTENT_ORIGIN: 'http://env.example.test:1',
      ARTIFREE_FRAME_ANCESTORS: 'https://env.example.test',
    },
    [
      '--content-origin',
      `http://localhost:${port}`,
      '--frame-ancestors',
      'https://flag.example.test',
    ],
  );
  try {
    assert.equal(server.content_origin, `http://localhost:${port}`);
    const { csp } = await previewHeaders(server, port, 'flag-wins');
    assert.match(csp, /frame-ancestors https:\/\/flag\.example\.test$/);
    assert.doesNotMatch(csp, /env\.example\.test/);
  } finally {
    await server.stop();
  }
});

test('empty variables mean unset and keep the defaults', async () => {
  const port = await freePort();
  const server = await serveWith('empty', port, {
    ARTIFREE_CONTENT_ORIGIN: '',
    ARTIFREE_FRAME_ANCESTORS: '',
  });
  try {
    assert.equal(server.content_origin, `http://127.0.0.1:${port}`);
    const { csp } = await previewHeaders(server, port, 'empty');
    assert.match(csp, /frame-ancestors http:\/\/127\.0\.0\.1:\* /);
  } finally {
    await server.stop();
  }
});

const refused = [
  [
    'an origin with another scheme',
    { ARTIFREE_CONTENT_ORIGIN: 'ftp://preview.example.test' },
    /ARTIFREE_CONTENT_ORIGIN must be a valid HTTP or HTTPS origin/,
  ],
  [
    'an origin with credentials',
    { ARTIFREE_CONTENT_ORIGIN: 'https://user:secret@preview.example.test' },
    /ARTIFREE_CONTENT_ORIGIN must be a valid HTTP or HTTPS origin: origin must not contain credentials/,
  ],
  [
    'the API origin as the preview origin',
    {
      ARTIFREE_API_ORIGIN: 'https://same.example.test',
      ARTIFREE_CONTENT_ORIGIN: 'https://same.example.test',
    },
    /ARTIFREE_CONTENT_ORIGIN must differ from ARTIFREE_API_ORIGIN/,
  ],
  [
    'a frame-ancestors list that starts another directive',
    { ARTIFREE_FRAME_ANCESTORS: "https://ui.example.test; script-src 'none'" },
    /ARTIFREE_FRAME_ANCESTORS must be space-separated sources/,
  ],
  [
    'a blank frame-ancestors list',
    { ARTIFREE_FRAME_ANCESTORS: '  ' },
    /ARTIFREE_FRAME_ANCESTORS must list at least one source/,
  ],
];

// Values that carry a password and fail an earlier check than the credentials
// check (a scheme, a port, a host, a form). No message may quote them.
const PASSWORD = 'hunter2-secret';
const withPassword = (rest) => `${rest.replace('@', `:${PASSWORD}@`)}`;
const leaking = [
  [
    'a credentialed origin with another scheme',
    { ARTIFREE_CONTENT_ORIGIN: withPassword('ftp://user@host.example.test') },
    [],
  ],
  [
    'a credentialed origin with a malformed port',
    {
      ARTIFREE_CONTENT_ORIGIN: withPassword(
        'https://user@host.example.test:notaport',
      ),
    },
    [],
  ],
  [
    'a credentialed API origin with a malformed host',
    { ARTIFREE_API_ORIGIN: withPassword('https://user@[not-an-ip]') },
    [],
  ],
  [
    'a credentialed allowed origin',
    { ARTIFREE_ALLOWED_ORIGINS: withPassword('ftp://user@host.example.test') },
    [],
  ],
  [
    'a credentialed allowed-origin flag',
    {},
    ['--allowed-origin', withPassword('ftp://user@host.example.test')],
  ],
  [
    'a credentialed frame-ancestors list that injects a directive',
    {
      ARTIFREE_FRAME_ANCESTORS: `${withPassword('https://user@host.example.test')}; x`,
    },
    [],
  ],
  [
    'a credentialed origin given as --content-origin=value',
    {},
    [`--content-origin=${withPassword('https://user@host.example.test')}`],
  ],
];
for (const [name, variables, args] of leaking) {
  test(`a refusal of ${name} does not print the password`, () => {
    const result = spawnSync(
      binary,
      ['serve', '--data-dir', join(scratch, 'leak-data'), ...args],
      { encoding: 'utf8', timeout: 20_000, env: cleanEnvironment(variables) },
    );
    assert.equal(result.status, 1, result.stderr);
    assert.ok(result.stderr.length > 0, 'a reason must be printed');
    assert.ok(
      !result.stderr.includes(PASSWORD) && !result.stdout.includes(PASSWORD),
      `the password reached the output: ${result.stderr}`,
    );
  });
}

for (const [name, variables, pattern] of refused) {
  test(`the server refuses to start with ${name}`, () => {
    const result = spawnSync(
      binary,
      ['serve', '--data-dir', join(scratch, 'refused-data')],
      {
        encoding: 'utf8',
        timeout: 20_000,
        env: cleanEnvironment(variables),
      },
    );
    assert.equal(result.status, 1, result.stderr);
    assert.equal(result.stdout, '', 'nothing may be announced as ready');
    assert.match(result.stderr, pattern);
  });
}

test('an allowed origin written differently from the browser origin still matches', async () => {
  const server = await startServer(
    binary,
    join(scratch, 'cors-data'),
    ['--allowed-origin', 'https://UI.example.test:443/'],
    {
      env: cleanEnvironment({
        ARTIFREE_ALLOWED_ORIGINS: 'https://Other.example.test',
      }),
    },
  );
  try {
    const preflight = (origin) =>
      fetch(`${server.api_url}/v1/artifacts`, {
        method: 'OPTIONS',
        headers: {
          Origin: origin,
          'Access-Control-Request-Method': 'GET',
        },
      });
    for (const origin of [
      'https://ui.example.test',
      'https://other.example.test',
    ]) {
      const allowed = await preflight(origin);
      assert.equal(allowed.status, 204, origin);
      assert.equal(allowed.headers.get('access-control-allow-origin'), origin);
    }
    assert.equal(
      (await preflight('https://unlisted.example.test')).status,
      403,
    );
  } finally {
    await server.stop();
  }
});

test('the server refuses an allowed origin that could never match', () => {
  const result = spawnSync(
    binary,
    ['serve', '--data-dir', join(scratch, 'refused-cors-data')],
    {
      encoding: 'utf8',
      timeout: 20_000,
      env: cleanEnvironment({ ARTIFREE_ALLOWED_ORIGINS: '*' }),
    },
  );
  assert.equal(result.status, 1, result.stderr);
  assert.match(
    result.stderr,
    /ARTIFREE_ALLOWED_ORIGINS must list HTTP or HTTPS origins/,
  );
});
