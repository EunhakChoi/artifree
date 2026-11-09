import assert from 'node:assert/strict';
import { afterEach, mock, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The frame script is bundled as the server embeds it, then run against a
// stand-in window and a real MessageChannel. The host side is the test. This
// file covers the download request; the reads are in
// runtime-assets-bootstrap.test.mjs and the saves in
// runtime-asset-writes-bootstrap.test.mjs.
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
// The workspace host's parser, bundled as it ships, to check that the download
// the frame sends is a call the host accepts.
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
/** The largest asset, 5 MiB, in bytes (decision 0045, rule 4). */
const MAX_ASSET_BYTES = 5_242_880;
/** The closed wording of each failure, as the frame states it. */
const WORDING = {
  invalid_request: 'The runtime request was not valid.',
  denied: 'The runtime request was not permitted.',
  stale_view: 'The runtime view is out of date.',
  conflict: 'The uploaded assets are not at the version the call named.',
  not_found: 'The requested source was not found.',
  rate_limited: 'Too many runtime requests.',
  declined: 'The user declined the request.',
  unavailable: 'The runtime request is unavailable.',
  expired: 'The runtime view has expired.',
};

async function settle() {
  for (let round = 0; round < 20; round += 1) await Promise.resolve();
  // A message crosses the port on the event loop, not in a microtask.
  for (let round = 0; round < 5; round += 1)
    await new Promise((resolve) => setImmediate(resolve));
}

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

/** The listeners of every frame a test opened; closed after each test. */
const opened = [];

/**
 * Starts the script on a stand-in window; the test plays the workspace. Each
 * message the frame sends is recorded, then `respond` may answer it.
 */
function frame(config = CONFIG, respond = () => undefined) {
  const listeners = {};
  const received = [];
  let port;
  const window = {
    artifree: undefined,
    addEventListener: (type, listener) => {
      listeners[type] = listener;
    },
    setTimeout: (run, ms) => setTimeout(run, ms),
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
  assert.equal(outcome.error?.message, WORDING.unavailable, label);
}

test('A download request sends exactly its asset ID once, and the host reads it as the same call', async () => {
  const f = frame();
  const settled = f.api.assets.requestDownload({ assetId: ASSET }).then(
    () => 'resolved',
    (error) => error.code,
  );
  await settle();
  assert.deepEqual(f.received, [
    { type: 'assets.download', id: 1, assetId: ASSET },
  ]);
  assert.deepEqual(parseCall(f.received[0]), f.received[0]);
  // The view ends while the call waits, so the call ends with it.
  f.hangUp();
  assert.equal(await settled, 'expired');
});

test('A download sends the asset ID the caller passed at the call, not one it changes afterwards', async () => {
  const f = frame();
  const request = { assetId: ASSET };
  const settled = f.api.assets.requestDownload(request).then(
    () => 'resolved',
    (error) => error.code,
  );
  request.assetId = OTHER_ASSET;
  await settle();
  assert.deepEqual(f.received, [
    { type: 'assets.download', id: 1, assetId: ASSET },
  ]);
  f.hangUp();
  assert.equal(await settled, 'expired');
});

test('A hostile download argument is refused as invalid, and nothing is sent', async () => {
  let getterRuns = 0;
  const revoked = Proxy.revocable({}, {});
  revoked.revoke();
  const hidden = Object.defineProperty({}, 'assetId', {
    value: ASSET,
    enumerable: false,
  });
  class Shaped {
    constructor() {
      this.assetId = ASSET;
    }
  }
  const bad = [
    undefined,
    null,
    5,
    ASSET,
    [ASSET],
    {},
    { assetId: ASSET, extra: 1 },
    { assetId: ASSET, [Symbol('marked')]: 1 },
    hidden,
    new Shaped(),
    {
      get assetId() {
        getterRuns += 1;
        return ASSET;
      },
    },
    revoked.proxy,
    new Proxy(
      { assetId: ASSET },
      {
        ownKeys() {
          throw new Error('trap ran');
        },
      },
    ),
    { assetId: 'A'.repeat(64) },
    { assetId: 'a'.repeat(63) },
    { assetId: 'a'.repeat(65) },
    { assetId: 'g'.repeat(64) },
    { assetId: ` ${ASSET}` },
    { assetId: `${ASSET}\n` },
    // A String object, not a string.
    { assetId: Object(ASSET) },
    { assetId: undefined },
    { assetId: null },
    { assetId: 5 },
    { assetId: [ASSET] },
    { path: 'a.txt' },
    { id: ASSET },
  ];
  // A call that is sent is answered at once, so a broken guard fails fast
  // rather than waiting out the two-minute wait.
  const f = frame(CONFIG, (message) => ({
    id: message.id,
    ok: false,
    code: 'unavailable',
  }));
  for (const [index, argument] of bad.entries()) {
    let promise;
    assert.doesNotThrow(() => {
      promise = f.api.assets.requestDownload(argument);
    }, `argument ${index}`);
    assert.equal(
      (await code(promise)).code,
      'invalid_request',
      `argument ${index}`,
    );
  }
  await settle();
  assert.deepEqual(f.received, [], 'nothing was sent for a refused download');
  assert.equal(getterRuns, 0, 'no getter of the argument ran');
  f.hangUp();
});

test('A download receipt is accepted when it names the asset asked for, with from 0 to 5 MiB of bytes', async () => {
  for (const bytes of [0, 5, MAX_ASSET_BYTES]) {
    const receipt = { started: true, assetId: ASSET, bytes };
    const outcome = await answerOnce(
      (api) => api.assets.requestDownload({ assetId: ASSET }),
      { ok: true, receipt },
    );
    assert.deepEqual(outcome.value, receipt, `${bytes} bytes`);
  }
});

test('A download receipt is refused as unavailable when it names another asset, is not started, or has bytes outside 0 to 5 MiB', async () => {
  const receipt = { started: true, assetId: ASSET, bytes: 5 };
  const refused = {
    'started as false': { ...receipt, started: false },
    'started as the text "true"': { ...receipt, started: 'true' },
    'started as 1': { ...receipt, started: 1 },
    'no started field': { assetId: ASSET, bytes: 5 },
    'another asset named': { ...receipt, assetId: OTHER_ASSET },
    'the asset ID in capitals': { ...receipt, assetId: 'A'.repeat(64) },
    'a 63-digit asset ID': { ...receipt, assetId: 'a'.repeat(63) },
    'no asset ID': { started: true, bytes: 5 },
    'a negative byte count': { ...receipt, bytes: -1 },
    'a fractional byte count': { ...receipt, bytes: 1.5 },
    'a byte count as text': { ...receipt, bytes: '5' },
    'a byte count past the safe integers': { ...receipt, bytes: 2 ** 53 },
    'a byte count of NaN': { ...receipt, bytes: Number.NaN },
    'a byte count of Infinity': { ...receipt, bytes: Infinity },
    'one byte over 5 MiB': { ...receipt, bytes: MAX_ASSET_BYTES + 1 },
    'no byte count': { started: true, assetId: ASSET },
    'an array holding the receipt': [receipt],
    'a null receipt': null,
    'no receipt': undefined,
  };
  for (const [label, value] of Object.entries(refused)) {
    const outcome = await answerOnce(
      (api) => api.assets.requestDownload({ assetId: ASSET }),
      { ok: true, receipt: value },
    );
    assertUnavailable(outcome, label);
  }
});

test('A download receipt passes on only the three fields the frame checked', async () => {
  const outcome = await answerOnce(
    (api) => api.assets.requestDownload({ assetId: ASSET }),
    {
      ok: true,
      receipt: {
        started: true,
        assetId: ASSET,
        bytes: 5,
        path: '/private/notes.txt',
        kind: 'file',
      },
    },
  );
  assert.deepEqual(outcome.value, { started: true, assetId: ASSET, bytes: 5 });
});

test('A download is not cut short at the ten-second read timeout, and fails as unavailable at two minutes after the frame cancels it', async () => {
  mock.timers.enable({ apis: ['setTimeout'] });
  const f = frame();
  let outcome;
  f.api.assets.requestDownload({ assetId: ASSET }).then(
    (value) => {
      outcome = { value };
    },
    (error) => {
      outcome = { error };
    },
  );
  await settle();
  assert.equal(f.received.length, 1, 'the download is posted');
  await passTime(10_000);
  assert.equal(outcome, undefined, 'still waiting at ten seconds');
  await passTime(109_999);
  assert.equal(outcome, undefined, 'still waiting at 119,999 ms');
  await passTime(1);
  assert.notEqual(outcome, undefined, 'refused at two minutes');
  assertUnavailable(outcome, 'at two minutes');
  assert.deepEqual(f.received.at(-1), { type: 'cancel', id: 1 });
  // Nothing is posted again, however long the workspace is left to answer.
  await passTime(200_000);
  assert.equal(f.received.length, 2, 'no retry after the timeout');
  f.hangUp();
});

test('A refused download is posted once, and no retry follows however long the workspace waits', async () => {
  mock.timers.enable({ apis: ['setTimeout'] });
  for (const refusal of Object.keys(WORDING)) {
    const f = frame(CONFIG, (message) => ({
      id: message.id,
      ok: false,
      code: refusal,
    }));
    let outcome;
    f.api.assets.requestDownload({ assetId: ASSET }).then(
      (value) => {
        outcome = { value };
      },
      (error) => {
        outcome = { error };
      },
    );
    await settleUntil(
      () => outcome !== undefined,
      `${refusal}: the refusal was not processed`,
    );
    assert.equal(outcome.error?.code, refusal, refusal);
    // Far past the two-minute wait.
    await passTime(200_000);
    assert.equal(f.received.length, 1, `${refusal}: posted once`);
    f.hangUp();
  }
});

test('A refusal reaches the page as its own code from the closed set, with that code wording and no text from the workspace', async () => {
  for (const [refusal, wording] of Object.entries(WORDING)) {
    const outcome = await answerOnce(
      (api) => api.assets.requestDownload({ assetId: ASSET }),
      {
        ok: false,
        code: refusal,
        message: 'SECRET /private/notes.txt was refused',
      },
    );
    assert.equal(outcome.error?.name, 'RuntimeRequestFailure', refusal);
    assert.equal(outcome.error?.code, refusal, refusal);
    assert.equal(outcome.error?.message, wording, refusal);
  }
});

test('A refusal with a code outside the closed set reaches the page as denied', async () => {
  const replies = [
    { ok: false },
    { ok: false, code: undefined },
    { ok: false, code: 'NOT_FOUND' },
    { ok: false, code: '<script>alert(1)</script>' },
    { ok: false, code: 'toString' },
    { ok: false, code: '__proto__' },
    { ok: false, code: { code: 'denied' } },
    { ok: false, code: 7 },
  ];
  for (const reply of replies) {
    const outcome = await answerOnce(
      (api) => api.assets.requestDownload({ assetId: ASSET }),
      reply,
    );
    assert.equal(outcome.error?.code, 'denied', JSON.stringify(reply));
    assert.equal(outcome.error?.message, WORDING.denied, JSON.stringify(reply));
  }
});

test('A download called after its view has ended is refused as expired, and nothing is sent', async () => {
  // With no timers mocked, a call that wrongly waited would hold the process
  // open for two minutes; the fake clock keeps the failure fast.
  mock.timers.enable({ apis: ['setTimeout'] });
  const f = frame();
  f.hangUp();
  let outcome;
  f.api.assets.requestDownload({ assetId: ASSET }).then(
    (value) => {
      outcome = { value };
    },
    (error) => {
      outcome = { error };
    },
  );
  await settle();
  assert.equal(outcome?.error?.code, 'expired');
  assert.deepEqual(f.received, [], 'nothing was sent after the view ended');
});
