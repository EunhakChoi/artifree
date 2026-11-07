import assert from 'node:assert/strict';
import { once } from 'node:events';
import { mkdtemp, rm } from 'node:fs/promises';
import { connect } from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, before, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { buildServer, startServer } from '../lib/server-process.mjs';

// A request refused with 413 after the server read part of its body: the
// client must read the 413 and see the connection end, not a reset (decision
// 0048). The sender is a raw socket that keeps writing, so the server's
// close with unread data would show as a socket error. The whole body is
// 4 MiB beyond the 36 MiB request limit, sent once with a declared length and
// once chunked.
const root = fileURLToPath(new URL('../../', import.meta.url));
const MIB = 1024 * 1024;
// Mirrors MAX_BODY_BYTES of the server (36 MiB, the request limit of /mcp and
// of the API). The body must exceed it by more than the unread rest a drain
// can leave in socket buffers, or the test would not exercise the refusal.
const REQUEST_LIMIT = 36 * MIB;
const BODY_BYTES = REQUEST_LIMIT + 4 * MIB;
// A silent socket is destroyed after IDLE_MS; a case that is still going after
// OVERALL_MS (a sender that never finishes, an answer that never ends) too.
const IDLE_MS = 20_000;
const OVERALL_MS = 60_000;
let binary;
let scratch;
let server;

before(async () => {
  binary = buildServer(root);
  scratch = await mkdtemp(join(tmpdir(), 'artifree-refused-body-test-'));
  // The whole suite is one actor; its budget is not under test here.
  server = await startServer(binary, join(scratch, 'data'), [
    '--request-burst',
    '1000',
  ]);
});

after(async () => {
  await server?.stop();
  await rm(scratch, { recursive: true, force: true });
});

const ROUTES = {
  '/mcp': {
    accept: 'application/json, text/event-stream',
    'mcp-protocol-version': '2025-11-25',
  },
  '/v1/artifacts': { 'idempotency-key': 'refused-body-drain-1' },
};

/** Writes `data`, waiting for the socket's buffer to drain. */
async function write(socket, data) {
  if (!socket.write(data)) await once(socket, 'drain');
}

/**
 * Sends one POST of BODY_BYTES bytes with `Connection: close` and resolves
 * when the socket closes, with what it received and how it ended.
 */
function post(path, shape) {
  const { port } = new URL(server.api_url);
  const headers = {
    host: `127.0.0.1:${port}`,
    authorization: `Bearer ${server.token}`,
    'content-type': 'application/json',
    connection: 'close',
    ...ROUTES[path],
    ...(shape === 'declared'
      ? { 'content-length': String(BODY_BYTES) }
      : { 'transfer-encoding': 'chunked' }),
  };
  const head = `POST ${path} HTTP/1.1\r\n${Object.entries(headers)
    .map(([name, value]) => `${name}: ${value}\r\n`)
    .join('')}\r\n`;
  const chunk = Buffer.alloc(MIB, 'x');
  const framed = Buffer.concat([
    Buffer.from(`${MIB.toString(16)}\r\n`),
    chunk,
    Buffer.from('\r\n'),
  ]);
  return new Promise((resolve) => {
    const socket = connect({ host: '127.0.0.1', port: Number(port) });
    const received = [];
    const outcome = { ended: false, error: undefined };
    const fail = (message, code) =>
      socket.destroy(Object.assign(new Error(message), { code }));
    socket.setTimeout(IDLE_MS, () =>
      fail('no answer in time', 'TEST_IDLE_TIMEOUT'),
    );
    const overall = setTimeout(
      () => fail('the case did not finish in time', 'TEST_OVERALL_TIMEOUT'),
      OVERALL_MS,
    );
    socket.on('data', (data) => received.push(data));
    socket.on('end', () => {
      outcome.ended = true;
    });
    socket.on('error', (error) => {
      outcome.error = error;
    });
    socket.on('close', () => {
      clearTimeout(overall);
      resolve({ ...outcome, text: Buffer.concat(received).toString('utf8') });
    });
    // A failed write surfaces as the socket's error event, so it only has to
    // stop the sender.
    (async () => {
      await write(socket, head);
      for (let sent = 0; sent < BODY_BYTES; sent += MIB) {
        await write(socket, shape === 'declared' ? chunk : framed);
      }
      if (shape === 'chunked') await write(socket, '0\r\n\r\n');
    })().catch(() => {});
  });
}

for (const path of Object.keys(ROUTES)) {
  for (const shape of ['declared', 'chunked']) {
    test(`${path} refuses a ${shape} body over the limit with 413 and a clean end`, async () => {
      const { text, ended, error } = await post(path, shape);
      assert.match(text, /^HTTP\/1\.1 413 /, `answer: ${text.slice(0, 200)}`);
      assert.match(
        text,
        path === '/mcp' ? /too large/i : /payload_too_large/,
        `answer: ${text.slice(0, 400)}`,
      );
      assert.equal(
        error,
        undefined,
        `socket error ${error?.code}: the client could not finish reading the 413`,
      );
      assert.equal(ended, true, 'the connection did not end cleanly');
    });
  }
}
