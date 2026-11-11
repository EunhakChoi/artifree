import assert from 'node:assert/strict';
import { afterEach, mock, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// connectFrame is where the host pauses, resumes, refuses and replaces a
// frame's view (decision 0054, rules 8, 9 and 13). These tests drive it as the
// host does, bundled as it ships. Its waits read performance.now() and the
// global setTimeout, so each test moves both together with advance() and
// restores performance.now() afterwards. A frame's messages reach the connected
// frame through handle(), as the port handler gives them. The client is a fake
// that answers as the server does, and an answer can be held back, so that a
// call stays on its way until the test opens the gate.
/**
 * A module of the web app, bundled as it ships and loaded. lucide-react has no
 * exports map, so its module field is read as the browser build reads it. The
 * neutral platform reads no field unless named.
 */
async function loadWebModule(relativePath) {
  const bundle = await build({
    entryPoints: [fileURLToPath(new URL(relativePath, import.meta.url))],
    bundle: true,
    format: 'esm',
    platform: 'neutral',
    mainFields: ['module', 'main'],
    write: false,
  });
  return import(
    `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
  );
}

const { connectFrame, createDesk } = await loadWebModule(
  '../../apps/web/src/artifacts/runtime-host.ts',
);
// The desk of a download prompt. createDesk() reads window.setTimeout, which
// Node does not have, so a case that opens a prompt builds the desk itself on
// the clock the waits read.
const { OfferDesk } = await loadWebModule(
  '../../apps/web/src/artifacts/offer-desk.ts',
);

const ARTIFACT = `art_${'0'.repeat(32)}`;
/** The frame transport that a source call is given. */
const FRAME = Object.freeze({ transport: 'frame' });
/** The data path the data calls name. */
const PATH = 'notes/a';
/** A retry identity of the length and alphabet the frame's writes carry. */
const RETRY_ID = 'retry_0001';
/** The lease of each view, on the clock the waits read. */
const LEASE_MS = 900_000;
/** The call id that the single-call cases send. */
const ID = 1;

/** A view at generation 1 that holds the data.read and data.write grants. */
const VIEW = {
  url: `http://127.0.0.1:7411/r/${'b'.repeat(64)}/`,
  enabled: true,
  session_id: 'e'.repeat(32),
  artifact_id: ARTIFACT,
  revision: 1,
  head_revision: 1,
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
  lease_ms: LEASE_MS,
};
/** The view the server answers an adoption with: the next generation. */
const ADOPTED_VIEW = { ...VIEW, generation: 2, lifecycle: 'adopt' };
/** The asset a download call names: 64 lowercase hexadecimal digits. */
const ASSET = 'b'.repeat(64);
/** The offer the server makes for ASSET, which the desk shows the user. */
const DOWNLOAD_OFFER = {
  assetId: ASSET,
  fileName: 'photo.txt',
  size: 5,
  sha256: 'd'.repeat(64),
};
/** The desk's clock: the clock the waits read, and the global timer. */
const DESK_CLOCK = {
  now: () => performance.now(),
  after: (ms, run) => {
    const timer = setTimeout(run, ms);
    return () => clearTimeout(timer);
  },
};

/** What the server answers a read of PATH that holds a value. */
const FOUND = {
  artifact_id: ARTIFACT,
  path: PATH,
  data_version: 3,
  exists: true,
  value: '{"a":1}',
};
/** What the server answers a metadata read of the artifact. */
const METADATA = {
  artifact_id: ARTIFACT,
  title: 'Notes',
  current_revision: 1,
  metadata_revision: 1,
};
/** The receipt of a data write that changed the value at PATH. */
const DATA_RECEIPT = {
  artifact_id: ARTIFACT,
  path: PATH,
  data_version: 4,
  changed: true,
  replayed: false,
};
/** The receipt of a source write of index.html. */
const SOURCE_RECEIPT = {
  path: 'index.html',
  revision: 2,
  size: 5,
  sha256: 'a'.repeat(64),
};
/** The receipt of an asset upload of five bytes of text. */
const UPLOAD_RECEIPT = {
  asset: {
    id: 'b'.repeat(64),
    content_type: 'text/plain',
    size_bytes: 5,
    sha256: 'c'.repeat(64),
    created_at_ms: 1_700_000_000_000,
  },
  asset_version: 2,
};

/** The frame's metadata call under call id `id`. */
const metadataCall = (id) => ({ type: 'metadata', id });
/** The frame's read of PATH under call id `id`. */
const dataGetCall = (id) => ({ type: 'data.get', id, path: PATH });
/** The frame's download of ASSET under call id `id`. */
const downloadCall = (id) => ({
  type: 'assets.download',
  id,
  assetId: ASSET,
});
/** The frame's cancel of the call with id `id`. */
const cancelCall = (id) => ({ type: 'cancel', id });
/** The reply that answers the call with id `id` unavailable. */
const unavailableReply = (id) => ({
  message: { id, ok: false, code: 'unavailable' },
  transfer: [],
});
/** The reply to a metadata call the server answered with METADATA. */
const metadataReply = (id) => ({
  id,
  ok: true,
  metadata: {
    artifactId: ARTIFACT,
    title: 'Notes',
    currentRevision: 1,
    metadataRevision: 1,
  },
});
/** The reply to a read of PATH the server answered with FOUND. */
const dataGetReply = (id) => ({
  id,
  ok: true,
  data: { path: PATH, dataVersion: 3, exists: true, value: '{"a":1}' },
});
/** The notice the frame gets when its view is adopted (decision 0054, rule 10). */
const ADOPTED_EVENT = {
  type: 'event',
  event: 'view.adopted',
  reason: 'permissions',
  leaseMs: LEASE_MS,
};

/** The bytes of `text`, as a frame transfers them in an ArrayBuffer. */
function textBytes(text) {
  return new TextEncoder().encode(text).buffer;
}

/** Orders replies by the call they answer. */
const byId = (a, b) => a.id - b.id;

/** The reads a call on its way can be, each with the server's answer. */
const READS = [
  { kind: 'metadata', call: metadataCall, answer: METADATA },
  { kind: 'data.get', call: dataGetCall, answer: FOUND },
];

/**
 * The writes, each as the frame sends it, with the receipt the server answers
 * and the reply the frame is owed for it.
 */
const WRITES = [
  {
    kind: 'source.write',
    call: () => ({
      type: 'source.write',
      id: ID,
      retryId: RETRY_ID,
      path: 'index.html',
      bytes: textBytes('hello'),
    }),
    receipt: SOURCE_RECEIPT,
    reply: {
      id: ID,
      ok: true,
      written: {
        path: 'index.html',
        revision: 2,
        size: 5,
        sha256: 'a'.repeat(64),
      },
    },
  },
  {
    kind: 'assets.upload',
    call: () => ({
      type: 'assets.upload',
      id: ID,
      retryId: RETRY_ID,
      baseAssetVersion: 1,
      contentType: 'text/plain',
      bytes: textBytes('hello'),
    }),
    receipt: UPLOAD_RECEIPT,
    reply: {
      id: ID,
      ok: true,
      uploaded: {
        asset: {
          id: 'b'.repeat(64),
          contentType: 'text/plain',
          size: 5,
          sha256: 'c'.repeat(64),
          createdAtMs: 1_700_000_000_000,
        },
        assetVersion: 2,
      },
    },
  },
  {
    kind: 'data.set',
    call: () => ({
      type: 'data.set',
      id: ID,
      path: PATH,
      value: '{"a":2}',
      baseDataVersion: 3,
      retryId: RETRY_ID,
    }),
    receipt: DATA_RECEIPT,
    reply: {
      id: ID,
      ok: true,
      written: { path: PATH, dataVersion: 4, changed: true },
    },
  },
];

const realNow = Object.getOwnPropertyDescriptor(performance, 'now');

/** Makes performance.now() read `clock`, the clock the waits read. */
function installClock(clock) {
  Object.defineProperty(performance, 'now', {
    value: () => clock.ms,
    configurable: true,
    writable: true,
  });
}

/** Moves the clock and the timers together. */
function advance(clock, ms) {
  clock.ms += ms;
  mock.timers.tick(ms);
}

afterEach(() => {
  mock.timers.reset();
  if (realNow === undefined) delete performance.now;
  else Object.defineProperty(performance, 'now', realNow);
});

/** Lets the work that is ready run up to its next wait. */
function settle() {
  return new Promise((resolve) => setImmediate(resolve));
}

/**
 * Waits, a turn at a time, for a condition made by asynchronous work, and fails
 * with `message` if it never holds.
 */
async function until(condition, message) {
  const deadline = Date.now() + 10_000;
  while (!condition()) {
    if (Date.now() > deadline) assert.fail(message);
    await settle();
  }
}

/**
 * Follows a promise without awaiting it. `done` turns true when the promise
 * settles and `error` holds its rejection, so a case that expects a promise to
 * stay open asks `done` and fails, rather than hangs.
 */
function track(promise) {
  const state = { done: false, error: undefined };
  promise.then(
    () => {
      state.done = true;
    },
    (error) => {
      state.done = true;
      state.error = error;
    },
  );
  return state;
}

/** Pauses a frame with nothing on its way: the pause resolves at once. */
async function pauseIdle(frame, reason) {
  const paused = track(frame.pause(reason));
  await settle();
  assert.equal(
    paused.done,
    true,
    'with nothing on its way the pause resolves at once',
  );
}

/** An answer that the test opens with `open`, so a call stays on its way. */
function answerGate() {
  let open;
  const promise = new Promise((resolve) => {
    open = resolve;
  });
  return { promise, open };
}

/**
 * The client the connected frame is given, answering as the server does. A call
 * that reaches it is recorded in `calls` under the generation of the view it was
 * made under. With a `gate`, every answer waits until the gate opens. An
 * adoption is answered with `adoption`, or rejected with it when it is an error,
 * and each adoption request is recorded in `adoptions`.
 */
function fakeClient({ calls, adoptions, gate, adoption }) {
  const answer = (name, view, value) => {
    calls.push({ name, generation: view.generation });
    return gate === undefined ? value : gate.promise;
  };
  const source = {
    readRuntimeData: async (_frame, view) =>
      answer('readRuntimeData', view, FOUND),
    setRuntimeData: async (_frame, view) =>
      answer('setRuntimeData', view, DATA_RECEIPT),
    writeSource: async (_frame, view) =>
      answer('writeSource', view, SOURCE_RECEIPT),
    uploadRuntimeAsset: async (_frame, view) =>
      answer('uploadRuntimeAsset', view, UPLOAD_RECEIPT),
    offerRuntimeAssetDownload: async (_frame, view) =>
      answer('offerRuntimeAssetDownload', view, DOWNLOAD_OFFER),
    fetchRuntimeAssetDownload: async (_frame, view) =>
      answer('fetchRuntimeAssetDownload', view, new Uint8Array(5)),
  };
  return {
    runtimeCall: (view, run) => run(source, FRAME),
    runtimeMetadata: async (view) => answer('runtimeMetadata', view, METADATA),
    createRuntimeView: async (
      _artifactId,
      revision,
      _viewId,
      generation,
      options,
    ) => {
      adoptions.push({
        revision,
        generation,
        replaces: options.adopt.generation,
      });
      if (adoption instanceof Error) throw adoption;
      return adoption;
    },
  };
}

/**
 * A connected frame, connected as runtime-host.ts connects one, on the fake
 * client, a desk and a clock at zero. `sent` holds what the frame was told,
 * `calls` what reached the client, and `adoptions` the adoption requests made
 * of it.
 */
function connect({ gate, adoption = ADOPTED_VIEW, desk = createDesk() } = {}) {
  const clock = { ms: 0 };
  installClock(clock);
  mock.timers.enable({ apis: ['setTimeout'] });
  const calls = [];
  const adoptions = [];
  const sent = [];
  const frame = connectFrame({
    client: fakeClient({ calls, adoptions, gate, adoption }),
    view: VIEW,
    desk,
    dataChanges: { subscribe: () => () => undefined },
    deadline: clock.ms + LEASE_MS,
    send: (message, transfer) => {
      sent.push({ message, transfer });
    },
  });
  return { frame, sent, calls, adoptions, clock };
}

test('calls made while paused are held and sent none, then go out under the new generation after adoption', async () => {
  const { frame, sent, calls, adoptions } = connect();
  await pauseIdle(frame, 'permissions');

  frame.handle(metadataCall(1));
  frame.handle(dataGetCall(2));
  await settle();
  assert.deepEqual(calls, [], 'no paused call reaches the client');
  assert.deepEqual(sent, [], 'nothing is sent while the calls are held');

  const adopting = track(
    frame.adopt({ reason: 'permissions', revision: 1, generation: 2 }),
  );
  await until(
    () => adopting.done && sent.length === 3,
    'the adoption did not answer the held calls',
  );
  assert.equal(adopting.error, undefined, 'the adoption succeeds');
  assert.deepEqual(adoptions, [{ revision: 1, generation: 2, replaces: 1 }]);
  assert.equal(
    frame.view().generation,
    2,
    'the frame is bound to generation 2',
  );
  assert.deepEqual(
    calls,
    [
      { name: 'runtimeMetadata', generation: 2 },
      { name: 'readRuntimeData', generation: 2 },
    ],
    'both held calls reach the client under the new generation',
  );
  assert.deepEqual(
    sent[0],
    { message: ADOPTED_EVENT, transfer: [] },
    'the frame is told of the adoption before any held call is answered',
  );
  const replies = sent.slice(1).map(({ message }) => message);
  assert.deepEqual(
    replies.sort(byId),
    [metadataReply(1), dataGetReply(2)],
    'each held call is answered',
  );
});

for (const write of WRITES)
  test(`the ${write.kind} call on its way is waited for until its request ends, at 5,000 ms and at 20,000 ms alike`, async () => {
    const gate = answerGate();
    const { frame, sent, calls, clock } = connect({ gate });
    frame.handle(write.call());
    await until(() => calls.length === 1, 'the write never reached the client');
    const paused = track(frame.pause('permissions'));

    advance(clock, 5_000);
    await settle();
    assert.equal(
      paused.done,
      false,
      'the pause does not abandon the write at 5,000 ms',
    );
    assert.deepEqual(sent, [], 'no reply is sent for the write at 5,000 ms');

    advance(clock, 15_000);
    await settle();
    assert.equal(
      paused.done,
      false,
      'the pause does not abandon the write at 20,000 ms',
    );
    assert.deepEqual(sent, [], 'no reply is sent for the write at 20,000 ms');

    gate.open(write.receipt);
    await until(
      () => paused.done,
      'the pause did not resolve when the write ended',
    );
    assert.deepEqual(
      sent,
      [{ message: write.reply, transfer: [] }],
      'the write is answered when its request ends',
    );
  });

for (const read of READS)
  test(`the ${read.kind} call on its way is abandoned after 5 s and answered unavailable, and its late answer is not sent`, async () => {
    const gate = answerGate();
    const { frame, sent, calls, clock } = connect({ gate });
    frame.handle(read.call(ID));
    await until(() => calls.length === 1, 'the call never reached the client');
    const paused = track(frame.pause('permissions'));

    advance(clock, 4_999);
    await settle();
    assert.equal(paused.done, false, 'the pause still waits at 4,999 ms');
    assert.deepEqual(sent, [], 'no reply is sent at 4,999 ms');

    advance(clock, 1);
    await settle();
    assert.equal(
      paused.done,
      true,
      'the pause resolves once the call is abandoned at 5,000 ms',
    );
    assert.deepEqual(
      sent,
      [{ message: { id: ID, ok: false, code: 'unavailable' }, transfer: [] }],
      'the call is answered unavailable',
    );

    gate.open(read.answer);
    await settle();
    assert.equal(sent.length, 1, 'the late answer sends nothing more');
  });

test('a pause released without adoption sends the held calls under the old generation', async () => {
  const { frame, sent, calls, adoptions } = connect();
  await pauseIdle(frame, 'permissions');
  frame.handle(metadataCall(ID));
  await settle();
  assert.deepEqual(calls, [], 'the held call reaches no client while paused');

  frame.resume();
  await until(
    () => sent.length === 1,
    'the held call was not sent when the pause ended',
  );
  assert.deepEqual(
    calls,
    [{ name: 'runtimeMetadata', generation: 1 }],
    'the held call goes out under the old generation',
  );
  assert.deepEqual(sent, [{ message: metadataReply(ID), transfer: [] }]);
  assert.deepEqual(adoptions, [], 'no adoption is asked for');
});

test('after refuse, held and later calls are answered unavailable at once, until the next resume', async () => {
  const { frame, sent, calls } = connect();
  await pauseIdle(frame, 'permissions');
  frame.handle(metadataCall(1));
  frame.refuse();
  await settle();
  assert.deepEqual(
    sent,
    [{ message: { id: 1, ok: false, code: 'unavailable' }, transfer: [] }],
    'the held call is answered unavailable',
  );

  frame.handle(dataGetCall(2));
  await settle();
  assert.deepEqual(
    sent[1],
    { message: { id: 2, ok: false, code: 'unavailable' }, transfer: [] },
    'a later call is answered unavailable at once',
  );
  assert.deepEqual(calls, [], 'the client is never called while refusing');

  frame.resume();
  frame.handle(metadataCall(3));
  await until(
    () => sent.length === 3,
    'the call after the resume was not answered',
  );
  assert.deepEqual(
    calls,
    [{ name: 'runtimeMetadata', generation: 1 }],
    'only the call after the resume reaches the client',
  );
  assert.deepEqual(sent[2], { message: metadataReply(3), transfer: [] });
});

test('a failed adoption answers the held calls expired, serves none, and ignores later calls', async () => {
  const conflict = Object.assign(new Error('The view moved on.'), {
    status: 409,
    code: 'revision_conflict',
  });
  const { frame, sent, calls, adoptions } = connect({ adoption: conflict });
  await pauseIdle(frame, 'permissions');
  frame.handle(metadataCall(1));
  await settle();

  const adopting = track(
    frame.adopt({ reason: 'permissions', revision: 1, generation: 2 }),
  );
  await settle();
  assert.equal(adopting.done, true, 'the adoption settles');
  assert.equal(
    adopting.error,
    conflict,
    'the adoption rejects with the error the server gave',
  );
  assert.deepEqual(
    adoptions,
    [{ revision: 1, generation: 2, replaces: 1 }],
    'the adoption was asked once',
  );
  assert.deepEqual(sent, [], 'the frame is told nothing of a failed adoption');
  assert.deepEqual(calls, [], 'the held call is served by no client');

  frame.fail();
  await settle();
  assert.deepEqual(
    sent,
    [{ message: { id: 1, ok: false, code: 'expired' }, transfer: [] }],
    'the held call is answered expired',
  );

  frame.handle(metadataCall(2));
  await settle();
  assert.equal(sent.length, 1, 'a call after the failure is ignored');
  assert.deepEqual(calls, [], 'no client is called after the failure');
});

test('a cancel of a held call keeps the call from being served when the pause ends, and frees its id', async () => {
  const { frame, sent, calls } = connect();
  await pauseIdle(frame, 'permissions');
  frame.handle(metadataCall(ID));
  frame.handle(cancelCall(ID));
  frame.resume();
  await settle();
  assert.deepEqual(
    calls,
    [],
    'the cancelled call reaches no client when the pause ends',
  );
  assert.deepEqual(sent, [], 'the cancelled call is answered nothing');

  frame.handle(metadataCall(ID));
  await until(
    () => sent.length === 1,
    'the id of the cancelled call was not free to use again',
  );
  assert.deepEqual(
    calls,
    [{ name: 'runtimeMetadata', generation: 1 }],
    'only the new call reaches the client',
  );
  assert.deepEqual(sent, [{ message: metadataReply(ID), transfer: [] }]);
});

test('a call that arrives after the lease ended is held through a pause, then served when an adoption moves the lease', async () => {
  const { frame, sent, calls, clock } = connect();
  await pauseIdle(frame, 'permissions');
  advance(clock, LEASE_MS);
  frame.handle(metadataCall(ID));
  await settle();
  assert.deepEqual(calls, [], 'the late call reaches no client while paused');
  assert.deepEqual(sent, [], 'the late call is not answered while paused');

  const adopting = track(
    frame.adopt({ reason: 'permissions', revision: 1, generation: 2 }),
  );
  await until(() => adopting.done, 'the adoption did not settle');
  await settle();
  assert.equal(adopting.error, undefined, 'the adoption succeeds');
  assert.deepEqual(
    calls,
    [{ name: 'runtimeMetadata', generation: 2 }],
    'the held call goes out under generation 2',
  );
  assert.deepEqual(
    sent,
    [
      { message: ADOPTED_EVENT, transfer: [] },
      { message: metadataReply(ID), transfer: [] },
    ],
    'the frame is told of the adoption, then answered the held call',
  );
});

test('refuse withdraws an open download prompt: the call is answered unavailable and its bytes are not fetched', async () => {
  const desk = new OfferDesk(DESK_CLOCK);
  const { frame, sent, calls } = connect({ desk });
  frame.handle(downloadCall(ID));
  await until(
    () => desk.snapshot().offer !== undefined,
    'the download prompt did not open',
  );
  assert.deepEqual(sent, [], 'an open prompt is answered nothing yet');

  frame.refuse();
  await settle();
  assert.deepEqual(
    sent,
    [unavailableReply(ID)],
    'refuse answers the prompted call unavailable',
  );
  assert.deepEqual(
    calls,
    [{ name: 'offerRuntimeAssetDownload', generation: 1 }],
    'the bytes are never fetched',
  );
});

test('past the lease an abandoned call and fail() send the frame nothing', async () => {
  const gate = answerGate();
  const { frame, sent, calls, clock } = connect({ gate });
  frame.handle(metadataCall(ID));
  // A write is waited for until its request ends, so it is still pending
  // when fail() is called.
  frame.handle({ ...WRITES[0].call(), id: ID + 1 });
  await until(() => calls.length === 2, 'the calls never reached the client');
  const paused = track(frame.pause('permissions'));

  // The clock reaches the lease's end before the read's 5,000 ms wait ends,
  // so the read is abandoned after the lease.
  advance(clock, LEASE_MS);
  await settle();
  assert.equal(
    paused.done,
    false,
    'the pause still waits for the write on its way',
  );
  assert.deepEqual(sent, [], 'the abandoned read is answered nothing');

  frame.fail();
  await settle();
  assert.deepEqual(
    sent,
    [],
    'fail() answers the pending write nothing after the lease',
  );
});

test('a download the frame cancels after a pause abandoned it is not answered unavailable a second time', async () => {
  const gate = answerGate();
  const desk = new OfferDesk(DESK_CLOCK);
  const { frame, sent, calls, clock } = connect({ gate, desk });
  frame.handle(downloadCall(ID));
  await until(
    () => calls.length === 1,
    'the download never reached the client',
  );
  const paused = track(frame.pause('permissions'));

  advance(clock, 5_000);
  await settle();
  assert.equal(
    paused.done,
    true,
    'the pause resolves once the call is abandoned at 5,000 ms',
  );
  assert.deepEqual(
    sent,
    [unavailableReply(ID)],
    'the call on its way is answered unavailable once',
  );

  frame.handle(cancelCall(ID));
  gate.open(DOWNLOAD_OFFER);
  await settle();
  assert.deepEqual(
    sent,
    [unavailableReply(ID)],
    'the cancelled call is not answered again when its offer arrives',
  );
  assert.deepEqual(
    calls,
    [{ name: 'offerRuntimeAssetDownload', generation: 1 }],
    'the bytes are never asked for',
  );
});
