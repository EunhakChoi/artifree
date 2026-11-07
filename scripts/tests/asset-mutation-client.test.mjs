import assert from 'node:assert/strict';
import { createHash, webcrypto } from 'node:crypto';
import { getEventListeners } from 'node:events';
import { mkdtemp, rm } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, before, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { runInNewContext } from 'node:vm';
import {
  ArtifreeClient,
  createUser,
  deleteAsset,
  listAssets,
  readAsset,
  uploadAsset,
} from '../../packages/client/dist/index.js';
import { buildServer, startServer } from '../lib/server-process.mjs';

// Decision 0045, "Proposed R09b2". Fake-fetch tests pin the client's own
// checks and the hostile-but-legal transport cases; the real-server tests
// below them prove the behavior against the Rust service. A test that waits
// for real time is a bug: stalls run on an injected clock.

const encoder = new TextEncoder();
const tick = () => new Promise((resolve) => setImmediate(resolve));
/** Lets every promise step of a pending client call run (digest, fetch, body). */
const flush = async () => {
  for (let step = 0; step < 4; step += 1) await tick();
};
const hang = () => new Promise(() => {});

const ARTIFACT = `art_${'a'.repeat(32)}`;
const ID = 'b'.repeat(64);
const OTHER_ID = 'c'.repeat(64);
const KEY = 'upload-key-0001';
const MIB = 1024 * 1024;
const BYTES = Buffer.from('﻿hello 한글\r\n');
const sha = (bytes) => createHash('sha256').update(bytes).digest('hex');
const ASSET = {
  id: ID,
  content_type: 'text/plain',
  size_bytes: BYTES.length,
  sha256: sha(BYTES),
  created_at_ms: 12,
};
const UPLOAD_RECEIPT = { asset: ASSET, asset_version: 1, replayed: false };
const DELETE_RECEIPT = {
  asset_id: ID,
  asset_version: 4,
  deleted: true,
  replayed: false,
};
const uploadRequest = () => ({
  base_asset_version: 0,
  content_type: 'text/plain',
  content_base64: BYTES.toString('base64'),
});
const invalid = { code: 'invalid_response' };

/** A clock that only moves when a test says so. */
class FakeClock {
  now = 0;
  #timers = new Map();
  #next = 0;

  constructor({ immediate = false } = {}) {
    this.immediate = immediate;
  }

  after(callback, milliseconds) {
    if (this.immediate) {
      callback();
      return () => {};
    }
    const id = this.#next++;
    this.#timers.set(id, { at: this.now + milliseconds, callback });
    return () => {
      this.#timers.delete(id);
    };
  }

  get pending() {
    return this.#timers.size;
  }

  async advance(milliseconds) {
    const target = this.now + milliseconds;
    for (;;) {
      let due;
      for (const [id, timer] of this.#timers)
        if (timer.at <= target && (due === undefined || timer.at < due.at))
          due = { id, ...timer };
      if (due === undefined) break;
      this.#timers.delete(due.id);
      this.now = due.at;
      due.callback();
      await flush();
    }
    this.now = target;
    await flush();
  }
}

/** A body the test feeds by hand; `cancelled.count` counts its cancels. */
function pipe() {
  let controller;
  const cancelled = { count: 0 };
  const body = new ReadableStream({
    start(c) {
      controller = c;
    },
    cancel() {
      cancelled.count += 1;
    },
  });
  return {
    body,
    cancelled,
    push(part) {
      controller.enqueue(
        typeof part === 'string' ? encoder.encode(part) : part,
      );
    },
    end() {
      controller.close();
    },
  };
}

/** A fetch that records every call and answers with `respond`. */
function recorder(respond) {
  const calls = [];
  return {
    calls,
    fetch: async (url, init) => {
      calls.push({ url, init });
      return respond(url, init, calls.length);
    },
  };
}

function clientWith(fetch, clock) {
  return new ArtifreeClient({
    baseUrl: 'http://localhost:7410',
    accessToken: 'token',
    fetch,
    ...(clock === undefined ? {} : { clock }),
  });
}

function clientAnswering(body, status = 201) {
  const answer = recorder(() => new Response(JSON.stringify(body), { status }));
  return { ...answer, client: clientWith(answer.fetch) };
}

async function withCrypto(value, run) {
  const descriptor = Object.getOwnPropertyDescriptor(globalThis, 'crypto');
  Object.defineProperty(globalThis, 'crypto', {
    configurable: true,
    value,
  });
  try {
    return await run();
  } finally {
    Object.defineProperty(globalThis, 'crypto', descriptor);
  }
}

const upload = (
  client,
  request = uploadRequest(),
  options = { idempotencyKey: KEY },
) => uploadAsset(client, ARTIFACT, request, options);
const remove = (
  client,
  request = { base_asset_version: 3 },
  options = { idempotencyKey: 'delete-key-0001' },
) => deleteAsset(client, ARTIFACT, ID, request, options);

test('an upload is one authenticated no-store redirect-refusing POST of exactly the envelope', async () => {
  const { client, calls } = clientAnswering({
    ...UPLOAD_RECEIPT,
    asset_version: 8,
  });
  const request = { ...uploadRequest(), base_asset_version: 7 };
  const receipt = await upload(client, request, { idempotencyKey: KEY });
  assert.equal(calls.length, 1);
  const { url, init } = calls[0];
  assert.equal(url, `http://localhost:7410/v1/artifacts/${ARTIFACT}/assets`);
  assert.equal(init.method, 'POST');
  assert.equal(init.redirect, 'error');
  assert.equal(init.cache, 'no-store');
  assert.equal(init.headers.authorization, 'Bearer token');
  assert.equal(init.headers['idempotency-key'], KEY);
  assert.equal(init.headers['content-type'], 'application/json');
  assert.equal(
    init.body,
    JSON.stringify({
      base_asset_version: 7,
      content_type: 'text/plain',
      content_base64: BYTES.toString('base64'),
    }),
  );
  assert.deepEqual(receipt, { ...UPLOAD_RECEIPT, asset_version: 8 });
});

test('an upload receipt is returned as the checked, plain value', async () => {
  const { client } = clientAnswering({
    ...UPLOAD_RECEIPT,
    extra: 'dropped',
    asset: { ...ASSET, extra: 'dropped' },
  });
  assert.deepEqual(await upload(client), UPLOAD_RECEIPT);
});

test('a delete is one authenticated no-store redirect-refusing DELETE of the base', async () => {
  const { client, calls } = clientAnswering(DELETE_RECEIPT, 200);
  assert.deepEqual(await remove(client), DELETE_RECEIPT);
  assert.equal(calls.length, 1);
  const { url, init } = calls[0];
  assert.equal(
    url,
    `http://localhost:7410/v1/artifacts/${ARTIFACT}/assets/${ID}`,
  );
  assert.equal(init.method, 'DELETE');
  assert.equal(init.redirect, 'error');
  assert.equal(init.cache, 'no-store');
  assert.equal(init.headers['idempotency-key'], 'delete-key-0001');
  assert.equal(init.body, '{"base_asset_version":3}');
});

// ---------------------------------------------------------------------------
// Stable envelope: refused before any request.

const smallestOverBase64 = (() => {
  // 5 MiB + 1 bytes, ending in a full three-byte group: one byte over.
  return Buffer.alloc(5 * MIB + 1, 97).toString('base64');
})();

const badUploads = [
  ['a negative base', { base_asset_version: -1 }],
  ['a fractional base', { base_asset_version: 1.5 }],
  ['a string base', { base_asset_version: '0' }],
  ['an unsafe base', { base_asset_version: 2 ** 53 }],
  ['a base whose successor is unsafe', { base_asset_version: 2 ** 53 - 1 }],
  ['an uppercase MIME', { content_type: 'Text/Plain' }],
  ['a MIME with parameters', { content_type: 'text/plain; charset=utf-8' }],
  ['a MIME without a slash', { content_type: 'text' }],
  ['an empty MIME', { content_type: '' }],
  ['a MIME of 128 characters', { content_type: `text/${'a'.repeat(123)}` }],
  ['a non-string MIME', { content_type: 5 }],
  ['a non-string payload', { content_base64: 5 }],
  ['whitespace in base64', { content_base64: 'aGVs bG8=' }],
  ['URL-safe base64', { content_base64: '__-_' }],
  ['unpadded base64', { content_base64: 'aGVsbG8' }],
  ['noncanonical trailing bits', { content_base64: 'aGVsbG9=' }],
  ['noncanonical bits before two pads', { content_base64: 'aGVsbB==' }],
  ['padding inside the text', { content_base64: 'aG=sbG8=' }],
  ['length that is not a multiple of four', { content_base64: 'aGVsQ=' }],
  ['padding of three characters', { content_base64: 'aGVsb===' }],
  ['base64 of more than 5 MiB', { content_base64: smallestOverBase64 }],
  [
    'base64 text far over 5 MiB',
    { content_base64: `${smallestOverBase64}AAAA` },
  ],
];
for (const [name, change] of badUploads) {
  test(`an upload with ${name} is refused with no request`, async () => {
    const { client, calls } = clientAnswering(UPLOAD_RECEIPT);
    await assert.rejects(upload(client, { ...uploadRequest(), ...change }), {
      code: 'invalid_request',
      status: 0,
    });
    assert.equal(calls.length, 0);
  });
}

for (const [name, key] of [
  ['too short', 'short'],
  ['too long', 'k'.repeat(129)],
  ['spaced', 'has space in key'],
  ['not ASCII', 'key-한글-12345'],
  ['not a string', 12345678],
  ['absent', undefined],
]) {
  test(`an idempotency key that is ${name} is refused with no request`, async () => {
    const { client, calls } = clientAnswering(UPLOAD_RECEIPT);
    await assert.rejects(
      upload(client, uploadRequest(), { idempotencyKey: key }),
      {
        code: 'invalid_request',
        status: 0,
      },
    );
    await assert.rejects(
      remove(client, { base_asset_version: 0 }, { idempotencyKey: key }),
      {
        code: 'invalid_request',
        status: 0,
      },
    );
    assert.equal(calls.length, 0);
  });
}

test('identifiers, a missing request and a value that is not a signal are refused with no request', async () => {
  const { client, calls } = clientAnswering(UPLOAD_RECEIPT);
  const options = { idempotencyKey: KEY };
  await assert.rejects(
    uploadAsset(client, '../users', uploadRequest(), options),
    {
      code: 'invalid_request',
    },
  );
  await assert.rejects(
    deleteAsset(
      client,
      ARTIFACT,
      '../content',
      { base_asset_version: 0 },
      options,
    ),
    {
      code: 'invalid_request',
    },
  );
  await assert.rejects(uploadAsset(client, ARTIFACT, null, options), {
    code: 'invalid_request',
  });
  await assert.rejects(
    uploadAsset(client, ARTIFACT, uploadRequest(), undefined),
    {
      code: 'invalid_request',
    },
  );
  await assert.rejects(
    uploadAsset(client, ARTIFACT, uploadRequest(), { ...options, signal: {} }),
    { code: 'invalid_request' },
  );
  await assert.rejects(
    deleteAsset(
      client,
      ARTIFACT,
      ID,
      { base_asset_version: 2 ** 53 - 1 },
      options,
    ),
    { code: 'invalid_request' },
  );
  assert.equal(calls.length, 0);
});

const notSignals = [
  ['an object with none of the members', {}],
  ['null', null],
  ['a string', 'signal'],
  [
    'an object without removeEventListener',
    { aborted: false, addEventListener() {} },
  ],
  [
    'an object without addEventListener',
    { aborted: false, removeEventListener() {} },
  ],
  [
    'an object whose aborted is not a boolean',
    { aborted: 0, addEventListener() {}, removeEventListener() {} },
  ],
  [
    'an object whose listeners are not functions',
    { aborted: false, addEventListener: true, removeEventListener: true },
  ],
];
for (const [name, signal] of notSignals) {
  test(`a signal argument that is ${name} is refused with no request`, async () => {
    const { client, calls } = clientAnswering(UPLOAD_RECEIPT);
    const refusal = {
      code: 'invalid_request',
      status: 0,
      message: 'signal must be an AbortSignal.',
    };
    await assert.rejects(
      upload(client, uploadRequest(), { idempotencyKey: KEY, signal }),
      refusal,
    );
    await assert.rejects(
      deleteAsset(
        client,
        ARTIFACT,
        ID,
        { base_asset_version: 3 },
        { idempotencyKey: 'delete-key-0001', signal },
      ),
      refusal,
    );
    assert.equal(calls.length, 0);
  });
}

for (const bytes of [
  Buffer.from('a'),
  Buffer.from('ab'),
  Buffer.from('abc'),
  Buffer.from('abcd'),
  Buffer.from('\ufeffa\tb\r\n한'),
]) {
  test(`${bytes.length} bytes are decoded exactly: the digest in the receipt is the original's`, async () => {
    const receipt = {
      asset: { ...ASSET, size_bytes: bytes.length, sha256: sha(bytes) },
      asset_version: 1,
      replayed: false,
    };
    const { client, calls } = clientAnswering(receipt);
    assert.deepEqual(
      await upload(client, text(bytes, 'text/plain', 0)),
      receipt,
    );
    assert.equal(
      JSON.parse(calls[0].init.body).content_base64,
      bytes.toString('base64'),
    );
  });
}

test('exactly 5 MiB is sent within the 8 MiB envelope; the empty payload is sent too', async () => {
  const bytes = Buffer.alloc(5 * MIB, 97);
  const big = recorder(
    () =>
      new Response(
        JSON.stringify({
          ...UPLOAD_RECEIPT,
          asset: { ...ASSET, size_bytes: bytes.length, sha256: sha(bytes) },
        }),
        { status: 201 },
      ),
  );
  const receipt = await upload(clientWith(big.fetch), {
    ...uploadRequest(),
    content_base64: bytes.toString('base64'),
  });
  assert.equal(receipt.asset.size_bytes, 5 * MIB);
  assert.ok(Buffer.byteLength(big.calls[0].init.body) < 8 * MIB);
  // The server alone refuses empty content, with its own error.
  const empty = recorder(() =>
    Response.json(
      { error: { code: 'invalid_asset', message: 'empty' } },
      { status: 422 },
    ),
  );
  await assert.rejects(
    upload(clientWith(empty.fetch), { ...uploadRequest(), content_base64: '' }),
    { code: 'invalid_asset', status: 422 },
  );
  assert.equal(empty.calls.length, 1);
  assert.equal(JSON.parse(empty.calls[0].init.body).content_base64, '');
});

test('the client does not narrow MIME to the server allowlist: a historical image envelope replays', async () => {
  const png = Buffer.from([0x89, 0x50, 0x4e, 0x47]);
  const receipt = {
    asset: {
      ...ASSET,
      content_type: 'image/png',
      size_bytes: png.length,
      sha256: sha(png),
    },
    asset_version: 1,
    replayed: true,
  };
  const { client, calls } = clientAnswering(receipt);
  const result = await upload(client, {
    base_asset_version: 0,
    content_type: 'image/png',
    content_base64: png.toString('base64'),
  });
  assert.deepEqual(result, receipt);
  assert.equal(calls.length, 1);
  // A declaration the server will refuse is still the server's to refuse.
  const refusing = recorder(() =>
    Response.json(
      { error: { code: 'asset_type_unsupported', message: 'no' } },
      { status: 422 },
    ),
  );
  await assert.rejects(
    upload(clientWith(refusing.fetch), {
      ...uploadRequest(),
      content_type: 'text/html',
    }),
    { code: 'asset_type_unsupported', status: 422 },
  );
  assert.equal(refusing.calls.length, 1);
});

// ---------------------------------------------------------------------------
// Receipt checks: each field is corrupted on its own, initial and replayed.

const badUploadReceipts = [
  ['asset id too short', { asset: { ...ASSET, id: 'b'.repeat(63) } }],
  ['asset id uppercase', { asset: { ...ASSET, id: 'B'.repeat(64) } }],
  ['asset id not a string', { asset: { ...ASSET, id: 5 } }],
  [
    'MIME of another allowed type',
    { asset: { ...ASSET, content_type: 'text/csv' } },
  ],
  [
    'MIME of a historical image',
    { asset: { ...ASSET, content_type: 'image/png' } },
  ],
  ['MIME in other case', { asset: { ...ASSET, content_type: 'Text/Plain' } }],
  ['size one larger', { asset: { ...ASSET, size_bytes: BYTES.length + 1 } }],
  ['size one smaller', { asset: { ...ASSET, size_bytes: BYTES.length - 1 } }],
  [
    'size as a string',
    { asset: { ...ASSET, size_bytes: String(BYTES.length) } },
  ],
  ['digest of other bytes', { asset: { ...ASSET, sha256: sha('other') } }],
  [
    'digest in uppercase',
    { asset: { ...ASSET, sha256: ASSET.sha256.toUpperCase() } },
  ],
  ['digest missing', { asset: { ...ASSET, sha256: undefined } }],
  ['version equal to the base', { asset_version: 0 }],
  ['version two past the base', { asset_version: 2 }],
  ['fractional version', { asset_version: 1.5 }],
  ['unsafe version', { asset_version: 2 ** 53 }],
  ['string version', { asset_version: '1' }],
  ['creation time fractional', { asset: { ...ASSET, created_at_ms: 1.5 } }],
  ['creation time a string', { asset: { ...ASSET, created_at_ms: '12' } }],
  ['replayed flag missing', { replayed: undefined }],
  ['replayed flag a string', { replayed: 'false' }],
  ['asset missing', { asset: undefined }],
  ['asset a list', { asset: [] }],
];
for (const replayed of [false, true]) {
  for (const [name, change] of badUploadReceipts) {
    test(`an ${replayed ? 'replayed' : 'initial'} upload receipt with ${name} is invalid`, async () => {
      const { client, calls } = clientAnswering({
        ...UPLOAD_RECEIPT,
        replayed,
        ...change,
      });
      await assert.rejects(upload(client), invalid);
      assert.equal(calls.length, 1);
    });
  }
}

const badDeleteReceipts = [
  ['another asset id', { asset_id: OTHER_ID }],
  ['an invalid asset id', { asset_id: 'x' }],
  ['version equal to the base for a deletion', { asset_version: 3 }],
  ['version two past the base', { asset_version: 5 }],
  [
    'a version past the base for no deletion',
    { deleted: false, asset_version: 4 },
  ],
  [
    'a version below the base for no deletion',
    { deleted: false, asset_version: 2 },
  ],
  ['fractional version', { asset_version: 3.5 }],
  ['string version', { asset_version: '4' }],
  ['deleted missing', { deleted: undefined }],
  ['deleted a string', { deleted: 'true' }],
  ['replayed missing', { replayed: undefined }],
  ['replayed a number', { replayed: 0 }],
];
for (const replayed of [false, true]) {
  for (const [name, change] of badDeleteReceipts) {
    test(`an ${replayed ? 'replayed' : 'initial'} delete receipt with ${name} is invalid`, async () => {
      const { client, calls } = clientAnswering(
        { ...DELETE_RECEIPT, replayed, ...change },
        200,
      );
      await assert.rejects(remove(client), invalid);
      assert.equal(calls.length, 1);
    });
  }
}

test('a deletion that removed nothing keeps the base version, and a replay keeps its first result', async () => {
  for (const replayed of [false, true]) {
    const receipt = {
      ...DELETE_RECEIPT,
      deleted: false,
      asset_version: 3,
      replayed,
    };
    const { client } = clientAnswering(receipt, 200);
    assert.deepEqual(await remove(client), receipt);
  }
  const replayedDeletion = { ...DELETE_RECEIPT, replayed: true };
  assert.deepEqual(
    await remove(clientAnswering(replayedDeletion, 200).client),
    replayedDeletion,
  );
});

// ---------------------------------------------------------------------------
// Only exactly 201 / 200 is a receipt.

const wrongSuccessStatuses = [200, 202, 203, 204, 205, 206];
for (const replayed of [false, true]) {
  for (const status of wrongSuccessStatuses.filter((s) => s !== 200)) {
    test(`an upload answered ${status} is invalid even with a valid ${replayed ? 'replayed' : 'initial'} receipt, and its body is cancelled unread`, async () => {
      const source = pipe();
      source.push(JSON.stringify({ ...UPLOAD_RECEIPT, replayed }));
      const nullBody = status === 204 || status === 205;
      const answer = recorder(
        () => new Response(nullBody ? null : source.body, { status }),
      );
      await assert.rejects(upload(clientWith(answer.fetch)), {
        ...invalid,
        status,
      });
      assert.equal(source.cancelled.count, nullBody ? 0 : 1);
    });
  }
  test(`an upload answered 200 is invalid even with a valid ${replayed ? 'replayed' : 'initial'} receipt`, async () => {
    const { client } = clientAnswering({ ...UPLOAD_RECEIPT, replayed }, 200);
    await assert.rejects(upload(client), { ...invalid, status: 200 });
  });
  for (const status of [201, 202, 204]) {
    test(`a delete answered ${status} is invalid even with a valid ${replayed ? 'replayed' : 'initial'} receipt`, async () => {
      const answer = recorder(() =>
        status === 204
          ? new Response(null, { status })
          : new Response(JSON.stringify({ ...DELETE_RECEIPT, replayed }), {
              status,
            }),
      );
      await assert.rejects(remove(clientWith(answer.fetch)), {
        ...invalid,
        status,
      });
    });
  }
}

test('a redirect status handed back by a fetch wrapper is not a receipt', async () => {
  for (const status of [301, 302, 307]) {
    const answer = recorder(
      () =>
        new Response(JSON.stringify(UPLOAD_RECEIPT), {
          status,
          headers: { location: '/elsewhere' },
        }),
    );
    await assert.rejects(upload(clientWith(answer.fetch)), {
      ...invalid,
      status,
    });
    assert.equal(answer.calls.length, 1);
  }
});

// ---------------------------------------------------------------------------
// Bounds of the response bodies.

function oversized(size, status, onCancel) {
  return new Response(
    new ReadableStream({
      start(controller) {
        controller.enqueue(new Uint8Array(size));
        // One queued byte past the limit: the guard must cancel before EOF.
        controller.enqueue(new Uint8Array(1));
        controller.close();
      },
      cancel: onCancel,
    }),
    { status },
  );
}

for (const [name, run, status] of [
  ['upload', upload, 201],
  ['delete', remove, 200],
]) {
  test(`a ${name} success body over 64 KiB is invalid and cancelled before its end`, async () => {
    let cancelled = 0;
    const answer = recorder(() =>
      oversized(65_537, status, () => {
        cancelled += 1;
      }),
    );
    await assert.rejects(run(clientWith(answer.fetch)), invalid);
    assert.equal(cancelled, 1);
  });
  test(`a ${name} error body over 4 KiB is no API body: the status decides, and it is cancelled`, async () => {
    let cancelled = 0;
    const answer = recorder(() =>
      oversized(4097, 409, () => {
        cancelled += 1;
      }),
    );
    await assert.rejects(run(clientWith(answer.fetch)), {
      code: 'invalid_response',
      status: 409,
    });
    assert.equal(cancelled, 1);
  });
  test(`a ${name} error body of exactly 4 KiB keeps its API error`, async () => {
    const text = JSON.stringify({
      error: {
        code: 'asset_conflict',
        message: 'stale',
        current_asset_version: 9,
      },
    }).padEnd(4096, ' ');
    const answer = recorder(() => new Response(text, { status: 409 }));
    await assert.rejects(run(clientWith(answer.fetch)), (error) => {
      assert.equal(error.code, 'asset_conflict');
      assert.equal(error.status, 409);
      assert.equal(error.body.error.current_asset_version, 9);
      return true;
    });
  });
  test(`a ${name} success body of exactly 64 KiB is read`, async () => {
    const receipt = name === 'upload' ? UPLOAD_RECEIPT : DELETE_RECEIPT;
    const answer = recorder(
      () =>
        new Response(JSON.stringify(receipt).padEnd(65_536, ' '), { status }),
    );
    assert.deepEqual(await run(clientWith(answer.fetch)), receipt);
  });
  for (const [what, response] of [
    ['malformed JSON', () => new Response('{"asset":', { status })],
    ['an empty body', () => new Response(null, { status })],
    ['a JSON scalar', () => new Response('7', { status })],
    ['a list', () => new Response('[]', { status })],
  ]) {
    test(`a ${name} receipt that is ${what} is invalid`, async () => {
      await assert.rejects(run(clientWith(recorder(response).fetch)), invalid);
    });
  }
}

test('a proxy page without an API body leaves the status in charge and never a receipt', async () => {
  const answer = recorder(
    () => new Response('<html>bad gateway</html>', { status: 502 }),
  );
  await assert.rejects(upload(clientWith(answer.fetch)), {
    code: 'invalid_response',
    status: 502,
  });
  assert.equal(answer.calls.length, 1);
});

// ---------------------------------------------------------------------------
// Stalls, lost bodies and the clock.

for (const [name, run, status, valid] of [
  ['upload', upload, 201, UPLOAD_RECEIPT],
  ['delete', remove, 200, DELETE_RECEIPT],
]) {
  test(`a ${name} body silent for 45 seconds after its headers is a network failure that cancels it`, async () => {
    const clock = new FakeClock();
    const source = pipe();
    const answer = recorder(() => new Response(source.body, { status }));
    const call = run(clientWith(answer.fetch, clock));
    const settled = assert.rejects(call, { code: 'network_error', status });
    await flush();
    await flush();
    await clock.advance(44_999);
    assert.equal(source.cancelled.count, 0);
    await clock.advance(1);
    await settled;
    assert.equal(source.cancelled.count, 1);
    assert.equal(clock.pending, 0);
    assert.equal(answer.calls.length, 1);
  });

  test(`empty chunks are not bytes: a ${name} body stalls 45 seconds after its last real byte`, async () => {
    const clock = new FakeClock();
    const source = pipe();
    const answer = recorder(() => new Response(source.body, { status }));
    const settled = assert.rejects(run(clientWith(answer.fetch, clock)), {
      code: 'network_error',
    });
    await flush();
    source.push(JSON.stringify(valid).slice(0, 5));
    await flush();
    await clock.advance(30_000);
    for (let index = 0; index < 5; index += 1) {
      source.push(new Uint8Array(0));
      await clock.advance(3000);
    }
    await clock.advance(1000);
    await settled;
    assert.equal(source.cancelled.count, 1);
    assert.equal(clock.pending, 0);
  });

  test(`a clock that calls back at once stalls a ${name} body without leaving a timer`, async () => {
    const clock = new FakeClock({ immediate: true });
    const source = pipe();
    const answer = recorder(() => new Response(source.body, { status }));
    await assert.rejects(run(clientWith(answer.fetch, clock)), {
      code: 'network_error',
    });
    assert.equal(source.cancelled.count, 1);
    assert.equal(clock.pending, 0);
  });

  test(`a ${name} body already used is a network failure, not a receipt`, async () => {
    const used = new Response(JSON.stringify(valid), { status });
    await used.text();
    const answer = recorder(() => used);
    await assert.rejects(run(clientWith(answer.fetch)), {
      code: 'network_error',
    });
  });

  test(`a ${name} body that errors after a first chunk is a network failure`, async () => {
    const answer = recorder(
      () =>
        new Response(
          new ReadableStream({
            start(controller) {
              controller.enqueue(encoder.encode('{"asset'));
              controller.error(new Error('reset'));
            },
          }),
          { status },
        ),
    );
    await assert.rejects(run(clientWith(answer.fetch)), {
      code: 'network_error',
    });
  });

  test(`a ${name} error body that stalls leaves the status in charge`, async () => {
    const clock = new FakeClock();
    const source = pipe();
    const answer = recorder(() => new Response(source.body, { status: 409 }));
    const settled = assert.rejects(run(clientWith(answer.fetch, clock)), {
      code: 'invalid_response',
      status: 409,
    });
    await flush();
    await clock.advance(45_000);
    await settled;
    assert.equal(source.cancelled.count, 1);
    assert.equal(clock.pending, 0);
  });

  test(`a ${name} body whose read and cancel never settle still ends at the stall limit`, async () => {
    const clock = new FakeClock();
    let cancels = 0;
    const answer = recorder(
      () =>
        new Response(
          new ReadableStream({
            pull: hang,
            cancel() {
              cancels += 1;
              return hang();
            },
          }),
          { status },
        ),
    );
    const settled = assert.rejects(run(clientWith(answer.fetch, clock)), {
      code: 'network_error',
    });
    await flush();
    await clock.advance(45_000);
    await settled;
    assert.equal(cancels, 1);
  });

  test(`aborting a ${name} while its body is read is a network failure that cancels the body`, async () => {
    const controller = new AbortController();
    const source = pipe();
    const answer = recorder(() => new Response(source.body, { status }));
    const options = { idempotencyKey: KEY, signal: controller.signal };
    const call =
      name === 'upload'
        ? upload(clientWith(answer.fetch), uploadRequest(), options)
        : remove(clientWith(answer.fetch), { base_asset_version: 3 }, options);
    const settled = assert.rejects(call, { code: 'network_error' });
    await flush();
    await flush();
    controller.abort();
    await settled;
    assert.equal(source.cancelled.count, 1);
    assert.equal(getEventListeners(controller.signal, 'abort').length, 0);
  });

  test(`a ${name} is one request: a lost connection, 503 and 429 are never retried or rotated`, async () => {
    const lost = recorder(() => {
      throw new TypeError('fetch failed');
    });
    await assert.rejects(run(clientWith(lost.fetch)), {
      code: 'network_error',
      status: 0,
    });
    assert.equal(lost.calls.length, 1);
    const busy = recorder(() =>
      Response.json(
        { error: { code: 'unavailable', message: 'later' } },
        { status: 503 },
      ),
    );
    await assert.rejects(run(clientWith(busy.fetch)), {
      code: 'unavailable',
      status: 503,
    });
    assert.equal(busy.calls.length, 1);
    const limited = recorder(() =>
      Response.json(
        { error: { code: 'rate_limited', message: 'slow' } },
        { status: 429, headers: { 'retry-after': '7' } },
      ),
    );
    await assert.rejects(run(clientWith(limited.fetch)), {
      code: 'rate_limited',
      retryAfterSeconds: 7,
    });
    assert.equal(limited.calls.length, 1);
  });
}

// ---------------------------------------------------------------------------
// Capture of the caller's objects, Web Crypto and abort steps.

test('the attempt and its receipt checks use the values captured before the first await', async () => {
  const request = uploadRequest();
  const options = { idempotencyKey: KEY };
  const original = JSON.stringify(request);
  const mutate = () => {
    request.base_asset_version = 99;
    request.content_type = 'text/csv';
    request.content_base64 = Buffer.from('changed').toString('base64');
    options.idempotencyKey = 'changed-key-0001';
  };
  // Mutated while the digest is pending, and again while the request is held.
  const crypto = {
    subtle: {
      digest: async (algorithm, data) => {
        mutate();
        return webcrypto.subtle.digest(algorithm, data);
      },
    },
  };
  const held = recorder(async () => {
    mutate();
    await flush();
    return new Response(JSON.stringify(UPLOAD_RECEIPT), { status: 201 });
  });
  const receipt = await withCrypto(crypto, () =>
    upload(clientWith(held.fetch), request, options),
  );
  assert.deepEqual(receipt, UPLOAD_RECEIPT);
  assert.equal(held.calls[0].init.body, original);
  assert.equal(held.calls[0].init.headers['idempotency-key'], KEY);
  // A receipt for the mutated values is not accepted for the original attempt.
  const next = uploadRequest();
  const mutated = recorder(() => {
    next.content_type = 'text/csv';
    return new Response(
      JSON.stringify({
        asset: {
          ...ASSET,
          content_type: 'text/csv',
          size_bytes: 7,
          sha256: sha('changed'),
        },
        asset_version: 100,
        replayed: false,
      }),
      { status: 201 },
    );
  });
  await assert.rejects(upload(clientWith(mutated.fetch), next), invalid);
});

test('a delete likewise keeps its captured base, key and asset id', async () => {
  const request = { base_asset_version: 3 };
  const options = { idempotencyKey: 'delete-key-0001' };
  const held = recorder(async () => {
    request.base_asset_version = 40;
    options.idempotencyKey = 'changed-key-0001';
    await flush();
    return new Response(JSON.stringify(DELETE_RECEIPT), { status: 200 });
  });
  assert.deepEqual(
    await remove(clientWith(held.fetch), request, options),
    DELETE_RECEIPT,
  );
  assert.equal(held.calls[0].init.body, '{"base_asset_version":3}');
  assert.equal(
    held.calls[0].init.headers['idempotency-key'],
    'delete-key-0001',
  );
});

const brokenCryptos = [
  ['absent', undefined],
  ['without subtle', {}],
  [
    'with a failing digest',
    {
      subtle: {
        digest: async () => {
          throw new Error('no');
        },
      },
    },
  ],
  [
    'with a digest that throws at once',
    {
      subtle: {
        digest: () => {
          throw new Error('no');
        },
      },
    },
  ],
];
for (const [name, crypto] of brokenCryptos) {
  test(`Web Crypto ${name} refuses an upload with zero requests`, async () => {
    const { client, calls } = clientAnswering(UPLOAD_RECEIPT);
    const controller = new AbortController();
    await withCrypto(crypto, () =>
      assert.rejects(
        upload(client, uploadRequest(), {
          idempotencyKey: KEY,
          signal: controller.signal,
        }),
        { code: 'invalid_request', status: 0 },
      ),
    );
    assert.equal(calls.length, 0);
    assert.equal(getEventListeners(controller.signal, 'abort').length, 0);
  });
}

test('a digest that never settles ends by abort with zero requests and no listener', async () => {
  const controller = new AbortController();
  const { client, calls } = clientAnswering(UPLOAD_RECEIPT);
  const crypto = { subtle: { digest: hang } };
  await withCrypto(crypto, async () => {
    const call = upload(client, uploadRequest(), {
      idempotencyKey: KEY,
      signal: controller.signal,
    });
    const settled = assert.rejects(call, { code: 'network_error', status: 0 });
    await flush();
    controller.abort();
    await settled;
  });
  assert.equal(calls.length, 0);
  assert.equal(getEventListeners(controller.signal, 'abort').length, 0);
});

test('an abort before the call, during the digest and after it sends nothing', async () => {
  const { client, calls } = clientAnswering(UPLOAD_RECEIPT);
  const before = new AbortController();
  before.abort();
  await assert.rejects(
    upload(client, uploadRequest(), {
      idempotencyKey: KEY,
      signal: before.signal,
    }),
    { code: 'network_error', status: 0 },
  );
  await assert.rejects(
    deleteAsset(
      client,
      ARTIFACT,
      ID,
      { base_asset_version: 0 },
      { idempotencyKey: KEY, signal: before.signal },
    ),
    { code: 'network_error', status: 0 },
  );
  // The digest resolves in the same step that the signal aborts.
  const during = new AbortController();
  const crypto = {
    subtle: {
      digest: async (algorithm, data) => {
        const digest = await webcrypto.subtle.digest(algorithm, data);
        during.abort();
        return digest;
      },
    },
  };
  await withCrypto(crypto, () =>
    assert.rejects(
      upload(client, uploadRequest(), {
        idempotencyKey: KEY,
        signal: during.signal,
      }),
      { code: 'network_error', status: 0 },
    ),
  );
  assert.equal(calls.length, 0);
  assert.equal(getEventListeners(during.signal, 'abort').length, 0);
});

/**
 * A signal-like object from another JavaScript realm. node:vm has no
 * AbortController, so its three members are written inside that realm. The
 * tests check that it is not an AbortSignal of this realm.
 */
const FOREIGN_SIGNAL = `(() => {
  const listeners = [];
  const signal = {
    aborted: false,
    addEventListener(type, listener, options) {
      if (type === 'abort')
        listeners.push({ listener, once: options?.once === true });
    },
    removeEventListener(type, listener) {
      const at = listeners.findIndex((entry) => entry.listener === listener);
      if (type === 'abort' && at >= 0) listeners.splice(at, 1);
    },
  };
  return {
    signal,
    listenerCount: () => listeners.length,
    abort() {
      if (signal.aborted) return;
      signal.aborted = true;
      for (const entry of [...listeners]) {
        if (entry.once) listeners.splice(listeners.indexOf(entry), 1);
        entry.listener({ type: 'abort' });
      }
    },
  };
})()`;
const foreignSignal = () => runInNewContext(FOREIGN_SIGNAL);
const abortedNothingSent = {
  code: 'network_error',
  status: 0,
  message: 'The asset request was aborted; nothing was sent.',
};

test('a signal from another realm is accepted, and it is passed with the request', async () => {
  const foreign = foreignSignal();
  assert.equal(foreign.signal instanceof AbortSignal, false);
  const { client, calls } = clientAnswering(UPLOAD_RECEIPT);
  await assert.doesNotReject(
    upload(client, uploadRequest(), {
      idempotencyKey: KEY,
      signal: foreign.signal,
    }),
    'a signal from another realm must pass the check',
  );
  assert.equal(calls.length, 1);
  assert.equal(calls[0].init.signal, foreign.signal);
  assert.equal(foreign.listenerCount(), 0);
});

test('a foreign-realm signal aborted before the call sends nothing', async () => {
  const foreign = foreignSignal();
  foreign.abort();
  const { client, calls } = clientAnswering(UPLOAD_RECEIPT);
  await assert.rejects(
    upload(client, uploadRequest(), {
      idempotencyKey: KEY,
      signal: foreign.signal,
    }),
    abortedNothingSent,
  );
  assert.equal(calls.length, 0);
  assert.equal(foreign.listenerCount(), 0);
});

test('a foreign-realm signal aborted while the digest is pending sends nothing', async () => {
  const foreign = foreignSignal();
  const { client, calls } = clientAnswering(UPLOAD_RECEIPT);
  await withCrypto({ subtle: { digest: hang } }, async () => {
    const call = upload(client, uploadRequest(), {
      idempotencyKey: KEY,
      signal: foreign.signal,
    });
    // Observed at once so that an early rejection is reported by the assertion
    // below rather than as an unhandled rejection.
    call.catch(() => undefined);
    await flush();
    foreign.abort();
    await assert.rejects(call, abortedNothingSent);
  });
  assert.equal(calls.length, 0);
  assert.equal(foreign.listenerCount(), 0);
});

test('an abort landing as the digest settles, before dispatch, sends nothing', async () => {
  const controller = new AbortController();
  const { signal } = controller;
  // The digest removes its abort listener as it settles, its last step before
  // dispatch. Aborting in that same step leaves no listener to report it.
  const removeListener = AbortSignal.prototype.removeEventListener;
  signal.removeEventListener = (type, listener, options) => {
    removeListener.call(signal, type, listener, options);
    controller.abort();
  };
  const { client, calls } = clientAnswering(UPLOAD_RECEIPT);
  await assert.rejects(
    upload(client, uploadRequest(), { idempotencyKey: KEY, signal }),
    abortedNothingSent,
  );
  assert.equal(calls.length, 0);
  assert.equal(getEventListeners(signal, 'abort').length, 0);
});

test('a finished call leaves no listener on its signal, whether it succeeded or failed', async () => {
  const refusal = { error: { code: 'asset_conflict', message: 'stale' } };
  for (const [run, answer] of [
    [upload, () => Response.json(UPLOAD_RECEIPT, { status: 201 })],
    [upload, () => Response.json(refusal, { status: 409 })],
    [remove, () => Response.json(DELETE_RECEIPT, { status: 200 })],
    [remove, () => Response.json(refusal, { status: 409 })],
  ]) {
    const controller = new AbortController();
    const client = clientWith(recorder(answer).fetch);
    const options = { idempotencyKey: KEY, signal: controller.signal };
    await Promise.allSettled([
      run(
        client,
        run === upload ? uploadRequest() : { base_asset_version: 3 },
        options,
      ),
    ]);
    assert.equal(getEventListeners(controller.signal, 'abort').length, 0);
  }
});

test('aborting while the request is pending ends the call and leaves no listener', async () => {
  const controller = new AbortController();
  let entered;
  const started = new Promise((resolve) => {
    entered = resolve;
  });
  const answer = recorder((_url, init) => {
    entered();
    return new Promise((_resolve, reject) => {
      init.signal.addEventListener('abort', () => reject(init.signal.reason));
    });
  });
  const call = upload(clientWith(answer.fetch), uploadRequest(), {
    idempotencyKey: KEY,
    signal: controller.signal,
  });
  const settled = assert.rejects(call, { code: 'network_error', status: 0 });
  await started;
  controller.abort();
  await settled;
  assert.equal(answer.calls.length, 1);
});

test('headers with no body at all hold a clocked read until the stall limit, never a receipt', async () => {
  const clock = new FakeClock();
  const answer = recorder(
    () => new Response(new ReadableStream({ pull: hang }), { status: 201 }),
  );
  const settled = assert.rejects(upload(clientWith(answer.fetch, clock)), {
    code: 'network_error',
  });
  await flush();
  assert.equal(clock.pending, 1);
  await clock.advance(45_000);
  await settled;
  assert.equal(clock.pending, 0);
});

// ---------------------------------------------------------------------------
// Native Fetch: redirects follow neither credentials nor body.

test('native Fetch refuses a redirect of either mutation without contacting its target', async () => {
  const hits = { origin: [], target: 0 };
  const server = createServer((request, response) => {
    if (request.url === '/target') {
      hits.target += 1;
      response.end('{}');
      return;
    }
    hits.origin.push(`${request.method} ${request.headers.authorization}`);
    request.resume();
    request.on('end', () => {
      response.writeHead(307, { location: '/target' });
      response.end();
    });
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  try {
    const client = new ArtifreeClient({
      baseUrl: `http://127.0.0.1:${server.address().port}`,
      accessToken: 'token',
    });
    await assert.rejects(upload(client), { code: 'network_error' });
    await assert.rejects(remove(client), { code: 'network_error' });
    assert.equal(hits.target, 0);
    assert.deepEqual(hits.origin, ['POST Bearer token', 'DELETE Bearer token']);
  } finally {
    server.closeAllConnections();
    await new Promise((resolve) => server.close(resolve));
  }
});

// ---------------------------------------------------------------------------
// The real Rust server.

const root = fileURLToPath(new URL('../../', import.meta.url));
let server;
let scratch;
let api;
let seed = 0;
const fresh = (label) =>
  `${label}-${(seed += 1).toString().padStart(4, '0')}-key`;

before(async () => {
  const binary = buildServer(root);
  scratch = await mkdtemp(join(tmpdir(), 'artifree-asset-mutation-'));
  server = await startServer(binary, scratch);
  api = new ArtifreeClient({
    baseUrl: server.api_url,
    accessToken: server.token,
  });
});

after(async () => {
  await server?.stop();
  if (scratch !== undefined)
    await rm(scratch, { recursive: true, force: true });
});

async function newArtifact(title) {
  const created = await api.createArtifact(
    {
      title,
      files: [
        { path: 'index.html', content: `<h1>${title}</h1>`, encoding: 'utf8' },
      ],
    },
    { idempotencyKey: fresh('artifact') },
  );
  return created.artifact.id;
}

const CSV = Buffer.from('name,qty\r\n"Ünïcode, comma",3\r\nplain,4\r\n');
const text = (bytes, contentType = 'text/plain', base = 0) => ({
  base_asset_version: base,
  content_type: contentType,
  content_base64: Buffer.from(bytes).toString('base64'),
});

test('real server: upload, list, metadata, exact read and delete, with independent versions', async () => {
  const id = await newArtifact('Asset lifecycle');
  const artifactBefore = await api.getArtifact(id);
  const dataBefore = await api.getData(id);
  const first = await uploadAsset(api, id, text(BYTES), {
    idempotencyKey: fresh('up'),
  });
  assert.equal(first.replayed, false);
  assert.equal(first.asset_version, 1);
  assert.equal(first.asset.content_type, 'text/plain');
  assert.equal(first.asset.size_bytes, BYTES.length);
  assert.equal(first.asset.sha256, sha(BYTES));
  const second = await uploadAsset(api, id, text(CSV, 'text/csv', 1), {
    idempotencyKey: fresh('up'),
  });
  assert.equal(second.asset_version, 2);
  assert.notEqual(second.asset.id, first.asset.id);
  const listed = await listAssets(api, id);
  assert.equal(listed.asset_version, 2);
  assert.deepEqual(
    listed.assets.map((asset) => asset.id).sort(),
    [first.asset.id, second.asset.id].sort(),
  );
  assert.deepEqual(
    Buffer.from((await readAsset(api, id, first.asset.id)).bytes),
    BYTES,
  );
  assert.deepEqual(
    Buffer.from((await readAsset(api, id, second.asset.id)).bytes),
    CSV,
  );
  // A source save and a data write neither rewind nor advance the asset space.
  await api.saveRevision(
    id,
    {
      base_revision: artifactBefore.current_revision,
      changes: [
        { op: 'put', path: 'note.txt', content: 'n', encoding: 'utf8' },
      ],
    },
    { idempotencyKey: fresh('save') },
  );
  await api.setData(
    id,
    { path: 'k', base_data_version: dataBefore.data_version, value: 1 },
    { idempotencyKey: fresh('data') },
  );
  assert.equal((await listAssets(api, id)).asset_version, 2);
  const removed = await deleteAsset(
    api,
    id,
    first.asset.id,
    { base_asset_version: 2 },
    {
      idempotencyKey: fresh('del'),
    },
  );
  assert.deepEqual(
    { ...removed },
    {
      asset_id: first.asset.id,
      asset_version: 3,
      deleted: true,
      replayed: false,
    },
  );
  await assert.rejects(readAsset(api, id, first.asset.id), {
    code: 'not_found',
    status: 404,
  });
  assert.deepEqual(
    (await listAssets(api, id)).assets.map((asset) => asset.id),
    [second.asset.id],
  );
  // Deleting what is absent records the key and keeps the version.
  const absent = await deleteAsset(
    api,
    id,
    OTHER_ID,
    { base_asset_version: 3 },
    {
      idempotencyKey: fresh('del'),
    },
  );
  assert.deepEqual(
    { ...absent },
    { asset_id: OTHER_ID, asset_version: 3, deleted: false, replayed: false },
  );
  // Source and data changed only through their own writes.
  const artifactAfter = await api.getArtifact(id);
  assert.equal(
    artifactAfter.current_revision,
    artifactBefore.current_revision + 1,
  );
  assert.equal(
    (await api.getData(id)).data_version,
    dataBefore.data_version + 1,
  );
});

test('real server: a foreign user cannot upload or delete, and sees 404', async () => {
  const id = await newArtifact('Asset scope');
  const owned = await uploadAsset(api, id, text(BYTES), {
    idempotencyKey: fresh('up'),
  });
  const alice = await createUser(api, { label: 'Alice assets' });
  const token = await api.createToken({
    name: 'Alice assets',
    user_id: alice.id,
  });
  const foreign = new ArtifreeClient({
    baseUrl: server.api_url,
    accessToken: token.secret,
  });
  await assert.rejects(
    uploadAsset(foreign, id, text(BYTES), { idempotencyKey: fresh('up') }),
    { code: 'not_found', status: 404 },
  );
  await assert.rejects(
    deleteAsset(
      foreign,
      id,
      owned.asset.id,
      { base_asset_version: 1 },
      {
        idempotencyKey: fresh('del'),
      },
    ),
    { code: 'not_found', status: 404 },
  );
  const listed = await listAssets(api, id);
  assert.equal(listed.asset_version, 1);
  assert.equal(listed.assets.length, 1);
});

/** Fetch that really sends each request, then loses the answer to the first `lost` calls. */
function losingFetch(lost) {
  let remaining = lost;
  return async (url, init) => {
    const response = await fetch(url, init);
    if (remaining > 0) {
      remaining -= 1;
      await response.arrayBuffer();
      throw new TypeError('connection reset before the answer');
    }
    return response;
  };
}

test('real server: a lost committed upload replays exactly with the same envelope and key', async () => {
  const id = await newArtifact('Lost upload');
  const lossy = new ArtifreeClient({
    baseUrl: server.api_url,
    accessToken: server.token,
    fetch: losingFetch(1),
  });
  const request = text(BYTES);
  const options = { idempotencyKey: fresh('lost') };
  await assert.rejects(uploadAsset(lossy, id, request, options), {
    code: 'network_error',
  });
  // The first attempt did commit; the identical retry replays its receipt.
  const replay = await uploadAsset(lossy, id, request, options);
  assert.equal(replay.replayed, true);
  assert.equal(replay.asset_version, 1);
  const listed = await listAssets(api, id);
  assert.equal(listed.asset_version, 1);
  assert.deepEqual(listed.assets, [replay.asset]);
  // Replay after the base moved on is still the original receipt.
  await uploadAsset(api, id, text(CSV, 'text/csv', 1), {
    idempotencyKey: fresh('up'),
  });
  const late = await uploadAsset(api, id, request, options);
  assert.deepEqual(late, replay);
  assert.equal((await listAssets(api, id)).assets.length, 2);
});

test('real server: delete replay keeps its result, and an upload replay after deletion does not resurrect bytes', async () => {
  const id = await newArtifact('Replay after delete');
  const request = text(BYTES);
  const uploadOptions = { idempotencyKey: fresh('up') };
  const uploaded = await uploadAsset(api, id, request, uploadOptions);
  const deleteOptions = { idempotencyKey: fresh('del') };
  const deleteRequest = { base_asset_version: 1 };
  const first = await deleteAsset(
    api,
    id,
    uploaded.asset.id,
    deleteRequest,
    deleteOptions,
  );
  assert.equal(first.deleted, true);
  assert.equal(first.asset_version, 2);
  const again = await deleteAsset(
    api,
    id,
    uploaded.asset.id,
    deleteRequest,
    deleteOptions,
  );
  assert.deepEqual(again, { ...first, replayed: true });
  const lost = new ArtifreeClient({
    baseUrl: server.api_url,
    accessToken: server.token,
    fetch: losingFetch(1),
  });
  // A lost answer to a delete of the now-absent asset replays its own result.
  const absentRequest = { base_asset_version: 2 };
  const absentOptions = { idempotencyKey: fresh('del') };
  await assert.rejects(
    deleteAsset(lost, id, uploaded.asset.id, absentRequest, absentOptions),
    { code: 'network_error' },
  );
  assert.deepEqual(
    await deleteAsset(api, id, uploaded.asset.id, absentRequest, absentOptions),
    {
      asset_id: uploaded.asset.id,
      asset_version: 2,
      deleted: false,
      replayed: true,
    },
  );
  const replay = await uploadAsset(api, id, request, uploadOptions);
  assert.deepEqual(replay, { ...uploaded, replayed: true });
  await assert.rejects(readAsset(api, id, uploaded.asset.id), {
    code: 'not_found',
  });
  const listed = await listAssets(api, id);
  assert.deepEqual(listed.assets, []);
  assert.equal(listed.asset_version, 2);
});

test('real server: key reuse, a stale base, type and quota refusals', async () => {
  const id = await newArtifact('Refusals');
  const key = fresh('first');
  const uploaded = await uploadAsset(api, id, text(BYTES), {
    idempotencyKey: key,
  });
  // The same key with other bytes, and with another operation.
  await assert.rejects(
    uploadAsset(api, id, text(CSV, 'text/csv'), { idempotencyKey: key }),
    {
      code: 'request_key_reused',
      status: 422,
    },
  );
  await assert.rejects(
    deleteAsset(
      api,
      id,
      uploaded.asset.id,
      { base_asset_version: 1 },
      { idempotencyKey: key },
    ),
    { code: 'request_key_reused', status: 422 },
  );
  // A new key against a stale base reports the current version.
  await assert.rejects(
    uploadAsset(api, id, text(CSV, 'text/csv', 0), {
      idempotencyKey: fresh('stale'),
    }),
    (error) => {
      assert.equal(error.code, 'asset_conflict');
      assert.equal(error.status, 409);
      assert.equal(error.body.error.current_asset_version, 1);
      return true;
    },
  );
  await assert.rejects(
    deleteAsset(
      api,
      id,
      uploaded.asset.id,
      { base_asset_version: 0 },
      {
        idempotencyKey: fresh('stale'),
      },
    ),
    { code: 'asset_conflict', status: 409 },
  );
  // Types and content the server does not admit now.
  await assert.rejects(
    uploadAsset(api, id, text('<p>x</p>', 'text/html', 1), {
      idempotencyKey: fresh('type'),
    }),
    { code: 'asset_type_unsupported', status: 422 },
  );
  await assert.rejects(
    uploadAsset(api, id, text('\x89PNG', 'image/png', 1), {
      idempotencyKey: fresh('type'),
    }),
    { code: 'asset_type_unsupported', status: 422 },
  );
  await assert.rejects(
    uploadAsset(api, id, text('', 'text/plain', 1), {
      idempotencyKey: fresh('empty'),
    }),
    { code: 'invalid_asset', status: 422 },
  );
  await assert.rejects(
    uploadAsset(
      api,
      id,
      text(Buffer.from([0xff, 0xfe, 0x00]), 'text/plain', 1),
      {
        idempotencyKey: fresh('utf8'),
      },
    ),
    { code: 'invalid_asset', status: 422 },
  );
  assert.equal((await listAssets(api, id)).asset_version, 1);
});

test('real server: the 25 MiB space quota refuses the asset that would overflow it', async () => {
  const id = await newArtifact('Quota');
  const big = Buffer.alloc(5 * MIB, 97);
  for (let version = 0; version < 5; version += 1) {
    const stored = await uploadAsset(
      api,
      id,
      text(big, 'text/plain', version),
      {
        idempotencyKey: fresh('big'),
      },
    );
    assert.equal(stored.asset_version, version + 1);
    assert.equal(stored.asset.size_bytes, 5 * MIB);
  }
  await assert.rejects(
    uploadAsset(api, id, text('x', 'text/plain', 5), {
      idempotencyKey: fresh('over'),
    }),
    { code: 'asset_limit_exceeded', status: 422 },
  );
  const listed = await listAssets(api, id);
  assert.equal(listed.asset_version, 5);
  assert.equal(listed.bytes, 25 * MIB);
});

test('real server: a rejected receipt followed by the same key changes the server exactly once', async () => {
  const id = await newArtifact('Rejected receipt');
  // The first answer is the real committed receipt, altered in transit.
  const tamper = (change, status) => async (url, init) => {
    const response = await fetch(url, init);
    const body = await response.json();
    return new Response(JSON.stringify(change(body)), {
      status: status ?? response.status,
    });
  };
  const tampered = new ArtifreeClient({
    baseUrl: server.api_url,
    accessToken: server.token,
    fetch: tamper((body) => ({
      ...body,
      asset: { ...body.asset, sha256: sha('x') },
    })),
  });
  const request = text(BYTES);
  const options = { idempotencyKey: fresh('tamper') };
  await assert.rejects(uploadAsset(tampered, id, request, options), invalid);
  const replay = await uploadAsset(api, id, request, options);
  assert.equal(replay.replayed, true);
  assert.equal(replay.asset_version, 1);
  assert.equal((await listAssets(api, id)).asset_version, 1);
  const wrongStatus = new ArtifreeClient({
    baseUrl: server.api_url,
    accessToken: server.token,
    fetch: tamper((body) => body, 202),
  });
  const deletion = { base_asset_version: 1 };
  const deleteOptions = { idempotencyKey: fresh('tamper') };
  await assert.rejects(
    deleteAsset(wrongStatus, id, replay.asset.id, deletion, deleteOptions),
    { ...invalid, status: 202 },
  );
  const deleted = await deleteAsset(
    api,
    id,
    replay.asset.id,
    deletion,
    deleteOptions,
  );
  assert.equal(deleted.replayed, true);
  assert.equal(deleted.deleted, true);
  assert.equal(deleted.asset_version, 2);
  assert.equal((await listAssets(api, id)).asset_version, 2);
});
