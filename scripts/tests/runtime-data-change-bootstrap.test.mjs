import assert from 'node:assert/strict';
import { afterEach, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The frame script is bundled as the server embeds it, then run against a
// stand-in window and a real MessageChannel. The test plays the workspace: it
// sends the data change hint as a message with no call id, then checks which
// listeners hear it and that nothing is sent back to the host (decision 0053,
// rule 16).
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
  lease_ms: 900_000,
};

let current;

/**
 * Starts the script on a stand-in window; the test plays the workspace. Each
 * message the frame sends over the port is recorded in `received`, and the
 * test sends one with `reply`. `toParent` records each message posted to the
 * parent window, starting with the connect message.
 */
function frame() {
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
    api: window.artifree,
    received,
    reply: (message) => port.postMessage(message),
    toParent,
  };
}

afterEach(() => {
  current?.listeners.pagehide?.();
  current = undefined;
  delete globalThis.window;
});

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

/** Messages the frame must ignore: an event it does not know, or no usable version. */
const IGNORED = [
  [
    'an event that is not data.changed',
    { type: 'event', event: 'data.other', dataVersion: 4 },
  ],
  ['data.changed with a version of -1', hint(-1)],
  ['data.changed with a version of 1.5', hint(1.5)],
  ['data.changed with a version of "7"', hint('7')],
  ['data.changed with a version of NaN', hint(NaN)],
  [
    'data.changed with a version past the safe integers',
    hint(Number.MAX_SAFE_INTEGER + 1),
  ],
  ['data.changed with a version of null', hint(null)],
  ['data.changed with no version', { type: 'event', event: 'data.changed' }],
];

/** The edges of a usable version: the smallest and the largest safe integer. */
const DELIVERED = [
  ['zero', 0],
  ['the largest safe integer', Number.MAX_SAFE_INTEGER],
];

/** Values that are not a function, so cannot be a listener. */
const NOT_LISTENERS = [
  ['undefined', undefined],
  ['null', null],
  ['an object', {}],
  ['a string', 'listener'],
];

test('registering a data change listener sends nothing to the host', async () => {
  const f = frame();
  f.api.data.onChange(() => {});
  await settle();
  assert.equal(f.toParent.length, 1, 'only the connect message was posted');
  assert.deepEqual(f.received, [], 'nothing is sent over the port');
});

test('a data change hint calls the listener once, with one argument', async () => {
  const f = frame();
  const { calls, listener } = recorder();
  f.api.data.onChange(listener);
  f.reply(hint(7));
  await settle();
  assert.equal(calls.length, 1, 'the listener is called once');
  assert.equal(calls[0].length, 1, 'with exactly one argument');
  assert.deepEqual(
    calls[0][0],
    { dataVersion: 7 },
    'that argument is the change',
  );
});

test('the same function registered twice is called twice for one hint', async () => {
  const f = frame();
  const { calls, listener } = recorder();
  f.api.data.onChange(listener);
  f.api.data.onChange(listener);
  f.reply(hint(7));
  await settle();
  assert.deepEqual(calls, [[{ dataVersion: 7 }], [{ dataVersion: 7 }]]);
});

test('a remover removes its own registration, and calling it again changes nothing', async () => {
  const f = frame();
  const { calls, listener } = recorder();
  const removeFirst = f.api.data.onChange(listener);
  f.api.data.onChange(listener);
  removeFirst();
  f.reply(hint(7));
  await settle();
  removeFirst();
  f.reply(hint(8));
  await settle();
  assert.deepEqual(calls, [[{ dataVersion: 7 }], [{ dataVersion: 8 }]]);
});

test('the second remover removes the last registration', async () => {
  const f = frame();
  const { calls, listener } = recorder();
  const sentinel = recorder();
  const removeFirst = f.api.data.onChange(listener);
  const removeSecond = f.api.data.onChange(listener);
  f.api.data.onChange(sentinel.listener);
  removeFirst();
  removeSecond();
  f.reply(hint(7));
  await settle();
  assert.deepEqual(calls, [], 'the removed function is not called');
  assert.deepEqual(
    sentinel.calls,
    [[{ dataVersion: 7 }]],
    'the hint was delivered',
  );
});

test('a listener that throws does not stop the others, and nothing is sent to the host', async () => {
  const f = frame();
  const { calls, listener } = recorder();
  f.api.data.onChange(() => {
    throw new Error('The first listener fails.');
  });
  f.api.data.onChange(listener);
  const logged = [];
  const logError = console.error;
  console.error = (...args) => {
    logged.push(args);
  };
  try {
    f.reply(hint(3));
    await settle();
  } finally {
    console.error = logError;
  }
  assert.deepEqual(
    calls,
    [[{ dataVersion: 3 }]],
    'the second listener is called once',
  );
  assert.equal(logged.length, 1, 'the failure is logged once');
  assert.deepEqual(f.received, [], 'nothing is sent to the host');
});

test('a listener removed during a dispatch is not called for that hint, and later listeners still are', async () => {
  const f = frame();
  const removed = recorder();
  const later = recorder();
  // The first listener removes the second while the hint is being delivered,
  // so the second is skipped for that hint.
  let removeSecond;
  f.api.data.onChange(() => {
    removeSecond();
  });
  removeSecond = f.api.data.onChange(removed.listener);
  f.api.data.onChange(later.listener);
  f.reply(hint(7));
  await settle();
  assert.deepEqual(removed.calls, [], 'the removed listener is not called');
  assert.deepEqual(
    later.calls,
    [[{ dataVersion: 7 }]],
    'the listener after it is called',
  );
});

test('a listener added during a dispatch is not called for that hint, but is for the next', async () => {
  const f = frame();
  const added = recorder();
  // The adder registers one listener on the first hint. It is removed before
  // the next hint, so that hint does not register another.
  const removeAdder = f.api.data.onChange(() => {
    f.api.data.onChange(added.listener);
  });
  f.reply(hint(7));
  await settle();
  assert.deepEqual(added.calls, [], 'not called for this hint');
  removeAdder();
  f.reply(hint(8));
  await settle();
  assert.deepEqual(
    added.calls,
    [[{ dataVersion: 8 }]],
    'called once for the next hint',
  );
});

test('a data change hint does not settle a pending data.get', async () => {
  const f = frame();
  const { calls, listener } = recorder();
  f.api.data.onChange(listener);
  const read = f.api.data.get({ path: 'notes' }).then(
    (value) => ({ value }),
    (error) => ({ error }),
  );
  await settle();
  assert.equal(f.received.length, 1, 'the read is sent once');
  const [sent] = f.received;
  assert.equal(sent.type, 'data.get');
  f.reply(hint(5));
  await settle();
  assert.deepEqual(
    calls,
    [[{ dataVersion: 5 }]],
    'the listener is called once',
  );
  f.reply({
    id: sent.id,
    ok: true,
    data: { path: 'notes', dataVersion: 5, exists: true, value: '{"a":1}' },
  });
  const outcome = await read;
  assert.equal(outcome.error, undefined, 'the read must resolve');
  assert.deepEqual(outcome.value, {
    path: 'notes',
    dataVersion: 5,
    exists: true,
    value: { a: 1 },
  });
  assert.deepEqual(
    calls,
    [[{ dataVersion: 5 }]],
    'the reply calls no listener',
  );
});

for (const [label, message] of IGNORED) {
  test(`ignores ${label}`, async () => {
    const f = frame();
    const { calls, listener } = recorder();
    f.api.data.onChange(listener);
    f.reply(message);
    // Messages arrive in order: the valid hint after the ignored one shows
    // that the ignored one was handled, and that the listener heard nothing.
    f.reply(hint(7));
    await settle();
    assert.deepEqual(calls, [[{ dataVersion: 7 }]], 'only the valid hint');
    assert.deepEqual(f.received, [], 'nothing is sent to the host');
  });
}

for (const [label, dataVersion] of DELIVERED) {
  test(`delivers data.changed with a version of ${label}`, async () => {
    const f = frame();
    const { calls, listener } = recorder();
    f.api.data.onChange(listener);
    f.reply(hint(dataVersion));
    await settle();
    assert.deepEqual(calls, [[{ dataVersion }]]);
  });
}

for (const [label, value] of NOT_LISTENERS) {
  test(`refuses ${label} as a listener with a TypeError, and sends nothing`, async () => {
    const f = frame();
    const { calls, listener } = recorder();
    assert.throws(() => f.api.data.onChange(value), TypeError);
    f.api.data.onChange(listener);
    f.reply(hint(7));
    await settle();
    assert.deepEqual(calls, [[{ dataVersion: 7 }]], 'the frame still delivers');
    assert.deepEqual(f.received, [], 'nothing is sent to the host');
  });
}
