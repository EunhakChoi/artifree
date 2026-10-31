import assert from 'node:assert/strict';
import { request as httpRequest } from 'node:http';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

import {
  ArtifreeClient,
  encodeFile,
  newIdempotencyKey,
} from '@artifree/client';
import { buildServer, startServer } from '../lib/server-process.mjs';

const root = fileURLToPath(new URL('../../', import.meta.url));
const serverBinary = buildServer(root);

let fixtureWebDir = null;

async function getWebDir() {
  if (!fixtureWebDir) {
    fixtureWebDir = await mkdtemp(join(tmpdir(), 'artifree-prod-web-fixture-'));
    await mkdir(join(fixtureWebDir, 'assets'), { recursive: true });
    await writeFile(
      join(fixtureWebDir, 'index.html'),
      '<!doctype html><html><head><meta charset="utf-8"><title>Artifree</title></head><body><div id="root">Artifree Web</div></body></html>',
      'utf8',
    );
    await writeFile(
      join(fixtureWebDir, 'assets', 'app.12345678.js'),
      'console.log("artifree");',
      'utf8',
    );
  }
  return fixtureWebDir;
}

test.after(async () => {
  if (fixtureWebDir) {
    await rm(fixtureWebDir, { recursive: true, force: true });
  }
});

test('Production web serving: web_url emitted, assets served, SPA fallback, API errors remain JSON', async () => {
  const webDir = await getWebDir();
  const dataDir = await mkdtemp(join(tmpdir(), 'artifree-prod-web-'));
  const server = await startServer(serverBinary, dataDir, [
    '--web-dir',
    webDir,
  ]);

  try {
    assert.ok(
      server.web_url !== undefined,
      'readiness line must contain web_url',
    );
    assert.equal(server.web_url, server.api_url);

    // 1. Root / serves index.html without authentication
    const rootRes = await fetch(server.web_url + '/');
    assert.equal(rootRes.status, 200);
    assert.equal(
      rootRes.headers.get('content-type'),
      'text/html; charset=utf-8',
    );
    assert.equal(rootRes.headers.get('x-content-type-options'), 'nosniff');
    const rootHtml = await rootRes.text();
    assert.ok(rootHtml.includes('<html'), 'must serve HTML');

    // 2. SPA fallback for client routes (no file extension)
    for (const route of ['/artifacts', '/artifacts/art_sample', '/trash']) {
      const spaRes = await fetch(server.web_url + route);
      assert.equal(spaRes.status, 200, `failed on ${route}`);
      assert.equal(
        spaRes.headers.get('content-type'),
        'text/html; charset=utf-8',
      );
      const spaHtml = await spaRes.text();
      assert.equal(
        spaHtml,
        rootHtml,
        `SPA fallback must serve index.html on ${route}`,
      );
    }

    // 3. Missing asset with extension returns 404 (not SPA fallback index.html)
    const missingRes = await fetch(server.web_url + '/missing.js');
    assert.equal(missingRes.status, 404);
    const missingText = await missingRes.text();
    assert.ok(
      !missingText.includes('<html'),
      'must not serve index.html for missing asset',
    );

    // 4. API routes never fallback to SPA and support CORS for cross-origin web apps
    const unauthApiRes = await fetch(server.web_url + '/v1/artifacts');
    assert.equal(unauthApiRes.status, 401);
    assert.equal(unauthApiRes.headers.get('content-type'), 'application/json');
    const unauthJson = await unauthApiRes.json();
    assert.equal(unauthJson.error?.code, 'unauthenticated');

    const notFoundApiRes = await fetch(server.web_url + '/v1/nonexistent', {
      headers: { authorization: `Bearer ${server.token}` },
    });
    assert.equal(notFoundApiRes.status, 404);
    assert.equal(
      notFoundApiRes.headers.get('content-type'),
      'application/json',
    );
    const notFoundJson = await notFoundApiRes.json();
    assert.equal(notFoundJson.error?.code, 'not_found');

    // 4a. CORS preflight OPTIONS to /v1/artifacts
    const preflightRes = await fetch(server.web_url + '/v1/artifacts', {
      method: 'OPTIONS',
      headers: {
        origin: 'http://localhost:5173',
        'access-control-request-method': 'POST',
        'access-control-request-headers': 'authorization, content-type',
      },
    });
    assert.equal(preflightRes.status, 204);
    assert.equal(
      preflightRes.headers.get('access-control-allow-origin'),
      'http://localhost:5173',
    );
    assert.ok(
      preflightRes.headers
        .get('access-control-allow-methods')
        ?.includes('POST'),
    );

    // 4b. Cross-origin rejection for sandbox preview origin and null origin
    const sandboxCorsRes = await fetch(server.web_url + '/v1/artifacts', {
      headers: {
        origin: server.content_origin,
        authorization: `Bearer ${server.token}`,
      },
    });
    assert.equal(sandboxCorsRes.status, 403);

    const nullCorsRes = await fetch(server.web_url + '/v1/artifacts', {
      headers: {
        origin: 'null',
        authorization: `Bearer ${server.token}`,
      },
    });
    assert.equal(nullCorsRes.status, 403);

    // 5. End-to-end API operations using ArtifreeClient over web_url
    const client = new ArtifreeClient({
      baseUrl: server.web_url,
      accessToken: server.token,
    });

    const created = await client.createArtifact(
      {
        title: 'Production Web Test',
        files: [encodeFile('index.html', Buffer.from('<p>Web Production</p>'))],
      },
      { idempotencyKey: newIdempotencyKey() },
    );
    assert.equal(created.artifact.title, 'Production Web Test');

    const fetched = await client.getArtifact(created.artifact.id);
    assert.equal(fetched.title, 'Production Web Test');

    // Preview
    const preview = await client.createPreview(created.artifact.id, 1);
    assert.ok(preview.url.startsWith(server.content_origin));

    const previewRes = await fetch(preview.url);
    assert.equal(previewRes.status, 200);
    const previewHtml = await previewRes.text();
    assert.ok(previewHtml.includes('Web Production'));
    const csp = previewRes.headers.get('content-security-policy') ?? '';
    assert.ok(
      csp.includes(
        'sandbox allow-scripts allow-forms allow-modals allow-downloads',
      ),
    );
    assert.ok(csp.includes('frame-ancestors'));
  } finally {
    await server.stop();
    await rm(dataDir, { recursive: true, force: true });
  }
});

test('Production web serving: restart persistence', async () => {
  const dataDir = await mkdtemp(join(tmpdir(), 'artifree-prod-restart-'));
  const webDir = await getWebDir();
  let artifactId;

  // Run 1: create artifact
  {
    const server = await startServer(serverBinary, dataDir, [
      '--web-dir',
      webDir,
    ]);
    try {
      const client = new ArtifreeClient({
        baseUrl: server.web_url,
        accessToken: server.token,
      });
      const created = await client.createArtifact(
        {
          title: 'Persistent Artifact',
          files: [encodeFile('index.html', Buffer.from('<p>Persistent</p>'))],
        },
        { idempotencyKey: newIdempotencyKey() },
      );
      artifactId = created.artifact.id;
    } finally {
      await server.stop();
    }
  }

  // Run 2: start server again with same data-dir, verify artifact is intact
  {
    const server = await startServer(serverBinary, dataDir, [
      '--web-dir',
      webDir,
    ]);
    try {
      const client = new ArtifreeClient({
        baseUrl: server.web_url,
        accessToken: server.token,
      });
      const fetched = await client.getArtifact(artifactId);
      assert.equal(fetched.title, 'Persistent Artifact');

      const fileBytes = await client.readFile(artifactId, 1, 'index.html');
      assert.equal(
        Buffer.from(fileBytes).toString('utf8'),
        '<p>Persistent</p>',
      );
    } finally {
      await server.stop();
      await rm(dataDir, { recursive: true, force: true });
    }
  }
});

test('Production web configuration errors: missing web-dir or missing index.html fails startup', async () => {
  const dataDir = await mkdtemp(join(tmpdir(), 'artifree-prod-err-'));
  try {
    // 1. Non-existent web directory
    const nonExistent = join(dataDir, 'does-not-exist');
    await assert.rejects(async () => {
      await startServer(serverBinary, dataDir, ['--web-dir', nonExistent]);
    }, /does not exist/);

    // 2. Empty directory (missing index.html)
    const emptyDir = join(dataDir, 'empty-dir');
    await mkdir(emptyDir);
    await assert.rejects(async () => {
      await startServer(serverBinary, dataDir, ['--web-dir', emptyDir]);
    }, /is missing index\.html/);
  } finally {
    await rm(dataDir, { recursive: true, force: true });
  }
});

test('MCP loopback policy rejects foreign Host and Origin headers over HTTP', async () => {
  const dataDir = await mkdtemp(join(tmpdir(), 'artifree-prod-mcp-'));
  const webDir = await getWebDir();
  const server = await startServer(serverBinary, dataDir, [
    '--web-dir',
    webDir,
  ]);

  try {
    const rpcBody = JSON.stringify({
      jsonrpc: '2.0',
      id: 1,
      method: 'tools/list',
      params: {},
    });

    // 1. Calling /mcp with LAN/external Host header -> 403 Forbidden
    const url = new URL(server.api_url);
    const hostStatus = await new Promise((res, rej) => {
      const req = httpRequest(
        {
          hostname: url.hostname,
          port: url.port,
          path: '/mcp',
          method: 'POST',
          headers: {
            host: '192.168.1.100:7410',
            authorization: `Bearer ${server.token}`,
            'content-type': 'application/json',
            accept: 'application/json, text/event-stream',
            'mcp-protocol-version': '2025-11-25',
          },
        },
        (response) => {
          res(response.statusCode);
        },
      );
      req.on('error', rej);
      req.end(rpcBody);
    });
    assert.equal(hostStatus, 403, 'MCP must reject foreign Host header');

    // 2. Calling /mcp with Origin header -> 403 Forbidden
    const resOrigin = await fetch(server.api_url + '/mcp', {
      method: 'POST',
      headers: {
        origin: 'http://192.168.1.100:7410',
        authorization: `Bearer ${server.token}`,
        'content-type': 'application/json',
        accept: 'application/json, text/event-stream',
        'mcp-protocol-version': '2025-11-25',
      },
      body: rpcBody,
    });
    assert.equal(resOrigin.status, 403, 'MCP must reject Origin header');

    // 3. Loopback Host and no Origin -> 200 OK
    const resOk = await fetch(server.api_url + '/mcp', {
      method: 'POST',
      headers: {
        authorization: `Bearer ${server.token}`,
        'content-type': 'application/json',
        accept: 'application/json, text/event-stream',
        'mcp-protocol-version': '2025-11-25',
      },
      body: rpcBody,
    });
    assert.equal(resOk.status, 200, 'MCP accepts loopback call without Origin');
    const reply = await resOk.json();
    assert.ok(reply.result !== undefined);
  } finally {
    await server.stop();
    await rm(dataDir, { recursive: true, force: true });
  }
});

test('LAN configuration: bracket IPv6 and derive preview origin from overridden content-listen', async () => {
  const dataDir = await mkdtemp(join(tmpdir(), 'artifree-prod-lan-'));
  const webDir = await getWebDir();

  // 1. IPv6 --lan ::1 starts and brackets socket listeners properly
  const serverV6 = await startServer(serverBinary, dataDir, [
    '--web-dir',
    webDir,
    '--lan',
    '::1',
  ]);
  try {
    assert.ok(serverV6.api_url.includes('127.0.0.1'));
    assert.ok(serverV6.content_origin.includes('127.0.0.1'));
  } finally {
    await serverV6.stop();
  }

  // 2. Overridden --content-listen with --lan uses the overridden port for content_origin
  const serverOverridden = await startServer(serverBinary, dataDir, [
    '--web-dir',
    webDir,
    '--lan',
    '127.0.0.1',
  ]);
  try {
    // startServer passes --content-listen 127.0.0.1:0, which overrides default port 7411
    const contentPort = new URL(serverOverridden.content_origin).port;
    assert.notEqual(
      contentPort,
      '7411',
      'must derive origin from actual listener port',
    );
  } finally {
    await serverOverridden.stop();
    await rm(dataDir, { recursive: true, force: true });
  }
});
