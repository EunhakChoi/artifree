import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  ArtifreeApiError,
  ArtifreeClient,
  encodeFile,
} from '../../packages/client/dist/index.js';

const artifact = {
  id: `art_${'0'.repeat(32)}`,
  kind: 'html',
  title: 'Demo',
  current_revision: 1,
  created_at_ms: 1,
  updated_at_ms: 1,
};

/** A client whose every request is answered by `respond`. */
function clientWith(respond) {
  return new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:7410',
    accessToken: 'token',
    fetch: async () => respond(),
  });
}

/** A response whose body ends with a stream error after `prefix`. */
function cutOff(status, prefix) {
  const body = new ReadableStream({
    start(controller) {
      controller.enqueue(new TextEncoder().encode(prefix));
      controller.error(new Error('connection reset'));
    },
  });
  return new Response(body, {
    status,
    headers: { 'content-type': 'application/json' },
  });
}

async function failure(promise) {
  const error = await promise.then(
    () => assert.fail('The request must fail.'),
    (rejection) => rejection,
  );
  assert.ok(error instanceof ArtifreeApiError, String(error));
  return error;
}

test('A body cut off mid-stream is a network error, not a contract error', async () => {
  // The outcome of a mutation is unknown, so callers must be told to retry.
  for (const status of [200, 201, 503]) {
    const client = clientWith(() => cutOff(status, '{"artifact":'));
    const error = await failure(client.getArtifact(artifact.id));
    assert.equal(error.code, 'network_error', `status ${status}`);
  }
  const reader = clientWith(() => cutOff(200, '<h1>'));
  const error = await failure(reader.readFile(artifact.id, 1, 'index.html'));
  assert.equal(error.code, 'network_error');
});

test('A complete body that is not the contract is an invalid response', async () => {
  const client = clientWith(
    () => new Response('<html>proxy error</html>', { status: 200 }),
  );
  const error = await failure(client.getArtifact(artifact.id));
  assert.equal(error.code, 'invalid_response');
});

test('An error with a null current revision keeps its code', async () => {
  const client = clientWith(() =>
    Response.json(
      {
        error: {
          code: 'not_found',
          message: 'gone',
          current_revision: null,
        },
      },
      { status: 404 },
    ),
  );
  const error = await failure(client.getArtifact(artifact.id));
  assert.equal(error.code, 'not_found');
  assert.equal(error.currentRevision, undefined);
});

test('Uploads use the smaller wire form and keep exact bytes', () => {
  const encoder = new TextEncoder();
  const text = encodeFile('a.txt', encoder.encode('﻿plain text\r\n'));
  assert.deepEqual(text, {
    path: 'a.txt',
    encoding: 'utf8',
    content: '﻿plain text\r\n',
  });
  // JSON escapes NUL as \u0000 (six bytes), so base64 is smaller.
  const controls = encodeFile('b.txt', new Uint8Array(30));
  assert.equal(controls.encoding, 'base64');
  assert.equal(controls.content, Buffer.alloc(30).toString('base64'));
  const binary = encodeFile('c.png', new Uint8Array([0x89, 0xff, 0x00]));
  assert.equal(binary.encoding, 'base64');
});

test('A rate-limited response keeps its code and retry delay', async () => {
  const client = clientWith(() =>
    Response.json(
      { error: { code: 'rate_limited', message: 'too many requests' } },
      { status: 429, headers: { 'retry-after': '3' } },
    ),
  );
  const error = await failure(client.getArtifact(artifact.id));
  assert.equal(error.code, 'rate_limited');
  assert.equal(error.status, 429);
  assert.equal(error.retryAfterSeconds, 3);
});

/** A change feed response whose body is `text`, then ends. */
function feed(text) {
  return new Response(text, {
    status: 200,
    headers: { 'content-type': 'text/event-stream' },
  });
}

test('A change kind this client does not know is delivered as unknown', async () => {
  const client = clientWith(() =>
    feed(
      'event: change\nid: 7\ndata: {"cursor":7,"artifact_id":"art_1","change":"renamed","revision":3,"committed_at_ms":1}\n\n' +
        'event: future\nid: 8\ndata: {}\n\n' +
        ': comment\n\n',
    ),
  );
  const events = [];
  for await (const event of await client.openChanges({ after: 6 }))
    events.push(event);
  assert.deepEqual(events, [
    {
      type: 'change',
      change: {
        cursor: 7,
        artifact_id: 'art_1',
        change: 'unknown',
        revision: 3,
        committed_at_ms: 1,
      },
    },
  ]);
  // A known event that breaks the contract is still refused.
  const broken = clientWith(() =>
    feed('event: change\ndata: {"cursor":1}\n\n'),
  );
  const error = await failure(
    (async () => {
      for await (const event of await broken.openChanges()) void event;
    })(),
  );
  assert.equal(error.code, 'invalid_response');
});

test('Aborting while disconnected stops following at once', async () => {
  // The server ends every stream at once and asks for a long wait.
  const client = clientWith(() =>
    Response.json(
      { error: { code: 'rate_limited', message: 'too many streams' } },
      { status: 429, headers: { 'retry-after': '600' } },
    ),
  );
  const controller = new AbortController();
  const started = performance.now();
  await client.followChanges({
    signal: controller.signal,
    onEvent() {},
    onDisconnect(error, retryInMs) {
      assert.equal(error?.code, 'rate_limited');
      assert.equal(retryInMs, 600_000);
      controller.abort();
    },
  });
  assert.ok(
    performance.now() - started < 1000,
    'followChanges waited for the retry delay after an abort',
  );
});

/** A change feed response delivered in the given chunks. */
function chunks(...parts) {
  const body = new ReadableStream({
    start(controller) {
      for (const part of parts)
        controller.enqueue(new TextEncoder().encode(part));
      controller.close();
    },
  });
  return new Response(body, {
    status: 200,
    headers: { 'content-type': 'text/event-stream' },
  });
}

test('Line endings split across chunks or ended by CR keep events whole', async () => {
  const data =
    '{"cursor":4,"artifact_id":"art_1","change":"revised","revision":2,"committed_at_ms":1}';
  const read = async (response) => {
    const client = clientWith(() => response);
    const events = [];
    for await (const event of await client.openChanges({ after: 3 }))
      events.push(event);
    return events;
  };
  const change = {
    type: 'change',
    change: {
      cursor: 4,
      artifact_id: 'art_1',
      change: 'revised',
      revision: 2,
      committed_at_ms: 1,
    },
  };
  // A CRLF split after its CR is one line end, not a blank line.
  assert.deepEqual(
    await read(chunks('event: change\r', `\ndata: ${data}\r`, '\n\r', '\n')),
    [change],
  );
  // CR-only line ends, with the final blank line's CR as the last byte.
  assert.deepEqual(await read(chunks(`event: change\rdata: ${data}\r\r`)), [
    change,
  ]);
  // An event cut off before its blank line is dropped.
  assert.deepEqual(await read(chunks(`event: change\ndata: ${data}\n`)), []);
});

test('A proxy error while the server restarts is retried; a 4xx one is not', async () => {
  // A proxy answers with an empty 502 until the server is back.
  const answers = [
    () => new Response('', { status: 502 }),
    () => chunks('event: ready\nid: 0\ndata: {"cursor":0}\n\n'),
  ];
  const controller = new AbortController();
  const events = [];
  const disconnects = [];
  const client = new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:7410',
    accessToken: 'token',
    fetch: async () =>
      (answers.shift() ?? (() => new Response('', { status: 502 })))(),
  });
  await client.followChanges({
    signal: controller.signal,
    retryDelayMs: { min: 1, max: 1 },
    onEvent(event) {
      events.push(event);
      controller.abort();
    },
    onDisconnect(error) {
      disconnects.push([error?.status, error?.code]);
    },
  });
  assert.deepEqual(disconnects, [[502, 'invalid_response']]);
  assert.deepEqual(events, [{ type: 'ready', cursor: 0 }]);

  const missing = clientWith(
    () =>
      new Response('<h1>Not here</h1>', {
        status: 404,
        headers: { 'content-type': 'text/html' },
      }),
  );
  const error = await failure(
    missing.followChanges({
      signal: new AbortController().signal,
      onEvent() {},
    }),
  );
  assert.deepEqual([error.status, error.code], [404, 'invalid_response']);
});
