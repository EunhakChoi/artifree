import assert from 'node:assert/strict';
import { afterEach, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The frame script is bundled as the server embeds it, then run against a
// stand-in window and a real MessageChannel. The test plays the workspace: it
// answers each write in the reply shape the host sends. The workspace host's
// parser is bundled too, so every write the frame sends must be a call the host
// accepts (decision 0053, rules 5, 13 and 15).
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
/** The most bytes of JSON text one data write carries (decision 0053, rule 15). */
const MAX_WRITE = 262_144;
/** A retry identity of ten characters, within the eight to thirty-six allowed. */
const RETRY = 'retry-0001';
/** The value a set or an update starts from, and the JSON text sent for it. */
const VALUE = { a: [1, null] };
const VALUE_TEXT = '{"a":[1,null]}';
/** A write that changed the data from version 3 to version 4. */
const CHANGED = { path: 'notes', dataVersion: 4, changed: true };
/** A write that changed nothing: the version stays at 3. */
const UNCHANGED = { path: 'notes', dataVersion: 3, changed: false };

/**
 * The three writes. `text` names the field that carries JSON text, which remove
 * does not have. `other` names a field that belongs to another write, which
 * each write must refuse.
 */
const WRITES = [
  { method: 'set', text: 'value', other: 'patch' },
  { method: 'update', text: 'patch', other: 'value' },
  { method: 'remove', text: undefined, other: 'value' },
];
const TEXT_WRITES = WRITES.filter((write) => write.text !== undefined);

/** The message each write sends for its complete request, as a frame's first call. */
const SENT = {
  set: {
    id: 1,
    type: 'data.set',
    path: 'notes',
    value: VALUE_TEXT,
    baseDataVersion: 3,
    retryId: RETRY,
  },
  update: {
    id: 1,
    type: 'data.update',
    path: 'notes',
    patch: VALUE_TEXT,
    baseDataVersion: 3,
    retryId: RETRY,
  },
  remove: {
    id: 1,
    type: 'data.remove',
    path: 'notes',
    baseDataVersion: 3,
    retryId: RETRY,
  },
};

let current;

/**
 * Starts the script on a stand-in window; the test plays the workspace. Each
 * message the frame sends is recorded, and the test answers one with `reply`.
 */
function frame() {
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
    hangUp: () => listeners.pagehide(),
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

/** Resolves once `promise` is refused, and returns the refusal. */
async function refusal(promise) {
  const error = await promise.then(
    () => assert.fail('The call must be refused.'),
    (rejection) => rejection,
  );
  assert.equal(error.name, 'RuntimeRequestFailure');
  return error;
}

/**
 * Makes one write of `request` with `method`, answers the message it sends with
 * `reply`, and says how the call settled: with its value or its error, the
 * message the frame sent, and how many messages it sent in all once the answer
 * was handled, so that a retry would show.
 */
async function answered(method, request, reply) {
  const f = frame();
  const settled = f.api.data[method](request).then(
    (value) => ({ value }),
    (error) => ({ error }),
  );
  await settle();
  assert.equal(f.received.length, 1, 'one call was sent');
  const [sent] = f.received;
  f.reply({ id: sent.id, ...reply });
  const outcome = await settled;
  await settle();
  f.hangUp();
  return { ...outcome, sent, count: f.received.length };
}

/**
 * The call with `request` is refused as invalid_request, and nothing is sent.
 * Whether anything was sent is checked before the refusal is awaited, so a call
 * that is not refused fails at once instead of waiting out its timeout.
 */
async function assertRefusedUnsent(method, request) {
  const f = frame();
  const refused = refusal(f.api.data[method](request));
  await settle();
  assert.deepEqual(f.received, [], 'nothing was sent');
  const error = await refused;
  assert.equal(error.code, 'invalid_request');
  f.hangUp();
}

/** The host's parser accepts `sent` and reads it back as the same call. */
function assertAccepted(sent) {
  assert.deepEqual(parseCall(sent), sent, JSON.stringify(sent));
}

/**
 * The call was refused as unavailable, with the closed wording. The value is
 * compared as a boolean: a value that should have been refused would otherwise
 * be diffed in full by the failure report.
 */
function assertUnavailable(outcome, label) {
  assert.equal(
    outcome.value === undefined,
    true,
    `${label}: the call resolved instead of being refused`,
  );
  assert.equal(outcome.error?.name, 'RuntimeRequestFailure', label);
  assert.equal(outcome.error?.code, 'unavailable', label);
  assert.equal(
    outcome.error?.message,
    'The runtime request is unavailable.',
    label,
  );
}

/** A complete request of `write` as the frame sends it, with `overrides` on top. */
function complete(write, overrides = {}) {
  const request = { path: 'notes', baseDataVersion: 3, retryId: RETRY };
  if (write.text !== undefined) request[write.text] = VALUE;
  return Object.assign(request, overrides);
}

/** A copy of `request` without `key`. */
function without(request, key) {
  const copy = { ...request };
  delete copy[key];
  return copy;
}

/** `request` with its `key` made hidden from enumeration. */
function hidden(request, key) {
  Object.defineProperty(request, key, { enumerable: false });
  return request;
}

/** `request` with its `key` read through a getter that returns `value`. */
function getter(request, key, value) {
  Object.defineProperty(request, key, {
    get: () => value,
    enumerable: true,
    configurable: true,
  });
  return request;
}

/** An array of `depth` arrays, one inside the other, around null. */
function nested(depth) {
  let value = null;
  for (let level = 0; level < depth; level += 1) value = [value];
  return value;
}

/** An array whose second element was deleted, so it has a hole. */
function holed() {
  const value = [1, 2, 3];
  delete value[1];
  return value;
}

/** An object that refers to itself. */
function cyclic() {
  const value = {};
  value.self = value;
  return value;
}

const REVOKED = Proxy.revocable({}, {});
REVOKED.revoke();

/**
 * The JSON text a set or an update sends for each value (decision 0053, rule 5):
 * the text of the value, never the value itself.
 */
const TEXTS = [
  ['an object', { a: [1, null] }, '{"a":[1,null]}'],
  [
    'an object with its keys in the order given',
    { b: 1, a: 2 },
    '{"b":1,"a":2}',
  ],
  ['the JSON null', null, 'null'],
  ['a number', 5, '5'],
  ['a boolean', false, 'false'],
  ['a string', 'x', '"x"'],
  ['a string that JSON escapes', 'a"b\\c\n', '"a\\"b\\\\c\\n"'],
  ['a string of non-ASCII text', 'é', '"é"'],
  ['an array', [1, 'two', [null]], '[1,"two",[null]]'],
  ['an empty object', {}, '{}'],
  [
    'an array 64 deep, the deepest a value may be',
    nested(64),
    '['.repeat(64) + 'null' + ']'.repeat(64),
  ],
];

/**
 * Arguments that are not the exact object of a write, each built for the write
 * it is tried on. Each is refused without posting.
 */
const BAD_ARGUMENTS = [
  ['no argument', () => undefined],
  ['null', () => null],
  ['a string', () => 'notes'],
  ['an array', () => ['notes']],
  ['a path that is not text', (write) => complete(write, { path: 1 })],
  ['no path', (write) => without(complete(write), 'path')],
  ['no base version', (write) => without(complete(write), 'baseDataVersion')],
  ['no retry identity', (write) => without(complete(write), 'retryId')],
  [
    'a field of another write',
    (write) => complete(write, { [write.other]: VALUE }),
  ],
  ['a field the write does not take', (write) => complete(write, { extra: 1 })],
  [
    'a field that is not enumerable',
    (write) => hidden(complete(write), 'path'),
  ],
  [
    'a field behind a getter',
    (write) => getter(complete(write), 'path', 'notes'),
  ],
  [
    'an object that is not a plain object',
    (write) => Object.assign(new (class Request {})(), complete(write)),
  ],
  ['a revoked proxy', () => REVOKED.proxy],
];

/** Retry identities outside the eight to thirty-six characters of the grammar. */
const BAD_RETRY_IDS = [
  ['an empty retry identity', ''],
  ['a retry identity of seven characters', 'abcdefg'],
  ['a retry identity of thirty-seven characters', 'a'.repeat(37)],
  ['a retry identity with a space', 'retry 0001'],
  ['a retry identity with a slash', 'retry/0001'],
  ['a retry identity with a dot', 'retry.0001'],
  ['a retry identity with a trailing newline', 'retry-0001\n'],
  ['a retry identity that is not text', 12_345_678],
];

/** Bases a write may not name: not a count, or not one a write can follow. */
const BAD_BASES = [
  ['a negative base', -1],
  ['a fractional base', 1.5],
  ['a base that is text', '3'],
  ['a base that is not a number', NaN],
  ['an infinite base', Infinity],
  ['the largest safe integer as a base', Number.MAX_SAFE_INTEGER],
  ['a base past the safe integers', 2 ** 53],
];

/**
 * Values JSON.stringify would change or that have no JSON form, each refused
 * before anything is sent (decision 0053, rule 5).
 */
const BAD_TEXTS = [
  ['NaN', NaN],
  ['Infinity', Infinity],
  ['minus Infinity', -Infinity],
  ['undefined', undefined],
  ['a function', () => 1],
  ['a symbol', Symbol('value')],
  ['a bigint', 1n],
  ['a Date', new Date(0)],
  ['a Map', new Map()],
  ['an instance of a class', Object.assign(new (class Point {})(), { x: 1 })],
  ['an array with a hole', holed()],
  [
    'an array with a property beyond its elements',
    Object.assign([1], { extra: true }),
  ],
  ['an object with an undefined member', { a: undefined }],
  ['an object with a function member', { f: () => 1 }],
  ['an object with a NaN member', { n: NaN }],
  ['an object with a symbol key', { [Symbol('key')]: 1 }],
  [
    'an object with a hidden member',
    Object.defineProperty({ a: 1 }, 'hidden', { value: 2, enumerable: false }),
  ],
  [
    'an object with a getter',
    {
      get a() {
        return 1;
      },
    },
  ],
  ['an object with a toJSON method', { toJSON: () => 'x' }],
  ['an object that refers to itself', cyclic()],
  ['an array 65 deep, one level past the limit', nested(65)],
  ['a revoked proxy', REVOKED.proxy],
];

/** Texts of exactly the bound: one byte a character, then two bytes a character. */
const AT_BOUND = [
  ['one byte per character', 'a'.repeat(MAX_WRITE - 2)],
  ['two bytes per character', 'é'.repeat(131_071)],
];

/**
 * Texts over the bound in bytes. The second has fewer UTF-16 units than the
 * bound, so only its byte count refuses it.
 */
const OVER_BOUND = [
  ['one byte per character', 'a'.repeat(MAX_WRITE - 1)],
  ['two bytes per character', 'é'.repeat(131_072)],
];

/**
 * The edges of the grammar that are sent: the shortest and longest retry
 * identities, and the smallest and largest bases. Each row names the receipt
 * its write is answered with.
 */
const ACCEPTED_FIELDS = [
  ['a retry identity of eight characters', { retryId: 'abcdefgh' }, UNCHANGED],
  [
    'a retry identity of thirty-six characters',
    { retryId: 'a'.repeat(36) },
    UNCHANGED,
  ],
  [
    'a base of zero',
    { baseDataVersion: 0 },
    { path: 'notes', dataVersion: 0, changed: false },
  ],
  [
    'the largest base a write may name',
    { baseDataVersion: Number.MAX_SAFE_INTEGER - 1 },
    {
      path: 'notes',
      dataVersion: Number.MAX_SAFE_INTEGER - 1,
      changed: false,
    },
  ],
];

/** Receipts a write resolves with: the path, the version and the change flag. */
const ACCEPTED_RECEIPTS = [
  ['a change, one version past the base', CHANGED],
  ['no change, the base kept', UNCHANGED],
];

/**
 * Answers that the SDK refuses as unavailable: a receipt that names another path
 * or a version that does not follow the write, a receipt with a field of the
 * wrong type, and no receipt at all.
 */
const REFUSED_RECEIPTS = [
  [
    'a receipt whose version is not the base plus one is refused: the base, after a change',
    { ok: true, written: { path: 'notes', dataVersion: 3, changed: true } },
  ],
  [
    'a receipt whose version is not the base plus one is refused: two past the base, after a change',
    { ok: true, written: { path: 'notes', dataVersion: 5, changed: true } },
  ],
  [
    'a receipt whose version is not the base is refused: the base plus one, with no change',
    { ok: true, written: { path: 'notes', dataVersion: 4, changed: false } },
  ],
  [
    'a receipt whose version is not the base is refused: below the base, with no change',
    { ok: true, written: { path: 'notes', dataVersion: 2, changed: false } },
  ],
  [
    'a receipt for another path is refused',
    { ok: true, written: { path: 'other', dataVersion: 4, changed: true } },
  ],
  [
    'a receipt whose version is text is refused',
    { ok: true, written: { path: 'notes', dataVersion: '4', changed: true } },
  ],
  [
    'a receipt whose version is not a whole number is refused',
    { ok: true, written: { path: 'notes', dataVersion: 4.5, changed: true } },
  ],
  [
    'a receipt whose change flag is not a boolean is refused',
    { ok: true, written: { path: 'notes', dataVersion: 4, changed: 'yes' } },
  ],
  ['no receipt is refused', { ok: true }],
];

/** Answers after which the frame must not send the write again. */
const REFUSED_ANSWERS = [
  ['a conflict', { ok: false, code: 'conflict', currentDataVersion: 7 }],
  ['a refusal', { ok: false, code: 'denied' }],
  [
    'a receipt that fails the check',
    { ok: true, written: { path: 'other', dataVersion: 4, changed: true } },
  ],
];

for (const write of WRITES) {
  test(`${write.method} sends its message and the host accepts it`, async () => {
    const outcome = await answered(write.method, complete(write), {
      ok: true,
      written: CHANGED,
    });
    assert.equal(outcome.error, undefined, 'the call must resolve');
    assert.deepEqual(outcome.sent, SENT[write.method]);
    assertAccepted(outcome.sent);
    assert.deepEqual(outcome.value, CHANGED);
  });
}

for (const write of TEXT_WRITES) {
  for (const [label, value, text] of TEXTS) {
    test(`${write.method} sends the JSON text of ${label}`, async () => {
      const outcome = await answered(
        write.method,
        complete(write, { [write.text]: value }),
        { ok: true, written: CHANGED },
      );
      assert.equal(outcome.error, undefined, 'the call must resolve');
      assert.equal(outcome.sent[write.text], text, 'the text sent');
      assertAccepted(outcome.sent);
    });
  }
}

for (const write of TEXT_WRITES) {
  for (const [label, value] of AT_BOUND) {
    test(`${write.method} sends a text of the bound in bytes: ${label}`, async () => {
      const outcome = await answered(
        write.method,
        complete(write, { [write.text]: value }),
        { ok: true, written: CHANGED },
      );
      assert.equal(outcome.error, undefined, 'the call must resolve');
      assert.equal(
        Buffer.byteLength(outcome.sent[write.text]),
        MAX_WRITE,
        'the text sent is the bound',
      );
      assertAccepted(outcome.sent);
    });
  }
  for (const [label, value] of OVER_BOUND) {
    test(`${write.method} refuses a text over the bound in bytes without posting: ${label}`, async () => {
      await assertRefusedUnsent(
        write.method,
        complete(write, { [write.text]: value }),
      );
    });
  }
  test(`${write.method} refuses a request with no ${write.text} without posting`, async () => {
    await assertRefusedUnsent(
      write.method,
      without(complete(write), write.text),
    );
  });
  for (const [label, value] of BAD_TEXTS) {
    test(`${write.method} refuses ${label} as its text without posting`, async () => {
      await assertRefusedUnsent(
        write.method,
        complete(write, { [write.text]: value }),
      );
    });
  }
}

for (const write of WRITES) {
  for (const [label, build] of BAD_ARGUMENTS) {
    test(`${write.method} refuses ${label} without posting`, async () => {
      await assertRefusedUnsent(write.method, build(write));
    });
  }
  for (const [label, retryId] of BAD_RETRY_IDS) {
    test(`${write.method} refuses ${label} without posting`, async () => {
      await assertRefusedUnsent(write.method, complete(write, { retryId }));
    });
  }
  for (const [label, baseDataVersion] of BAD_BASES) {
    test(`${write.method} refuses ${label} without posting`, async () => {
      await assertRefusedUnsent(
        write.method,
        complete(write, { baseDataVersion }),
      );
    });
  }
  for (const [label, overrides, written] of ACCEPTED_FIELDS) {
    test(`${write.method} sends ${label}`, async () => {
      const outcome = await answered(write.method, complete(write, overrides), {
        ok: true,
        written,
      });
      assert.equal(outcome.error, undefined, 'the call must resolve');
      assert.deepEqual(outcome.sent, { ...SENT[write.method], ...overrides });
      assertAccepted(outcome.sent);
      assert.deepEqual(outcome.value, written);
    });
  }
  for (const [label, written] of ACCEPTED_RECEIPTS) {
    test(`${write.method} resolves with its receipt: ${label}`, async () => {
      const outcome = await answered(write.method, complete(write), {
        ok: true,
        written,
      });
      assert.equal(outcome.error, undefined, 'the call must resolve');
      assert.deepEqual(outcome.value, written);
    });
  }
  for (const [label, reply] of REFUSED_RECEIPTS) {
    test(`${write.method} rejects unavailable: ${label}`, async () => {
      const outcome = await answered(write.method, complete(write), reply);
      assertUnavailable(outcome, label);
      assertAccepted(outcome.sent);
    });
  }
  test(`${write.method} rejects a conflict with the data version the host names`, async () => {
    const outcome = await answered(write.method, complete(write), {
      ok: false,
      code: 'conflict',
      currentDataVersion: 7,
    });
    assert.equal(outcome.value, undefined, 'the call must be refused');
    assert.equal(outcome.error?.name, 'RuntimeRequestFailure');
    assert.equal(outcome.error?.code, 'conflict');
    assert.equal(outcome.error?.currentDataVersion, 7);
    assert.equal(outcome.error?.currentAssetVersion, undefined);
    assert.equal(
      outcome.error?.message,
      'The application data is not at the version the call named.',
    );
    assertAccepted(outcome.sent);
  });
  for (const [label, reply] of REFUSED_ANSWERS) {
    test(`${write.method} is not sent again after ${label}`, async () => {
      const outcome = await answered(write.method, complete(write), reply);
      assert.equal(outcome.value, undefined, 'the call must be refused');
      assert.equal(outcome.count, 1, 'one message was sent in all');
    });
  }
}
