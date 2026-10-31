import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, before, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { buildServer, startServer } from '../lib/server-process.mjs';

const root = fileURLToPath(new URL('../../', import.meta.url));
let binary;
let scratch;

before(async () => {
  binary = buildServer(root);
  scratch = await mkdtemp(join(tmpdir(), 'artifree-prod-web-test-'));
});

after(async () => {
  await rm(scratch, { recursive: true, force: true });
});

test('production web assets serve alongside API with SPA fallback and CORS protection', async () => {
  const webDir = join(scratch, 'dist');
  const dataDir = join(scratch, 'data');
  await mkdir(join(webDir, 'assets'), { recursive: true });

  await writeFile(
    join(webDir, 'index.html'),
    '<!doctype html><title>Production Artifree</title>',
    'utf8',
  );
  await writeFile(
    join(webDir, 'assets', 'app.a1b2c3d4.js'),
    'console.log("artifree-production");',
    'utf8',
  );

  const server = await startServer(binary, dataDir, ['--web-dir', webDir]);
  try {
    assert.ok(server.web_url, 'readiness must report web_url');
    assert.equal(server.web_url, server.api_url);

    // 1. Root index.html
    const rootRes = await fetch(`${server.web_url}/`);
    assert.equal(rootRes.status, 200);
    assert.equal(
      rootRes.headers.get('content-type'),
      'text/html; charset=utf-8',
    );
    assert.equal(rootRes.headers.get('cache-control'), 'no-cache');
    const rootHtml = await rootRes.text();
    assert.equal(rootHtml, '<!doctype html><title>Production Artifree</title>');

    // 2. Health check
    const healthRes = await fetch(`${server.web_url}/healthz`);
    assert.equal(healthRes.status, 200);
    assert.equal(await healthRes.text(), 'ok\n');

    // 3. Fingerprinted asset
    const assetRes = await fetch(`${server.web_url}/assets/app.a1b2c3d4.js`);
    assert.equal(assetRes.status, 200);
    assert.match(
      assetRes.headers.get('content-type'),
      /^(?:text|application)\/javascript/,
    );
    assert.equal(
      assetRes.headers.get('cache-control'),
      'public, max-age=31536000, immutable',
    );
    assert.equal(await assetRes.text(), 'console.log("artifree-production");');

    // 4. SPA routing fallback for deep client route without extension
    const spaRes = await fetch(`${server.web_url}/artifacts/art-12345`);
    assert.equal(spaRes.status, 200);
    assert.equal(
      spaRes.headers.get('content-type'),
      'text/html; charset=utf-8',
    );
    assert.equal(
      await spaRes.text(),
      '<!doctype html><title>Production Artifree</title>',
    );

    // 5. Missing file with extension does NOT fall back to index.html
    const missingRes = await fetch(`${server.web_url}/assets/nonexistent.js`);
    assert.equal(missingRes.status, 404);

    // 6. API endpoint on same origin works with auth
    const unauthApi = await fetch(`${server.web_url}/v1/artifacts`);
    assert.equal(unauthApi.status, 401);

    const authApi = await fetch(`${server.web_url}/v1/artifacts`, {
      headers: { Authorization: `Bearer ${server.token}` },
    });
    assert.equal(authApi.status, 200);
    const list = await authApi.json();
    assert.deepEqual(list.artifacts, []);

    // 7. CORS preflight from allowed origin
    const corsRes = await fetch(`${server.web_url}/v1/artifacts`, {
      method: 'OPTIONS',
      headers: {
        Origin: 'http://192.168.1.200:5173',
        'Access-Control-Request-Method': 'GET',
      },
    });
    assert.equal(corsRes.status, 204);
    assert.equal(
      corsRes.headers.get('access-control-allow-origin'),
      'http://192.168.1.200:5173',
    );
    assert.ok(corsRes.headers.get('access-control-expose-headers'));

    // 8. CORS preflight from preview content origin is refused
    const previewCors = await fetch(`${server.web_url}/v1/artifacts`, {
      method: 'OPTIONS',
      headers: {
        Origin: server.content_origin,
        'Access-Control-Request-Method': 'GET',
      },
    });
    assert.equal(previewCors.status, 403);

    // 9. CORS preflight from 'null' origin is refused
    const nullCors = await fetch(`${server.web_url}/v1/artifacts`, {
      method: 'OPTIONS',
      headers: {
        Origin: 'null',
        'Access-Control-Request-Method': 'GET',
      },
    });
    assert.equal(nullCors.status, 403);
  } finally {
    await server.stop();
  }
});

test('runtime replacement of index.html outside web root is rejected with 404', async () => {
  const webDir = join(scratch, 'swap-dist');
  const dataDir = join(scratch, 'swap-data');
  const outsideSecret = join(scratch, 'external-secret.txt');
  await mkdir(webDir, { recursive: true });
  await writeFile(join(webDir, 'index.html'), '<p>original safe</p>', 'utf8');
  await writeFile(outsideSecret, 'SYNTHETIC_EXTERNAL_SECRET', 'utf8');

  const server = await startServer(binary, dataDir, ['--web-dir', webDir]);
  try {
    const { unlink, symlink } = await import('node:fs/promises');
    await unlink(join(webDir, 'index.html'));
    await symlink(outsideSecret, join(webDir, 'index.html'));

    const rootRes = await fetch(`${server.web_url}/`);
    assert.equal(rootRes.status, 404);

    const directRes = await fetch(`${server.web_url}/index.html`);
    assert.equal(directRes.status, 404);

    const spaRes = await fetch(`${server.web_url}/artifacts/test`);
    assert.equal(spaRes.status, 404);
  } finally {
    await server.stop();
  }
});

test('unspecified lan addresses are rejected and bracketed lan is accepted', async () => {
  const { spawnSync } = await import('node:child_process');

  // 1. Unspecified IPv4 0.0.0.0
  const v4Result = spawnSync(
    binary,
    ['serve', '--data-dir', join(scratch, 'unused-data-1'), '--lan', '0.0.0.0'],
    { encoding: 'utf8', timeout: 5000 },
  );
  assert.notEqual(v4Result.status, 0);
  assert.match(v4Result.stderr, /not an unspecified address \(0\.0\.0\.0\)/);

  // 2. Unspecified IPv6 ::
  const v6Result = spawnSync(
    binary,
    ['serve', '--data-dir', join(scratch, 'unused-data-2'), '--lan', '::'],
    { encoding: 'utf8', timeout: 5000 },
  );
  assert.notEqual(v6Result.status, 0);
  assert.match(v6Result.stderr, /not an unspecified address \(::\)/);

  // 3. Unspecified bracketed IPv6 [::]
  const v6BracketResult = spawnSync(
    binary,
    ['serve', '--data-dir', join(scratch, 'unused-data-3'), '--lan', '[::]'],
    { encoding: 'utf8', timeout: 5000 },
  );
  assert.notEqual(v6BracketResult.status, 0);
  assert.match(v6BracketResult.stderr, /not an unspecified address \(\[::\]\)/);

  // 4. Bracketed loopback IPv6 [::1] is accepted
  const bracketV6Server = await startServer(
    binary,
    join(scratch, 'bracket-v6-data'),
    [
      '--lan',
      '[::1]',
      '--api-listen',
      '[::1]:0',
      '--content-listen',
      '[::1]:0',
    ],
  );
  try {
    assert.ok(bracketV6Server.api_url);
    assert.match(bracketV6Server.api_url, /http:\/\/\[::1\]:\d+/);
    assert.match(bracketV6Server.content_origin, /http:\/\/\[::1\]:\d+/);
  } finally {
    await bracketV6Server.stop();
  }
});

test('expanded IPv6 content-origin canonicalization matches compressed browser origin', async () => {
  const dataDir = join(scratch, 'ipv6-canonical-data');
  const server = await startServer(binary, dataDir, [
    '--content-origin',
    'http://[0:0:0:0:0:0:0:1]:80/',
  ]);
  try {
    // Browser sends canonical WHATWG origin http://[::1]
    const preflight = await fetch(`${server.api_url}/v1/artifacts`, {
      method: 'OPTIONS',
      headers: {
        Origin: 'http://[::1]',
        'Access-Control-Request-Method': 'GET',
      },
    });
    // Server must reject with 403 Forbidden because content origin canonicalized to http://[::1]
    assert.equal(preflight.status, 403);
  } finally {
    await server.stop();
  }
});
