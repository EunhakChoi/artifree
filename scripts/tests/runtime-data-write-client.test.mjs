import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  ArtifreeApiError,
  ArtifreeClient,
} from '../../packages/client/dist/index.js';

/*
 * The application data writes of a runtime view: set, update and remove
 * (decision 0053, rules 4, 5, 13, 14 and 15). Each write is made through
 * ArtifreeClient.runtimeCall, the path the frame RPC takes
 * (apps/web/src/artifacts/frame-calls/data.ts), with the HTTP answer faked at
 * fetch. A write the client refuses must never reach fetch, so those refusals
 * also assert that no request was sent. The bounds and the request bodies are
 * written out here rather than taken from the client, so the test states the
 * rules' numbers on its own.
 */

const SESSION = 'e'.repeat(32);
const VIEW_ID = 'd'.repeat(32);
const INIT = 'c'.repeat(64);
const ARTIFACT = `art_${'0'.repeat(32)}`;
const OTHER_ARTIFACT = `art_${'1'.repeat(32)}`;
const VIEW_ROUTE = `/v1/artifacts/${ARTIFACT}/revisions/3/runtime-views/${VIEW_ID}/1`;
const ROUTE = `http://127.0.0.1:7410${VIEW_ROUTE}`;
const PATH = 'member';
const OTHER_PATH = 'members';
const BASE = 3;
const RETRY_ID = 'retry-0001';
const TEXT = '{"a":1}';
const WRITE_BYTES = 262_144;
const CONFLICT_VERSION = 7;

const view = {
  url: `http://127.0.0.1:7411/r/${'b'.repeat(64)}/`,
  enabled: true,
  session_id: SESSION,
  artifact_id: ARTIFACT,
  revision: 3,
  head_revision: 3,
  sdk: 'artifree.app.v1',
  declared: ['data.read', 'data.write'],
  supported: ['data.read', 'data.write'],
  granted: ['data.read', 'data.write'],
  grant_version: 1,
  view_id: VIEW_ID,
  generation: 1,
  initialization: INIT,
  expires_at_ms: 1_000_000,
  lease_ms: 900_000,
};

/*
 * The three writes. `input` is what a frame passes to the client, `wire` is the
 * body the client must send for it, and `url` is the route it must go to. A
 * write with JSON text names the field that carries the text.
 */
const SET = {
  kind: 'set',
  url: `${ROUTE}/data/set`,
  field: 'value',
  run: (source, frame, input) =>
    source.setRuntimeData(frame, view, input, undefined),
  input: { path: PATH, value: TEXT, baseDataVersion: BASE, retryId: RETRY_ID },
  wire: {
    initialization: INIT,
    path: PATH,
    value: TEXT,
    base_data_version: BASE,
    retry_id: RETRY_ID,
  },
};

const UPDATE = {
  kind: 'update',
  url: `${ROUTE}/data/update`,
  field: 'patch',
  run: (source, frame, input) =>
    source.updateRuntimeData(frame, view, input, undefined),
  input: { path: PATH, patch: TEXT, baseDataVersion: BASE, retryId: RETRY_ID },
  wire: {
    initialization: INIT,
    path: PATH,
    patch: TEXT,
    base_data_version: BASE,
    retry_id: RETRY_ID,
  },
};

const REMOVE = {
  kind: 'remove',
  url: `${ROUTE}/data/delete`,
  run: (source, frame, input) =>
    source.removeRuntimeData(frame, view, input, undefined),
  input: { path: PATH, baseDataVersion: BASE, retryId: RETRY_ID },
  wire: {
    initialization: INIT,
    path: PATH,
    base_data_version: BASE,
    retry_id: RETRY_ID,
  },
};

const WRITES = [SET, UPDATE, REMOVE];
const TEXT_WRITES = [SET, UPDATE];

/** A client whose frame calls are answered by `respond(url, body)`. */
async function clientWith(respond) {
  const requests = [];
  const client = new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:7410',
    accessToken: 'token',
    fetch: async (url, init) => {
      if (url.endsWith('/v1/runtime-sessions'))
        return Response.json({ session_id: SESSION });
      const body = init?.body === undefined ? undefined : JSON.parse(init.body);
      requests.push({ url, method: init?.method, body });
      return respond(url, body);
    },
  });
  await client.openRuntimeSession();
  return { client, requests };
}

/** The write of `input` through the client, as the frame RPC makes it. */
const writeThrough = (client, write, input) =>
  client.runtimeCall(view, (source, frame) => write.run(source, frame, input));

/** The input of `write` with `changes` applied over its valid input. */
const inputWith = (write, changes) => ({ ...write.input, ...changes });

/**
 * The error of a call that must be refused. A call that resolves fails here,
 * and the message names the code it was expected to be refused with.
 */
const refusalOf = (attempt, code) =>
  attempt.then(
    () => assert.fail(`Expected ${code}, but the call resolved.`),
    (rejection) => rejection,
  );

/** What the server states for a write of the base: a change, unless told. */
const receiptOf = (changes = {}) => ({
  artifact_id: ARTIFACT,
  path: PATH,
  data_version: BASE + 1,
  changed: true,
  replayed: false,
  ...changes,
});

/** A JSON answer with `status`, as the server gives one. */
const answer = (body, status = 200) => Response.json(body, { status });

/** The JSON text of an object whose one member is `text`: 8 bytes of framing. */
const objectText = (text) => JSON.stringify({ s: text });

// ---- writes that are sent ----

for (const write of WRITES)
  test(`A ${write.kind} is one POST to its route, with the view's secret, the path, the base, the retry identity and any text`, async () => {
    const { client, requests } = await clientWith(() => answer(receiptOf()));
    await writeThrough(client, write, write.input);
    assert.deepEqual(requests, [
      { url: write.url, method: 'POST', body: write.wire },
    ]);
  });

// ---- receipts that match the write ----

const matchingReceipts = [
  ['a change', receiptOf()],
  ['no change', receiptOf({ changed: false, data_version: BASE })],
  ['a replay of a change', receiptOf({ replayed: true })],
  [
    'a replay of no change',
    receiptOf({ changed: false, data_version: BASE, replayed: true }),
  ],
];

for (const write of WRITES)
  for (const [name, receipt] of matchingReceipts)
    test(`A ${write.kind} answered with ${name} resolves to that receipt`, async () => {
      const { client } = await clientWith(() => answer(receipt));
      assert.deepEqual(await writeThrough(client, write, write.input), receipt);
    });

// ---- inputs the client sends as given ----

for (const write of WRITES)
  test(`A ${write.kind} of the root path is sent, since the empty path is the root`, async () => {
    const { client, requests } = await clientWith(() =>
      answer(receiptOf({ path: '' })),
    );
    await writeThrough(client, write, inputWith(write, { path: '' }));
    assert.equal(requests.length, 1, 'The write was not sent once.');
    assert.equal(requests[0].body.path, '');
  });

const acceptedRetryIds = [
  ['8 characters', 'a'.repeat(8)],
  ['36 characters', 'a'.repeat(36)],
];

for (const write of WRITES)
  for (const [name, retryId] of acceptedRetryIds)
    test(`A ${write.kind} with a retry identity of ${name} is sent`, async () => {
      const { client, requests } = await clientWith(() => answer(receiptOf()));
      await writeThrough(client, write, inputWith(write, { retryId }));
      assert.equal(requests.length, 1, 'The write was not sent once.');
      assert.equal(requests[0].body.retry_id, retryId);
    });

for (const write of WRITES)
  test(`A ${write.kind} from the highest base version the client accepts is sent and moves to the safe-integer limit`, async () => {
    const { client, requests } = await clientWith(() =>
      answer(receiptOf({ data_version: 2 ** 53 - 1 })),
    );
    const receipt = await writeThrough(
      client,
      write,
      inputWith(write, { baseDataVersion: 2 ** 53 - 2 }),
    );
    assert.equal(receipt.data_version, 2 ** 53 - 1);
    assert.equal(requests.length, 1, 'The write was not sent once.');
    assert.equal(requests[0].body.base_data_version, 2 ** 53 - 2);
  });

// Any JSON text is a value or a patch, not only an object: null is a stored
// value (decision 0053, rule 4).
const acceptedTexts = [
  ['null', 'null'],
  ['an array', '[1,2]'],
];

for (const write of TEXT_WRITES)
  for (const [name, text] of acceptedTexts)
    test(`A ${write.kind} of the JSON text ${name} is sent`, async () => {
      const { client, requests } = await clientWith(() => answer(receiptOf()));
      await writeThrough(
        client,
        write,
        inputWith(write, { [write.field]: text }),
      );
      assert.equal(requests.length, 1, 'The write was not sent once.');
      assert.equal(requests[0].body[write.field], text);
    });

// The bound counts UTF-8 bytes, so both texts sit exactly on it. The second has
// about half as many UTF-16 units, so the byte count is what it reaches.
const boundaryTexts = [
  ['of ASCII', objectText('a'.repeat(WRITE_BYTES - 8))],
  ['of two-byte characters', objectText('é'.repeat((WRITE_BYTES - 8) / 2))],
];

for (const write of TEXT_WRITES)
  for (const [name, text] of boundaryTexts)
    test(`A ${write.kind} of exactly 262,144 bytes ${name} is sent`, async () => {
      assert.equal(Buffer.byteLength(text), WRITE_BYTES);
      const { client, requests } = await clientWith(() => answer(receiptOf()));
      await writeThrough(
        client,
        write,
        inputWith(write, { [write.field]: text }),
      );
      assert.equal(requests.length, 1, 'The write was not sent once.');
      // A boolean rather than deepEqual: a text that differs would print megabytes.
      assert.equal(
        requests[0].body[write.field] === text,
        true,
        'The text sent differs from the text given.',
      );
    });

// ---- inputs the client refuses before anything is sent ----

const badRetryIds = [
  ['a retry identity of 7 characters', 'a'.repeat(7)],
  ['a retry identity of 37 characters', 'a'.repeat(37)],
  ['a retry identity with a space', 'retry 0001'],
  ['a retry identity with a dot', 'retry.0001'],
  ['a retry identity with a letter outside ASCII', 'rétry-0001'],
];

const badBases = [
  ['a base version at the safe-integer limit', Number.MAX_SAFE_INTEGER],
  ['a base version past the safe-integer limit', 2 ** 53],
  ['a negative base version', -1],
  ['a fractional base version', 1.5],
  ['a base version given as text', '3'],
];

const badPaths = [
  ['a path that is a number', 7],
  ['a path that is missing', undefined],
];

const badTexts = [
  ['text that is not JSON ("{")', '{'],
  ['text that is not JSON ("nope")', 'nope'],
  ['empty text', ''],
  ['text of 262,145 bytes of ASCII', objectText('a'.repeat(WRITE_BYTES - 7))],
  // 131,077 UTF-16 units: only the byte count refuses it.
  [
    'text of 262,146 bytes of two-byte characters',
    objectText('é'.repeat(131_069)),
  ],
];

/** Declares a test that `write` with `changes` is refused before any send. */
function refusedBeforeSending(write, name, changes) {
  test(`A ${write.kind} with ${name} is refused before anything is sent`, async () => {
    const { client, requests } = await clientWith(() => answer(receiptOf()));
    const error = await refusalOf(
      writeThrough(client, write, inputWith(write, changes)),
      'invalid_request',
    );
    assert.ok(error instanceof ArtifreeApiError);
    assert.equal(error.code, 'invalid_request');
    assert.equal(error.status, 0);
    assert.equal(requests.length, 0, 'A request was sent.');
  });
}

for (const write of WRITES) {
  for (const [name, retryId] of badRetryIds)
    refusedBeforeSending(write, name, { retryId });
  for (const [name, base] of badBases)
    refusedBeforeSending(write, name, { baseDataVersion: base });
  for (const [name, path] of badPaths)
    refusedBeforeSending(write, name, { path });
}

for (const write of TEXT_WRITES)
  for (const [name, text] of badTexts)
    refusedBeforeSending(write, name, { [write.field]: text });

// ---- a receipt that does not match the write is refused ----

const corruptedReceipts = [
  [
    'a receipt that names another artifact',
    receiptOf({ artifact_id: OTHER_ARTIFACT }),
  ],
  ['a receipt that names another path', receiptOf({ path: OTHER_PATH })],
  [
    'a receipt whose version skips past base + 1',
    receiptOf({ data_version: BASE + 2 }),
  ],
  [
    'a receipt whose version stays at the base after a change',
    receiptOf({ data_version: BASE }),
  ],
  [
    'a receipt that reports no change but moves the version',
    receiptOf({ changed: false, data_version: BASE + 1 }),
  ],
  [
    'a replayed receipt whose version skips past base + 1',
    receiptOf({ replayed: true, data_version: BASE + 2 }),
  ],
  [
    'a replayed receipt that reports no change but moves the version',
    receiptOf({ replayed: true, changed: false, data_version: BASE + 1 }),
  ],
  [
    'a receipt whose changed flag is not a boolean',
    receiptOf({ changed: 'true' }),
  ],
  [
    'a receipt whose replayed flag is not a boolean',
    receiptOf({ replayed: 'false' }),
  ],
];

for (const write of WRITES)
  for (const [name, receipt] of corruptedReceipts)
    test(`${write.kind}: ${name} is refused`, async () => {
      const { client } = await clientWith(() => answer(receipt));
      const error = await refusalOf(
        writeThrough(client, write, write.input),
        'invalid_response',
      );
      assert.ok(error instanceof ArtifreeApiError);
      assert.equal(error.code, 'invalid_response');
      assert.equal(error.status, 200);
    });

// ---- a write is sent once: a failure is reported, never retried ----

const CONFLICT = {
  error: {
    code: 'data_conflict',
    message: 'the application data has moved on',
    current_data_version: CONFLICT_VERSION,
  },
};

const UNAVAILABLE = {
  error: { code: 'unavailable', message: 'the service is unavailable' },
};

const failedAnswers = [
  ['a 409 data_conflict', CONFLICT, 409, 'data_conflict'],
  ['a 503 unavailable', UNAVAILABLE, 503, 'unavailable'],
];

for (const write of WRITES)
  for (const [name, body, status, code] of failedAnswers)
    test(`A ${write.kind} answered with ${name} is sent once and not retried`, async () => {
      const { client, requests } = await clientWith(() => answer(body, status));
      const error = await refusalOf(
        writeThrough(client, write, write.input),
        code,
      );
      assert.ok(error instanceof ArtifreeApiError);
      assert.equal(error.status, status);
      assert.equal(error.code, code);
      assert.equal(requests.length, 1, 'The write was sent more than once.');
    });

for (const write of WRITES)
  test(`A ${write.kind} answered 409 data_conflict keeps the data version the server stated in its body`, async () => {
    const { client } = await clientWith(() => answer(CONFLICT, 409));
    const error = await refusalOf(
      writeThrough(client, write, write.input),
      'data_conflict',
    );
    assert.ok(error instanceof ArtifreeApiError);
    assert.equal(error.status, 409);
    assert.equal(error.code, 'data_conflict');
    assert.equal(error.currentDataVersion, CONFLICT_VERSION);
    assert.deepEqual(error.body, CONFLICT);
  });
