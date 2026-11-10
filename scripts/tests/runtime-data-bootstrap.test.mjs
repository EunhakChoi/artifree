import assert from 'node:assert/strict';
import { afterEach, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The frame script is bundled as the server embeds it, then run against a
// stand-in window and a real MessageChannel. The host side is the test: it
// answers each data.get in the reply shape the host sends. The workspace host's
// parser is bundled too, so every call the frame makes must be one the host
// accepts (decision 0053, rule 13).
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
/** The most bytes of JSON text one data read returns (decision 0053, rule 15). */
const MAX_TEXT = 1_048_576;

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
 * Makes one data read of `request`, answers the message it sends with `reply`,
 * and says how the call settled: with its value or its error, and with the
 * message the frame sent.
 */
async function readAnswered(request, reply) {
  const f = frame();
  const settled = f.api.data.get(request).then(
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

/** A data answer for `path` as the host sends it, with `fields` changed. */
function answerFor(path, fields = {}) {
  return { path, dataVersion: 3, exists: true, value: '{}', ...fields };
}

test('data.get sends a call the host accepts and resolves with the parsed value', async () => {
  const outcome = await readAnswered(
    { path: 'notes' },
    { ok: true, data: answerFor('notes', { value: '{"a":[1,null]}' }) },
  );
  assert.equal(outcome.error, undefined, 'the call must resolve');
  assert.deepEqual(outcome.sent, { type: 'data.get', path: 'notes', id: 1 });
  assertAccepted(outcome.sent);
  assert.deepEqual(outcome.value, {
    path: 'notes',
    dataVersion: 3,
    exists: true,
    value: { a: [1, null] },
  });
});

/** Answers that resolve, each with the value the SDK must return for it. */
const RESOLVED = [
  [
    'the JSON null',
    'notes',
    answerFor('notes', { value: 'null' }),
    { path: 'notes', dataVersion: 3, exists: true, value: null },
  ],
  [
    'an absent path, with the null the host sends for it',
    'notes',
    { path: 'notes', dataVersion: 3, exists: false, value: null },
    { path: 'notes', dataVersion: 3, exists: false, value: undefined },
  ],
  [
    'the root as an empty object',
    '',
    answerFor('', { dataVersion: 1, value: '{}' }),
    { path: '', dataVersion: 1, exists: true, value: {} },
  ],
];

for (const [label, path, data, expected] of RESOLVED) {
  test(`resolves with the parsed value: ${label}`, async () => {
    const outcome = await readAnswered({ path }, { ok: true, data });
    assert.equal(outcome.error, undefined, 'the call must resolve');
    assert.deepEqual(outcome.sent, { type: 'data.get', path, id: 1 });
    assertAccepted(outcome.sent);
    assert.deepEqual(outcome.value, expected);
  });
}

/**
 * Texts of exactly the bound. The first is one byte per character, so its
 * UTF-16 length and its byte count are both the bound; the second is two bytes
 * per character, so only its byte count reaches the bound.
 */
const AT_BOUND = [
  ['one byte per character', 'a'.repeat(MAX_TEXT - 2)],
  ['two bytes per character', 'é'.repeat(524_287)],
];

for (const [label, text] of AT_BOUND) {
  test(`a value text at the bound resolves: ${label}`, async () => {
    const outcome = await readAnswered(
      { path: 'notes' },
      { ok: true, data: answerFor('notes', { value: JSON.stringify(text) }) },
    );
    assert.equal(outcome.error, undefined, 'the call must resolve');
    assert.equal(
      outcome.value?.value === text,
      true,
      'the parsed string differs from the text sent',
    );
  });
}

/**
 * Answers the host never sends for the path asked, each refused as unavailable.
 * A row is a label, the path the call asks for, and the answer the host sends,
 * which names the path it answers.
 */
const UNAVAILABLE = [
  ['other path', 'notes', answerFor('other')],
  ['the root for a named path', 'notes', answerFor('')],
  [
    'a data version below zero',
    'notes',
    answerFor('notes', { dataVersion: -1 }),
  ],
  [
    'a fractional data version',
    'notes',
    answerFor('notes', { dataVersion: 1.5 }),
  ],
  [
    'a data version past the safe integers',
    'notes',
    answerFor('notes', { dataVersion: 2 ** 53 }),
  ],
  ['a data version as text', 'notes', answerFor('notes', { dataVersion: '3' })],
  [
    'an exists flag that is not a boolean',
    'notes',
    answerFor('notes', { exists: 'yes' }),
  ],
  ['a value that is not text', 'notes', answerFor('notes', { value: 5 })],
  [
    'a value of null for an existing path',
    'notes',
    answerFor('notes', { value: null }),
  ],
  [
    'no value for an existing path',
    'notes',
    { path: 'notes', dataVersion: 3, exists: true },
  ],
  ['text that is not JSON', 'notes', answerFor('notes', { value: '{' })],
  [
    'text one byte over the bound',
    'notes',
    answerFor('notes', { value: JSON.stringify('a'.repeat(MAX_TEXT - 1)) }),
  ],
  [
    'text over the bound in UTF-8 bytes',
    'notes',
    answerFor('notes', { value: JSON.stringify('é'.repeat(524_288)) }),
  ],
  ['a root that is an array', '', answerFor('', { value: '[]' })],
  ['a root that is null', '', answerFor('', { value: 'null' })],
  ['a root that is a number', '', answerFor('', { value: '5' })],
  ['a root that is a string', '', answerFor('', { value: '"x"' })],
  [
    'a root that does not exist',
    '',
    { path: '', dataVersion: 3, exists: false, value: null },
  ],
  [
    'an absent path with a value',
    'notes',
    { path: 'notes', dataVersion: 3, exists: false, value: 'x' },
  ],
  [
    'an absent path with no value',
    'notes',
    { path: 'notes', dataVersion: 3, exists: false },
  ],
];

for (const [label, path, data] of UNAVAILABLE) {
  test(`rejects unavailable: ${label}`, async () => {
    const outcome = await readAnswered({ path }, { ok: true, data });
    assertUnavailable(outcome, label);
    assertAccepted(outcome.sent);
  });
}

const REVOKED = Proxy.revocable({}, {});
REVOKED.revoke();

/** Arguments that are not exactly `{ path: string }`, each refused unsent. */
const INVALID = [
  ['no argument', undefined],
  ['null', null],
  ['a string', 'notes'],
  ['an array', ['notes']],
  ['a path that is not text', { path: 1 }],
  ['no path', {}],
  ['a key besides path', { path: 'notes', extra: 1 }],
  [
    'a path that is not enumerable',
    Object.defineProperty({}, 'path', { value: 'notes', enumerable: false }),
  ],
  [
    'a path behind a getter',
    {
      get path() {
        return 'notes';
      },
    },
  ],
  ['a revoked proxy', REVOKED.proxy],
];

for (const [label, request] of INVALID) {
  test(`refuses invalid_request without posting: ${label}`, async () => {
    const f = frame();
    const error = await refusal(f.api.data.get(request));
    assert.equal(error.code, 'invalid_request');
    await settle();
    assert.deepEqual(f.received, [], 'nothing was sent');
    f.hangUp();
  });
}

test('a refusal from the host rejects with its code and the closed wording', async () => {
  const outcome = await readAnswered(
    { path: 'notes' },
    { ok: false, code: 'denied' },
  );
  assert.equal(outcome.value, undefined, 'the call must be refused');
  assert.equal(outcome.error?.name, 'RuntimeRequestFailure');
  assert.equal(outcome.error?.code, 'denied');
  assert.equal(
    outcome.error?.message,
    'The runtime request was not permitted.',
  );
  assertAccepted(outcome.sent);
});
