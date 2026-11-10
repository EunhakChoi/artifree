import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  ArtifreeApiError,
  ArtifreeClient,
} from '../../packages/client/dist/index.js';

/*
 * The application data read of a runtime view (decision 0053, rules 4, 6, 11
 * and 13). The read is tested through ArtifreeClient.runtimeCall, the path the
 * frame RPC takes (apps/web/src/artifacts/frame-calls/data.ts), with the HTTP
 * answer faked at fetch. The bound is written out here rather than imported
 * from the client, so the test states the rule's number on its own.
 */

const SESSION = 'e'.repeat(32);
const VIEW_ID = 'd'.repeat(32);
const INIT = 'c'.repeat(64);
const ARTIFACT = `art_${'0'.repeat(32)}`;
const OTHER_ARTIFACT = `art_${'1'.repeat(32)}`;
const VIEW_ROUTE = `/v1/artifacts/${ARTIFACT}/revisions/3/runtime-views/${VIEW_ID}/1`;
const DATA_URL = `http://127.0.0.1:7410${VIEW_ROUTE}/data/read`;
const ONE_MIB = 1_048_576;

const view = {
  url: `http://127.0.0.1:7411/r/${'b'.repeat(64)}/`,
  enabled: true,
  session_id: SESSION,
  artifact_id: ARTIFACT,
  revision: 3,
  head_revision: 3,
  sdk: 'artifree.app.v1',
  declared: ['data.read'],
  supported: ['data.read'],
  granted: ['data.read'],
  grant_version: 1,
  view_id: VIEW_ID,
  generation: 1,
  initialization: INIT,
  expires_at_ms: 1_000_000,
  lease_ms: 900_000,
};

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

/** The data read of `path` through the client, as the frame RPC makes it. */
const readData = (client, path) =>
  client.runtimeCall(view, (source, frame) =>
    source.readRuntimeData(frame, view, path, undefined),
  );

/**
 * The error of a call that must be refused. A call that resolves fails here,
 * and the message names the code it was expected to be refused with.
 */
const refusalOf = (attempt, code) =>
  attempt.then(
    () => assert.fail(`Expected ${code}, but the call resolved.`),
    (rejection) => rejection,
  );

/** What the server answers for `path` when a value is stored there. */
const stored = (path, text) => ({
  artifact_id: ARTIFACT,
  path,
  data_version: 4,
  exists: true,
  value: text,
});

/** What the server answers for `path` when nothing is stored there. */
const absent = (path) => ({
  artifact_id: ARTIFACT,
  path,
  data_version: 4,
  exists: false,
});

/** The JSON text of an object whose one member is `text`: 8 bytes of framing. */
const objectText = (text) => JSON.stringify({ s: text });

// ---- the request ----

const requestedPaths = [
  ['the root', ''],
  ['a member', 'member'],
  ['a member of a member', 'parent/child'],
];

for (const [name, path] of requestedPaths)
  test(`A read of ${name} is one POST to the data route, with the view's secret and the path`, async () => {
    const { client, requests } = await clientWith(() =>
      Response.json(stored(path, '{}')),
    );
    await readData(client, path);
    assert.deepEqual(requests, [
      { url: DATA_URL, method: 'POST', body: { initialization: INIT, path } },
    ]);
  });

// ---- answers that are accepted ----

const acceptedAnswers = [
  ['JSON text of an object', 'member', stored('member', '{"a":1}')],
  ['the text null, which is a stored null', 'member', stored('member', 'null')],
  ['nothing at the path, and so no value', 'member', absent('member')],
  [
    'nothing at the path, in a space nobody wrote',
    'member',
    {
      ...absent('member'),
      data_version: 0,
    },
  ],
  [
    'the largest safe data version',
    'member',
    {
      ...stored('member', '{}'),
      data_version: 2 ** 53 - 1,
    },
  ],
  ['the root as an empty object', '', stored('', '{}')],
];

for (const [name, path, answer] of acceptedAnswers)
  test(`A read accepts an answer with ${name}`, async () => {
    const { client } = await clientWith(() => Response.json(answer));
    assert.deepEqual(await readData(client, path), answer);
  });

test('A read accepts a value of exactly 1,048,576 bytes of JSON text', async () => {
  const text = objectText('a'.repeat(ONE_MIB - 8));
  assert.equal(Buffer.byteLength(text), ONE_MIB);
  const { client } = await clientWith(() =>
    Response.json(stored('member', text)),
  );
  const answer = await readData(client, 'member');
  assert.equal(answer.path, 'member');
  // A boolean rather than deepEqual: a text that differs would print megabytes.
  assert.equal(
    answer.value === text,
    true,
    'The stored text came back changed.',
  );
});

// ---- answers that are refused ----

// One leading space, then 524,284 characters of two bytes each (U+00E9), with
// the 8 bytes of framing, make 1,048,577 bytes. The text has 524,293 UTF-16
// units, so only the byte bound refuses it.
const twoByteText = ` ${objectText('é'.repeat(524_284))}`;

const refusedAnswers = [
  [
    'another artifact',
    'member',
    { ...stored('member', '{"a":1}'), artifact_id: OTHER_ARTIFACT },
  ],
  [
    'another path',
    'member',
    { ...stored('member', '{"a":1}'), path: 'members' },
  ],
  [
    'a data version of -1',
    'member',
    { ...stored('member', '{}'), data_version: -1 },
  ],
  [
    'a data version of 1.5',
    'member',
    { ...stored('member', '{}'), data_version: 1.5 },
  ],
  [
    'a data version of 2^53',
    'member',
    { ...stored('member', '{}'), data_version: 2 ** 53 },
  ],
  [
    'an exists flag that is the text yes',
    'member',
    { ...stored('member', '{}'), exists: 'yes' },
  ],
  ['exists true and no value', 'member', { ...absent('member'), exists: true }],
  [
    'exists true and a null value',
    'member',
    { ...stored('member', '{}'), value: null },
  ],
  [
    'exists false and a value',
    'member',
    { ...absent('member'), value: '{"a":1}' },
  ],
  [
    'exists false and a null value',
    'member',
    { ...absent('member'), value: null },
  ],
  ['the root reported as absent', '', absent('')],
  ['a root whose text is an array', '', stored('', '[]')],
  [
    'a value that is an object, not text',
    'member',
    { ...stored('member', '{}'), value: { a: 1 } },
  ],
  ["a value that is not JSON ('{')", 'member', stored('member', '{')],
  [
    '1,048,577 bytes of two-byte characters',
    'member',
    stored('member', twoByteText),
  ],
  [
    '1,048,577 bytes of ASCII characters',
    'member',
    stored('member', objectText('a'.repeat(ONE_MIB - 7))),
  ],
];

for (const [name, path, answer] of refusedAnswers)
  test(`A read whose answer has ${name} is refused with invalid_response`, async () => {
    const { client } = await clientWith(() => Response.json(answer));
    const error = await refusalOf(readData(client, path), 'invalid_response');
    assert.ok(error instanceof ArtifreeApiError);
    assert.equal(error.code, 'invalid_response');
    assert.equal(error.status, 200);
  });
