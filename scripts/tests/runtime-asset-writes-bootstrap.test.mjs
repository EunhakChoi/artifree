import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { afterEach, mock, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The frame script is bundled as the server embeds it, then run against a
// stand-in window and a real MessageChannel. The host side is the test. This
// file covers the two write calls; the reads are in
// runtime-assets-bootstrap.test.mjs.
const bundle = await build({
  entryPoints: [
    fileURLToPath(
      new URL('../../packages/runtime-sdk/src/bootstrap.ts', import.meta.url),
    ),
  ],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const { start } = await import(
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
);
// The workspace host's parser, bundled as it ships, to check that every write
// the frame sends is one the host accepts.
const hostBundle = await build({
  entryPoints: [
    fileURLToPath(
      new URL('../../apps/web/src/artifacts/frame-rpc.ts', import.meta.url),
    ),
  ],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const { parseCall } = await import(
  `data:text/javascript;base64,${Buffer.from(hostBundle.outputFiles[0].text).toString('base64')}`
);

const CONFIG = {
  view_id: 'd'.repeat(32),
  generation: 1,
  initialization: 'c'.repeat(64),
  lease_ms: 900_000,
};
const ASSET = 'a'.repeat(64);
const OTHER_ASSET = 'b'.repeat(64);
/** The base version every write names; a receipt states it plus one. */
const BASE = 3;
const UPLOAD_RETRY = 'upload-0001';
const DELETE_RETRY = 'delete-0001';
/** The closed wording of a conflict, as the frame states it. */
const CONFLICT_MESSAGE =
  'The uploaded assets are not at the version the call named.';

async function settle() {
  for (let round = 0; round < 20; round += 1) await Promise.resolve();
  // A message crosses the port on the event loop, not in a microtask.
  for (let round = 0; round < 5; round += 1)
    await new Promise((resolve) => setImmediate(resolve));
}

/** The listeners of every frame a test opened; closed after each test. */
const opened = [];

/**
 * Starts the script on a stand-in window; the test plays the workspace. Each
 * message the frame sends is recorded, then `respond` may answer it.
 */
function frame(config = CONFIG, respond = () => undefined) {
  const listeners = {};
  const received = [];
  /** The delay of each timer the frame starts, in the order it starts them. */
  const timers = [];
  let port;
  const window = {
    artifree: undefined,
    addEventListener: (type, listener) => {
      listeners[type] = listener;
    },
    setTimeout: (run, ms) => {
      timers.push(ms);
      return setTimeout(run, ms);
    },
    parent: {
      postMessage: (_message, _origin, ports) => {
        port = ports[0];
        port.onmessage = (event) => {
          received.push(event.data);
          const reply = respond(event.data);
          if (reply !== undefined) port.postMessage(reply);
        };
      },
    },
  };
  globalThis.window = window;
  start(config);
  opened.push(listeners);
  return {
    api: window.artifree,
    received,
    timers,
    /** Answers call `id` as the workspace. */
    reply: (message) => port.postMessage(message),
    hangUp: () => listeners.pagehide(),
  };
}

afterEach(() => {
  // Every frame is closed, even one whose test failed before it hung up.
  for (const listeners of opened.splice(0)) listeners.pagehide?.();
  delete globalThis.window;
  mock.timers.reset();
});

/** Moves the fake clock on by `ms`, then lets the port deliver. */
async function passTime(ms) {
  mock.timers.tick(ms);
  await settle();
}

/**
 * Lets the port deliver until `done` holds. The bound makes a frame that never
 * gets there fail with `label` rather than wait without end.
 */
async function settleUntil(done, label) {
  for (let round = 0; round < 50; round += 1) {
    if (done()) return;
    await settle();
  }
  assert.fail(label);
}

async function code(promise) {
  const error = await promise.then(
    () => assert.fail('The call must be refused.'),
    (rejection) => rejection,
  );
  assert.equal(error.name, 'RuntimeRequestFailure');
  return error;
}

/**
 * Makes one call, answers the message it sends with `reply`, and says how the
 * call settled: with its value or its error, and with the message the frame
 * sent.
 */
async function answerOnce(call, reply) {
  const f = frame();
  const settled = call(f.api).then(
    (value) => ({ value }),
    (error) => ({ error }),
  );
  await settle();
  assert.equal(f.received.length, 1, 'one call was sent');
  const [sent] = f.received;
  f.reply({ id: sent.id, ...reply });
  const outcome = await settled;
  f.hangUp();
  return { ...outcome, sent };
}

/** The call was refused as unavailable, with the closed wording. */
function assertUnavailable(outcome, label) {
  // A boolean, not the value itself: a value that should have been refused
  // would otherwise be diffed in full by the failure report.
  assert.equal(
    outcome.value === undefined,
    true,
    `${label}: the call must be refused`,
  );
  assert.equal(outcome.error?.name, 'RuntimeRequestFailure', label);
  assert.equal(outcome.error?.code, 'unavailable', label);
  assert.equal(
    outcome.error?.message,
    'The runtime request is unavailable.',
    label,
  );
}

/** The SHA-256 of some bytes, from node:crypto rather than the SDK's own. */
function digestOf(bytes) {
  return createHash('sha256').update(bytes).digest('hex');
}

/** An upload of "hello" as text/plain at BASE, with some fields changed. */
function uploadRequest(changes = {}) {
  return {
    contentType: 'text/plain',
    content: 'hello',
    baseAssetVersion: BASE,
    retryId: UPLOAD_RETRY,
    ...changes,
  };
}

/** A deletion of ASSET at BASE, with some fields changed. */
function deleteRequest(changes = {}) {
  return {
    assetId: ASSET,
    baseAssetVersion: BASE,
    retryId: DELETE_RETRY,
    ...changes,
  };
}

/** The asset the workspace stores for "hello", with some fields changed. */
function helloAsset(changes = {}) {
  return {
    id: ASSET,
    contentType: 'text/plain',
    size: 5,
    sha256: digestOf('hello'),
    createdAtMs: 1_700_000_000_000,
    ...changes,
  };
}

/** The receipt for uploading "hello", naming `assetVersion` as its version. */
function helloReceipt(changes = {}, assetVersion = BASE + 1) {
  return { asset: helloAsset(changes), assetVersion };
}

test('An upload sends exactly its fields once, and the host reads it as a call', async () => {
  const f = frame();
  const settled = f.api.assets.upload(uploadRequest()).then(
    () => 'resolved',
    (error) => error.code,
  );
  await settle();
  assert.equal(f.received.length, 1, 'one message was sent');
  const [{ bytes, ...sent }] = f.received;
  assert.deepEqual(sent, {
    type: 'assets.upload',
    id: 1,
    retryId: UPLOAD_RETRY,
    baseAssetVersion: BASE,
    contentType: 'text/plain',
  });
  assert.equal(
    Buffer.from(bytes).equals(Buffer.from('hello')),
    true,
    'the bytes are the text, as UTF-8',
  );
  assert.equal(
    parseCall(f.received[0]) !== undefined,
    true,
    'the host reads the upload as a call',
  );
  f.hangUp();
  await settled;
});

test('An upload transfers a private copy: the caller keeps its buffer, and a later change does not reach the workspace', async () => {
  const f = frame();
  const mine = Uint8Array.from([1, 2, 3]);
  const settled = f.api.assets.upload(uploadRequest({ content: mine })).then(
    () => 'resolved',
    (error) => error.code,
  );
  // The caller changes its array straight after the call.
  mine[0] = 9;
  await settle();
  assert.equal(f.received.length, 1, 'one message was sent');
  assert.equal(
    Buffer.from(f.received[0].bytes).equals(Buffer.from([1, 2, 3])),
    true,
    'the workspace got the bytes as they were at the call',
  );
  assert.equal(
    mine.buffer.byteLength,
    3,
    'the caller keeps its own buffer, which was not transferred',
  );
  f.hangUp();
  await settled;
});

test('An upload sends only the bytes its view covers, not the buffer behind the view', async () => {
  const f = frame();
  const backing = Uint8Array.of(9, 9, 1, 2, 3, 9, 9, 9);
  const settled = f.api.assets
    .upload(uploadRequest({ content: new Uint8Array(backing.buffer, 2, 3) }))
    .then(
      () => 'resolved',
      (error) => error.code,
    );
  await settle();
  assert.equal(f.received.length, 1, 'one message was sent');
  const [{ bytes }] = f.received;
  assert.equal(bytes.byteLength, 3, 'three bytes were sent, not eight');
  assert.equal(
    Buffer.from(bytes).equals(Buffer.from([1, 2, 3])),
    true,
    'the three bytes are those of the view',
  );
  f.hangUp();
  await settled;
});

test('An upload receipt that names the bytes sent is accepted as the asset it names', async () => {
  const outcome = await answerOnce(
    (api) => api.assets.upload(uploadRequest()),
    { ok: true, uploaded: helloReceipt() },
  );
  assert.equal(outcome.error, undefined);
  assert.deepEqual(outcome.value, {
    asset: {
      id: ASSET,
      contentType: 'text/plain',
      size: 5,
      sha256: digestOf('hello'),
      createdAtMs: 1_700_000_000_000,
    },
    assetVersion: 4,
  });
});

test('An upload receipt that names other bytes, another type or another version is refused as unavailable', async () => {
  const refused = {
    'another digest of the same size': helloReceipt({
      sha256: digestOf('jello'),
    }),
    'the digest in capitals': helloReceipt({
      sha256: digestOf('hello').toUpperCase(),
    }),
    'a size one more than the bytes sent': helloReceipt({ size: 6 }),
    'a size one less than the bytes sent': helloReceipt({ size: 4 }),
    'another media type': helloReceipt({ contentType: 'text/csv' }),
    'an image media type': helloReceipt({ contentType: 'image/png' }),
    'the base version itself': helloReceipt({}, BASE),
    'two versions past the base': helloReceipt({}, BASE + 2),
    'a version as text': { asset: helloAsset(), assetVersion: '4' },
    'no asset': { assetVersion: BASE + 1 },
    'an asset ID in capitals': helloReceipt({ id: 'A'.repeat(64) }),
    'a negative creation time': helloReceipt({ createdAtMs: -1 }),
  };
  for (const [label, uploaded] of Object.entries(refused)) {
    const outcome = await answerOnce(
      (api) => api.assets.upload(uploadRequest()),
      { ok: true, uploaded },
    );
    assertUnavailable(outcome, label);
  }
});

test('An upload receipt states the base version the call sent, plus one', async () => {
  for (const base of [0, 10, 2 ** 53 - 2]) {
    const invoke = (api) =>
      api.assets.upload(uploadRequest({ baseAssetVersion: base }));
    const next = await answerOnce(invoke, {
      ok: true,
      uploaded: helloReceipt({}, base + 1),
    });
    assert.deepEqual(
      next.value,
      { asset: helloAsset(), assetVersion: base + 1 },
      `base ${base}`,
    );
    const unchanged = await answerOnce(invoke, {
      ok: true,
      uploaded: helloReceipt({}, base),
    });
    assertUnavailable(unchanged, `base ${base}, version unchanged`);
  }
});

test('A rate-limited upload is posted once, and no retry follows however long the workspace waits', async () => {
  mock.timers.enable({ apis: ['setTimeout'] });
  const f = frame(CONFIG, (message) => ({
    id: message.id,
    ok: false,
    code: 'rate_limited',
  }));
  let outcome;
  f.api.assets.upload(uploadRequest()).then(
    () => {
      outcome = 'resolved';
    },
    (error) => {
      outcome = error.code;
    },
  );
  // The refusal has been processed once the call is settled, or once the frame
  // has started the wait before a retry. Only then does the clock move.
  await settleUntil(
    () => outcome !== undefined || f.timers.length > 1,
    'the frame never processed the refusal',
  );
  assert.equal(f.received.length, 1, 'the upload is posted once');
  // Far past every wait in the backoff table (3.75 s in all) and the write timeout.
  await passTime(60_000);
  assert.equal(f.received.length, 1, 'no retry is posted');
  assert.equal(outcome, 'rate_limited', 'the refusal reaches the page');
  f.hangUp();
});

test('A rate-limited deletion is posted once, and no retry follows however long the workspace waits', async () => {
  mock.timers.enable({ apis: ['setTimeout'] });
  const f = frame(CONFIG, (message) => ({
    id: message.id,
    ok: false,
    code: 'rate_limited',
  }));
  let outcome;
  f.api.assets.delete(deleteRequest()).then(
    () => {
      outcome = 'resolved';
    },
    (error) => {
      outcome = error.code;
    },
  );
  // As for uploads: the refusal is processed before the clock moves.
  await settleUntil(
    () => outcome !== undefined || f.timers.length > 1,
    'the frame never processed the refusal',
  );
  assert.equal(f.received.length, 1, 'the deletion is posted once');
  await passTime(60_000);
  assert.equal(f.received.length, 1, 'no retry is posted');
  assert.equal(outcome, 'rate_limited', 'the refusal reaches the page');
  f.hangUp();
});

test('A conflict names the version the workspace has now, when it names a count', async () => {
  const operations = {
    upload: (api) => api.assets.upload(uploadRequest()),
    delete: (api) => api.assets.delete(deleteRequest()),
  };
  for (const [operation, invoke] of Object.entries(operations)) {
    const outcome = await answerOnce(invoke, {
      ok: false,
      code: 'conflict',
      currentAssetVersion: 7,
    });
    assert.equal(outcome.error?.code, 'conflict', operation);
    assert.equal(outcome.error?.currentAssetVersion, 7, operation);
    assert.equal(outcome.error?.message, CONFLICT_MESSAGE, operation);
  }
});

test('A conflict that names no usable version carries none, and no other refusal carries one', async () => {
  const replies = [
    ['a conflict with no version', { code: 'conflict' }],
    [
      'a conflict with a negative version',
      { code: 'conflict', currentAssetVersion: -1 },
    ],
    [
      'a conflict with a fractional version',
      { code: 'conflict', currentAssetVersion: 1.5 },
    ],
    [
      'a conflict with a version as text',
      { code: 'conflict', currentAssetVersion: '7' },
    ],
    [
      'a conflict with a version past the safe integers',
      { code: 'conflict', currentAssetVersion: 2 ** 53 },
    ],
    [
      'an unavailable call with a version',
      { code: 'unavailable', currentAssetVersion: 7 },
    ],
    [
      'a stale view with a version',
      { code: 'stale_view', currentAssetVersion: 7 },
    ],
  ];
  const operations = {
    upload: (api) => api.assets.upload(uploadRequest()),
    delete: (api) => api.assets.delete(deleteRequest()),
  };
  for (const [operation, invoke] of Object.entries(operations)) {
    for (const [label, reply] of replies) {
      const outcome = await answerOnce(invoke, { ok: false, ...reply });
      assert.equal(outcome.error?.code, reply.code, `${operation}: ${label}`);
      assert.equal(
        outcome.error?.currentAssetVersion,
        undefined,
        `${operation}: ${label}`,
      );
    }
  }
});

test('A deletion sends exactly its fields once, and the host reads it as a call', async () => {
  const f = frame();
  const settled = f.api.assets.delete(deleteRequest()).then(
    () => 'resolved',
    (error) => error.code,
  );
  await settle();
  assert.equal(f.received.length, 1, 'one message was sent');
  assert.deepEqual(f.received[0], {
    type: 'assets.delete',
    id: 1,
    retryId: DELETE_RETRY,
    baseAssetVersion: BASE,
    assetId: ASSET,
  });
  assert.equal(
    parseCall(f.received[0]) !== undefined,
    true,
    'the host reads the deletion as a call',
  );
  f.hangUp();
  await settled;
});

test('A deletion that removes the asset says so, and its version is the base plus one', async () => {
  const outcome = await answerOnce(
    (api) => api.assets.delete(deleteRequest()),
    {
      ok: true,
      removed: { assetId: ASSET, assetVersion: 4, deleted: true },
    },
  );
  assert.deepEqual(outcome.value, {
    assetId: ASSET,
    assetVersion: 4,
    deleted: true,
  });
});

test('A deletion of an absent asset says so, and its version stays at the base', async () => {
  const outcome = await answerOnce(
    (api) => api.assets.delete(deleteRequest()),
    {
      ok: true,
      removed: { assetId: ASSET, assetVersion: 3, deleted: false },
    },
  );
  assert.deepEqual(outcome.value, {
    assetId: ASSET,
    assetVersion: 3,
    deleted: false,
  });
});

test('A deletion receipt whose version does not follow from deleted is refused as unavailable', async () => {
  const refused = {
    'deleted, but the version did not move': {
      assetId: ASSET,
      assetVersion: 3,
      deleted: true,
    },
    'nothing deleted, but the version moved': {
      assetId: ASSET,
      assetVersion: 4,
      deleted: false,
    },
    'deleted, two versions on': {
      assetId: ASSET,
      assetVersion: 5,
      deleted: true,
    },
    'nothing deleted, a version back': {
      assetId: ASSET,
      assetVersion: 2,
      deleted: false,
    },
    'another asset named': {
      assetId: OTHER_ASSET,
      assetVersion: 4,
      deleted: true,
    },
    'deleted as text': { assetId: ASSET, assetVersion: 4, deleted: 'true' },
    'deleted as a number': { assetId: ASSET, assetVersion: 4, deleted: 1 },
    'no deleted field': { assetId: ASSET, assetVersion: 4 },
    'no version': { assetId: ASSET, deleted: true },
    'no receipt': undefined,
  };
  for (const [label, removed] of Object.entries(refused)) {
    const outcome = await answerOnce(
      (api) => api.assets.delete(deleteRequest()),
      { ok: true, removed },
    );
    assertUnavailable(outcome, label);
  }
});

test('A hostile upload argument is refused as invalid, and nothing is sent', async () => {
  let getterRuns = 0;
  const fields = uploadRequest();
  const hidden = Object.defineProperty(uploadRequest(), 'content', {
    enumerable: false,
  });
  class Shaped {
    constructor() {
      Object.assign(this, uploadRequest());
    }
  }
  const revoked = Proxy.revocable({}, {});
  revoked.revoke();
  const bad = [
    undefined,
    null,
    5,
    'hello',
    ['text/plain', 'hello', BASE, UPLOAD_RETRY],
    {},
    { ...fields, extra: 1 },
    { ...fields, [Symbol('marked')]: 1 },
    hidden,
    new Shaped(),
    {
      get contentType() {
        getterRuns += 1;
        return 'text/plain';
      },
      content: 'hello',
      baseAssetVersion: BASE,
      retryId: UPLOAD_RETRY,
    },
    revoked.proxy,
    new Proxy(uploadRequest(), {
      ownKeys() {
        throw new Error('trap ran');
      },
    }),
    { ...fields, contentType: 'application/pdf' },
    { ...fields, contentType: 'text/plain; charset=utf-8' },
    { ...fields, contentType: 'TEXT/PLAIN' },
    { ...fields, contentType: 'image/gif' },
    { ...fields, contentType: undefined },
    { ...fields, baseAssetVersion: -1 },
    { ...fields, baseAssetVersion: 1.5 },
    { ...fields, baseAssetVersion: '3' },
    { ...fields, baseAssetVersion: 2 ** 53 - 1 },
    { ...fields, baseAssetVersion: 2 ** 53 },
    { ...fields, baseAssetVersion: Number.NaN },
    { ...fields, baseAssetVersion: undefined },
    { ...fields, retryId: '1234567' },
    { ...fields, retryId: 'r'.repeat(37) },
    { ...fields, retryId: 'retry:001' },
    { ...fields, retryId: 'retry 001' },
    { ...fields, retryId: 'retry-001-ü' },
    { ...fields, retryId: '' },
    { ...fields, retryId: 7 },
    { ...fields, retryId: undefined },
    { ...fields, content: 5 },
    { ...fields, content: null },
    { ...fields, content: ['h', 'e', 'l', 'l', 'o'] },
    { ...fields, content: new Uint16Array(2) },
    { ...fields, content: new ArrayBuffer(5) },
    { ...fields, content: 'a'.repeat(1_048_577) },
    // Under 1 MiB in UTF-16 units, but 1.2 MB once encoded as UTF-8.
    { ...fields, content: 'é'.repeat(600_000) },
    { ...fields, content: new Uint8Array(1_048_577) },
    { content: 'hello', baseAssetVersion: BASE, retryId: UPLOAD_RETRY },
    {
      contentType: 'text/plain',
      baseAssetVersion: BASE,
      retryId: UPLOAD_RETRY,
    },
    { contentType: 'text/plain', content: 'hello', retryId: UPLOAD_RETRY },
    { contentType: 'text/plain', content: 'hello', baseAssetVersion: BASE },
  ];
  // A call that is sent is answered at once, so a broken guard fails fast
  // rather than waiting out the write timeout.
  const f = frame(CONFIG, (message) => ({
    id: message.id,
    ok: false,
    code: 'unavailable',
  }));
  for (const [index, argument] of bad.entries()) {
    let promise;
    assert.doesNotThrow(() => {
      promise = f.api.assets.upload(argument);
    }, `argument ${index}`);
    assert.equal(
      (await code(promise)).code,
      'invalid_request',
      `argument ${index}`,
    );
  }
  await settle();
  assert.equal(f.received.length, 0, 'nothing was sent for a refused upload');
  assert.equal(getterRuns, 0, 'no getter of the argument ran');
  f.hangUp();
});

test('A hostile deletion argument is refused as invalid, and nothing is sent', async () => {
  const fields = deleteRequest();
  const revoked = Proxy.revocable({}, {});
  revoked.revoke();
  const bad = [
    undefined,
    null,
    5,
    ASSET,
    [ASSET, BASE, DELETE_RETRY],
    {},
    { ...fields, extra: 1 },
    { ...fields, [Symbol('marked')]: 1 },
    revoked.proxy,
    new Proxy(deleteRequest(), {
      ownKeys() {
        throw new Error('trap ran');
      },
    }),
    { ...fields, assetId: 'A'.repeat(64) },
    { ...fields, assetId: 'a'.repeat(63) },
    { ...fields, assetId: 'a'.repeat(65) },
    { ...fields, assetId: 'g'.repeat(64) },
    { ...fields, assetId: ` ${ASSET}` },
    { ...fields, assetId: undefined },
    { ...fields, assetId: 5 },
    { ...fields, baseAssetVersion: -1 },
    { ...fields, baseAssetVersion: 1.5 },
    { ...fields, baseAssetVersion: '3' },
    { ...fields, baseAssetVersion: 2 ** 53 - 1 },
    { ...fields, retryId: '1234567' },
    { ...fields, retryId: 'delete:001' },
    { ...fields, retryId: 'r'.repeat(37) },
    { ...fields, retryId: undefined },
    { assetId: ASSET, baseAssetVersion: BASE },
    { assetId: ASSET, retryId: DELETE_RETRY },
    { baseAssetVersion: BASE, retryId: DELETE_RETRY },
  ];
  // As for uploads: a sent call is answered at once.
  const f = frame(CONFIG, (message) => ({
    id: message.id,
    ok: false,
    code: 'unavailable',
  }));
  for (const [index, argument] of bad.entries()) {
    let promise;
    assert.doesNotThrow(() => {
      promise = f.api.assets.delete(argument);
    }, `argument ${index}`);
    assert.equal(
      (await code(promise)).code,
      'invalid_request',
      `argument ${index}`,
    );
  }
  await settle();
  assert.equal(f.received.length, 0, 'nothing was sent for a refused deletion');
  f.hangUp();
});
