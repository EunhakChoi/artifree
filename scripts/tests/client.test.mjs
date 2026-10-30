import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  ArtifreeApiError,
  ArtifreeClient,
  encodeFile,
  newIdempotencyKey,
  newRandomId,
} from '../../packages/client/dist/index.js';

const artifact = {
  id: `art_${'0'.repeat(32)}`,
  kind: 'app',
  title: 'Demo',
  pinned: false,
  current_revision: 1,
  metadata_revision: 1,
  deleted_at_ms: null,
  created_at_ms: 1,
  updated_at_ms: 1,
};

test('Random identifiers work without randomUUID and never use a weak fallback', () => {
  const originalUuid = Object.getOwnPropertyDescriptor(crypto, 'randomUUID');
  const originalValues = Object.getOwnPropertyDescriptor(
    crypto,
    'getRandomValues',
  );
  try {
    Object.defineProperty(crypto, 'randomUUID', {
      configurable: true,
      value: undefined,
    });
    Object.defineProperty(crypto, 'getRandomValues', {
      configurable: true,
      value: (bytes) => {
        assert.equal(bytes.length, 16);
        bytes.set([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]);
        return bytes;
      },
    });
    assert.equal(newRandomId(), '000102030405060708090a0b0c0d0e0f');
    assert.equal(newIdempotencyKey(), 'key-00010203-0405060708090a0b0c0d0e0f');
    Object.defineProperty(crypto, 'getRandomValues', {
      configurable: true,
      value: () => {
        throw new Error('secure randomness unavailable');
      },
    });
    assert.throws(newRandomId, /secure randomness unavailable/);
    assert.throws(newIdempotencyKey, /secure randomness unavailable/);
  } finally {
    if (originalUuid === undefined) delete crypto.randomUUID;
    else Object.defineProperty(crypto, 'randomUUID', originalUuid);
    if (originalValues === undefined) delete crypto.getRandomValues;
    else Object.defineProperty(crypto, 'getRandomValues', originalValues);
  }
});

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

test('Runtime responses must match the requested view and valid metadata', async () => {
  const viewId = 'a'.repeat(32);
  const view = {
    url: `http://127.0.0.1:7411/r/${'b'.repeat(64)}/`,
    enabled: true,
    artifact_id: artifact.id,
    revision: 1,
    view_id: viewId,
    generation: 1,
    initialization: 'c'.repeat(64),
    expires_at_ms: 1_000_000,
    lease_ms: 900_000,
  };
  let sent;
  const client = new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:7410',
    accessToken: 'token',
    fetch: async (url, init) => {
      sent = { url, init };
      return Response.json(view, { status: 201 });
    },
  });
  assert.deepEqual(
    await client.createRuntimeView(artifact.id, 1, viewId, 1),
    view,
  );
  assert.deepEqual(JSON.parse(sent.init.body), {
    view_id: viewId,
    generation: 1,
  });
  assert.equal(sent.init.headers.authorization, 'Bearer token');
  assert.deepEqual(
    await clientWith(() =>
      Response.json({ ...view, lease_ms: 0 }),
    ).createRuntimeView(artifact.id, 1, viewId, 1),
    { ...view, lease_ms: 0 },
  );
  for (const body of [
    { ...view, url: 'javascript:alert(1)' },
    { ...view, enabled: 'yes' },
    { ...view, initialization: 'invalid' },
    { ...view, artifact_id: `art_${'1'.repeat(32)}` },
    { ...view, revision: 2 },
    { ...view, view_id: 'd'.repeat(32) },
    { ...view, generation: 2 },
    { ...view, expires_at_ms: Number.MAX_SAFE_INTEGER + 1 },
    { ...view, lease_ms: -1 },
    { ...view, lease_ms: 0.5 },
    { ...view, lease_ms: 2_147_483_648 },
  ]) {
    const error = await failure(
      clientWith(() => Response.json(body)).createRuntimeView(
        artifact.id,
        1,
        viewId,
        1,
      ),
    );
    assert.equal(error.code, 'invalid_response', JSON.stringify(body));
  }
  const valid = {
    artifact_id: artifact.id,
    title: 'New name',
    current_revision: 1,
    metadata_revision: 2,
  };
  assert.deepEqual(
    await clientWith(() => Response.json(valid)).runtimeMetadata(view),
    valid,
  );
  for (const body of [
    { ...valid, artifact_id: `art_${'1'.repeat(32)}` },
    { ...valid, current_revision: 0 },
    { ...valid, metadata_revision: Number.MAX_SAFE_INTEGER + 1 },
  ]) {
    const error = await failure(
      clientWith(() => Response.json(body)).runtimeMetadata(view),
    );
    assert.equal(error.code, 'invalid_response', JSON.stringify(body));
  }
});

test('An export is a ZIP archive, whole, or a failure', async () => {
  // The end record of an empty archive.
  const zip = new Uint8Array(22);
  zip.set([0x50, 0x4b, 0x05, 0x06]);
  const answer = (type) => () =>
    new Response(zip, { status: 200, headers: { 'content-type': type } });
  assert.deepEqual(
    await clientWith(answer('application/zip')).exportRevision(artifact.id, 1),
    zip,
  );
  // A proxy's page or a server that does not export yet is not an archive.
  for (const type of ['text/html; charset=utf-8', 'application/json']) {
    const error = await failure(
      clientWith(answer(type)).exportRevision(artifact.id, 1),
    );
    assert.equal(error.code, 'invalid_response', type);
  }
  const cut = clientWith(() => {
    const response = cutOff(200, 'PK');
    response.headers.set('content-type', 'application/zip');
    return response;
  });
  const error = await failure(cut.exportRevision(artifact.id, 1));
  assert.equal(error.code, 'network_error');
});

/** Each whole number from 0 to `last`. */
function upTo(last) {
  return Array.from({ length: last + 1 }, (_, number) => number);
}

test('A part is accepted only when it is exactly the bytes asked for', async () => {
  const bytes = new TextEncoder().encode('abcdef');
  const ranges = upTo(4).flatMap((offset) =>
    [undefined, 1, 2, 3, 4, 5].map((length) => ({ offset, length })),
  );
  // Every Content-Range of positions up to 5, with a body of its length or
  // one byte off.
  const answers = upTo(5).flatMap((first) =>
    upTo(5).flatMap((last) =>
      upTo(5).flatMap((size) =>
        [-1, 0, 1].map((off) => ({
          first,
          last,
          size,
          received: Math.max(0, last - first + 1 + off),
        })),
      ),
    ),
  );
  for (const { offset, length } of ranges) {
    const range = length === undefined ? { offset } : { offset, length };
    for (const { first, last, size, received } of answers) {
      let sent;
      const client = new ArtifreeClient({
        baseUrl: 'http://127.0.0.1:7410',
        accessToken: 'token',
        fetch: async (_url, init) => {
          sent = init.headers.range;
          return new Response(bytes.slice(first, first + received), {
            status: 206,
            headers: { 'content-range': `bytes ${first}-${last}/${size}` },
          });
        },
      });
      // The size the answer states decides where the part ends.
      const end = Math.min(length === undefined ? size : offset + length, size);
      const exact =
        offset < end &&
        first === offset &&
        last === end - 1 &&
        received === end - offset;
      const what = `${JSON.stringify(range)}: bytes ${first}-${last}/${size}, ${received} bytes`;
      const read = client.readFilePart(artifact.id, 1, 'a.txt', range);
      if (exact)
        assert.deepEqual(
          await read,
          { content: bytes.slice(offset, end), offset, size },
          what,
        );
      else assert.equal((await failure(read)).code, 'invalid_response', what);
      const lastAsked = length === undefined ? '' : offset + length - 1;
      assert.equal(sent, `bytes=${offset}-${lastAsked}`, what);
    }
  }
});

test('A part comes only as one well-formed byte range', async () => {
  const answer =
    (status, headers, body = 'ab') =>
    () =>
      new Response(body, { status, headers });
  const read = (respond) =>
    clientWith(respond).readFilePart(artifact.id, 1, 'a.txt', {
      offset: 0,
      length: 2,
    });
  // The unit is matched without regard to case.
  const part = await read(answer(206, { 'content-range': 'BYTES 0-1/4' }));
  assert.deepEqual(
    [part.content, part.size],
    [new TextEncoder().encode('ab'), 4],
  );
  for (const [status, headers] of [
    // A server or proxy that ignored the range sent the whole file.
    [200, {}],
    [200, { 'content-range': 'bytes 0-1/4' }],
    [206, {}],
    [206, { 'content-range': 'bytes 0-1/*' }],
    [206, { 'content-range': 'bytes */4' }],
    [206, { 'content-range': 'bytes=0-1/4' }],
    [206, { 'content-range': 'bytes 0-1/4, bytes 0-1/4' }],
    [206, { 'content-range': 'items 0-1/4' }],
    [206, { 'content-range': 'bytes 0x0-1/4' }],
    [206, { 'content-range': 'bytes 0-1/9007199254740993' }],
    [206, { 'content-type': 'multipart/byteranges; boundary=x' }],
  ]) {
    const error = await failure(read(answer(status, headers)));
    assert.equal(error.code, 'invalid_response', JSON.stringify(headers));
  }
  // The whole file from a server or proxy that ignored the range is not
  // read. The stream is finite, so a client that reads it still ends.
  let cancelled = false;
  let chunks = 0;
  const whole = new ReadableStream({
    pull(controller) {
      chunks += 1;
      if (chunks > 64) controller.close();
      else controller.enqueue(new Uint8Array(1024));
    },
    cancel() {
      cancelled = true;
    },
  });
  const ignored = await failure(read(() => new Response(whole)));
  assert.deepEqual([ignored.code, cancelled], ['invalid_response', true]);
  // A range that starts at or past the end is the server's refusal.
  const refusal = await failure(
    read(() =>
      Response.json(
        { error: { code: 'range_not_satisfiable', message: 'past the end' } },
        { status: 416, headers: { 'content-range': 'bytes */0' } },
      ),
    ),
  );
  assert.deepEqual(
    [refusal.status, refusal.code],
    [416, 'range_not_satisfiable'],
  );
});

test('A range that cannot be asked for is refused before sending', async () => {
  let requests = 0;
  const client = clientWith(() => {
    requests += 1;
    return new Response('a', {
      status: 206,
      headers: {
        'content-range': `bytes ${Number.MAX_SAFE_INTEGER - 1}-${Number.MAX_SAFE_INTEGER - 1}/${Number.MAX_SAFE_INTEGER}`,
      },
    });
  });
  for (const range of [
    { offset: -1 },
    { offset: 1.5 },
    { offset: Number.NaN },
    { offset: 0, length: 0 },
    { offset: 0, length: 1.5 },
    { offset: Number.MAX_SAFE_INTEGER, length: 2 },
    { offset: 2, length: Number.MAX_SAFE_INTEGER },
  ]) {
    const error = await failure(
      client.readFilePart(artifact.id, 1, 'a.txt', range),
    );
    assert.deepEqual(
      [error.status, error.code],
      [0, 'invalid_request'],
      JSON.stringify(range),
    );
  }
  assert.equal(requests, 0);
  // Positions up to the largest safe integer can be asked for and read.
  const part = await client.readFilePart(artifact.id, 1, 'a.txt', {
    offset: Number.MAX_SAFE_INTEGER - 1,
    length: 1,
  });
  assert.equal(part.size, Number.MAX_SAFE_INTEGER);
});

test('Every artifact carries its trash time: null or a time', async () => {
  const read = (body) =>
    clientWith(() => Response.json(body)).getArtifact(artifact.id);
  assert.equal((await read(artifact)).deleted_at_ms, null);
  assert.equal(
    (await read({ ...artifact, deleted_at_ms: 5 })).deleted_at_ms,
    5,
  );
  const missing = { ...artifact };
  delete missing.deleted_at_ms;
  for (const body of [missing, { ...artifact, deleted_at_ms: '5' }]) {
    const error = await failure(read(body));
    assert.equal(error.code, 'invalid_response', JSON.stringify(body));
  }
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

test("A proxy's 429 is a rate limit although its body is not an API error", async () => {
  const client = clientWith(
    () =>
      new Response('<h1>Too Many Requests</h1>', {
        status: 429,
        headers: { 'retry-after': '4', 'content-type': 'text/html' },
      }),
  );
  const error = await failure(client.listArtifacts());
  assert.equal(error.code, 'rate_limited');
  assert.equal(error.retryAfterSeconds, 4);
});

test('A read is cancelled through its signal', async () => {
  let received;
  const client = new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:7410',
    accessToken: 'token',
    fetch: async (url, init) => {
      received = init.signal;
      init.signal.throwIfAborted();
      return Response.json(artifact);
    },
  });
  const controller = new AbortController();
  controller.abort();
  const error = await failure(
    client.getArtifact(artifact.id, { signal: controller.signal }),
  );
  assert.equal(received, controller.signal);
  assert.equal(error.code, 'network_error');
});

test('Lists send only the given query and return the next page token', async () => {
  const requests = [];
  const client = new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:7410',
    accessToken: 'token',
    fetch: async (url) => {
      requests.push(new URL(url));
      return Response.json({ artifacts: [artifact], next_page_token: 'next' });
    },
  });
  assert.deepEqual(await client.listArtifacts(), {
    artifacts: [artifact],
    next_page_token: 'next',
  });
  await client.listArtifacts({
    q: 'a plan & more',
    sort: 'created',
    pinned: false,
    limit: 2,
    page_token: 'next',
  });
  assert.equal(requests[0].search, '');
  assert.deepEqual(Object.fromEntries(requests[1].searchParams), {
    q: 'a plan & more',
    sort: 'created',
    pinned: 'false',
    limit: '2',
    page_token: 'next',
  });
  const missingFields = clientWith(() =>
    Response.json({ artifacts: [{ ...artifact, pinned: undefined }] }),
  );
  assert.equal(
    (await failure(missingFields.listArtifacts())).code,
    'invalid_response',
  );
});

test('A metadata conflict reports the current metadata revision', async () => {
  const client = clientWith(() =>
    Response.json(
      {
        error: {
          code: 'metadata_conflict',
          message: 'artifact metadata changed',
          current_metadata_revision: 4,
        },
      },
      { status: 409 },
    ),
  );
  const error = await failure(
    client.updateArtifact(
      artifact.id,
      { base_metadata_revision: 3, title: 'New' },
      { idempotencyKey: 'test-key-1' },
    ),
  );
  assert.equal(error.code, 'metadata_conflict');
  assert.equal(error.currentMetadataRevision, 4);
  assert.equal(error.currentRevision, undefined);
});

test('A restore names the revision, base, and key; manifests need digests', async () => {
  const file = {
    path: 'index.html',
    size: 3,
    sha256: 'ab'.repeat(32),
    media_type: 'text/html; charset=utf-8',
  };
  const commit = (files) => ({
    artifact: { ...artifact, current_revision: 3 },
    revision: { revision: 3, created_by: 'owner', created_at_ms: 1, files },
    replayed: false,
  });
  const sent = [];
  let files = [file];
  const client = new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:7410',
    accessToken: 'token',
    fetch: async (url, init) => {
      sent.push([init.method, String(url), init.headers, init.body]);
      return Response.json(commit(files), { status: 201 });
    },
  });
  const restore = () =>
    client.restoreRevision(
      artifact.id,
      1,
      { base_revision: 2 },
      { idempotencyKey: 'test-key-1' },
    );
  const restored = await restore();
  assert.deepEqual(restored.revision.files, [file]);
  const [method, url, headers, body] = sent[0];
  assert.equal(method, 'POST');
  assert.equal(
    url,
    `http://127.0.0.1:7410/v1/artifacts/${artifact.id}/revisions/1/restore`,
  );
  assert.equal(new Headers(headers).get('idempotency-key'), 'test-key-1');
  assert.deepEqual(JSON.parse(body), { base_revision: 2 });
  for (const sha256 of [undefined, 'AB'.repeat(32), 'ab'.repeat(31)]) {
    files = [{ ...file, sha256 }];
    const error = await failure(restore());
    assert.equal(error.code, 'invalid_response', String(sha256));
  }
});

test('A duplicate names the revision and title; a copy must say what it copied', async () => {
  const sent = [];
  let copied = ['files'];
  const client = new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:7410',
    accessToken: 'token',
    fetch: async (url, init) => {
      sent.push([String(url), init.body]);
      return Response.json(
        {
          artifact: { ...artifact, id: 'art_copy' },
          revision: {
            revision: 1,
            created_by: 'owner',
            created_at_ms: 1,
            files: [],
          },
          copied,
          replayed: false,
        },
        { status: 201 },
      );
    },
  });
  const duplicate = (request) =>
    client.duplicateArtifact(artifact.id, 2, request, {
      idempotencyKey: 'test-key-1',
    });
  assert.deepEqual((await duplicate({})).copied, ['files']);
  await duplicate({ title: 'Mine' });
  assert.deepEqual(sent, [
    [
      `http://127.0.0.1:7410/v1/artifacts/${artifact.id}/revisions/2/duplicate`,
      '{}',
    ],
    [
      `http://127.0.0.1:7410/v1/artifacts/${artifact.id}/revisions/2/duplicate`,
      '{"title":"Mine"}',
    ],
  ]);
  // A part this client does not know could be data it would not report.
  for (const parts of [undefined, 'files', ['files', 'data']]) {
    copied = parts;
    const error = await failure(duplicate({}));
    assert.equal(error.code, 'invalid_response', String(parts));
  }
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
    metadata_revision: 1,
    committed_at_ms: 1,
  });
const changeEvent = (sequence, change = 'revised') => ({
  type: 'change',
  change: {
    cursor: cursor(sequence),
    artifact_id: artifact.id,
    change,
    revision: sequence,
    metadata_revision: 1,
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
    `event: change\ndata: {"cursor":"${cursor(5)}","artifact_id":"${artifact.id}",\ndata: "change":"created","revision":5,"metadata_revision":1,"committed_at_ms":1}\n\n`,
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

test('Deletes and recoveries arrive as themselves', async () => {
  assert.deepEqual(
    await eventsOf(
      stream([
        `event: change\ndata: ${changeData(7, 'deleted')}\n\n`,
        `event: change\ndata: ${changeData(8, 'recovered')}\n\n`,
      ]),
    ),
    [changeEvent(7, 'deleted'), changeEvent(8, 'recovered')],
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
