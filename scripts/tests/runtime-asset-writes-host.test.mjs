import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';
import { ArtifreeClient } from '../../packages/client/dist/index.js';

// The frame's rpc is web code without DOM or React imports; bundle it as it
// ships. The client is a fake at the runtime-call boundary, or the real client
// over a fake fetch where the wire matters (runtime-assets-host.test.mjs does
// the same for the reads).
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
/** A retry identity the frame made: 8 to 36 of the allowed characters. */
const RETRY = 'upload-0001';
/** A file of three bytes, `hi!`, as the frame holds it. */
const HELLO = [104, 105, 33];
/** The most bytes a write may carry: 1 MiB (decision 0045, rule 12). */
const ONE_MIB = 1_048_576;
/** The frame transport that a source call is given. */
const FRAME = Object.freeze({ transport: 'frame' });

/** A view that holds the assets.read and assets.write grants. */
const view = {
  url: `http://127.0.0.1:7411/r/${'b'.repeat(64)}/`,
  enabled: true,
  session_id: SESSION,
  artifact_id: `art_${'0'.repeat(32)}`,
  revision: 3,
  head_revision: 3,
  sdk: 'artifree.app.v1',
  declared: ['assets.read', 'assets.write'],
  supported: ['assets.read', 'assets.write'],
  granted: ['assets.read', 'assets.write'],
  grant_version: 1,
  view_id: 'd'.repeat(32),
  generation: 1,
  initialization: 'c'.repeat(64),
  expires_at_ms: 1_000_000,
  lease_ms: 900_000,
};
/** The path of the view's frame calls. */
const ROUTE = `/v1/artifacts/${view.artifact_id}/revisions/${view.revision}/runtime-views/${view.view_id}/${view.generation}`;

/** An upload receipt as the client parses it: the asset's own fields. */
const UPLOADED = {
  asset: {
    id: ASSET,
    content_type: 'text/plain',
    size_bytes: 3,
    sha256: DIGEST,
    created_at_ms: 1_700_000_000_000,
  },
  asset_version: 4,
  replayed: true,
};
/** A delete receipt as the client parses it: the asset went, the version moved. */
const REMOVED = {
  asset_id: ASSET,
  asset_version: 4,
  deleted: true,
  replayed: false,
};
/** A source whose two writes are answered with the receipts above. */
const ANSWERING = {
  uploadRuntimeAsset: () => Promise.resolve(UPLOADED),
  deleteRuntimeAsset: () => Promise.resolve(REMOVED),
};

/** An upload call as the SDK sends it, with some fields replaced. */
function uploadCall(changes = {}) {
  return {
    type: 'assets.upload',
    id: 1,
    retryId: RETRY,
    baseAssetVersion: 3,
    contentType: 'text/plain',
    bytes: Uint8Array.from(HELLO).buffer,
    ...changes,
  };
}

/** A delete call as the SDK sends it, with some fields replaced. */
function deleteCall(changes = {}) {
  return {
    type: 'assets.delete',
    id: 2,
    retryId: RETRY,
    baseAssetVersion: 3,
    assetId: ASSET,
    ...changes,
  };
}

/** Whether the host serves `message` as a call; a boolean keeps failures small. */
function accepts(message) {
  return parseCall(message) !== undefined;
}

/** A copy of `message` without `key`. */
function without(message, key) {
  const copy = { ...message };
  delete copy[key];
  return copy;
}

/** Lets settled promises run their continuations. */
async function settle() {
  for (let round = 0; round < 20; round += 1) await Promise.resolve();
}

/** Waits, a turn at a time, for a condition made by asynchronous work. */
async function until(condition) {
  const deadline = Date.now() + 10_000;
  while (!condition()) {
    if (Date.now() > deadline) assert.fail('The condition never became true.');
    await new Promise((resolve) => setImmediate(resolve));
  }
}

/**
 * A rpc whose runtime calls are the test's: `runtimeCall` hands the callback
 * the source the test gives and the frame transport, and records each view it
 * was asked for. `realClient` replaces that fake with a real client; `take`
 * replaces the budget, which admits every call by default.
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
    view,
    ask: () => assert.fail('A write call never prompts.'),
    save: () => assert.fail('A write call never saves.'),
    take: overrides.take ?? (() => true),
    now: () => 1000,
    deadline: 1000 + 900_000,
    send: (message, transfer) => sent.push({ message, transfer }),
  });
  return { rpc, sent, runtimeCalls };
}

/**
 * A real client whose server answers each frame call with `status` and `body`,
 * and `requests` lists each call as the server received it. The runtime session
 * is opened first, as a page does before any runtime call.
 */
async function serverAnswering(status, body) {
  const requests = [];
  const client = new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:7410',
    accessToken: 'token',
    fetch: async (url, init) => {
      if (url.endsWith('/v1/runtime-sessions'))
        return Response.json({ session_id: SESSION });
      requests.push({ url, body: JSON.parse(init.body) });
      return Response.json(body, { status });
    },
  });
  await client.openRuntimeSession();
  return { client, requests };
}

// ---- parsing ----

test('parseCall accepts the write calls exactly as the SDK sends them', () => {
  const bytes = Uint8Array.from(HELLO).buffer;
  assert.deepEqual(parseCall(uploadCall({ bytes })), {
    type: 'assets.upload',
    id: 1,
    retryId: RETRY,
    baseAssetVersion: 3,
    contentType: 'text/plain',
    bytes,
  });
  assert.deepEqual(parseCall(deleteCall()), {
    type: 'assets.delete',
    id: 2,
    retryId: RETRY,
    baseAssetVersion: 3,
    assetId: ASSET,
  });
});

/** Writes at the edges of what the host admits. */
const ACCEPTED_WRITES = [
  [
    'an upload with a retry ID of 8 characters',
    uploadCall({ retryId: 'a'.repeat(8) }),
  ],
  [
    'an upload with a retry ID of 36 characters',
    uploadCall({ retryId: 'a'.repeat(36) }),
  ],
  [
    'an upload whose retry ID has hyphens and underscores',
    uploadCall({ retryId: 'retry-0_1' }),
  ],
  ['an upload at base version 0', uploadCall({ baseAssetVersion: 0 })],
  [
    'an upload at the last safe integer as its base version',
    uploadCall({ baseAssetVersion: Number.MAX_SAFE_INTEGER }),
  ],
  ['an upload of one byte', uploadCall({ bytes: new ArrayBuffer(1) })],
  [
    'an upload of exactly 1 MiB',
    uploadCall({ bytes: new ArrayBuffer(ONE_MIB) }),
  ],
  [
    'an upload whose media type is 127 characters long',
    uploadCall({ contentType: `a/${'a'.repeat(125)}` }),
  ],
  ['a delete at base version 0', deleteCall({ baseAssetVersion: 0 })],
  [
    'a delete with a retry ID of 36 characters',
    deleteCall({ retryId: 'a'.repeat(36) }),
  ],
];
for (const [name, message] of ACCEPTED_WRITES)
  test(`parseCall accepts ${name}`, () => {
    assert.equal(accepts(message), true);
  });

const BAD_RETRY_IDS = [
  ['seven characters', 'a'.repeat(7)],
  ['thirty-seven characters', 'a'.repeat(37)],
  ['a space', 'retry 0001'],
  ['a dot', 'retry.0001'],
  ['empty', ''],
  ['a number', 12_345_678],
];
for (const [name, retryId] of BAD_RETRY_IDS) {
  test(`parseCall refuses an upload whose retry ID is ${name}`, () => {
    assert.equal(accepts(uploadCall({ retryId })), false);
  });
  test(`parseCall refuses a delete whose retry ID is ${name}`, () => {
    assert.equal(accepts(deleteCall({ retryId })), false);
  });
}

const BAD_BASES = [
  ['a negative number', -1],
  ['a fraction', 1.5],
  ['the first unsafe integer', 2 ** 53],
  ['infinity', Number.POSITIVE_INFINITY],
  ['a string', '3'],
  ['null', null],
];
for (const [name, baseAssetVersion] of BAD_BASES) {
  test(`parseCall refuses an upload whose base version is ${name}`, () => {
    assert.equal(accepts(uploadCall({ baseAssetVersion })), false);
  });
  test(`parseCall refuses a delete whose base version is ${name}`, () => {
    assert.equal(accepts(deleteCall({ baseAssetVersion })), false);
  });
}

const BAD_DELETE_ASSET_IDS = [
  ['uppercase hexadecimal digits', ASSET.toUpperCase()],
  ['one digit too few', ASSET.slice(1)],
  ['one digit too many', `${ASSET}0`],
  ['a letter that is not hexadecimal', `g${ASSET.slice(1)}`],
  ['a number', 42],
  ['empty', ''],
];
for (const [name, assetId] of BAD_DELETE_ASSET_IDS)
  test(`parseCall refuses a delete whose asset ID is ${name}`, () => {
    assert.equal(accepts(deleteCall({ assetId })), false);
  });

const BAD_UPLOAD_BYTES = [
  ['one byte over 1 MiB', new ArrayBuffer(ONE_MIB + 1)],
  ['a typed array rather than an ArrayBuffer', new Uint8Array(HELLO)],
  ['an array of numbers', [...HELLO]],
  ['a string', 'hi!'],
];
for (const [name, bytes] of BAD_UPLOAD_BYTES)
  test(`parseCall refuses an upload whose bytes are ${name}`, () => {
    assert.equal(accepts(uploadCall({ bytes })), false);
  });

const BAD_MEDIA_TYPES = [
  ['is spelled in upper-case letters', 'Text/Plain'],
  ['has no subtype', 'text'],
  ['is 128 characters long', `a/${'a'.repeat(126)}`],
  ['is a number', 1],
];
for (const [name, contentType] of BAD_MEDIA_TYPES)
  test(`parseCall refuses an upload whose media type ${name}`, () => {
    assert.equal(accepts(uploadCall({ contentType })), false);
  });

test('parseCall refuses an upload that carries a key only a delete has', () => {
  assert.equal(accepts(uploadCall({ assetId: ASSET })), false);
});

test('parseCall refuses a delete that carries a key only an upload has', () => {
  assert.equal(accepts(deleteCall({ bytes: new ArrayBuffer(1) })), false);
});

test('parseCall refuses an upload without its bytes', () => {
  assert.equal(accepts(without(uploadCall(), 'bytes')), false);
});

test('parseCall refuses a delete without its asset ID', () => {
  assert.equal(accepts(without(deleteCall(), 'assetId')), false);
});

// ---- the rpc ----

test('An upload reaches the source with the fields the frame sent, and the bytes', async () => {
  let seen;
  const { rpc } = rpcWith({
    uploadRuntimeAsset(frame, callView, input, signal) {
      seen = { frame, callView, input, signal };
      return Promise.resolve(UPLOADED);
    },
  });
  rpc.handle(uploadCall());
  await settle();
  assert.ok(seen, 'the source was asked');
  assert.equal(seen.frame, FRAME);
  assert.equal(seen.callView, view);
  assert.ok(seen.signal instanceof AbortSignal);
  assert.deepEqual(Object.keys(seen.input).sort(), [
    'baseAssetVersion',
    'bytes',
    'contentType',
    'retryId',
  ]);
  assert.equal(seen.input.retryId, RETRY);
  assert.equal(seen.input.baseAssetVersion, 3);
  assert.equal(seen.input.contentType, 'text/plain');
  assert.equal(seen.input.bytes instanceof Uint8Array, true);
  assert.equal(seen.input.bytes.length, HELLO.length);
});

test('A delete reaches the source with the asset, the retry identity and the base, and nothing else', async () => {
  let input;
  const { rpc } = rpcWith({
    deleteRuntimeAsset(_frame, _callView, given) {
      input = given;
      return Promise.resolve(REMOVED);
    },
  });
  rpc.handle(deleteCall());
  await settle();
  assert.deepEqual(input, {
    assetId: ASSET,
    retryId: RETRY,
    baseAssetVersion: 3,
  });
});

test('An upload is answered with the asset and the version it made, and no replayed flag', async () => {
  const { rpc, sent } = rpcWith(ANSWERING);
  rpc.handle(uploadCall({ id: 5 }));
  await settle();
  assert.deepEqual(sent, [
    {
      message: {
        id: 5,
        ok: true,
        uploaded: {
          asset: {
            id: ASSET,
            contentType: 'text/plain',
            size: 3,
            sha256: DIGEST,
            createdAtMs: 1_700_000_000_000,
          },
          assetVersion: 4,
        },
      },
      transfer: [],
    },
  ]);
});

test('A delete that removed the asset is answered deleted, at the version it moved to', async () => {
  const { rpc, sent } = rpcWith(ANSWERING);
  rpc.handle(deleteCall({ id: 6 }));
  await settle();
  assert.deepEqual(sent, [
    {
      message: {
        id: 6,
        ok: true,
        removed: { assetId: ASSET, assetVersion: 4, deleted: true },
      },
      transfer: [],
    },
  ]);
});

test('A delete of an asset the artifact no longer has is answered not deleted, at the version it held', async () => {
  const gone = {
    asset_id: ASSET,
    asset_version: 3,
    deleted: false,
    replayed: true,
  };
  const { rpc, sent } = rpcWith({
    deleteRuntimeAsset: () => Promise.resolve(gone),
  });
  rpc.handle(deleteCall({ id: 7 }));
  await settle();
  assert.deepEqual(sent, [
    {
      message: {
        id: 7,
        ok: true,
        removed: { assetId: ASSET, assetVersion: 3, deleted: false },
      },
      transfer: [],
    },
  ]);
});

test('Each upload and each delete takes exactly one token of the frame budget', async () => {
  let taken = 0;
  const { rpc } = rpcWith(ANSWERING, {
    take: () => {
      taken += 1;
      return true;
    },
  });
  rpc.handle(uploadCall({ id: 1 }));
  assert.equal(taken, 1, 'an upload takes one token');
  rpc.handle(deleteCall({ id: 2 }));
  assert.equal(taken, 2, 'a delete takes one token');
  await settle();
});

test('A write the budget refuses is answered rate_limited, and reaches no server', () => {
  const { rpc, sent, runtimeCalls } = rpcWith(ANSWERING, {
    take: () => false,
  });
  rpc.handle(uploadCall({ id: 1 }));
  rpc.handle(deleteCall({ id: 2 }));
  assert.deepEqual(
    sent.map(({ message }) => message),
    [
      { id: 1, ok: false, code: 'rate_limited' },
      { id: 2, ok: false, code: 'rate_limited' },
    ],
  );
  assert.equal(runtimeCalls.length, 0, 'nothing reached the server');
});

test('A malformed write is answered invalid_request at once, holds no slot and reaches no server', async () => {
  const { rpc, sent, runtimeCalls } = rpcWith(ANSWERING);
  rpc.handle(uploadCall({ id: 1, retryId: 'short' }));
  rpc.handle(uploadCall({ id: 2, bytes: new ArrayBuffer(ONE_MIB + 1) }));
  rpc.handle(deleteCall({ id: 3, assetId: 'G'.repeat(64) }));
  rpc.handle(uploadCall({ id: 4, path: 'a.txt' }));
  await settle();
  assert.deepEqual(
    sent.map(({ message }) => message),
    [
      { id: 1, ok: false, code: 'invalid_request' },
      { id: 2, ok: false, code: 'invalid_request' },
      { id: 3, ok: false, code: 'invalid_request' },
      { id: 4, ok: false, code: 'invalid_request' },
    ],
  );
  assert.equal(rpc.pending, 0, 'a refused call holds no slot');
  assert.equal(runtimeCalls.length, 0, 'nothing reached the server');
});

test('An upload sends the bytes the frame made, though the frame changes its buffer after the call', async () => {
  const frameBytes = Uint8Array.from(HELLO).buffer;
  const receipt = {
    asset: {
      id: ASSET,
      content_type: 'text/plain',
      size_bytes: 3,
      sha256: createHash('sha256').update(Uint8Array.from(HELLO)).digest('hex'),
      created_at_ms: 1_700_000_000_000,
    },
    asset_version: 4,
    replayed: false,
  };
  const { client, requests } = await serverAnswering(200, receipt);
  const { rpc, sent } = rpcWith(null, { realClient: client });
  rpc.handle(uploadCall({ id: 1, bytes: frameBytes }));
  // The frame changes its own bytes after the call; the request must not carry
  // the change, because the host copied the bytes when the call arrived.
  new Uint8Array(frameBytes).fill(0);
  await until(() => sent.length === 1);
  assert.equal(requests.length, 1);
  assert.deepEqual(requests[0].body, {
    initialization: view.initialization,
    retry_id: RETRY,
    base_asset_version: 3,
    content_type: 'text/plain',
    content_base64: 'aGkh',
  });
});

test('A delete asks the server for the asset, its retry identity and its base version', async () => {
  const { client, requests } = await serverAnswering(200, REMOVED);
  const { rpc, sent } = rpcWith(null, { realClient: client });
  rpc.handle(deleteCall({ id: 2 }));
  await until(() => sent.length === 1);
  assert.equal(requests.length, 1);
  assert.equal(new URL(requests[0].url).pathname, `${ROUTE}/assets/delete`);
  assert.deepEqual(requests[0].body, {
    initialization: view.initialization,
    asset_id: ASSET,
    base_asset_version: 3,
    retry_id: RETRY,
  });
});

/** The two writes a frame makes, each as a call with the id it is given. */
const WRITES = [
  ['upload', (id) => uploadCall({ id })],
  ['delete', (id) => deleteCall({ id })],
];
for (const [kind, call] of WRITES)
  test(`A ${kind} refused as an asset conflict that names a version is answered conflict, with that version`, async () => {
    const { client, requests } = await serverAnswering(409, {
      error: {
        code: 'asset_conflict',
        message: 'SECRET detail',
        current_asset_version: 7,
      },
    });
    const { rpc, sent } = rpcWith(null, { realClient: client });
    rpc.handle(call(1));
    await until(() => sent.length === 1);
    assert.deepEqual(
      sent.map(({ message }) => message),
      [{ id: 1, ok: false, code: 'conflict', currentAssetVersion: 7 }],
    );
    assert.equal(requests.length, 1, 'a refused write is sent once');
    assert.equal(new URL(requests[0].url).pathname, `${ROUTE}/assets/${kind}`);
    assert.equal(JSON.stringify(sent).includes('SECRET'), false);
  });

/** Asset conflicts whose body names no version the frame can use. */
const UNNAMED_CONFLICTS = [
  ['names no version', {}],
  ['names a version that is a string', { current_asset_version: '7' }],
  ['names a negative version', { current_asset_version: -1 }],
  ['names a fraction as its version', { current_asset_version: 1.5 }],
  [
    'names the first unsafe integer as its version',
    { current_asset_version: 2 ** 53 },
  ],
];
for (const [name, extra] of UNNAMED_CONFLICTS)
  test(`An asset conflict that ${name} is answered unavailable`, async () => {
    const { client, requests } = await serverAnswering(409, {
      error: { code: 'asset_conflict', message: 'SECRET detail', ...extra },
    });
    const { rpc, sent } = rpcWith(null, { realClient: client });
    rpc.handle(uploadCall({ id: 1 }));
    await until(() => sent.length === 1);
    assert.deepEqual(
      sent.map(({ message }) => message),
      [{ id: 1, ok: false, code: 'unavailable' }],
    );
    assert.equal(requests.length, 1, 'a refused write is sent once');
  });

test('A revision conflict is answered stale_view, and gives the frame no asset version', async () => {
  const { client } = await serverAnswering(409, {
    error: {
      code: 'revision_conflict',
      message: 'SECRET detail',
      current_revision: 5,
      current_asset_version: 7,
    },
  });
  const { rpc, sent } = rpcWith(null, { realClient: client });
  rpc.handle(uploadCall({ id: 1 }));
  await until(() => sent.length === 1);
  assert.deepEqual(
    sent.map(({ message }) => message),
    [{ id: 1, ok: false, code: 'stale_view' }],
  );
});

/** The HTTP answers an upload can get, and the word each becomes. */
const REFUSED_WRITES = [
  [404, 'not_found', 'not_found'],
  [403, 'forbidden', 'denied'],
  [429, 'rate_limited', 'rate_limited'],
  [413, 'payload_too_large', 'invalid_request'],
  [400, 'invalid_request', 'invalid_request'],
  [422, 'request_key_reused', 'invalid_request'],
  [422, 'asset_type_unsupported', 'invalid_request'],
  [500, 'internal', 'unavailable'],
];
for (const [status, code, word] of REFUSED_WRITES)
  test(`An upload refused with HTTP ${status} ${code} is answered ${word}, sent once, with nothing the server said`, async () => {
    const { client, requests } = await serverAnswering(status, {
      error: { code, message: 'SECRET detail' },
    });
    const { rpc, sent } = rpcWith(null, { realClient: client });
    rpc.handle(uploadCall({ id: 1 }));
    await until(() => sent.length === 1);
    assert.deepEqual(
      sent.map(({ message }) => message),
      [{ id: 1, ok: false, code: word }],
    );
    assert.equal(requests.length, 1, 'a refused write is sent once');
    assert.equal(JSON.stringify(sent).includes('SECRET'), false);
  });
