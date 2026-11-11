import assert from 'node:assert/strict';
import { afterEach, mock, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The frame script is bundled as the server embeds it, then run against a
// stand-in window and a real MessageChannel, with a fake clock. The test plays
// the workspace: it sends view.adopted events, and checks how the SDK moves its
// deadline and how it calls the view adoption listeners (decision 0054,
// rule 10).
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

const CONFIG = {
  view_id: 'd'.repeat(32),
  generation: 1,
  initialization: 'c'.repeat(64),
  lease_ms: 1000,
};

/** The timeout the SDK gives a read, in milliseconds. */
const READ_TIMEOUT_MS = 10_000;

const METADATA = {
  artifactId: 'a'.repeat(32),
  title: 'Notes',
  currentRevision: 3,
  metadataRevision: 2,
};

/** The reasons an adoption can give (decision 0054, rule 10). */
const ADOPTION_REASONS = ['permissions', 'saved', 'head', 'lease'];

let current;

afterEach(() => {
  current?.listeners.pagehide?.();
  current = undefined;
  mock.timers.reset();
  mock.restoreAll();
  delete globalThis.window;
});

/**
 * Starts the fake clock at zero. The timers the SDK sets fire when advance()
 * reaches them, and performance.now() reads the same moment. A timer reads the
 * clock at the end of the step that reaches it, so a case steps to the exact
 * moment it means.
 */
function startClock() {
  let now = 0;
  mock.timers.enable({ apis: ['setTimeout'] });
  mock.method(performance, 'now', () => now);
  return {
    advance(ms) {
      now += ms;
      mock.timers.tick(ms);
    },
  };
}

/**
 * Starts the script on a stand-in window; the test plays the workspace. Each
 * message the frame sends over the port is recorded in `received`, and the
 * test sends one with `reply`. `toParent` records each message posted to the
 * parent window, starting with the connect message. `advance` moves the clock.
 */
function frame() {
  const clock = startClock();
  const listeners = {};
  const toParent = [];
  const received = [];
  let port;
  const window = {
    artifree: undefined,
    addEventListener: (type, listener) => {
      listeners[type] = listener;
    },
    setTimeout: (run, ms) => setTimeout(run, ms),
    parent: {
      postMessage: (message, _origin, ports) => {
        toParent.push(message);
        port = ports[0];
        port.onmessage = (event) => {
          received.push(event.data);
        };
      },
    },
  };
  globalThis.window = window;
  start(CONFIG);
  current = { listeners };
  return {
    advance: clock.advance,
    api: window.artifree,
    received,
    reply: (message) => port.postMessage(message),
    toParent,
  };
}

/** A message crosses the port on the event loop, not in a microtask. */
async function settle() {
  for (let round = 0; round < 20; round += 1) await Promise.resolve();
  for (let round = 0; round < 5; round += 1)
    await new Promise((resolve) => setImmediate(resolve));
}

/** The message the workspace sends when the artifact's data changes. */
function hint(dataVersion) {
  return { type: 'event', event: 'data.changed', dataVersion };
}

/** The message the workspace sends when it adopts the view (decision 0054, rule 10). */
function adoption(reason, leaseMs) {
  return { type: 'event', event: 'view.adopted', reason, leaseMs };
}

/**
 * A listener that records each call as the array of its arguments. The rest
 * parameter keeps every argument, so a call with an extra argument shows.
 */
function recorder() {
  const calls = [];
  const listener = (...args) => {
    calls.push(args);
  };
  return { calls, listener };
}

/**
 * Records how a call ends: `{ state: 'pending' }` until it settles, then
 * `{ state: 'resolved', value }` or `{ state: 'rejected', code }`. Every call
 * is watched, so no rejection is left unhandled.
 */
function watch(promise) {
  const outcome = { state: 'pending' };
  promise.then(
    (value) => {
      Object.assign(outcome, { state: 'resolved', value });
    },
    (error) => {
      Object.assign(outcome, { state: 'rejected', code: error.code });
    },
  );
  return outcome;
}

/** Runs `action` and returns the arguments of each console.error it logs. */
async function captureErrors(action) {
  const logged = [];
  const logError = console.error;
  console.error = (...args) => {
    logged.push(args);
  };
  try {
    await action();
  } finally {
    console.error = logError;
  }
  return logged;
}

/** Values that are not a function, so cannot be a listener. */
const NOT_LISTENERS = [
  ['undefined', undefined],
  ['null', null],
  ['an object', {}],
  ['a string', 'listener'],
];

/** Adoption messages the frame must ignore (decision 0054, rule 10). */
const IGNORED = [
  [
    'a reason that is not one of the four',
    { type: 'event', event: 'view.adopted', reason: 'other', leaseMs: 5000 },
  ],
  ['no reason', { type: 'event', event: 'view.adopted', leaseMs: 5000 }],
  ['a leaseMs of 0', adoption('lease', 0)],
  ['a leaseMs of -1', adoption('lease', -1)],
  ['a leaseMs of 1.5', adoption('lease', 1.5)],
  ['a leaseMs that is a string', adoption('lease', '5000')],
  ['a leaseMs of NaN', adoption('lease', NaN)],
  ['a leaseMs past the timer range', adoption('lease', 2_147_483_648)],
  ['no leaseMs', { type: 'event', event: 'view.adopted', reason: 'lease' }],
  [
    'an event that is not view.adopted',
    { type: 'event', event: 'view.unknown', reason: 'lease', leaseMs: 5000 },
  ],
];

test('a call made after view.adopted is posted, not expired at the old deadline', async () => {
  const f = frame();
  f.advance(900);
  f.reply(adoption('lease', 5000));
  await settle();
  f.advance(600);
  const read = watch(f.api.currentMetadata());
  await settle();
  assert.equal(f.received.length, 1, 'the call is posted to the host');
  const [sent] = f.received;
  assert.equal(sent.type, 'metadata', 'as a metadata request');
  assert.deepEqual(read, { state: 'pending' }, 'still waiting at 1500 ms');
  f.reply({ id: sent.id, ok: true, metadata: METADATA });
  await settle();
  assert.deepEqual(read, { state: 'resolved', value: METADATA });
});

test('without view.adopted, a call made after the lease is expired and posts nothing', async () => {
  const f = frame();
  f.advance(1500);
  const read = watch(f.api.currentMetadata());
  await settle();
  assert.deepEqual(read, { state: 'rejected', code: 'expired' });
  assert.deepEqual(f.received, [], 'nothing is posted');
});

test('a call already waiting when view.adopted arrives is not expired at the old deadline', async () => {
  const f = frame();
  const read = watch(f.api.currentMetadata());
  await settle();
  assert.equal(f.received.length, 1, 'the call is posted to the host');
  const [sent] = f.received;
  f.advance(500);
  f.reply(adoption('lease', 60_000));
  await settle();
  f.advance(500);
  await settle();
  assert.deepEqual(read, { state: 'pending' }, 'still waiting at 1000 ms');
  f.advance(500);
  await settle();
  assert.deepEqual(read, { state: 'pending' }, 'still waiting at 1500 ms');
  f.reply({ id: sent.id, ok: true, metadata: METADATA });
  await settle();
  assert.deepEqual(
    read,
    { state: 'resolved', value: METADATA },
    'the host answer resolves it',
  );
});

test('without view.adopted, a waiting call is expired at the lease deadline', async () => {
  const f = frame();
  const read = watch(f.api.currentMetadata());
  await settle();
  f.advance(999);
  await settle();
  assert.deepEqual(read, { state: 'pending' }, 'still waiting at 999 ms');
  f.advance(1);
  await settle();
  assert.deepEqual(
    read,
    { state: 'rejected', code: 'expired' },
    'expired at 1000 ms',
  );
});

test('a call keeps its own read timeout when a longer lease is adopted', async () => {
  const f = frame();
  f.reply(adoption('lease', 600_000));
  await settle();
  const read = watch(f.api.currentMetadata());
  await settle();
  assert.equal(f.received.length, 1, 'the call is posted to the host');
  const [sent] = f.received;
  f.advance(READ_TIMEOUT_MS - 1);
  await settle();
  assert.deepEqual(read, { state: 'pending' }, 'still waiting at 9999 ms');
  f.advance(1);
  await settle();
  assert.deepEqual(
    read,
    { state: 'rejected', code: 'unavailable' },
    'ended at its own timeout',
  );
  assert.deepEqual(
    f.received,
    [sent, { type: 'cancel', id: sent.id }],
    'and cancelled at the host',
  );
});

for (const reason of ADOPTION_REASONS) {
  test(`a ${reason} adoption calls the listener once, with exactly {reason}`, async () => {
    const f = frame();
    const { calls, listener } = recorder();
    f.api.view.onAdopt(listener);
    await settle();
    assert.equal(f.toParent.length, 1, 'only the connect message was posted');
    f.reply(adoption(reason, 5000));
    await settle();
    assert.deepEqual(calls, [[{ reason }]], 'called once, with one argument');
    assert.equal(Object.isFrozen(calls[0][0]), true, 'the argument is frozen');
    assert.deepEqual(f.received, [], 'nothing is sent to the host');
  });
}

test('the same function registered twice is called twice for one adoption', async () => {
  const f = frame();
  const { calls, listener } = recorder();
  f.api.view.onAdopt(listener);
  f.api.view.onAdopt(listener);
  f.reply(adoption('head', 5000));
  await settle();
  assert.deepEqual(calls, [[{ reason: 'head' }], [{ reason: 'head' }]]);
});

test('a remover removes its own registration, and calling it again changes nothing', async () => {
  const f = frame();
  const { calls, listener } = recorder();
  const removeFirst = f.api.view.onAdopt(listener);
  f.api.view.onAdopt(listener);
  removeFirst();
  f.reply(adoption('saved', 5000));
  await settle();
  removeFirst();
  f.reply(adoption('permissions', 5000));
  await settle();
  assert.deepEqual(calls, [[{ reason: 'saved' }], [{ reason: 'permissions' }]]);
});

test('a listener that throws does not stop the others, and nothing is sent to the host', async () => {
  const f = frame();
  const { calls, listener } = recorder();
  f.api.view.onAdopt(() => {
    throw new Error('The first listener fails.');
  });
  f.api.view.onAdopt(listener);
  const logged = await captureErrors(async () => {
    f.reply(adoption('lease', 5000));
    await settle();
  });
  assert.deepEqual(
    calls,
    [[{ reason: 'lease' }]],
    'the second listener is called once',
  );
  assert.equal(logged.length, 1, 'the failure is logged once');
  assert.deepEqual(f.received, [], 'nothing is sent to the host');
});

for (const [label, value] of NOT_LISTENERS) {
  test(`refuses ${label} as a listener with a TypeError, and sends nothing`, async () => {
    const f = frame();
    const { calls, listener } = recorder();
    assert.throws(() => f.api.view.onAdopt(value), TypeError);
    f.api.view.onAdopt(listener);
    f.reply(adoption('lease', 5000));
    await settle();
    assert.deepEqual(
      calls,
      [[{ reason: 'lease' }]],
      'the frame still delivers',
    );
    assert.deepEqual(f.received, [], 'nothing is sent to the host');
  });
}

for (const [label, message] of IGNORED) {
  test(`ignores ${label}`, async () => {
    const f = frame();
    const { calls, listener } = recorder();
    const hints = recorder();
    f.api.view.onAdopt(listener);
    f.api.data.onChange(hints.listener);
    f.reply(message);
    // Messages arrive in order: the hint after the ignored message shows that
    // the message was handled first.
    f.reply(hint(7));
    await settle();
    assert.deepEqual(calls, [], 'the adoption listener is not called');
    assert.deepEqual(
      hints.calls,
      [[{ dataVersion: 7 }]],
      'the hint after it is delivered',
    );
    f.advance(1100);
    const read = watch(f.api.currentMetadata());
    await settle();
    assert.deepEqual(
      read,
      { state: 'rejected', code: 'expired' },
      'the deadline is still the one from the start',
    );
    assert.deepEqual(f.received, [], 'nothing is sent to the host');
  });
}
