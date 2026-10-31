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
