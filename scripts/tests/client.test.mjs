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

// The change feed. Each rule below is one way an earlier version failed:
// a split line end, an unknown kind or event, an aborted wait, a proxy
// error, and a successful response that is not the feed.

const FEED = 'f'.repeat(32);
const cursor = (sequence) => `${FEED}.${sequence}`;
const changeData = (sequence, change = 'revised') =>
  JSON.stringify({
    cursor: cursor(sequence),
    artifact_id: artifact.id,
    change,
    revision: sequence,
    committed_at_ms: 1,
  });
const changeEvent = (sequence, change = 'revised') => ({
  type: 'change',
  change: {
    cursor: cursor(sequence),
    artifact_id: artifact.id,
    change,
    revision: sequence,
    committed_at_ms: 1,
  },
});

/** An event stream response whose body arrives in `parts` (strings or bytes). */
function stream(parts, { status = 200, onCancel } = {}) {
  const encoder = new TextEncoder();
  const body = new ReadableStream({
    start(controller) {
      for (const part of parts)
        controller.enqueue(
          typeof part === 'string' ? encoder.encode(part) : part,
        );
      if (onCancel === undefined) controller.close();
    },
    cancel: onCancel,
  });
  return new Response(body, {
    status,
    headers: { 'content-type': 'text/event-stream; charset=utf-8' },
  });
}

async function eventsOf(response) {
  const events = [];
  for await (const event of await clientWith(() => response).openChanges())
    events.push(event);
  return events;
}

test('The feed parser follows the event stream rules', async () => {
  const data = changeData(4);
  // A CRLF split after its CR, CR-only and LF line ends, a final CR blank line.
  assert.deepEqual(
    await eventsOf(
      stream([
        'event: change\r',
        `\ndata: ${data}\r`,
        '\n\r',
        `\nevent:change\rdata:${data}\r\r`,
      ]),
    ),
    [changeEvent(4), changeEvent(4)],
  );
  assert.deepEqual(
    await eventsOf(stream([`event: change\ndata: ${data}\r`, '\r'])),
    [changeEvent(4)],
  );
  // A byte order mark and a character split across chunks; comments, unknown
  // fields and events, an event without data, and joined data lines.
  const bytes = new TextEncoder().encode(
    `event: change\ndata: {"cursor":"${cursor(5)}","artifact_id":"${artifact.id}",\ndata: "change":"created","revision":5,"committed_at_ms":1}\n\n`,
  );
  const split = bytes.indexOf(0x22, 20) + 1;
  assert.deepEqual(
    await eventsOf(
      stream([
        new Uint8Array([0xef, 0xbb, 0xbf]),
        ': heartbeat\n\nretry: 1\nid: x\nevent: future\ndata: {}\n\nevent: change\n\n',
        bytes.subarray(0, split),
        bytes.subarray(split),
      ]),
    ),
    [changeEvent(5, 'created')],
  );
  // An event the body ends before its blank line is dropped.
  assert.deepEqual(
    await eventsOf(stream([`event: change\ndata: ${data}\n`])),
    [],
  );
});

test('A change kind this client does not know is delivered as unknown', async () => {
  assert.deepEqual(
    await eventsOf(
      stream([`event: change\ndata: ${changeData(7, 'renamed')}\n\n`]),
    ),
    [changeEvent(7, 'unknown')],
  );
  const broken = await failure(
    eventsOf(stream(['event: ready\ndata: {"cursor":7}\n\n'])),
  );
  assert.deepEqual([broken.status, broken.code], [200, 'invalid_response']);
});

/**
 * Follows a feed whose successive connections are answered by `answers`;
 * the last answer repeats. Stops after `events` events, or rejects.
 */
async function followed(answers, { events: wanted = 1, after } = {}) {
  const controller = new AbortController();
  const requests = [];
  const events = [];
  const disconnects = [];
  const client = new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:7410',
    accessToken: 'token',
    fetch: async (url, init) => {
      requests.push(init.headers['last-event-id']);
      const answer = answers.length > 1 ? answers.shift() : answers[0];
      return answer();
    },
  });
  await client.followChanges({
    signal: controller.signal,
    retryDelayMs: { min: 1, max: 2 },
    ...(after === undefined ? {} : { after }),
    onEvent(event) {
      events.push(event);
      if (events.length === wanted) controller.abort();
    },
    onDisconnect(error, retryInMs) {
      disconnects.push([error?.status, error?.code, retryInMs]);
    },
  });
  return { requests, events, disconnects };
}

test('Following retries lost connections, 429, and any 5xx from the last cursor', async () => {
  const result = await followed(
    [
      () => stream([`event: change\ndata: ${changeData(1)}\n\n`]),
      () => {
        throw new TypeError('connection refused');
      },
      () => new Response('', { status: 502 }),
      () => new Response('<h1>Bad gateway</h1>', { status: 504 }),
      () =>
        Response.json(
          { error: { code: 'unavailable', message: 'busy' } },
          { status: 503, headers: { 'retry-after': '0' } },
        ),
      () =>
        Response.json(
          { error: { code: 'rate_limited', message: 'too many streams' } },
          { status: 429, headers: { 'retry-after': '0' } },
        ),
      () => stream([`event: change\ndata: ${changeData(2)}\n\n`]),
    ],
    { events: 2, after: cursor(0) },
  );
  assert.deepEqual(result.events, [changeEvent(1), changeEvent(2)]);
  assert.deepEqual(result.requests, [cursor(0), ...Array(6).fill(cursor(1))]);
  assert.deepEqual(result.disconnects, [
    [undefined, undefined, 1],
    [0, 'network_error', 2],
    [502, 'invalid_response', 2],
    [504, 'invalid_response', 2],
    [503, 'unavailable', 0],
    [429, 'rate_limited', 0],
  ]);
});

test('Following stops on failures a retry cannot fix', async () => {
  const fatal = [
    [
      () =>
        Response.json(
          { error: { code: 'unauthenticated', message: 'no' } },
          { status: 401 },
        ),
      [401, 'unauthenticated'],
    ],
    [
      () =>
        Response.json(
          { error: { code: 'invalid_request', message: 'bad cursor' } },
          { status: 400 },
        ),
      [400, 'invalid_request'],
    ],
    [
      () => new Response('<h1>Not found</h1>', { status: 404 }),
      [404, 'invalid_response'],
    ],
    // A sign-in page or other successful response that is not the feed.
    [
      () =>
        new Response('<html>Sign in</html>', {
          status: 200,
          headers: { 'content-type': 'text/html' },
        }),
      [200, 'invalid_response'],
    ],
    [
      () => stream(['event: change\ndata: {"cursor":1}\n\n']),
      [200, 'invalid_response'],
    ],
  ];
  for (const [answer, expected] of fatal) {
    const error = await failure(followed([answer]));
    assert.deepEqual([error.status, error.code], expected);
  }
});

test('Aborting stops following at once, also during a wait or a stream', async () => {
  // The server asks for a long wait; the abort comes in onDisconnect.
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
      assert.deepEqual([error?.code, retryInMs], ['rate_limited', 600_000]);
      controller.abort();
    },
  });
  assert.ok(
    performance.now() - started < 1000,
    'followChanges waited after the abort',
  );

  // An open stream is cancelled when following stops.
  let cancelled;
  const closed = new Promise((resolve) => {
    cancelled = resolve;
  });
  const open = new AbortController();
  await clientWith(() =>
    stream(['event: ready\ndata: {"cursor":"c.0"}\n\n'], {
      onCancel: cancelled,
    }),
  ).followChanges({
    signal: open.signal,
    onEvent() {
      setTimeout(() => open.abort(), 10);
    },
  });
  await closed;
});

test('No event is delivered after an abort, also from the same chunk', async () => {
  const chunk = [1, 2, 3, 4]
    .map((sequence) => `event: change\ndata: ${changeData(sequence)}\n\n`)
    .join('');
  const controller = new AbortController();
  const delivered = [];
  await clientWith(() => stream([chunk])).followChanges({
    signal: controller.signal,
    onEvent(event) {
      delivered.push(event);
      controller.abort();
    },
  });
  assert.deepEqual(delivered, [changeEvent(1)]);

  const reading = new AbortController();
  const read = [];
  for await (const event of await clientWith(() => stream([chunk])).openChanges(
    { signal: reading.signal },
  )) {
    read.push(event);
    reading.abort();
  }
  assert.deepEqual(read, [changeEvent(1)]);
});

test("A proxy's Retry-After is honored although its body is not an API error", async () => {
  const controller = new AbortController();
  const waits = [];
  await clientWith(
    () =>
      new Response('<h1>Service unavailable</h1>', {
        status: 503,
        headers: { 'retry-after': '7' },
      }),
  ).followChanges({
    signal: controller.signal,
    onEvent() {},
    onDisconnect(error, retryInMs) {
      waits.push([error?.code, retryInMs]);
      controller.abort();
    },
  });
  assert.deepEqual(waits, [['invalid_response', 7000]]);
});

test('What onEvent throws ends following, even an API error', async () => {
  let requests = 0;
  const client = new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:7410',
    accessToken: 'token',
    fetch: async () => {
      requests += 1;
      return stream([`event: change\ndata: ${changeData(1)}\n\n`]);
    },
  });
  const thrown = new ArtifreeApiError('handler failed', {
    status: 0,
    code: 'network_error',
  });
  const error = await client
    .followChanges({
      signal: new AbortController().signal,
      onEvent() {
        throw thrown;
      },
    })
    .then(
      () => assert.fail('following must reject'),
      (rejection) => rejection,
    );
  assert.equal(error, thrown);
  assert.equal(requests, 1, 'a handler failure was retried');
});
