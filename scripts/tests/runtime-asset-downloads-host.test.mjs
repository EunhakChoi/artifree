import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';
import { ArtifreeClient } from '../../packages/client/dist/index.js';

// The frame's rpc is web code without DOM or React imports; bundle it as it
// ships. The runtime calls are a fake at the call boundary, or the real client
// over a fake fetch where the wire matters, as runtime-asset-writes-host.test.mjs
// does for the writes.
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
/** The bytes of an uploaded file, `hi!`, as the server holds them. */
const HELLO = [104, 105, 33];
/** The SHA-256 digest of `hi!`, as 64 lowercase hexadecimal digits. */
const HELLO_SHA256 = createHash('sha256')
  .update(Uint8Array.from(HELLO))
  .digest('hex');
/** The name the server offers the file under: `asset-<id>.txt`. */
const FILE_NAME = `asset-${ASSET}.txt`;
/** The frame transport that a runtime call is given. */
const FRAME = Object.freeze({ transport: 'frame' });
/** The offer the server makes for the asset, as the client states it. */
const OFFER = Object.freeze({
  assetId: ASSET,
  fileName: FILE_NAME,
  size: 3,
  sha256: HELLO_SHA256,
});
/** The same offer as the server sends it. */
const OFFER_JSON = {
  asset_id: ASSET,
  file_name: FILE_NAME,
  size: 3,
  sha256: HELLO_SHA256,
};

/** A view that holds the assets.download grant. */
const view = {
  url: `http://127.0.0.1:7411/r/${'b'.repeat(64)}/`,
  enabled: true,
  session_id: SESSION,
  artifact_id: `art_${'0'.repeat(32)}`,
  revision: 3,
  head_revision: 3,
  sdk: 'artifree.app.v1',
  lifecycle: 'restart',
  declared: ['assets.download'],
  supported: ['assets.download'],
  granted: ['assets.download'],
  grant_version: 1,
  view_id: 'd'.repeat(32),
  generation: 1,
  initialization: 'c'.repeat(64),
  expires_at_ms: 1_000_000,
  lease_ms: 900_000,
};
/** The path of the view's frame calls. */
const ROUTE = `/v1/artifacts/${view.artifact_id}/revisions/${view.revision}/runtime-views/${view.view_id}/${view.generation}`;

/** A download call as the SDK sends it, with some fields replaced. */
function downloadCall(changes = {}) {
  return { type: 'assets.download', id: 1, assetId: ASSET, ...changes };
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

/** Lets settled promises run their continuations, and what they queue. */
async function settle() {
  await new Promise((resolve) => setImmediate(resolve));
}

/** Waits, a turn at a time, for a condition made by asynchronous work. */
async function until(condition) {
  const deadline = Date.now() + 10_000;
  while (!condition()) {
    if (Date.now() > deadline) assert.fail('The condition never became true.');
    await new Promise((resolve) => setImmediate(resolve));
  }
}

/** A promise that the test settles when it chooses. */
function deferred() {
  let resolve;
  const promise = new Promise((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

/** The answer of a prompt the user accepts. */
const acceptAnswer = () => Promise.resolve('accepted');

/** An error as the client makes one for a refused call: its name, status and code. */
function apiError(status, code) {
  return Object.assign(new Error('SECRET detail'), {
    name: 'ArtifreeApiError',
    status,
    code,
  });
}

/** A source whose offer the server refuses with `error`. */
function refusingOffer(error) {
  return { offerRuntimeAssetDownload: () => Promise.reject(error) };
}

/**
 * An rpc whose runtime calls reach `source` with the frame transport, unless
 * `client` replaces the client. `log` lists the steps in order: the offer, the
 * prompt, the fetch and the save. `answer` is the prompt's answer. Each prompt
 * is kept in `shown`, each save in `saved`, each reply in `sent` and each
 * runtime call in `runtimeCalls`.
 */
function rpcWith({
  source = {},
  client,
  answer = acceptAnswer,
  take = () => true,
} = {}) {
  const log = [];
  const shown = [];
  const saved = [];
  const sent = [];
  const runtimeCalls = [];
  const runtimeSource = {
    offerRuntimeAssetDownload() {
      log.push('offer');
      return Promise.resolve(OFFER);
    },
    fetchRuntimeAssetDownload() {
      log.push('fetch');
      return Promise.resolve(Uint8Array.from(HELLO));
    },
    ...source,
  };
  const fake = {
    runtimeCall(callView, run) {
      runtimeCalls.push(callView);
      return run(runtimeSource, FRAME);
    },
  };
  const rpc = new FrameRpc({
    client: client ?? fake,
    view,
    ask: (offer, signal) => {
      log.push('ask');
      shown.push(offer);
      return answer(offer, signal);
    },
    save: (bytes, fileName, type) => {
      log.push('save');
      saved.push({ bytes: [...bytes], fileName, type });
    },
    take,
    now: () => 1000,
    deadline: 1000 + 900_000,
    send: (message, transfer) => sent.push({ message, transfer }),
  });
  return { rpc, log, shown, saved, sent, runtimeCalls };
}

// ---- parsing ----

test('parseCall accepts an asset download exactly as the SDK sends it', () => {
  assert.deepEqual(parseCall(downloadCall({ id: 3 })), {
    type: 'assets.download',
    id: 3,
    assetId: ASSET,
  });
});

/** Asset IDs that are not 64 lowercase hexadecimal digits. */
const BAD_ASSET_IDS = [
  ['uppercase hexadecimal digits', ASSET.toUpperCase()],
  ['one digit too few', ASSET.slice(1)],
  ['one digit too many', `${ASSET}0`],
  ['a letter that is not hexadecimal', `g${ASSET.slice(1)}`],
  ['a source path', 'a.txt'],
  ['a number', 42],
  ['empty', ''],
];
for (const [name, assetId] of BAD_ASSET_IDS)
  test(`parseCall refuses an asset download whose asset ID is ${name}`, () => {
    assert.equal(accepts(downloadCall({ assetId })), false);
  });

/** Keys an asset download does not state, each of a kind another call carries. */
const FOREIGN_KEYS = [
  ['a source path', { path: 'a.txt' }],
  ['a source kind', { kind: 'file' }],
  ['a write retry ID', { retryId: 'retry-0001' }],
  ['an unknown key', { extra: true }],
];
for (const [name, extra] of FOREIGN_KEYS)
  test(`parseCall refuses an asset download that carries ${name}`, () => {
    assert.equal(accepts({ ...downloadCall(), ...extra }), false);
  });

test('parseCall refuses an asset download without its asset ID', () => {
  assert.equal(accepts(without(downloadCall(), 'assetId')), false);
});

// ---- the rpc, with a fake source ----

test('An asset download asks the server for the offer of the asset the frame named', async () => {
  let seen;
  const { rpc } = rpcWith({
    source: {
      offerRuntimeAssetDownload(frame, callView, assetId, signal) {
        seen = { frame, callView, assetId, signal };
        return Promise.resolve(OFFER);
      },
    },
  });
  rpc.handle(downloadCall({ id: 1 }));
  await settle();
  assert.equal(seen.frame, FRAME);
  assert.equal(seen.callView, view);
  assert.equal(seen.assetId, ASSET);
  assert.ok(seen.signal instanceof AbortSignal);
});

test('The prompt shows the offer the server made, as an asset with its name and size', async () => {
  const { rpc, shown } = rpcWith({
    answer: () => Promise.resolve('declined'),
  });
  rpc.handle(downloadCall({ id: 1 }));
  await settle();
  assert.deepEqual(shown, [{ kind: 'asset', fileName: FILE_NAME, bytes: 3 }]);
});

/** The refusals a server can give an offer, and the word each becomes. */
const REFUSED_OFFER_ERRORS = [
  ['a forbidden call', 403, 'forbidden', 'denied'],
  ['a missing asset', 404, 'not_found', 'not_found'],
  ['a view the head has outgrown', 409, 'revision_conflict', 'stale_view'],
];
for (const [name, status, code, word] of REFUSED_OFFER_ERRORS)
  test(`An offer refused as ${name} is answered ${word}, and no prompt is shown`, async () => {
    const { rpc, log, runtimeCalls, saved, sent, shown } = rpcWith({
      source: refusingOffer(apiError(status, code)),
    });
    rpc.handle(downloadCall({ id: 1 }));
    await settle();
    assert.deepEqual(sent, [
      { message: { id: 1, ok: false, code: word }, transfer: [] },
    ]);
    assert.equal(runtimeCalls.length, 1, 'the server was asked');
    assert.deepEqual([shown, saved, log], [[], [], []]);
    assert.equal(JSON.stringify(sent).includes('SECRET'), false);
  });

test('The bytes are fetched for the offer the server made, with the frame and the view', async () => {
  let seen;
  const { rpc } = rpcWith({
    source: {
      fetchRuntimeAssetDownload(frame, callView, offer, signal) {
        seen = { frame, callView, offer, signal };
        return Promise.resolve(Uint8Array.from(HELLO));
      },
    },
  });
  rpc.handle(downloadCall({ id: 1 }));
  await settle();
  assert.equal(seen.frame, FRAME);
  assert.equal(seen.callView, view);
  assert.deepEqual(seen.offer, OFFER);
  assert.ok(seen.signal instanceof AbortSignal);
});

/** The ways a prompt ends without an accept, and the word each is answered with. */
const UNACCEPTED = [
  ['declined', 'declined'],
  ['busy', 'rate_limited'],
  ['cooling', 'rate_limited'],
];
for (const [outcome, word] of UNACCEPTED)
  test(`A prompt that ends ${outcome} is answered ${word}, and no bytes are fetched or saved`, async () => {
    const { rpc, log, saved, sent } = rpcWith({
      answer: () => Promise.resolve(outcome),
    });
    rpc.handle(downloadCall({ id: 1 }));
    await settle();
    assert.deepEqual(sent, [
      { message: { id: 1, ok: false, code: word }, transfer: [] },
    ]);
    assert.deepEqual(log, ['offer', 'ask']);
    assert.deepEqual(saved, []);
    assert.equal(rpc.pending, 0, 'the call holds no slot');
  });

test('A prompt the desk withdraws is answered with no reply, and nothing is fetched or saved', async () => {
  const { rpc, log, saved, sent } = rpcWith({
    answer: () => Promise.resolve('withdrawn'),
  });
  rpc.handle(downloadCall({ id: 1 }));
  await settle();
  assert.deepEqual([sent, log, saved], [[], ['offer', 'ask'], []]);
  assert.equal(rpc.pending, 0, 'the call holds no slot');
});

test('A prompt the frame cancels saves nothing, even when its answer is accepted', async () => {
  let shownSignal;
  const prompt = deferred();
  const { rpc, log, saved, sent } = rpcWith({
    answer: (_offer, signal) => {
      shownSignal = signal;
      return prompt.promise;
    },
  });
  rpc.handle(downloadCall({ id: 1 }));
  await settle();
  assert.equal(shownSignal.aborted, false);
  rpc.handle({ type: 'cancel', id: 1 });
  assert.equal(shownSignal.aborted, true);
  prompt.resolve('accepted');
  await settle();
  assert.deepEqual([sent, log, saved], [[], ['offer', 'ask'], []]);
});

test('An accepted asset download fetches the bytes once after the answer, saves them once and is answered with its receipt', async () => {
  const { rpc, log, saved, sent } = rpcWith();
  rpc.handle(downloadCall({ id: 1 }));
  await settle();
  assert.deepEqual(log, ['offer', 'ask', 'fetch', 'save']);
  assert.deepEqual(saved, [
    { bytes: HELLO, fileName: FILE_NAME, type: 'application/octet-stream' },
  ]);
  assert.deepEqual(sent, [
    {
      message: {
        id: 1,
        ok: true,
        receipt: { started: true, assetId: ASSET, bytes: 3 },
      },
      transfer: [],
    },
  ]);
});

test('An asset download the frame budget refuses is answered rate_limited, and reaches no server and no prompt', async () => {
  const { rpc, log, runtimeCalls, sent, shown } = rpcWith({
    take: () => false,
  });
  rpc.handle(downloadCall({ id: 1 }));
  await settle();
  assert.deepEqual(sent, [
    { message: { id: 1, ok: false, code: 'rate_limited' }, transfer: [] },
  ]);
  assert.deepEqual([runtimeCalls.length, shown.length, log], [0, 0, []]);
});

test('An asset download takes exactly one token of the frame budget', async () => {
  let taken = 0;
  const { rpc } = rpcWith({
    take: () => {
      taken += 1;
      return true;
    },
  });
  rpc.handle(downloadCall({ id: 1 }));
  assert.equal(taken, 1);
  await settle();
});

test('A malformed asset download is answered invalid_request at once, holds no slot and reaches no server or prompt', async () => {
  const { rpc, log, runtimeCalls, sent, shown } = rpcWith();
  rpc.handle(downloadCall({ id: 1, assetId: ASSET.toUpperCase() }));
  rpc.handle(downloadCall({ id: 2, assetId: ASSET.slice(1) }));
  rpc.handle(without(downloadCall({ id: 3 }), 'assetId'));
  rpc.handle(downloadCall({ id: 4, path: 'a.txt' }));
  rpc.handle(downloadCall({ id: 5, kind: 'file' }));
  assert.deepEqual(
    sent.map(({ message }) => message),
    [1, 2, 3, 4, 5].map((id) => ({ id, ok: false, code: 'invalid_request' })),
  );
  await settle();
  assert.equal(rpc.pending, 0, 'a refused call holds no slot');
  assert.deepEqual([runtimeCalls.length, shown.length, log], [0, 0, []]);
});

test('A source download is still answered with the source receipt, which names its kind and no asset', async () => {
  const { rpc, saved, sent, shown } = rpcWith({
    client: {
      runtimeSourceOffer: () =>
        Promise.resolve({
          kind: 'file',
          path: 'a.txt',
          file_name: 'a.txt',
          size: 3,
          sha256: HELLO_SHA256,
        }),
      runtimeSourceDownload: () => Promise.resolve(Uint8Array.from(HELLO)),
    },
  });
  rpc.handle({ type: 'source.download', id: 4, kind: 'file', path: 'a.txt' });
  await settle();
  assert.deepEqual(shown, [
    { kind: 'file', path: 'a.txt', fileName: 'a.txt', size: 3 },
  ]);
  assert.deepEqual(saved, [
    { bytes: HELLO, fileName: 'a.txt', type: 'application/octet-stream' },
  ]);
  assert.deepEqual(sent, [
    {
      message: {
        id: 4,
        ok: true,
        receipt: { started: true, kind: 'file', bytes: 3 },
      },
      transfer: [],
    },
  ]);
});

// ---- the rpc, with the real client over a fake fetch ----

/** The JSON body the server answers a refused call with. */
function errorBody(code) {
  return { error: { code, message: 'SECRET detail' } };
}

/** The attachment the server answers a download with, for `bytes` of the offered file. */
function attachment(bytes) {
  return new Response(Uint8Array.from(bytes), {
    headers: {
      'content-type': 'application/octet-stream',
      'content-disposition': `attachment; filename="${FILE_NAME}"`,
    },
  });
}

/**
 * A real client whose server answers each frame call by its last path segment,
 * `offer` or `download`, with what `answers` gives for it. `requests` lists each
 * call as the server received it. The runtime session is opened first, as a
 * page does before any runtime call.
 */
async function serverAnswering(answers) {
  const requests = [];
  const client = new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:7410',
    accessToken: 'token',
    fetch: async (url, init) => {
      if (url.endsWith('/v1/runtime-sessions'))
        return Response.json({ session_id: SESSION });
      const { pathname } = new URL(url);
      requests.push({ path: pathname, body: JSON.parse(init.body) });
      return answers[pathname.split('/').at(-1)]();
    },
  });
  await client.openRuntimeSession();
  return { client, requests };
}

/** The answers a server can give an offer over the wire, and the word each becomes. */
const REFUSED_OFFER_ANSWERS = [
  [
    '403 forbidden',
    () => Response.json(errorBody('forbidden'), { status: 403 }),
    'denied',
  ],
  [
    '404 not_found',
    () => Response.json(errorBody('not_found'), { status: 404 }),
    'not_found',
  ],
  [
    '409 revision_conflict',
    () => Response.json(errorBody('revision_conflict'), { status: 409 }),
    'stale_view',
  ],
  [
    '200 naming another asset',
    () => Response.json({ ...OFFER_JSON, asset_id: 'f'.repeat(64) }),
    'unavailable',
  ],
];
for (const [name, answer, word] of REFUSED_OFFER_ANSWERS)
  test(`An offer the server answers ${name} is answered ${word}, and no prompt is shown`, async () => {
    const { client, requests } = await serverAnswering({ offer: answer });
    const { rpc, saved, sent, shown } = rpcWith({ client });
    rpc.handle(downloadCall({ id: 1 }));
    await until(() => sent.length === 1);
    assert.deepEqual(sent, [
      { message: { id: 1, ok: false, code: word }, transfer: [] },
    ]);
    assert.deepEqual([shown, saved], [[], []]);
    assert.deepEqual(
      requests.map(({ path }) => path),
      [`${ROUTE}/assets/offer`],
    );
    assert.equal(JSON.stringify(sent).includes('SECRET'), false);
  });

test('An accepted asset download over the wire is saved under the offered name and answered with its receipt', async () => {
  const { client, requests } = await serverAnswering({
    offer: () => Response.json(OFFER_JSON),
    download: () => attachment(HELLO),
  });
  const { rpc, saved, sent, shown } = rpcWith({ client });
  rpc.handle(downloadCall({ id: 1 }));
  await until(() => sent.length === 1);
  assert.deepEqual(shown, [{ kind: 'asset', fileName: FILE_NAME, bytes: 3 }]);
  assert.deepEqual(saved, [
    { bytes: HELLO, fileName: FILE_NAME, type: 'application/octet-stream' },
  ]);
  assert.deepEqual(sent, [
    {
      message: {
        id: 1,
        ok: true,
        receipt: { started: true, assetId: ASSET, bytes: 3 },
      },
      transfer: [],
    },
  ]);
  const body = { initialization: view.initialization, asset_id: ASSET };
  assert.deepEqual(requests, [
    { path: `${ROUTE}/assets/offer`, body },
    { path: `${ROUTE}/assets/download`, body },
  ]);
});

/** The answers a download can get after the user accepted, and the word each becomes. */
const REFUSED_DOWNLOAD_ANSWERS = [
  [
    '403 forbidden',
    () => Response.json(errorBody('forbidden'), { status: 403 }),
    'denied',
  ],
  [
    '404 not_found',
    () => Response.json(errorBody('not_found'), { status: 404 }),
    'not_found',
  ],
  [
    '409 revision_conflict',
    () => Response.json(errorBody('revision_conflict'), { status: 409 }),
    'stale_view',
  ],
  [
    'bytes that are not the offered digest',
    () => attachment([104, 105, 34]),
    'unavailable',
  ],
];
for (const [name, answer, word] of REFUSED_DOWNLOAD_ANSWERS)
  test(`A download the server answers ${name}, after the accept, saves nothing and is answered ${word}`, async () => {
    const { client, requests } = await serverAnswering({
      offer: () => Response.json(OFFER_JSON),
      download: answer,
    });
    const { rpc, saved, sent, shown } = rpcWith({ client });
    rpc.handle(downloadCall({ id: 1 }));
    await until(() => sent.length === 1);
    assert.deepEqual(sent, [
      { message: { id: 1, ok: false, code: word }, transfer: [] },
    ]);
    assert.equal(shown.length, 1, 'the prompt was shown and accepted');
    assert.deepEqual(saved, []);
    assert.equal(requests.length, 2, 'the bytes were requested once');
    assert.equal(JSON.stringify(sent).includes('SECRET'), false);
  });
