import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, mkdir, rm, writeFile, readFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createInterface } from 'node:readline';
import {
  ArtifreeClient,
  encodeFile,
  newIdempotencyKey,
} from '@artifree/client';

async function createFixtureWebDir() {
  const dir = await mkdtemp(join(tmpdir(), 'artifree-fixture-web-'));
  await writeFile(
    join(dir, 'index.html'),
    '<!doctype html><html><head><base href="/"><title>Artifree Fixture</title></head><body><div id="root">App</div></body></html>',
  );
  await mkdir(join(dir, 'assets'), { recursive: true });
  await writeFile(
    join(dir, 'assets', 'index.js'),
    'console.log("fixture-assets");',
  );
  return dir;
}

function startServer(args, env = {}) {
  const serverBin = join(process.cwd(), 'target', 'debug', 'artifree-server');
  const proc = spawn(serverBin, ['serve', ...args], {
    env: { ...process.env, ...env },
    stdio: ['ignore', 'pipe', 'pipe'],
  });

  let stderr = '';
  proc.stderr.on('data', (chunk) => {
    stderr += chunk.toString();
  });

  const readyPromise = new Promise((resolve, reject) => {
    const rl = createInterface({ input: proc.stdout });
    const timer = setTimeout(() => {
      proc.kill('SIGTERM');
      reject(
        new Error(
          `Server start timed out waiting for ready event. Stderr: ${stderr}`,
        ),
      );
    }, 15000);

    rl.on('line', (line) => {
      try {
        const parsed = JSON.parse(line);
        if (parsed.event === 'ready') {
          clearTimeout(timer);
          resolve(parsed);
        }
      } catch {
        // Non-JSON stdout lines ignored
      }
    });

    proc.on('exit', (code) => {
      clearTimeout(timer);
      if (code !== 0 && code !== null) {
        reject(new Error(`Server exited with code ${code}. Stderr: ${stderr}`));
      }
    });
  });

  return { proc, readyPromise };
}

test('Production web serving: assets, SPA fallback, CORS, restart persistence, LAN, and healthz', async (t) => {
  const tempDir = await mkdtemp(join(tmpdir(), 'artifree-prod-web-test-'));
  const fixtureWebDir = await createFixtureWebDir();

  t.after(async () => {
    await rm(tempDir, { recursive: true, force: true });
    await rm(fixtureWebDir, { recursive: true, force: true });
  });

  const dataDir = join(tempDir, 'data');

  // Test 1: Start server with web-dir and random available ports
  {
    const { proc, readyPromise } = startServer([
      '--data-dir',
      dataDir,
      '--api-listen',
      '127.0.0.1:0',
      '--content-listen',
      '127.0.0.1:0',
      '--web-dir',
      fixtureWebDir,
    ]);

    const ready = await readyPromise;
    assert.ok(ready.api_url, 'readiness line must contain api_url');
    assert.ok(ready.web_url, 'readiness line must contain web_url');
    assert.ok(
      ready.content_origin,
      'readiness line must contain content_origin',
    );
    assert.ok(ready.token_file, 'readiness line must contain token_file');

    const token = await readFile(ready.token_file, 'utf8');

    // 1. Root serves index.html
    const rootRes = await fetch(ready.web_url + '/');
    assert.equal(rootRes.status, 200);
    assert.equal(
      rootRes.headers.get('content-type'),
      'text/html; charset=utf-8',
    );
    const rootHtml = await rootRes.text();
    assert.ok(rootHtml.includes('Artifree Fixture'));

    // 2. Static asset serving
    const assetRes = await fetch(ready.web_url + '/assets/index.js');
    assert.equal(assetRes.status, 200);
    assert.equal(
      assetRes.headers.get('content-type'),
      'application/javascript; charset=utf-8',
    );
    assert.equal(
      assetRes.headers.get('cache-control'),
      'public, max-age=31536000, immutable',
    );
    const assetJs = await assetRes.text();
    assert.ok(assetJs.includes('fixture-assets'));

    // 3. SPA Fallback for client route
    const fallbackRes = await fetch(ready.web_url + '/artifacts/art_123');
    assert.equal(fallbackRes.status, 200);
    assert.equal(
      fallbackRes.headers.get('content-type'),
      'text/html; charset=utf-8',
    );
    const fallbackHtml = await fallbackRes.text();
    assert.ok(fallbackHtml.includes('Artifree Fixture'));

    // 3b. Nested subpath asset requests resolve to root asset
    const subpathAssetRes = await fetch(
      ready.web_url + '/artifacts/assets/index.js',
    );
    assert.equal(subpathAssetRes.status, 200);
    assert.equal(
      subpathAssetRes.headers.get('content-type'),
      'application/javascript; charset=utf-8',
    );

    // 4. Missing asset with extension returns 404, not SPA fallback
    const missingAssetRes = await fetch(ready.web_url + '/assets/missing.js');
    assert.equal(missingAssetRes.status, 404);

    // 5. Healthz endpoint: GET and HEAD return 200 OK without token
    const healthzGet = await fetch(ready.web_url + '/healthz');
    assert.equal(healthzGet.status, 200);
    assert.equal(
      healthzGet.headers.get('content-type'),
      'text/plain; charset=utf-8',
    );
    const healthzBody = await healthzGet.text();
    assert.equal(healthzBody, 'ok\n');

    const healthzHead = await fetch(ready.web_url + '/healthz', {
      method: 'HEAD',
    });
    assert.equal(healthzHead.status, 200);

    // 6. CORS preflight on /v1/artifacts
    const corsPreflight = await fetch(ready.web_url + '/v1/artifacts', {
      method: 'OPTIONS',
      headers: {
        origin: 'http://localhost:5173',
        'access-control-request-method': 'GET',
      },
    });
    assert.equal(corsPreflight.status, 204);
    assert.equal(
      corsPreflight.headers.get('access-control-allow-origin'),
      'http://localhost:5173',
    );
    const allowMethods =
      corsPreflight.headers.get('access-control-allow-methods') ?? '';
    assert.ok(allowMethods.includes('PATCH'));
    assert.ok(allowMethods.includes('HEAD'));
    const allowHeaders =
      corsPreflight.headers.get('access-control-allow-headers') ?? '';
    assert.ok(allowHeaders.includes('Last-Event-ID'));
    const exposeHeaders =
      corsPreflight.headers.get('access-control-expose-headers') ?? '';
    assert.ok(exposeHeaders.includes('Content-Range'));

    // 6b. Preflight on unrecognized API endpoint -> 204 with CORS headers
    const unrecPreflight = await fetch(ready.web_url + '/v1/unrecognized', {
      method: 'OPTIONS',
      headers: {
        origin: 'http://localhost:5173',
        'access-control-request-method': 'GET',
      },
    });
    assert.equal(unrecPreflight.status, 204);
    assert.equal(
      unrecPreflight.headers.get('access-control-allow-origin'),
      'http://localhost:5173',
    );

    // 6c. Cross-origin request to unrecognized API endpoint -> 404 JSON with CORS headers
    const unrecRes = await fetch(ready.web_url + '/v1/unrecognized', {
      headers: {
        origin: 'http://localhost:5173',
        authorization: `Bearer ${token.trim()}`,
      },
    });
    assert.equal(unrecRes.status, 404);
    assert.equal(
      unrecRes.headers.get('access-control-allow-origin'),
      'http://localhost:5173',
    );
    const unrecJson = await unrecRes.json();
    assert.equal(unrecJson.error?.code, 'not_found');

    // 6d. Preview origin cannot CORS fetch API
    const previewCors = await fetch(ready.web_url + '/v1/artifacts', {
      method: 'OPTIONS',
      headers: {
        origin: ready.content_origin,
        'access-control-request-method': 'GET',
      },
    });
    assert.equal(previewCors.status, 403);

    // 7. ArtifreeClient performs API operations over web_url
    const client = new ArtifreeClient({
      baseUrl: ready.web_url,
      accessToken: token.trim(),
    });
    const created = await client.createArtifact(
      {
        title: 'Production Web Test',
        files: [encodeFile('index.html', Buffer.from('<h1>Test</h1>'))],
      },
      { idempotencyKey: newIdempotencyKey() },
    );
    assert.ok(created.artifact.id);

    // Gracefully stop server
    proc.kill('SIGTERM');
    await new Promise((res) => proc.on('exit', res));
  }

  // Test 2: Restart server with same data dir; verify persistence
  {
    const { proc, readyPromise } = startServer([
      '--data-dir',
      dataDir,
      '--api-listen',
      '127.0.0.1:0',
      '--content-listen',
      '127.0.0.1:0',
      '--web-dir',
      fixtureWebDir,
    ]);

    const ready = await readyPromise;
    const token = await readFile(ready.token_file, 'utf8');

    const client = new ArtifreeClient({
      baseUrl: ready.web_url,
      accessToken: token.trim(),
    });

    const list = await client.listArtifacts();
    assert.equal(list.artifacts.length, 1);
    assert.equal(list.artifacts[0].title, 'Production Web Test');

    proc.kill('SIGTERM');
    await new Promise((res) => proc.on('exit', res));
  }

  // Test 3: LAN configuration with IPv6 bracketing and content listener override
  {
    const lanDir = join(tempDir, 'lan-data');
    const { proc, readyPromise } = startServer([
      '--data-dir',
      lanDir,
      '--lan',
      '::1',
      '--api-listen',
      '[::1]:0',
      '--content-listen',
      '[::1]:0',
    ]);

    const ready = await readyPromise;
    assert.ok(ready.api_url.includes('[::1]'));
    assert.ok(ready.content_origin.includes('[::1]'));

    proc.kill('SIGTERM');
    await new Promise((res) => proc.on('exit', res));
  }
});
