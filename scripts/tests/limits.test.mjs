import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { connect as connectTcp } from 'node:net';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { after, before, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import {
  Client,
  StreamableHTTPClientTransport,
} from '@modelcontextprotocol/client';
import { buildServer, startServer } from '../lib/server-process.mjs';

// Request limits on the real binary: the official MCP client and the CLI
// share one per-actor budget, a refusal does nothing, and the budget recovers.
const root = fileURLToPath(new URL('../../', import.meta.url));
let binary;
let scratch;

before(async () => {
  binary = buildServer(root);
  scratch = await mkdtemp(join(tmpdir(), 'artifree-limits-test-'));
});

after(async () => {
  await rm(scratch, { recursive: true, force: true });
});

function cli(server, args) {
  const env = {
    ...process.env,
    ARTIFREE_URL: server.api_url,
    ARTIFREE_TOKEN: server.token,
  };
  delete env.ARTIFREE_TOKEN_FILE;
  const result = spawnSync(
    process.execPath,
    ['apps/cli/dist/main.js', 'artifact', ...args],
    { cwd: root, encoding: 'utf8', timeout: 20_000, env },
  );
  if (result.error) throw result.error;
  return result;
}

const page = (key) => ({
  title: 'Limited',
  files: [{ path: 'index.html', encoding: 'utf8', content: '<p>once</p>' }],
  idempotency_key: key,
});

test('MCP and the CLI share a budget; a refusal does nothing and the budget recovers', async () => {
  const server = await startServer(binary, join(scratch, 'shared'), [
    '--request-burst',
    '3',
    '--requests-per-minute',
    '20',
  ]);
  const client = new Client(
    { name: 'artifree-limits-test', version: '0' },
    { versionNegotiation: { mode: { pin: '2026-07-28' } } },
  );
  try {
    await client.connect(
      new StreamableHTTPClientTransport(new URL(`${server.api_url}/mcp`), {
        requestInit: { headers: { authorization: `Bearer ${server.token}` } },
      }),
    );
    // Spend the rest of the burst, then go one over.
    let refused;
    for (let call = 0; call < 5 && refused === undefined; call += 1) {
      try {
        await client.callTool({ name: 'list_artifacts', arguments: {} });
      } catch (error) {
        refused = error;
      }
    }
    assert.ok(refused, 'a call beyond the burst must be refused');
    assert.match(String(refused), /429|rate_limited/);
    await assert.rejects(
      client.callTool({
        name: 'create_artifact',
        arguments: page('limited-0001'),
      }),
      /429|rate_limited/,
    );

    // The CLI draws on the same budget and says nothing happened.
    const blocked = cli(server, ['list']);
    assert.equal(blocked.status, 1);
    assert.equal(blocked.stdout, '');
    assert.match(blocked.stderr, /too many requests/);

    // 20 per minute: the next request is admitted three seconds later.
    await delay(3100);
    const created = await client.callTool({
      name: 'create_artifact',
      arguments: page('limited-0001'),
    });
    assert.equal(created.isError, false, JSON.stringify(created));
    // The refused attempt with the same key committed nothing.
    assert.equal(created.structuredContent.replayed, false);
  } finally {
    await client.close();
    await server.stop();
  }
});

test('The CLI reports a refused save as not done, with its key unused', async () => {
  const server = await startServer(binary, join(scratch, 'cli'), [
    '--request-burst',
    '1',
    '--requests-per-minute',
    '20',
  ]);
  try {
    const listed = cli(server, ['list']);
    assert.equal(listed.status, 0, listed.stderr);
    const site = join(scratch, 'cli-site');
    await mkdir(site, { recursive: true });
    await writeFile(join(site, 'index.html'), '<p>x</p>');
    const refused = cli(server, [
      'create',
      '--title',
      'T',
      '--dir',
      site,
      '--idempotency-key',
      'cli-limited-1',
    ]);
    assert.equal(refused.status, 1);
    assert.match(
      refused.stderr,
      /Nothing was done\. Retry after 3s; --idempotency-key cli-limited-1 is still unused\./,
    );
  } finally {
    await server.stop();
  }
});

/** Sends a request's headers and part of its body, then waits. */
function holdRequest(server, body) {
  const url = new URL(server.api_url);
  return new Promise((resolve, reject) => {
    const socket = connectTcp(Number(url.port), url.hostname, () => {
      socket.write(
        [
          'POST /v1/artifacts HTTP/1.1',
          `Host: ${url.host}`,
          `Authorization: Bearer ${server.token}`,
          'Content-Type: application/json',
          'Idempotency-Key: held-request-1',
          `Content-Length: ${Buffer.byteLength(body)}`,
          '',
          '',
        ].join('\r\n') + body.slice(0, 10),
      );
      resolve({
        abort() {
          socket.destroy();
        },
        finish() {
          return new Promise((done) => {
            let reply = '';
            socket.on('data', (chunk) => {
              reply += chunk;
              if (reply.includes('\r\n\r\n')) {
                socket.end();
                done(reply.split('\r\n')[0]);
              }
            });
            socket.write(body.slice(10));
          });
        },
      });
    });
    socket.once('error', reject);
  });
}

test('A refused large upload still receives the 429', async () => {
  const server = await startServer(binary, join(scratch, 'large'), [
    '--request-burst',
    '1',
    '--requests-per-minute',
    '20',
  ]);
  try {
    assert.equal(cli(server, ['list']).status, 0);
    // The server reads and drops the refused body, so the client is not cut
    // off mid-upload and sees the refusal and its delay.
    const response = await fetch(`${server.api_url}/v1/artifacts`, {
      method: 'POST',
      headers: {
        authorization: `Bearer ${server.token}`,
        'content-type': 'application/json',
        'idempotency-key': 'large-refused-1',
      },
      body: 'x'.repeat(30 * 1024 * 1024),
    });
    assert.equal(response.status, 429);
    assert.equal(response.headers.get('retry-after'), '3');
    assert.equal((await response.json()).error.code, 'rate_limited');
  } finally {
    await server.stop();
  }
});

test('A request holds its concurrency slot until it finishes', async () => {
  const server = await startServer(binary, join(scratch, 'concurrent'), [
    '--concurrent-requests',
    '1',
  ]);
  let held;
  try {
    const body = JSON.stringify({
      title: 'Held',
      files: [{ path: 'index.html', encoding: 'utf8', content: '<p>held</p>' }],
    });
    held = await holdRequest(server, body);
    // The held request was admitted once its headers arrived; wait for that.
    let busy;
    for (let attempt = 0; attempt < 50 && busy === undefined; attempt += 1) {
      const response = cli(server, ['list']);
      if (response.status === 1 && /running/.test(response.stderr))
        busy = response;
      else await delay(20);
    }
    assert.ok(busy, 'a second request must be refused while the first runs');
    assert.equal(await held.finish(), 'HTTP/1.1 201 Created');
    const after = cli(server, ['list']);
    assert.equal(after.status, 0, after.stderr);
    assert.equal(JSON.parse(after.stdout).artifacts.length, 1);
  } finally {
    // An open connection would hold graceful shutdown.
    held?.abort();
    await server.stop();
  }
});

test('Zero or invalid limits stop the server before it listens', async () => {
  for (const [flag, value] of [
    ['--request-burst', '0'],
    ['--requests-per-minute', 'many'],
    ['--concurrent-requests', '100001'],
  ]) {
    const result = spawnSync(
      binary,
      [
        'serve',
        '--data-dir',
        join(scratch, `bad-${flag.slice(2)}`),
        '--api-listen',
        '127.0.0.1:0',
        '--content-listen',
        '127.0.0.1:0',
        flag,
        value,
      ],
      { encoding: 'utf8', timeout: 20_000 },
    );
    assert.equal(result.status, 1, `${flag} ${value}`);
    assert.equal(result.stdout, '', 'no readiness line');
    assert.match(
      result.stderr,
      new RegExp(`${flag} must be a whole number from 1 to 100000`),
    );
  }
});
