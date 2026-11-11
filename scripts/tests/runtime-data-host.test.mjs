import assert from 'node:assert/strict';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The frame's rpc is web code without DOM or React imports; bundle it as it
// ships. The runtime call is a fake: each test gives the source the answer the
// client would give, so what is judged here is the host's own reading of it.
const bundle = await build({
  stdin: {
    contents: "export * from './frame-rpc.ts';",
    resolveDir: fileURLToPath(
      new URL('../../apps/web/src/artifacts/', import.meta.url),
    ),
  },
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const { FrameRpc, parseCall } = await import(
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
);

const ARTIFACT = `art_${'0'.repeat(32)}`;
/** The frame transport that a source call is given. */
const FRAME = Object.freeze({ transport: 'frame' });
/** The most UTF-8 bytes a value text may have: one mebibyte. */
const READ_BOUND = 1_048_576;
/** The longest data path: 16 segments of 64 bytes and 15 separators. */
const LONGEST_PATH = 1_039;

/** A view that holds the data.read grant. */
const view = {
  url: `http://127.0.0.1:7411/r/${'b'.repeat(64)}/`,
  enabled: true,
  session_id: 'e'.repeat(32),
  artifact_id: ARTIFACT,
  revision: 3,
  head_revision: 3,
  sdk: 'artifree.app.v1',
  lifecycle: 'restart',
  declared: ['data.read'],
  supported: ['data.read'],
  granted: ['data.read'],
  grant_version: 1,
  view_id: 'd'.repeat(32),
  generation: 1,
  initialization: 'c'.repeat(64),
  expires_at_ms: 1_000_000,
  lease_ms: 900_000,
};

/** A data read with some fields replaced; otherwise the SDK's shape. */
function dataRead(changes = {}) {
  return { type: 'data.get', id: 9, path: 'notes', ...changes };
}

/** What a source answers for a path that holds a value: the client's shape. */
function found(changes = {}) {
  return {
    artifact_id: ARTIFACT,
    path: 'notes',
    data_version: 3,
    exists: true,
    value: '{"a":1}',
    ...changes,
  };
}

/** What a source answers for a path with nothing at it: no value, as on the wire. */
function absent() {
  return {
    artifact_id: ARTIFACT,
    path: 'notes',
    data_version: 3,
    exists: false,
  };
}

class StatusError extends Error {
  constructor(status, code, message = 'SECRET detail from the server') {
    super(message);
    this.name = 'ArtifreeApiError';
    this.status = status;
    this.code = code;
  }
}

/** Waits, a turn at a time, for a condition made by asynchronous work. */
async function until(condition) {
  const deadline = Date.now() + 10_000;
  while (!condition()) {
    if (Date.now() > deadline) assert.fail('The condition never became true.');
    await new Promise((resolve) => setImmediate(resolve));
  }
}

/** Lets pending promise reactions run, so that a missing reply would show. */
async function settle() {
  for (let round = 0; round < 20; round += 1) await Promise.resolve();
}

/**
 * An rpc whose runtime call is the test's: `runtimeCall` hands the callback the
 * source the test gives and the frame transport, and records each view it was
 * asked for.
 */
function rpcWith(source) {
  const sent = [];
  const runtimeCalls = [];
  const client = {
    runtimeCall(callView, run) {
      runtimeCalls.push(callView);
      return run(source, FRAME);
    },
  };
  const rpc = new FrameRpc({
    client,
    view,
    ask: () => assert.fail('A data read never prompts.'),
    save: () => assert.fail('A data read never saves.'),
    take: () => true,
    now: () => 1000,
    deadline: 1000 + 900_000,
    send: (message, transfer) => sent.push({ message, transfer }),
  });
  return { rpc, sent, runtimeCalls };
}

// ---- parsing ----

test('parseCall accepts a data read exactly as the SDK sends it', () => {
  assert.deepEqual(parseCall({ type: 'data.get', id: 1, path: 'a/b' }), {
    type: 'data.get',
    id: 1,
    path: 'a/b',
  });
});

const ACCEPTED_PATHS = [
  ['the empty path', ''],
  ['the longest path, 1,039 characters', 'a'.repeat(LONGEST_PATH)],
];
for (const [name, path] of ACCEPTED_PATHS)
  test(`parseCall accepts a data read of ${name}`, () => {
    assert.deepEqual(parseCall({ type: 'data.get', id: 1, path }), {
      type: 'data.get',
      id: 1,
      path,
    });
  });

const REFUSED_DATA_CALLS = [
  ['no path', { type: 'data.get', id: 1 }],
  ['a path that is a number', { type: 'data.get', id: 1, path: 3 }],
  ['a path that is null', { type: 'data.get', id: 1, path: null }],
  [
    'a path one character over the bound',
    { type: 'data.get', id: 1, path: 'a'.repeat(LONGEST_PATH + 1) },
  ],
  [
    'an extra key that names another artifact',
    {
      type: 'data.get',
      id: 1,
      path: 'a/b',
      artifact_id: `art_${'1'.repeat(32)}`,
    },
  ],
];
for (const [name, message] of REFUSED_DATA_CALLS)
  test(`parseCall refuses a data read with ${name}`, () => {
    assert.equal(parseCall(message), undefined);
  });

// ---- the rpc ----

test('A data read asks the source for that path, with the frame and the view, and for nothing else', async () => {
  const asked = [];
  const { rpc, runtimeCalls } = rpcWith({
    readRuntimeData(frame, callView, path, signal) {
      asked.push({ frame, callView, path, signal });
      return Promise.resolve(absent());
    },
  });
  rpc.handle(dataRead({ path: 'notes/a' }));
  await settle();
  assert.equal(runtimeCalls.length, 1);
  assert.equal(runtimeCalls[0], view);
  assert.equal(asked.length, 1);
  assert.equal(asked[0].frame, FRAME);
  assert.equal(asked[0].callView, view);
  assert.equal(asked[0].path, 'notes/a');
  assert.ok(asked[0].signal instanceof AbortSignal);
});

test('A malformed data read is answered invalid_request at once, holds no slot and reaches no server', async () => {
  const { rpc, sent, runtimeCalls } = rpcWith({});
  const malformed = [
    { type: 'data.get', id: 1 },
    { type: 'data.get', id: 2, path: 3 },
    { type: 'data.get', id: 3, path: 'a'.repeat(LONGEST_PATH + 1) },
    { type: 'data.get', id: 4, path: 'notes', artifact_id: ARTIFACT },
  ];
  for (const message of malformed) rpc.handle(message);
  await settle();
  assert.deepEqual(
    sent.map(({ message }) => message),
    malformed.map(({ id }) => ({ id, ok: false, code: 'invalid_request' })),
  );
  assert.equal(rpc.pending, 0, 'a refused call holds no slot');
  assert.deepEqual(runtimeCalls, [], 'nothing reached the server');
});

// The host never parses a stored value (decision 0053, rule 5): the text goes
// to the frame as the client answered it, including text that is not JSON.
const STORED_VALUES = [
  ["text 'not json'", 'not json'],
  ['JSON that re-encodes differently, with spacing and 1.0', '{ "a" : 1.0 }'],
];
for (const [name, value] of STORED_VALUES)
  test(`A data read passes on the stored value of ${name} unparsed`, async () => {
    const { rpc, sent } = rpcWith({
      readRuntimeData: () => Promise.resolve(found({ value })),
    });
    rpc.handle(dataRead());
    await until(() => sent.length === 1);
    assert.deepEqual(sent, [
      {
        message: {
          id: 9,
          ok: true,
          data: { path: 'notes', dataVersion: 3, exists: true, value },
        },
        transfer: [],
      },
    ]);
  });

test('A data read that finds nothing is answered with exists false and value null', async () => {
  const { rpc, sent } = rpcWith({
    readRuntimeData: () => Promise.resolve(absent()),
  });
  rpc.handle(dataRead());
  await until(() => sent.length === 1);
  assert.deepEqual(sent, [
    {
      message: {
        id: 9,
        ok: true,
        data: { path: 'notes', dataVersion: 3, exists: false, value: null },
      },
      transfer: [],
    },
  ]);
});

test('A value text of exactly the read bound is passed on whole', async () => {
  const value = 'a'.repeat(READ_BOUND);
  const { rpc, sent } = rpcWith({
    readRuntimeData: () => Promise.resolve(found({ value })),
  });
  rpc.handle(dataRead());
  await until(() => sent.length === 1);
  const [{ message }] = sent;
  assert.equal(message.ok, true);
  // Compared without printing it: a mismatch would print a mebibyte of text.
  assert.ok(
    message.data.value === value,
    'the value text is passed on unchanged',
  );
});

// The bound is in UTF-8 bytes, so a text with fewer UTF-16 units than the bound
// can still be over it: U+20AC takes three bytes and one unit.
const REFUSED_ANSWERS = [
  [
    'a value text of 1,048,577 bytes',
    found({ value: 'a'.repeat(READ_BOUND + 1) }),
  ],
  [
    'a value text of 349,526 three-byte characters, 1,048,578 bytes',
    found({ value: '€'.repeat(349_526) }),
  ],
  ['an answer for another path than the one asked', found({ path: 'other' })],
  ['a data version that is not a safe integer', found({ data_version: 3.5 })],
  ['a negative data version', found({ data_version: -1 })],
  ['a data version given as text', found({ data_version: '3' })],
];
for (const [name, answer] of REFUSED_ANSWERS)
  test(`A data read is refused as unavailable for ${name}`, async () => {
    const { rpc, sent } = rpcWith({
      readRuntimeData: () => Promise.resolve(answer),
    });
    rpc.handle(dataRead());
    await until(() => sent.length === 1);
    const [{ message }] = sent;
    assert.equal(message.ok, false);
    assert.deepEqual(message, { id: 9, ok: false, code: 'unavailable' });
  });

const FAILED_READS = [
  ['a 403 from the server', new StatusError(403, 'forbidden'), 'denied'],
  ['a 404 from the server', new StatusError(404, 'not_found'), 'not_found'],
  [
    'a 409 revision conflict from the server',
    new StatusError(409, 'revision_conflict'),
    'stale_view',
  ],
  [
    'a 422 from the server',
    new StatusError(422, 'invalid_request'),
    'invalid_request',
  ],
  ['a 500 from the server', new StatusError(500, 'internal'), 'unavailable'],
  ['an error that is not an API error', new Error('SECRET'), 'unavailable'],
];
for (const [name, error, code] of FAILED_READS)
  test(`A failed data read is answered ${code} for ${name}, and nothing the server said is sent`, async () => {
    const { rpc, sent } = rpcWith({
      readRuntimeData: () => Promise.reject(error),
    });
    rpc.handle(dataRead());
    await until(() => sent.length === 1);
    const [{ message }] = sent;
    assert.equal(message.ok, false);
    assert.deepEqual(message, { id: 9, ok: false, code });
    assert.equal(JSON.stringify(sent).includes('SECRET'), false);
  });
