import assert from 'node:assert/strict';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The frame's rpc and its write budget are web code without DOM or React
// imports, so they are bundled as they ship (as runtime-data-host.test.mjs
// does). The runtime call is a fake: each test gives the source the answer the
// client would give, so what is judged here is the host's own reading of it.
const bundle = await build({
  stdin: {
    contents:
      "export * from './frame-rpc.ts';\nexport * from './frame-budget.ts';",
    resolveDir: fileURLToPath(
      new URL('../../apps/web/src/artifacts/', import.meta.url),
    ),
  },
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const {
  DATA_WRITE_BURST,
  DATA_WRITE_PER_MINUTE,
  FrameRpc,
  TokenBucket,
  parseCall,
} = await import(
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
);

const ARTIFACT = `art_${'0'.repeat(32)}`;
/** The frame transport that a source call is given. */
const FRAME = Object.freeze({ transport: 'frame' });
/** The most UTF-8 bytes a value or a patch may have: 256 KiB (rule 15). */
const WRITE_BOUND = 262_144;
/** The longest data path: 16 segments of 64 bytes and 15 separators. */
const LONGEST_PATH = 1_039;
/** A retry identity of the length and alphabet rule 15 gives. */
const RETRY_ID = 'retry_0001';
/** The detail of a failure, which must never reach the frame. */
const SECRET = 'SECRET detail from the server';

/** A view that holds the data.read and data.write grants. */
const view = {
  url: `http://127.0.0.1:7411/r/${'b'.repeat(64)}/`,
  enabled: true,
  session_id: 'e'.repeat(32),
  artifact_id: ARTIFACT,
  revision: 3,
  head_revision: 3,
  sdk: 'artifree.app.v1',
  lifecycle: 'restart',
  declared: ['data.read', 'data.write'],
  supported: ['data.read', 'data.write'],
  granted: ['data.read', 'data.write'],
  grant_version: 1,
  view_id: 'd'.repeat(32),
  generation: 1,
  initialization: 'c'.repeat(64),
  expires_at_ms: 1_000_000,
  lease_ms: 900_000,
};

/**
 * The three writes a frame makes (rule 4), each with the client method that
 * serves it and the fields it carries beyond its envelope, as the SDK sends
 * them. `text` names the field that carries JSON text (rule 5).
 */
const SET = {
  type: 'data.set',
  method: 'setRuntimeData',
  text: 'value',
  fields: {
    path: 'notes/a',
    value: '{"a":1}',
    baseDataVersion: 3,
    retryId: RETRY_ID,
  },
};
const UPDATE = {
  type: 'data.update',
  method: 'updateRuntimeData',
  text: 'patch',
  fields: {
    path: 'notes/a',
    patch: '{"b":2}',
    baseDataVersion: 3,
    retryId: RETRY_ID,
  },
};
const REMOVE = {
  type: 'data.remove',
  method: 'removeRuntimeData',
  fields: { path: 'notes/a', baseDataVersion: 3, retryId: RETRY_ID },
};
const WRITES = [SET, UPDATE, REMOVE];
const TEXT_WRITES = [SET, UPDATE];

/** A write of `write`'s type with some fields replaced; its id is 9 unless given. */
function writeOf(write, changes = {}) {
  return { type: write.type, id: 9, ...write.fields, ...changes };
}

/** A copy of `message` without `key`. */
function without(message, key) {
  const copy = { ...message };
  delete copy[key];
  return copy;
}

/** What a source answers for a read of a path that holds a value. */
function found() {
  return {
    artifact_id: ARTIFACT,
    path: 'notes',
    data_version: 3,
    exists: true,
    value: '{"a":1}',
  };
}

/** The receipt the client gives for a write to `input.path`: a change. */
function receiptOf(input, changes = {}) {
  return {
    artifact_id: ARTIFACT,
    path: input.path,
    data_version: input.baseDataVersion + 1,
    changed: true,
    replayed: false,
    ...changes,
  };
}

/** The answer of a source that counts every write as a change. */
async function acceptsChange(input) {
  return receiptOf(input);
}

/**
 * A failed call as the client throws it: an ArtifreeApiError whose body is the
 * server's answer, which the client keeps whole (client.ts, parseError).
 */
function failure(status, code, body) {
  return async () => {
    throw Object.assign(new Error(SECRET), {
      name: 'ArtifreeApiError',
      status,
      code,
      body,
    });
  };
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
 * An rpc wired as runtime-host.ts wires it: the frame's own budget never refuses
 * here, and the data write budget is rule 23's, a token bucket over the clock
 * that the test moves. The source's write methods record each call and answer
 * with `answer(input)`; a read is answered with `found()`.
 */
function harness({ answer = acceptsChange, clock = { ms: 0 } } = {}) {
  const sent = [];
  const runtimeCalls = [];
  const sourceCalls = [];
  const writeMethod = (method) => (frame, callView, input, signal) => {
    sourceCalls.push({ method, frame, callView, input, signal });
    return answer(input);
  };
  const source = {
    readRuntimeData: () => Promise.resolve(found()),
    setRuntimeData: writeMethod('setRuntimeData'),
    updateRuntimeData: writeMethod('updateRuntimeData'),
    removeRuntimeData: writeMethod('removeRuntimeData'),
  };
  const client = {
    runtimeCall(callView, run) {
      runtimeCalls.push(callView);
      return run(source, FRAME);
    },
  };
  const writes = new TokenBucket(
    () => clock.ms,
    DATA_WRITE_BURST,
    DATA_WRITE_PER_MINUTE,
  );
  const rpc = new FrameRpc({
    client,
    view,
    ask: () => assert.fail('A data write never prompts.'),
    save: () => assert.fail('A data write never saves.'),
    take: () => true,
    takeDataWrite: () => writes.take(),
    now: () => clock.ms,
    deadline: 900_000,
    send: (message, transfer) => sent.push({ message, transfer }),
  });
  return { rpc, sent, runtimeCalls, sourceCalls };
}

// ---- parsing ----

for (const write of WRITES)
  test(`parseCall accepts a ${write.type} exactly as the SDK sends it`, () => {
    const message = writeOf(write);
    assert.deepEqual(parseCall(message), message);
  });

// The host never parses a value or a patch (decision 0053, rule 5): a text is
// refused for its size and its type, never for what its JSON says.
for (const write of TEXT_WRITES)
  test(`parseCall accepts a ${write.type} whose ${write.text} is not JSON, since the host never parses it`, () => {
    const message = writeOf(write, { [write.text]: 'not json' });
    assert.deepEqual(parseCall(message), message);
  });

const ACCEPTED_PATHS = [
  ['the root path, the empty string', ''],
  ['the longest path, 1,039 characters', 'a'.repeat(LONGEST_PATH)],
];
for (const write of WRITES)
  for (const [name, path] of ACCEPTED_PATHS)
    test(`parseCall accepts a ${write.type} to ${name}`, () => {
      const message = writeOf(write, { path });
      assert.deepEqual(parseCall(message), message);
    });

const REFUSED_PATHS = [
  ['a path that is a number', 3],
  ['a path that is null', null],
  ['a path one character over the bound', 'a'.repeat(LONGEST_PATH + 1)],
];
for (const write of WRITES)
  for (const [name, path] of REFUSED_PATHS)
    test(`parseCall refuses a ${write.type} with ${name}`, () => {
      assert.equal(parseCall(writeOf(write, { path })), undefined);
    });

for (const write of WRITES)
  for (const key of Object.keys(writeOf(write)))
    test(`parseCall refuses a ${write.type} without its ${key}`, () => {
      assert.equal(parseCall(without(writeOf(write), key)), undefined);
    });

for (const write of WRITES)
  test(`parseCall refuses a ${write.type} with an extra artifact_id`, () => {
    const message = writeOf(write, { artifact_id: ARTIFACT });
    assert.equal(parseCall(message), undefined);
  });

const CROSSED_KEYS = [
  [SET, 'patch', '{"b":2}'],
  [UPDATE, 'value', '{"a":1}'],
  [REMOVE, 'value', '{"a":1}'],
  [REMOVE, 'patch', '{"b":2}'],
];
for (const [write, key, text] of CROSSED_KEYS)
  test(`parseCall refuses a ${write.type} that also carries a ${key}`, () => {
    assert.equal(parseCall(writeOf(write, { [key]: text })), undefined);
  });

const NOT_TEXT = [
  ['a number', 3],
  ['null', null],
  ['an object', { a: 1 }],
  ['an array', ['1']],
];
for (const write of TEXT_WRITES)
  for (const [name, text] of NOT_TEXT)
    test(`parseCall refuses a ${write.type} whose ${write.text} is ${name}`, () => {
      const message = writeOf(write, { [write.text]: text });
      assert.equal(parseCall(message), undefined);
    });

// The bound is in UTF-8 bytes. A three-byte character is one UTF-16 unit, so
// 87,382 of them are short in units and over the bound in bytes.
const AT_BOUND = [
  ['ASCII text of exactly 262,144 bytes', 'a'.repeat(WRITE_BOUND)],
  ['three-byte text of exactly 262,144 bytes', `${'€'.repeat(87_381)}a`],
];
for (const write of TEXT_WRITES)
  for (const [name, text] of AT_BOUND)
    test(`parseCall accepts a ${write.type} whose ${write.text} is ${name}`, () => {
      const call = parseCall(writeOf(write, { [write.text]: text }));
      // Compared without printing the text: a mismatch would print 256 KiB.
      assert.ok(
        call !== undefined && call[write.text] === text,
        'the text is passed on whole',
      );
    });

const OVER_BOUND = [
  ['ASCII text one byte over, 262,145 bytes', 'a'.repeat(WRITE_BOUND + 1)],
  ['three-byte text one byte over, 262,145 bytes', `${'€'.repeat(87_381)}ab`],
  ['three-byte text of 262,146 bytes in 87,382 characters', '€'.repeat(87_382)],
];
for (const write of TEXT_WRITES)
  for (const [name, text] of OVER_BOUND)
    test(`parseCall refuses a ${write.type} whose ${write.text} is ${name}`, () => {
      // Compared without printing the text: a mismatch would print 256 KiB.
      assert.ok(
        parseCall(writeOf(write, { [write.text]: text })) === undefined,
        'the text is over the bound',
      );
    });

const ACCEPTED_RETRY_IDS = [
  ['a retry identity of eight characters', 'r'.repeat(8)],
  ['a retry identity of thirty-six characters', 'r'.repeat(36)],
];
for (const write of WRITES)
  for (const [name, retryId] of ACCEPTED_RETRY_IDS)
    test(`parseCall accepts a ${write.type} with ${name}`, () => {
      const message = writeOf(write, { retryId });
      assert.deepEqual(parseCall(message), message);
    });

const REFUSED_RETRY_IDS = [
  ['a retry identity of seven characters', 'r'.repeat(7)],
  ['a retry identity of thirty-seven characters', 'r'.repeat(37)],
  ['a retry identity with a space', 'retry 0001'],
  ['a retry identity with a dot', 'retry.0001'],
  ['a retry identity with a letter outside ASCII', 'retry_é001'],
  ['a retry identity that is a number', 12345678],
];
for (const write of WRITES)
  for (const [name, retryId] of REFUSED_RETRY_IDS)
    test(`parseCall refuses a ${write.type} with ${name}`, () => {
      assert.equal(parseCall(writeOf(write, { retryId })), undefined);
    });

const ACCEPTED_BASES = [
  ['a base of zero', 0],
  [
    'the largest base whose next version is still safe',
    Number.MAX_SAFE_INTEGER - 1,
  ],
];
for (const write of WRITES)
  for (const [name, baseDataVersion] of ACCEPTED_BASES)
    test(`parseCall accepts a ${write.type} with ${name}`, () => {
      const message = writeOf(write, { baseDataVersion });
      assert.deepEqual(parseCall(message), message);
    });

const REFUSED_BASES = [
  ['a negative base', -1],
  ['a base that is not whole', 1.5],
  ['a base given as text', '3'],
  ['a base that is NaN', Number.NaN],
  [
    'the largest safe integer, whose next version is not safe',
    Number.MAX_SAFE_INTEGER,
  ],
  ['an integer past the safe range', Number.MAX_SAFE_INTEGER + 1],
];
for (const write of WRITES)
  for (const [name, baseDataVersion] of REFUSED_BASES)
    test(`parseCall refuses a ${write.type} with ${name}`, () => {
      assert.equal(parseCall(writeOf(write, { baseDataVersion })), undefined);
    });

// ---- the rpc ----

test('a malformed write is answered invalid_request at once, holds no slot and reaches no client', async () => {
  const { rpc, sent, runtimeCalls } = harness();
  const malformed = [
    writeOf(SET, { id: 1, value: 3 }),
    writeOf(UPDATE, { id: 2, retryId: 'short' }),
    writeOf(REMOVE, { id: 3, value: '{"a":1}' }),
    writeOf(SET, { id: 4, baseDataVersion: -1 }),
    writeOf(REMOVE, { id: 5, path: 3 }),
  ];
  for (const message of malformed) rpc.handle(message);
  await settle();
  assert.deepEqual(
    sent.map(({ message }) => message),
    malformed.map(({ id }) => ({ id, ok: false, code: 'invalid_request' })),
  );
  assert.equal(rpc.pending, 0, 'a refused call holds no slot');
  assert.deepEqual(runtimeCalls, [], 'nothing reached the client');
});

for (const write of WRITES)
  test(`A ${write.type} is sent to the client's ${write.method} with the frame, the view and its fields`, async () => {
    const { rpc, sent, runtimeCalls, sourceCalls } = harness();
    rpc.handle(writeOf(write));
    await until(() => sent.length === 1);
    assert.deepEqual(runtimeCalls, [view]);
    assert.equal(sourceCalls.length, 1);
    const [call] = sourceCalls;
    assert.equal(call.method, write.method);
    assert.equal(call.frame, FRAME);
    assert.equal(call.callView, view);
    assert.deepEqual(call.input, write.fields);
    assert.ok(call.signal instanceof AbortSignal);
  });

/**
 * The two outcomes a write can have from base version 3: the receipt the
 * client gives for each, and the reply it becomes (rule 13).
 */
const OUTCOMES = [
  {
    name: 'a change, which moves the version on by one',
    receipt: { data_version: 4, changed: true },
    written: { dataVersion: 4, changed: true },
  },
  {
    name: 'no change, which keeps the version',
    receipt: { data_version: 3, changed: false },
    written: { dataVersion: 3, changed: false },
  },
];
for (const write of WRITES)
  for (const outcome of OUTCOMES)
    test(`A ${write.type} with ${outcome.name} is answered with its receipt`, async () => {
      const { rpc, sent } = harness({
        answer: async () => ({
          artifact_id: ARTIFACT,
          path: 'notes/a',
          replayed: false,
          ...outcome.receipt,
        }),
      });
      rpc.handle(writeOf(write));
      await until(() => sent.length === 1);
      assert.deepEqual(sent, [
        {
          message: {
            id: 9,
            ok: true,
            written: { path: 'notes/a', ...outcome.written },
          },
          transfer: [],
        },
      ]);
    });

// The host does not parse a value or a patch (decision 0053, rule 5): the text
// reaches the client as the frame sent it.
const UNPARSED_TEXTS = [
  ['a value that is not JSON', 'not json'],
  ['JSON text with spacing and 1.0 as written', '{ "a" : 1.0 }'],
];
for (const write of TEXT_WRITES)
  for (const [name, text] of UNPARSED_TEXTS)
    test(`the host passes ${name} to the client unchanged: ${write.type}`, async () => {
      const { rpc, sent, sourceCalls } = harness();
      rpc.handle(writeOf(write, { [write.text]: text }));
      await until(() => sent.length === 1);
      assert.equal(sourceCalls.length, 1, 'the client is asked');
      assert.equal(sourceCalls[0].input[write.text], text);
      assert.deepEqual(sent, [
        {
          message: {
            id: 9,
            ok: true,
            written: { path: 'notes/a', dataVersion: 4, changed: true },
          },
          transfer: [],
        },
      ]);
    });

for (const write of WRITES)
  test(`ten writes in a burst are each answered ok: ${write.type}`, async () => {
    const { rpc, sent, runtimeCalls } = harness();
    for (let id = 1; id <= 10; id += 1) rpc.handle(writeOf(write, { id }));
    await until(() => sent.length === 10);
    assert.deepEqual(
      sent.map(({ message }) => message.ok),
      Array(10).fill(true),
    );
    assert.equal(runtimeCalls.length, 10);
  });

for (const write of WRITES)
  test(`the eleventh write in a burst is rate_limited without a client call: ${write.type}`, async () => {
    const { rpc, sent, runtimeCalls } = harness();
    for (let id = 1; id <= 10; id += 1) rpc.handle(writeOf(write, { id }));
    await until(() => sent.length === 10);
    rpc.handle(writeOf(write, { id: 11 }));
    await until(() => sent.length === 11);
    assert.deepEqual(sent[10].message, {
      id: 11,
      ok: false,
      code: 'rate_limited',
    });
    assert.equal(
      runtimeCalls.length,
      10,
      'the eleventh write reached no client',
    );
  });

for (const write of WRITES)
  test(`one token returns 1,000 ms after the burst is spent: ${write.type}`, async () => {
    const clock = { ms: 0 };
    const { rpc, sent } = harness({ clock });
    for (let id = 1; id <= 10; id += 1) rpc.handle(writeOf(write, { id }));
    await until(() => sent.length === 10);
    clock.ms = 999;
    rpc.handle(writeOf(write, { id: 11 }));
    await until(() => sent.length === 11);
    assert.deepEqual(
      sent[10].message,
      { id: 11, ok: false, code: 'rate_limited' },
      'no token has returned at 999 ms',
    );
    clock.ms = 1_000;
    rpc.handle(writeOf(write, { id: 12 }));
    await until(() => sent.length === 12);
    assert.deepEqual(
      sent[11].message,
      {
        id: 12,
        ok: true,
        written: { path: 'notes/a', dataVersion: 4, changed: true },
      },
      'one token has returned at 1,000 ms',
    );
  });

test('the three write kinds draw on one bucket: ten mixed writes pass and the eleventh of each kind is refused', async () => {
  const { rpc, sent, runtimeCalls } = harness();
  const mixed = [
    SET,
    UPDATE,
    REMOVE,
    SET,
    UPDATE,
    REMOVE,
    SET,
    UPDATE,
    REMOVE,
    SET,
  ];
  for (const [index, write] of mixed.entries())
    rpc.handle(writeOf(write, { id: index + 1 }));
  await until(() => sent.length === 10);
  assert.deepEqual(
    sent.map(({ message }) => message.ok),
    Array(10).fill(true),
  );
  for (const [index, write] of [SET, UPDATE, REMOVE].entries())
    rpc.handle(writeOf(write, { id: 11 + index }));
  await until(() => sent.length === 13);
  assert.deepEqual(
    sent.slice(10).map(({ message }) => message),
    [
      { id: 11, ok: false, code: 'rate_limited' },
      { id: 12, ok: false, code: 'rate_limited' },
      { id: 13, ok: false, code: 'rate_limited' },
    ],
  );
  assert.equal(runtimeCalls.length, 10, 'the refused writes reached no client');
});

test('a data read does not spend the write budget', async () => {
  const { rpc, sent, runtimeCalls } = harness();
  for (let id = 1; id <= 10; id += 1)
    rpc.handle({ type: 'data.get', id, path: 'notes' });
  await until(() => sent.length === 10);
  for (let id = 11; id <= 20; id += 1) rpc.handle(writeOf(SET, { id }));
  await until(() => sent.length === 20);
  rpc.handle(writeOf(SET, { id: 21 }));
  await until(() => sent.length === 21);
  assert.deepEqual(
    sent.slice(0, 10).map(({ message }) => message.ok),
    Array(10).fill(true),
    'the ten reads are answered',
  );
  assert.deepEqual(
    sent.slice(10, 20).map(({ message }) => message.ok),
    Array(10).fill(true),
    'ten writes after ten reads all pass',
  );
  assert.deepEqual(sent[20].message, {
    id: 21,
    ok: false,
    code: 'rate_limited',
  });
  assert.equal(runtimeCalls.length, 20, 'the refused write reached no client');
});

test('DATA_WRITE_BURST is 10 and DATA_WRITE_PER_MINUTE is 60 (decision 0053, rule 23)', () => {
  assert.equal(DATA_WRITE_BURST, 10);
  assert.equal(DATA_WRITE_PER_MINUTE, 60);
});

/** Receipts that break rule 13, each one wrong in a different way. */
const WRONG_RECEIPTS = [
  [
    'a receipt for another path',
    (input) => receiptOf(input, { path: 'other' }),
  ],
  [
    'a change whose version did not move on',
    (input) => receiptOf(input, { data_version: input.baseDataVersion }),
  ],
  [
    'a change whose version moved on by two',
    (input) => receiptOf(input, { data_version: input.baseDataVersion + 2 }),
  ],
  [
    'no change whose version moved on',
    (input) => receiptOf(input, { changed: false }),
  ],
  [
    'a version that is not a count',
    (input) =>
      receiptOf(input, { data_version: String(input.baseDataVersion + 1) }),
  ],
];
for (const write of WRITES)
  for (const [name, receipt] of WRONG_RECEIPTS)
    test(`A write answered with ${name} is unavailable: ${write.type}`, async () => {
      const { rpc, sent } = harness({
        answer: async (input) => receipt(input),
      });
      rpc.handle(writeOf(write));
      await until(() => sent.length === 1);
      assert.deepEqual(sent, [
        { message: { id: 9, ok: false, code: 'unavailable' }, transfer: [] },
      ]);
    });

/** Failures of a write and the reply each one becomes (decision 0053, rule 14). */
const FAILED_WRITES = [
  [
    'a 409 data_conflict that names the version the server holds',
    failure(409, 'data_conflict', {
      error: {
        code: 'data_conflict',
        message: SECRET,
        current_data_version: 7,
      },
    }),
    { id: 9, ok: false, code: 'conflict', currentDataVersion: 7 },
  ],
  [
    'a 409 data_path_conflict',
    failure(409, 'data_path_conflict', {
      error: { code: 'data_path_conflict', message: SECRET },
    }),
    { id: 9, ok: false, code: 'invalid_request' },
  ],
  [
    'a 409 data_conflict that names no version',
    failure(409, 'data_conflict', {
      error: { code: 'data_conflict', message: SECRET },
    }),
    { id: 9, ok: false, code: 'unavailable' },
  ],
  [
    'a 422 from the server',
    failure(422, 'invalid_request', {
      error: { code: 'invalid_request', message: SECRET },
    }),
    { id: 9, ok: false, code: 'invalid_request' },
  ],
];
for (const write of WRITES)
  for (const [name, answer, reply] of FAILED_WRITES)
    test(`A write refused by ${name} is answered ${reply.code}, and nothing the server said is sent: ${write.type}`, async () => {
      const { rpc, sent } = harness({ answer });
      rpc.handle(writeOf(write));
      await until(() => sent.length === 1);
      assert.deepEqual(sent, [{ message: reply, transfer: [] }]);
      assert.equal(JSON.stringify(sent).includes('SECRET'), false);
    });
