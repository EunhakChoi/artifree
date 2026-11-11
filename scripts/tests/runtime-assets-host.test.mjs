import assert from 'node:assert/strict';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';
import { ArtifreeClient } from '../../packages/client/dist/index.js';

// The frame's rpc is web code without DOM or React imports; bundle it as it
// ships. The client is a fake at the runtime-call boundary, or the real client
// over a fake fetch where the wire matters (runtime-host.test.mjs does the same
// for the rest of the workspace host).
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

const SESSION = 'e'.repeat(32);
/** An asset ID as the SDK sends it: 64 lowercase hexadecimal digits. */
const ASSET = '0123456789abcdef'.repeat(4);
const DIGEST = 'f'.repeat(64);
/** The frame transport that a source call is given. */
const FRAME = Object.freeze({ transport: 'frame' });

/** A view that holds the assets.read grant. */
const view = {
  url: `http://127.0.0.1:7411/r/${'b'.repeat(64)}/`,
  enabled: true,
  session_id: SESSION,
  artifact_id: `art_${'0'.repeat(32)}`,
  revision: 3,
  head_revision: 3,
  sdk: 'artifree.app.v1',
  lifecycle: 'restart',
  declared: ['assets.read'],
  supported: ['assets.read'],
  granted: ['assets.read'],
  grant_version: 1,
  view_id: 'd'.repeat(32),
  generation: 1,
  initialization: 'c'.repeat(64),
  expires_at_ms: 1_000_000,
  lease_ms: 900_000,
};
/** The same view before the user granted anything. */
const ungranted = { ...view, granted: [], grant_version: 0 };

/** What a source answers for a listing: the client's wire shape. */
const LISTING = {
  asset_version: 3,
  assets: [
    {
      id: ASSET,
      content_type: 'text/plain',
      size_bytes: 5,
      sha256: DIGEST,
      created_at_ms: 1_700_000_000_000,
      // Not part of the reply: the frame gets only the fields it is given.
      internal_ref: 'NOT FOR THE FRAME',
    },
  ],
  bytes: 5,
  max_assets: 64,
  max_bytes: 5_242_880,
};

/** What a source answers for a part: the client's camel-case part. */
function part(overrides = {}) {
  return {
    assetId: ASSET,
    contentType: 'text/plain',
    offset: 0,
    nextOffset: null,
    size: 3,
    sha256: DIGEST,
    bytes: Uint8Array.from([104, 105, 33]),
    ...overrides,
  };
}

/** An asset read with some fields replaced; otherwise the SDK's shape. */
function read(changes = {}) {
  return {
    type: 'assets.read',
    id: 9,
    assetId: ASSET,
    offset: 0,
    length: 1,
    ...changes,
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

function deferred() {
  let resolve;
  const promise = new Promise((ok) => {
    resolve = ok;
  });
  return { promise, resolve };
}

/** Waits, a turn at a time, for a condition made by asynchronous work. */
async function until(condition) {
  const deadline = Date.now() + 10_000;
  while (!condition()) {
    if (Date.now() > deadline) assert.fail('The condition never became true.');
    await new Promise((resolve) => setImmediate(resolve));
  }
}

async function settle() {
  for (let round = 0; round < 20; round += 1) await Promise.resolve();
}

/**
 * A rpc whose runtime calls are the test's: `runtimeCall` hands the callback
 * the source the test gives and the frame transport, and records each view it
 * was asked for. `realClient` replaces that fake with a real client.
 */
function rpcWith(source, overrides = {}) {
  const sent = [];
  const runtimeCalls = [];
  const fake = {
    runtimeCall(callView, run) {
      runtimeCalls.push(callView);
      return run(source, FRAME);
    },
  };
  const rpc = new FrameRpc({
    client: overrides.realClient ?? fake,
    view: overrides.view ?? view,
    ask: () => assert.fail('An asset call never prompts.'),
    save: () => assert.fail('An asset call never saves.'),
    take: () => true,
    now: () => 1000,
    deadline: 1000 + 900_000,
    send: (message, transfer) => sent.push({ message, transfer }),
  });
  return { rpc, sent, runtimeCalls };
}

// ---- parsing ----

test('parseCall accepts the asset calls exactly as the SDK sends them', () => {
  assert.deepEqual(parseCall({ type: 'assets.list', id: 1 }), {
    type: 'assets.list',
    id: 1,
  });
  // The SDK asks for the largest part; a later part starts further on.
  assert.deepEqual(
    parseCall({
      type: 'assets.read',
      id: 2,
      assetId: ASSET,
      offset: 524_288,
      length: 262_144,
    }),
    {
      type: 'assets.read',
      id: 2,
      assetId: ASSET,
      offset: 524_288,
      length: 262_144,
    },
  );
});

test('parseCall accepts the first byte, the last safe offset, and both length bounds', () => {
  const first = read({ offset: 0, length: 1 });
  const last = read({ offset: Number.MAX_SAFE_INTEGER, length: 262_144 });
  assert.deepEqual(parseCall(first), first);
  assert.deepEqual(parseCall(last), last);
});

const BAD_ASSET_IDS = [
  ['uppercase hexadecimal digits', ASSET.toUpperCase()],
  ['one digit too few', ASSET.slice(1)],
  ['one digit too many', `${ASSET}0`],
  ['a letter that is not hexadecimal', `g${ASSET.slice(1)}`],
  ['a leading space', ` ${ASSET}`],
  ['a trailing newline', `${ASSET}\n`],
  ['the empty string', ''],
  ['a file path', 'a.txt'],
  ['a number', 42],
  ['an array holding the ID', [ASSET]],
];
for (const [name, assetId] of BAD_ASSET_IDS)
  test(`parseCall refuses an asset read whose asset ID is ${name}`, () => {
    assert.equal(parseCall(read({ assetId })), undefined);
  });

const BAD_OFFSETS = [
  ['a negative number', -1],
  ['a fraction', 0.5],
  ['the first unsafe integer', 2 ** 53],
  ['infinity', Number.POSITIVE_INFINITY],
  ['not a number', Number.NaN],
  ['a string', '0'],
  ['null', null],
];
for (const [name, offset] of BAD_OFFSETS)
  test(`parseCall refuses an asset read whose offset is ${name}`, () => {
    assert.equal(parseCall(read({ offset })), undefined);
  });

const BAD_LENGTHS = [
  ['zero', 0],
  ['one byte over the part bound', 262_145],
  ['one mebibyte, the bound of a source part', 1_048_576],
  ['a negative number', -1],
  ['a fraction', 1.5],
  ['a string', '1'],
  ['null', null],
];
for (const [name, length] of BAD_LENGTHS)
  test(`parseCall refuses an asset read whose length is ${name}`, () => {
    assert.equal(parseCall(read({ length })), undefined);
  });

const BAD_CALL_IDS = [
  ['zero', 0],
  ['a negative number', -1],
  ['a fraction', 1.5],
  ['a string', '1'],
  ['the first unsafe integer', 2 ** 53],
];
for (const [name, id] of BAD_CALL_IDS)
  test(`parseCall refuses an asset list whose call ID is ${name}`, () => {
    assert.equal(parseCall({ type: 'assets.list', id }), undefined);
  });

const MALFORMED_CALLS = [
  [
    'an asset ID on assets.list',
    { type: 'assets.list', id: 9, assetId: ASSET },
  ],
  ['a path on assets.list', { type: 'assets.list', id: 9, path: 'a.txt' }],
  ['a path on assets.read', read({ path: 'a.txt' })],
  [
    'no length on assets.read',
    { type: 'assets.read', id: 9, assetId: ASSET, offset: 0 },
  ],
  [
    'no asset ID on assets.read',
    { type: 'assets.read', id: 9, offset: 0, length: 1 },
  ],
  [
    'the asset ID under the name asset_id',
    { type: 'assets.read', id: 9, asset_id: ASSET, offset: 0, length: 1 },
  ],
];
for (const [name, message] of MALFORMED_CALLS)
  test(`parseCall refuses ${name}`, () => {
    assert.equal(parseCall(message), undefined);
  });

// ---- the rpc ----

test('assets.list asks the source of the view, with the frame, that view and the call signal', async () => {
  const asked = [];
  const { rpc, runtimeCalls } = rpcWith({
    listRuntimeAssets(frame, callView, signal) {
      asked.push({ frame, callView, signal });
      return Promise.resolve(LISTING);
    },
  });
  rpc.handle({ type: 'assets.list', id: 4 });
  await settle();
  assert.equal(runtimeCalls.length, 1);
  assert.equal(runtimeCalls[0], view);
  assert.equal(asked.length, 1);
  assert.equal(asked[0].frame, FRAME);
  assert.equal(asked[0].callView, view);
  assert.ok(asked[0].signal instanceof AbortSignal);
});

test('assets.read asks the source for that part and for nothing else', async () => {
  const asked = [];
  const { rpc } = rpcWith({
    readRuntimeAsset(frame, callView, request, signal) {
      asked.push({ frame, callView, request, signal });
      return Promise.resolve(part());
    },
  });
  rpc.handle({
    type: 'assets.read',
    id: 3,
    assetId: ASSET,
    offset: 524_288,
    length: 262_144,
  });
  await settle();
  assert.equal(asked.length, 1);
  assert.equal(asked[0].frame, FRAME);
  assert.equal(asked[0].callView, view);
  assert.deepEqual(asked[0].request, {
    assetId: ASSET,
    offset: 524_288,
    length: 262_144,
  });
  assert.ok(asked[0].signal instanceof AbortSignal);
});

test('A malformed asset call is answered invalid_request at once, holds no slot and reaches no server', async () => {
  const { rpc, sent, runtimeCalls } = rpcWith({});
  const malformed = [
    { type: 'assets.list', id: 1, path: 'a.txt' },
    {
      type: 'assets.read',
      id: 2,
      assetId: 'G'.repeat(64),
      offset: 0,
      length: 1,
    },
    { type: 'assets.read', id: 3, assetId: ASSET, offset: -1, length: 1 },
    { type: 'assets.read', id: 4, assetId: ASSET, offset: 0, length: 0 },
    { type: 'assets.read', id: 5, assetId: ASSET, offset: 0, length: 262_145 },
    {
      type: 'assets.read',
      id: 6,
      assetId: ASSET,
      offset: 0,
      length: 1,
      path: 'a.txt',
    },
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

test('A cancel reaches the asset read it names, and that read sends no reply', async () => {
  const gate = deferred();
  let seen;
  const { rpc, sent } = rpcWith({
    readRuntimeAsset(_frame, _callView, _request, signal) {
      seen = signal;
      return gate.promise;
    },
  });
  rpc.handle({
    type: 'assets.read',
    id: 7,
    assetId: ASSET,
    offset: 0,
    length: 1,
  });
  await settle();
  assert.equal(seen.aborted, false);
  rpc.handle({ type: 'cancel', id: 7 });
  assert.equal(seen.aborted, true);
  gate.resolve(part());
  await settle();
  assert.deepEqual(sent, []);
  assert.equal(rpc.pending, 0);
});

test("An asset listing is answered with its version and each asset's own fields, and nothing else", async () => {
  const { rpc, sent } = rpcWith({
    listRuntimeAssets: () => Promise.resolve(LISTING),
  });
  rpc.handle({ type: 'assets.list', id: 4 });
  await settle();
  assert.deepEqual(sent, [
    {
      message: {
        id: 4,
        ok: true,
        listing: {
          assetVersion: 3,
          assets: [
            {
              id: ASSET,
              contentType: 'text/plain',
              size: 5,
              sha256: DIGEST,
              createdAtMs: 1_700_000_000_000,
            },
          ],
        },
      },
      transfer: [],
    },
  ]);
});

test('An asset part is answered with its fields, and its bytes travel by transfer', async () => {
  const { rpc, sent } = rpcWith({
    readRuntimeAsset: () => Promise.resolve(part({ nextOffset: 3, size: 5 })),
  });
  rpc.handle({
    type: 'assets.read',
    id: 5,
    assetId: ASSET,
    offset: 0,
    length: 262_144,
  });
  await settle();
  assert.equal(sent.length, 1);
  const { message, transfer } = sent[0];
  assert.equal(message.id, 5);
  assert.equal(message.ok, true);
  const { bytes, ...fields } = message.part;
  assert.deepEqual(fields, {
    assetId: ASSET,
    contentType: 'text/plain',
    offset: 0,
    nextOffset: 3,
    size: 5,
    sha256: DIGEST,
  });
  assert.ok(bytes instanceof ArrayBuffer);
  assert.deepEqual([...new Uint8Array(bytes)], [104, 105, 33]);
  assert.equal(transfer.length, 1);
  assert.equal(transfer[0], bytes, 'the bytes are transferred, not copied');
});

test('A failed asset call is reported as its closed word, and nothing the server said', async () => {
  for (const [error, code] of [
    [new StatusError(404, 'not_found'), 'not_found'],
    [new StatusError(403, 'forbidden'), 'denied'],
    [new StatusError(409, 'revision_conflict'), 'stale_view'],
    [new StatusError(429, 'rate_limited'), 'rate_limited'],
    [new StatusError(400, 'invalid_request'), 'invalid_request'],
    [new StatusError(416, 'invalid_request'), 'invalid_request'],
    [new StatusError(500, 'internal'), 'unavailable'],
    [new Error('SECRET'), 'unavailable'],
  ]) {
    const { rpc, sent } = rpcWith({
      listRuntimeAssets: async () => {
        throw error;
      },
      readRuntimeAsset: async () => {
        throw error;
      },
    });
    rpc.handle({ type: 'assets.list', id: 1 });
    await settle();
    rpc.handle({
      type: 'assets.read',
      id: 2,
      assetId: ASSET,
      offset: 0,
      length: 1,
    });
    await settle();
    assert.deepEqual(
      sent.map(({ message }) => message),
      [
        { id: 1, ok: false, code },
        { id: 2, ok: false, code },
      ],
      code,
    );
    assert.equal(JSON.stringify(sent).includes('SECRET'), false);
  }
});

test('Asset calls on a connection without the assets.read grant are refused as denied, with no bytes', async () => {
  const requests = [];
  const client = new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:7410',
    accessToken: 'token',
    fetch: async (url, init) => {
      if (url.endsWith('/v1/runtime-sessions'))
        return Response.json({ session_id: SESSION });
      requests.push({ url, body: JSON.parse(init.body) });
      // The server's refusal for a view that holds no grant of the capability.
      return Response.json(
        { error: { code: 'forbidden', message: 'SECRET detail' } },
        { status: 403 },
      );
    },
  });
  await client.openRuntimeSession();
  const { rpc, sent } = rpcWith(null, { realClient: client, view: ungranted });
  rpc.handle({ type: 'assets.list', id: 1 });
  await until(() => sent.length === 1);
  rpc.handle({
    type: 'assets.read',
    id: 2,
    assetId: ASSET,
    offset: 0,
    length: 262_144,
  });
  await until(() => sent.length === 2);
  assert.deepEqual(
    sent.map(({ message }) => message),
    [
      { id: 1, ok: false, code: 'denied' },
      { id: 2, ok: false, code: 'denied' },
    ],
  );
  assert.equal(JSON.stringify(sent).includes('SECRET'), false);
  const route = `/v1/artifacts/${ungranted.artifact_id}/revisions/${ungranted.revision}/runtime-views/${ungranted.view_id}/${ungranted.generation}`;
  const bodies = Object.fromEntries(
    requests.map(({ url, body }) => [new URL(url).pathname, body]),
  );
  assert.deepEqual(bodies, {
    [`${route}/assets/list`]: { initialization: ungranted.initialization },
    [`${route}/assets/read`]: {
      initialization: ungranted.initialization,
      asset_id: ASSET,
      offset: 0,
      length: 262_144,
    },
  });
});
