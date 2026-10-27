import assert from 'node:assert/strict';
import { afterEach, beforeEach, mock, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The gate is web code without DOM or React imports; bundle it as it ships.
const bundle = await build({
  entryPoints: [
    fileURLToPath(
      new URL('../../apps/web/src/artifacts/request-gate.ts', import.meta.url),
    ),
  ],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const { RequestGate, STALL_MS, requestKind } = await import(
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
);

beforeEach(() => mock.timers.enable({ apis: ['setTimeout', 'Date'] }));
afterEach(() => mock.timers.reset());

/** Lets settled promises run their continuations. */
async function settle() {
  for (let round = 0; round < 10; round += 1) await Promise.resolve();
}

async function advance(milliseconds) {
  mock.timers.tick(milliseconds);
  await settle();
}

/** A page whose visibility the test sets. */
function page() {
  const listeners = new Set();
  let visible = true;
  return {
    visibility: {
      visible: () => visible,
      subscribe(onChange) {
        listeners.add(onChange);
        return () => listeners.delete(onChange);
      },
    },
    listening: () => listeners.size,
    show(next) {
      visible = next;
      for (const listener of listeners) listener();
    },
  };
}

/**
 * A server the test answers by hand. `sent` lists each request as it is
 * sent; `answer(index, response)` settles one.
 */
function server() {
  const sent = [];
  return {
    sent,
    paths: () => sent.map(({ path }) => path),
    send: (input, init) =>
      new Promise((resolve, reject) => {
        sent.push({
          path: `${init?.method ?? 'GET'} ${new URL(input, 'http://x').pathname}`,
          signal: init?.signal,
          resolve,
          reject,
        });
      }),
    answer(index, response = new Response('{}')) {
      sent[index].resolve(response);
    },
  };
}

function limited(retryAfter, body = '{"error":{"code":"rate_limited"}}') {
  const headers =
    retryAfter === undefined ? {} : { 'retry-after': String(retryAfter) };
  return new Response(body, { status: 429, headers });
}

function setup() {
  const view = page();
  const api = server();
  const gate = new RequestGate(view.visibility, api.send);
  return { view, api, gate };
}

test('Requests are classified by what they change', () => {
  assert.equal(requestKind('GET', '/v1/artifacts'), 'read');
  assert.equal(requestKind('GET', 'http://h/v1/artifacts/a/revisions'), 'read');
  assert.equal(
    requestKind('POST', '/v1/artifacts/a/revisions/2/previews'),
    'read',
  );
  assert.equal(requestKind('GET', '/v1/changes'), 'stream');
  assert.equal(requestKind('POST', '/v1/artifacts'), 'write');
  assert.equal(requestKind('POST', '/v1/artifacts/a/revisions'), 'write');
  assert.equal(requestKind('PATCH', '/v1/artifacts/a'), 'write');
});

test('Reads are sent one at a time, in order; writes are not held', async () => {
  const { api, gate } = setup();
  const first = gate.fetch('/v1/artifacts');
  const second = gate.fetch('/v1/artifacts/a');
  gate.fetch('/v1/artifacts/a/revisions', { method: 'POST' });
  await settle();
  // The write goes out at once; the second read waits for the first.
  assert.deepEqual(api.paths(), [
    'POST /v1/artifacts/a/revisions',
    'GET /v1/artifacts',
  ]);
  api.answer(1, Response.json({ artifacts: [] }));
  const answer = await first;
  await settle();
  assert.equal(api.sent.length, 2, 'the first answer is still being read');
  assert.deepEqual(await answer.json(), { artifacts: [] });
  await settle();
  assert.deepEqual(api.paths().at(-1), 'GET /v1/artifacts/a');
  api.answer(2);
  assert.equal((await second).status, 200);
});

test('A withdrawn read is never sent', async () => {
  const { api, gate } = setup();
  gate.fetch('/v1/artifacts');
  const controller = new AbortController();
  const withdrawn = gate.fetch('/v1/artifacts/a', {
    signal: controller.signal,
  });
  await settle();
  controller.abort();
  await assert.rejects(withdrawn, { name: 'AbortError' });
  await assert.rejects(
    gate.fetch('/v1/artifacts/b', { signal: AbortSignal.abort() }),
    { name: 'AbortError' },
  );
  api.answer(0);
  await settle();
  assert.deepEqual(api.paths(), ['GET /v1/artifacts']);
});

test('A read abandoned after it was sent keeps its slot until it ends', async () => {
  const { api, gate } = setup();
  const controller = new AbortController();
  const abandoned = gate.fetch('/v1/artifacts', { signal: controller.signal });
  gate.fetch('/v1/artifacts/a');
  await settle();
  assert.equal(api.sent[0].signal, null, 'a sent read is not cancelled');
  controller.abort();
  await assert.rejects(abandoned, { name: 'AbortError' });
  await settle();
  assert.equal(api.sent.length, 1, 'the server is still working on it');
  api.answer(0);
  await settle();
  assert.deepEqual(api.paths(), ['GET /v1/artifacts', 'GET /v1/artifacts/a']);
});

test('A hidden page sends no read, also one asked for before it was hidden', async () => {
  const { view, api, gate } = setup();
  gate.fetch('/v1/artifacts');
  gate.fetch('/v1/artifacts/a');
  await settle();
  view.show(false);
  gate.fetch('/v1/artifacts/b');
  api.answer(0);
  await advance(60_000);
  assert.deepEqual(api.paths(), ['GET /v1/artifacts']);
  view.show(true);
  await settle();
  assert.deepEqual(api.paths(), ['GET /v1/artifacts', 'GET /v1/artifacts/a']);
});

test('A refused read holds every read for Retry-After, then is sent again first', async () => {
  const { api, gate } = setup();
  const refused = gate.fetch('/v1/artifacts');
  gate.fetch('/v1/artifacts/a');
  await settle();
  api.answer(0, limited(12));
  await settle();
  await advance(STALL_MS + 1_000);
  assert.deepEqual(
    api.paths(),
    ['GET /v1/artifacts'],
    'longer than the stall bound',
  );
  await advance(1_000);
  assert.deepEqual(api.paths(), ['GET /v1/artifacts', 'GET /v1/artifacts']);
  api.answer(1, Response.json({ artifacts: [] }));
  assert.deepEqual(await (await refused).json(), { artifacts: [] });
});

test("A proxy's 429 without an API body or Retry-After holds reads for a second", async () => {
  const { api, gate } = setup();
  gate.fetch('/v1/artifacts');
  await settle();
  api.answer(0, limited(undefined, '<h1>Too many requests</h1>'));
  await settle();
  await advance(999);
  assert.equal(api.sent.length, 1);
  await advance(1);
  assert.equal(api.sent.length, 2);
});

test('A later, shorter delay does not shorten a pending one', async () => {
  const { api, gate } = setup();
  gate.holdFor(5);
  gate.holdFor(1);
  gate.fetch('/v1/artifacts');
  await advance(4_999);
  assert.equal(api.sent.length, 0);
  await advance(1);
  assert.equal(api.sent.length, 1);
});

test('A delay that ends while the page is hidden sends nothing until shown', async () => {
  const { view, api, gate } = setup();
  gate.holdFor(2);
  gate.fetch('/v1/artifacts');
  view.show(false);
  await advance(5_000);
  assert.equal(api.sent.length, 0);
  view.show(true);
  await settle();
  assert.equal(api.sent.length, 1);
});

test('A stalled read stops holding back the next one, once', async () => {
  const { api, gate } = setup();
  const reads = ['', '/a', '/b'].map((path) =>
    gate.fetch(`/v1/artifacts${path}`),
  );
  await advance(STALL_MS - 1);
  assert.equal(api.sent.length, 1);
  await advance(1);
  assert.equal(api.sent.length, 2);
  // The stalled read ending must not free the slot of the one now running.
  api.answer(0);
  await (await reads[0]).text();
  await settle();
  assert.equal(api.sent.length, 2);
  api.answer(1);
  await (await reads[1]).text();
  await settle();
  assert.equal(api.sent.length, 3);
});

test('A stalled read does not end a pending delay', async () => {
  const { api, gate } = setup();
  gate.fetch('/v1/artifacts');
  await settle();
  gate.holdFor(30);
  gate.fetch('/v1/artifacts/a');
  await advance(STALL_MS);
  assert.equal(api.sent.length, 1, 'the stall bound frees only the slot');
  await advance(30_000 - STALL_MS - 1);
  assert.equal(api.sent.length, 1);
  await advance(1);
  assert.equal(api.sent.length, 2);
});

test('A failed read frees its slot', async () => {
  const { api, gate } = setup();
  const failed = gate.fetch('/v1/artifacts');
  gate.fetch('/v1/artifacts/a');
  await settle();
  api.sent[0].reject(new TypeError('network'));
  await assert.rejects(failed, TypeError);
  await settle();
  assert.equal(api.sent.length, 2);
});

test('The stream waits out a delay, and its 429 delays reads', async () => {
  const { api, gate } = setup();
  gate.holdFor(3);
  const controller = new AbortController();
  const withdrawn = gate.fetch('/v1/changes', { signal: controller.signal });
  gate.fetch('/v1/changes');
  await advance(2_999);
  controller.abort();
  await assert.rejects(withdrawn, { name: 'AbortError' });
  assert.equal(api.sent.length, 0);
  await advance(1);
  assert.deepEqual(api.paths(), ['GET /v1/changes']);
  api.answer(0, limited(4));
  await settle();
  gate.fetch('/v1/artifacts');
  await advance(3_999);
  assert.equal(api.sent.length, 1);
  await advance(1);
  assert.equal(api.sent.length, 2);
});

test('A write is sent during a delay, and its 429 delays reads', async () => {
  const { api, gate } = setup();
  gate.holdFor(10);
  gate.fetch('/v1/artifacts', { method: 'POST' });
  await settle();
  assert.equal(api.sent.length, 1);
  api.answer(0, limited(20));
  await settle();
  gate.fetch('/v1/artifacts');
  await advance(19_999);
  assert.equal(api.sent.length, 1);
  await advance(1);
  assert.equal(api.sent.length, 2);
});

test('The gate listens to visibility only while reads wait', async () => {
  const { view, api, gate } = setup();
  assert.equal(view.listening(), 0);
  view.show(false);
  const controller = new AbortController();
  const withdrawn = gate.fetch('/v1/artifacts', { signal: controller.signal });
  await settle();
  assert.equal(view.listening(), 1);
  controller.abort();
  await assert.rejects(withdrawn, { name: 'AbortError' });
  assert.equal(view.listening(), 0);
  view.show(true);
  gate.fetch('/v1/artifacts');
  await settle();
  assert.equal(view.listening(), 0, 'a read that started no longer waits');
  api.answer(0);
});

test('A read cancelled as its turn comes is not sent', async () => {
  const { api, gate } = setup();
  const controller = new AbortController();
  // The gate is idle, so the turn comes at once; the caller stops waiting
  // before the read could be sent, as an effect cleaned up right away does.
  const withdrawn = gate.fetch('/v1/artifacts', { signal: controller.signal });
  controller.abort();
  await assert.rejects(withdrawn, { name: 'AbortError' });
  const next = gate.fetch('/v1/artifacts/a');
  await settle();
  assert.deepEqual(api.paths(), ['GET /v1/artifacts/a']);
  api.answer(0);
  await next;
});

test('A refusal without delay still holds reads and keeps the refused read first', async () => {
  const { api, gate } = setup();
  const refused = gate.fetch('/v1/artifacts');
  gate.fetch('/v1/artifacts/b');
  await settle();
  api.answer(0, limited(0));
  await settle();
  assert.deepEqual(api.paths(), ['GET /v1/artifacts'], 'held for a second');
  await advance(999);
  assert.equal(api.sent.length, 1);
  await advance(1);
  assert.deepEqual(api.paths(), ['GET /v1/artifacts', 'GET /v1/artifacts']);
  api.answer(1);
  await refused;
});
