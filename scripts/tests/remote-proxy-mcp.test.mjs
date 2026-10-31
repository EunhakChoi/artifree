import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createServer as createNetServer } from 'node:net';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import https from 'node:https';
import http from 'node:http';
import { after, before, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { Client as V1Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StreamableHTTPClientTransport as V1Transport } from '@modelcontextprotocol/sdk/client/streamableHttp.js';
import { ArtifreeClient } from '../../packages/client/dist/index.js';
import { buildServer, startServer } from '../lib/server-process.mjs';

// Self-signed test certificates over TLS
process.env.NODE_TLS_REJECT_UNAUTHORIZED = '0';

const root = fileURLToPath(new URL('../../', import.meta.url));

async function freePort() {
  const server = createNetServer();
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const { port } = server.address();
  await new Promise((resolve) => server.close(resolve));
  return port;
}

let binary;
let scratch;
let server;
let apiProxy;
let contentProxy;
let internalApiPort;
let internalContentPort;
let apiProxyPort;
let contentProxyPort;
let ownerToken;

before(async () => {
  binary = buildServer(root);
  scratch = await mkdtemp(join(tmpdir(), 'artifree-proxy-mcp-test-'));

  const keyPath = join(scratch, 'key.pem');
  const certPath = join(scratch, 'cert.pem');
  execFileSync('openssl', [
    'req',
    '-x509',
    '-newkey',
    'ec',
    '-pkeyopt',
    'ec_paramgen_curve:prime256v1',
    '-nodes',
    '-sha256',
    '-keyout',
    keyPath,
    '-out',
    certPath,
    '-days',
    '1',
    '-subj',
    '/CN=localhost',
    '-addext',
    'subjectAltName=DNS:localhost,IP:127.0.0.1',
  ]);
  const tlsKey = await readFile(keyPath, 'utf8');
  const tlsCert = await readFile(certPath, 'utf8');

  internalApiPort = await freePort();
  internalContentPort = await freePort();
  apiProxyPort = await freePort();
  contentProxyPort = await freePort();

  server = await startServer(binary, join(scratch, 'data'), [
    '--api-listen',
    `127.0.0.1:${internalApiPort}`,
    '--content-listen',
    `127.0.0.1:${internalContentPort}`,
    '--api-origin',
    `https://localhost:${apiProxyPort}`,
    '--content-origin',
    `https://localhost:${contentProxyPort}`,
    '--trusted-proxy',
    '127.0.0.1/32',
    '--allowed-host',
    `localhost:${apiProxyPort}`,
    '--allowed-host',
    `localhost:${contentProxyPort}`,
    '--allow-remote-mcp',
  ]);
  ownerToken = server.token;

  // 1. API HTTPS Reverse Proxy
  apiProxy = https.createServer(
    { key: tlsKey, cert: tlsCert },
    (clientReq, clientRes) => {
      const clientHost = clientReq.headers.host || `localhost:${apiProxyPort}`;
      const options = {
        hostname: '127.0.0.1',
        port: internalApiPort,
        path: clientReq.url,
        method: clientReq.method,
        headers: {
          ...clientReq.headers,
          host: clientHost,
          'x-forwarded-for': '198.51.100.25',
          'x-forwarded-host': clientHost,
          'x-forwarded-proto': 'https',
        },
      };
      const proxyReq = http.request(options, (proxyRes) => {
        clientRes.writeHead(proxyRes.statusCode, proxyRes.headers);
        proxyRes.pipe(clientRes);
      });
      proxyReq.on('error', (err) => {
        clientRes.writeHead(502);
        clientRes.end(`API Proxy error: ${err.message}`);
      });
      clientReq.pipe(proxyReq);
    },
  );
  await new Promise((resolve) =>
    apiProxy.listen(apiProxyPort, '127.0.0.1', resolve),
  );

  // 2. Content (Preview) HTTPS Reverse Proxy
  contentProxy = https.createServer(
    { key: tlsKey, cert: tlsCert },
    (clientReq, clientRes) => {
      const clientHost =
        clientReq.headers.host || `localhost:${contentProxyPort}`;
      const options = {
        hostname: '127.0.0.1',
        port: internalContentPort,
        path: clientReq.url,
        method: clientReq.method,
        headers: {
          ...clientReq.headers,
          host: clientHost,
          'x-forwarded-for': '198.51.100.25',
          'x-forwarded-host': clientHost,
          'x-forwarded-proto': 'https',
        },
      };
      const proxyReq = http.request(options, (proxyRes) => {
        clientRes.writeHead(proxyRes.statusCode, proxyRes.headers);
        proxyRes.pipe(clientRes);
      });
      proxyReq.on('error', (err) => {
        clientRes.writeHead(502);
        clientRes.end(`Content Proxy error: ${err.message}`);
      });
      clientReq.pipe(proxyReq);
    },
  );
  await new Promise((resolve) =>
    contentProxy.listen(contentProxyPort, '127.0.0.1', resolve),
  );
});

after(async () => {
  if (apiProxy) {
    apiProxy.closeAllConnections?.();
    await new Promise((resolve) => apiProxy.close(resolve));
  }
  if (contentProxy) {
    contentProxy.closeAllConnections?.();
    await new Promise((resolve) => contentProxy.close(resolve));
  }
  if (server) await server.stop();
  if (scratch) await rm(scratch, { recursive: true, force: true });
});

test('official MCP SDK client creates and saves artifacts over HTTPS reverse proxy', async () => {
  const mcpClient = new V1Client(
    { name: 'official-sdk-test', version: '1.0.0' },
    { capabilities: {} },
  );
  const transport = new V1Transport(
    new URL(`https://localhost:${apiProxyPort}/mcp`),
    {
      requestInit: {
        headers: {
          authorization: `Bearer ${ownerToken}`,
        },
      },
    },
  );
  await mcpClient.connect(transport);

  // 1. List tools
  const tools = await mcpClient.listTools();
  assert.ok(
    tools.tools.some((t) => t.name === 'create_artifact'),
    'create_artifact present',
  );
  assert.ok(
    tools.tools.some((t) => t.name === 'save_revision'),
    'save_revision present',
  );

  // 2. Create artifact
  const createResult = await mcpClient.callTool({
    name: 'create_artifact',
    arguments: {
      title: 'HTTPS Proxy Artifact',
      idempotency_key: `create-${Date.now()}`,
      files: [
        {
          path: 'index.html',
          encoding: 'utf8',
          content: '<h1>Initial Content via HTTPS Proxy</h1>',
        },
      ],
    },
  });
  assert.equal(createResult.isError, false);
  const artifactId = createResult.structuredContent.artifact.id;
  const initialRevision =
    createResult.structuredContent.artifact.current_revision;
  assert.ok(artifactId);
  assert.equal(initialRevision, 1);

  // 3. Save new revision
  const saveResult = await mcpClient.callTool({
    name: 'save_revision',
    arguments: {
      artifact_id: artifactId,
      base_revision: initialRevision,
      idempotency_key: `save-${Date.now()}`,
      changes: [
        {
          op: 'put',
          path: 'index.html',
          encoding: 'utf8',
          content: '<h1>Updated Content via HTTPS Proxy</h1>',
        },
      ],
    },
  });
  assert.equal(saveResult.isError, false);
  const secondRevision = saveResult.structuredContent.artifact.current_revision;
  assert.equal(secondRevision, 2);

  // 4. Request preview ticket
  const previewResult = await mcpClient.callTool({
    name: 'create_preview',
    arguments: {
      artifact_id: artifactId,
      revision: secondRevision,
    },
  });
  assert.equal(previewResult.isError, false);
  const previewUrl = previewResult.structuredContent.url;
  assert.ok(previewUrl.startsWith(`https://localhost:${contentProxyPort}/p/`));

  // 5. Fetch preview over HTTPS preview proxy
  const previewRes = await fetch(previewUrl);
  assert.equal(previewRes.status, 200);
  const html = await previewRes.text();
  assert.ok(html.includes('<h1>Updated Content via HTTPS Proxy</h1>'));

  // Verify CSP frame-ancestors restricts framing to external API origin
  const csp = previewRes.headers.get('content-security-policy') || '';
  assert.ok(
    csp.includes(`frame-ancestors https://localhost:${apiProxyPort}`),
    `Expected CSP to contain frame-ancestors: ${csp}`,
  );

  await mcpClient.close();
});

test('SSE change feed streams mutations live over HTTPS reverse proxy', async () => {
  const client = new ArtifreeClient({
    baseUrl: `https://localhost:${apiProxyPort}`,
    accessToken: ownerToken,
  });

  const events = [];
  const controller = new AbortController();
  const followPromise = client.followChanges({
    signal: controller.signal,
    retryDelayMs: { min: 100, max: 200 },
    onEvent(event) {
      events.push(event);
    },
  });

  // Wait for initial connection / ready
  const readyDeadline = Date.now() + 5000;
  while (Date.now() < readyDeadline) {
    if (events.some((e) => e.type === 'ready')) break;
    await new Promise((r) => setTimeout(r, 100));
  }
  assert.ok(events.some((e) => e.type === 'ready'));

  // Perform a mutation: create an artifact
  const created = await client.createArtifact(
    {
      title: 'Feed Test Artifact',
      files: [{ path: 'index.html', encoding: 'utf8', content: '<p>Feed</p>' }],
    },
    { idempotencyKey: `feed-${Date.now()}` },
  );

  // Verify that change feed receives the mutation event over HTTPS
  const mutationDeadline = Date.now() + 5000;
  while (Date.now() < mutationDeadline) {
    if (
      events.some(
        (e) =>
          e.type === 'change' && e.change.artifact_id === created.artifact.id,
      )
    )
      break;
    await new Promise((r) => setTimeout(r, 100));
  }

  const mutationEvent = events.find(
    (e) => e.type === 'change' && e.change.artifact_id === created.artifact.id,
  );
  assert.ok(
    mutationEvent,
    'Change feed must receive mutation event over TLS proxy',
  );

  controller.abort();
  await followPromise.catch(() => {});
});

test('agent token issuance, MCP usage, and revocation over HTTPS reverse proxy', async () => {
  const client = new ArtifreeClient({
    baseUrl: `https://localhost:${apiProxyPort}`,
    accessToken: ownerToken,
  });

  // 1. Issue an agent token using owner credentials over HTTPS
  const issued = await client.createToken({ name: 'remote-agent' });
  const agentTokenId = issued.token.id;
  const agentTokenSecret = issued.secret;
  assert.ok(agentTokenId);
  assert.ok(agentTokenSecret);

  // 2. Connect MCP client using the new agent token over HTTPS proxy
  const agentClient = new V1Client(
    { name: 'agent-client', version: '1.0.0' },
    { capabilities: {} },
  );
  const agentTransport = new V1Transport(
    new URL(`https://localhost:${apiProxyPort}/mcp`),
    {
      requestInit: {
        headers: {
          authorization: `Bearer ${agentTokenSecret}`,
        },
      },
    },
  );
  await agentClient.connect(agentTransport);
  const tools = await agentClient.listTools();
  assert.ok(tools.tools.length > 0);
  await agentClient.close();

  // 3. Revoke the agent token using owner credentials over HTTPS
  await client.revokeToken(agentTokenId);

  // 4. Using revoked token over HTTPS proxy fails with 401 Unauthorized
  const failClient = new V1Client(
    { name: 'revoked-client', version: '1.0.0' },
    { capabilities: {} },
  );
  const failTransport = new V1Transport(
    new URL(`https://localhost:${apiProxyPort}/mcp`),
    {
      requestInit: {
        headers: {
          authorization: `Bearer ${agentTokenSecret}`,
        },
      },
    },
  );

  await assert.rejects(
    () => failClient.connect(failTransport),
    (err) => {
      return /unauthenticated|valid bearer/i.test(err.message);
    },
  );
});

function sendRequest(options) {
  return new Promise((resolve, reject) => {
    const transport = options.protocol === 'https:' ? https : http;
    const req = transport.request(options, (res) => {
      let body = '';
      res.on('data', (c) => {
        body += c;
      });
      res.on('end', () =>
        resolve({
          status: res.statusCode,
          headers: res.headers,
          body,
        }),
      );
    });
    req.on('error', reject);
    req.end();
  });
}

test('untrusted peer forwarding and host/origin policy enforcement', async () => {
  // 1. Health check through proxy with allowed host succeeds with 200 OK
  const proxyHealthRes = await sendRequest({
    protocol: 'https:',
    hostname: '127.0.0.1',
    port: apiProxyPort,
    path: '/healthz',
    method: 'GET',
    headers: {
      host: `localhost:${apiProxyPort}`,
    },
  });
  assert.equal(proxyHealthRes.status, 200);

  // 2. Request through proxy with disallowed Host header is rejected with 400 Bad Request
  const badHostRes = await sendRequest({
    protocol: 'https:',
    hostname: '127.0.0.1',
    port: apiProxyPort,
    path: '/healthz',
    method: 'GET',
    headers: {
      host: 'attacker.example.com',
    },
  });
  assert.equal(badHostRes.status, 400);

  // 3. Request directly to internal listener with disallowed Host is rejected with 400 Bad Request
  const directBadHostRes = await sendRequest({
    protocol: 'http:',
    hostname: '127.0.0.1',
    port: internalApiPort,
    path: '/healthz',
    method: 'GET',
    headers: {
      host: 'evil.attacker.org',
    },
  });
  assert.equal(directBadHostRes.status, 400);

  // 4. Request from preview origin to API is rejected by CORS with 403 Forbidden
  const corsRes = await fetch(
    `https://localhost:${apiProxyPort}/v1/artifacts`,
    {
      method: 'OPTIONS',
      headers: {
        origin: `https://localhost:${contentProxyPort}`,
        'access-control-request-method': 'GET',
      },
    },
  );
  assert.equal(corsRes.status, 403);
});
