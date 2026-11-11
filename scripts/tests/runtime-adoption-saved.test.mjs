import assert from 'node:assert/strict';
import { afterEach, mock, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The workspace side of a frame's save (decision 0054, rule 8) and the two
// counts it keeps of the frame's calls (rules 8 and 9). connectFrame is bundled
// as it ships. A source.write that commits tells the workspace its revision
// before its receipt is posted, so the pause of a `saved` adoption begins then,
// and the frame's next call is held until the adoption. The client is a fake
// that answers as the server does: a call of a kind the test holds waits until
// the test releases it, and an adoption always waits for the view the test
// gives it. The clock does not move.
const bundle = await build({
  stdin: {
    contents: [
      "export { ArtifreeApiError } from '@artifree/client';",
      "export { DataChangeHub } from './data-changes.ts';",
      "export { connectFrame, createDesk } from './runtime-host.ts';",
    ].join('\n'),
    resolveDir: fileURLToPath(
      new URL('../../apps/web/src/artifacts/', import.meta.url),
    ),
  },
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  // lucide-react has no exports map, so its module field is read as the
  // browser build reads it. The neutral platform reads no field unless named.
  mainFields: ['module', 'main'],
  write: false,
});
const { ArtifreeApiError, DataChangeHub, connectFrame, createDesk } =
  await import(
    `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
  );

const ARTIFACT = `art_${'a'.repeat(32)}`;
const VIEW_ID = 'd'.repeat(32);
/** The revision the frame's view starts at. */
const REVISION = 1;
/** The revision a save commits, which the adoption after it pins. */
const SAVED = 2;
/** The deadline of a frame's lease before any adoption, on the fake clock. */
const DEADLINE = 900_000;
/** The lease a view is issued with. */
const LEASE_MS = 900_000;
/** The file a save writes; its bytes are three, and so is the receipt's size. */
const SOURCE_PATH = 'index.html';
const SHA256 = 'c'.repeat(64);
/** The data path a data write names. */
const DATA_PATH = 'notes/a';
/** The server's answer to a save made on a stale base. */
const CONFLICT = new ArtifreeApiError('The file moved on.', {
  status: 409,
  code: 'revision_conflict',
});

afterEach(() => {
  mock.timers.reset();
  mock.restoreAll();
});

/**
 * Mocks setTimeout, so no timer runs unless a test starts one, and reads the
 * clock as zero, so every deadline is the lease's full length.
 */
function startClock() {
  mock.timers.enable({ apis: ['setTimeout'] });
  mock.method(performance, 'now', () => 0);
}

/** A promise that the test settles when it chooses. */
function deferred() {
  let resolve;
  const promise = new Promise((settle) => {
    resolve = settle;
  });
  return { promise, resolve };
}

/** Lets every answer that is already settled reach the frame. */
function flush() {
  return new Promise((resolve) => {
    setImmediate(resolve);
  });
}

/** Waits, a turn at a time, until `condition` holds; the limit is a time. */
async function until(condition, message = 'The condition never became true.') {
  const deadline = Date.now() + 10_000;
  while (!condition()) {
    if (Date.now() > deadline) assert.fail(message);
    await flush();
  }
}

/** A runtime view of ARTIFACT as the server issues it, pinned to `head`. */
function runtimeView({ generation, head = REVISION }) {
  return {
    artifact_id: ARTIFACT,
    view_id: VIEW_ID,
    generation,
    granted: ['data.read', 'data.write'],
    head_revision: head,
    revision: head,
    lease_ms: LEASE_MS,
    enabled: true,
    declared: ['data.read', 'data.write'],
    grant_version: 1,
    expires_at_ms: 1_000_000,
  };
}

/** The retry id of the write number `id`. */
function retryId(id) {
  return `retry_${String(id).padStart(4, '0')}`;
}

/** A metadata call that is number `id`. */
function metadataCall(id) {
  return { type: 'metadata', id };
}

/** A save of SOURCE_PATH, three bytes, that is number `id`. */
function sourceWriteCall(id) {
  return {
    type: 'source.write',
    id,
    retryId: retryId(id),
    path: SOURCE_PATH,
    bytes: Uint8Array.from([1, 2, 3]).buffer,
  };
}

/** A data write to DATA_PATH that is number `id`, on the base it names. */
function dataWriteCall(id) {
  return {
    type: 'data.set',
    id,
    path: DATA_PATH,
    value: 'v',
    baseDataVersion: id - 1,
    retryId: retryId(id),
  };
}

/** The reply to save number `id` that committed, as the frame receives it. */
function receiptReply(id) {
  return {
    id,
    ok: true,
    written: { path: SOURCE_PATH, revision: SAVED, size: 3, sha256: SHA256 },
  };
}

/** The reply to metadata call `id`, for a view whose head is `revision`. */
function metadataReply(id, revision) {
  return {
    id,
    ok: true,
    metadata: {
      artifactId: ARTIFACT,
      title: 'Notes',
      currentRevision: revision,
      metadataRevision: 1,
    },
  };
}

/** The event that tells a frame its view was adopted (rule 10). */
function adopted(reason, leaseMs) {
  return { type: 'event', event: 'view.adopted', reason, leaseMs };
}

/**
 * The client a frame and its workspace call, faked as the server answers. Each
 * call is recorded in `calls` by its name. A call of a kind named in `hold`
 * waits until the test releases it with `release`, oldest first; a kind named
 * in `fail` is rejected with its error; any other call is answered at once.
 * An adoption always waits.
 */
function fakeClient({ hold = [], fail = {} } = {}) {
  const calls = [];
  /** The calls the test has yet to release, oldest first. */
  const held = [];
  const holds = new Set(['createRuntimeView', ...hold]);
  /** Answers a call of `name` with `value`: rejected, held, or at once. */
  const answer = (name, value) => {
    if (fail[name] !== undefined) return Promise.reject(fail[name]);
    if (!holds.has(name)) return Promise.resolve(value);
    const waiting = deferred();
    held.push({ name, value, settle: waiting.resolve });
    return waiting.promise;
  };
  const source = {
    writeSource(_frame, _view, input) {
      calls.push({ name: 'writeSource', input });
      return answer('writeSource', {
        path: input.path,
        revision: SAVED,
        size: input.bytes.byteLength,
        sha256: SHA256,
      });
    },
    setRuntimeData(_frame, _view, input) {
      calls.push({ name: 'setRuntimeData', input });
      return answer('setRuntimeData', {
        artifact_id: ARTIFACT,
        path: input.path,
        data_version: input.baseDataVersion + 1,
        changed: true,
        replayed: false,
      });
    },
  };
  return {
    calls,
    /** How many calls of `name` are held. */
    heldCount: (name) => held.filter((entry) => entry.name === name).length,
    /** Answers the oldest held call of `name`, with `value` or its own answer. */
    release(name, value) {
      const index = held.findIndex((entry) => entry.name === name);
      assert.notEqual(index, -1, `No ${name} call is held.`);
      const [entry] = held.splice(index, 1);
      entry.settle(value ?? entry.value);
    },
    createRuntimeView(artifactId, revision, viewId, generation, options) {
      calls.push({
        name: 'createRuntimeView',
        args: [artifactId, revision, viewId, generation],
        options,
      });
      // The client reports the dispatch, which is when the server starts the
      // lease.
      options.onDispatch?.();
      return answer('createRuntimeView', undefined);
    },
    async runtimeMetadata(view) {
      calls.push({ name: 'runtimeMetadata', view });
      return answer('runtimeMetadata', {
        artifact_id: ARTIFACT,
        title: 'Notes',
        // The artifact's revision as the view pins it.
        current_revision: view.head_revision,
        metadata_revision: 1,
      });
    },
    runtimeCall: (_view, run) => run(source, {}),
  };
}

/**
 * A frame connected at generation 1 on the fake clock. Its messages are kept in
 * `sent`, and in `events` together with each save the workspace is told of when
 * `onSaved` is asked for, in the order they happen.
 */
function connect({ hold = [], fail = {}, onSaved = false } = {}) {
  startClock();
  const client = fakeClient({ hold, fail });
  const events = [];
  const sent = [];
  const frame = connectFrame({
    client,
    view: runtimeView({ generation: 1 }),
    desk: createDesk(),
    dataChanges: new DataChangeHub(),
    deadline: DEADLINE,
    send: (message) => {
      sent.push(message);
      events.push({ event: 'sent', message });
    },
    onSaved: onSaved
      ? (revision) => {
          events.push({ event: 'saved', revision });
        }
      : undefined,
  });
  return { client, events, frame, sent };
}

/**
 * The calls the workspace counts while they are on their way (decision 0054,
 * rules 8 and 9): the call, the client call that answers it, and whether it is
 * a source.write, the one kind that sets sourceWriting.
 */
const CALL_KINDS = [
  {
    name: 'a metadata call',
    call: metadataCall,
    answer: 'runtimeMetadata',
    sourceWrite: false,
  },
  {
    name: 'a data write',
    call: dataWriteCall,
    answer: 'setRuntimeData',
    sourceWrite: false,
  },
  {
    name: 'a source.write',
    call: sourceWriteCall,
    answer: 'writeSource',
    sourceWrite: true,
  },
];

for (const kind of CALL_KINDS) {
  test(`${kind.name} on its way is counted by onItsWay, and sourceWriting is ${kind.sourceWrite} until the client answers`, async () => {
    const { client, frame } = connect({ hold: [kind.answer] });
    frame.handle(kind.call(1));
    await until(() => client.heldCount(kind.answer) === 1);
    assert.equal(frame.onItsWay(), 1, 'the call on its way is counted');
    assert.equal(
      frame.sourceWriting(),
      kind.sourceWrite,
      'only a source.write sets sourceWriting',
    );
    client.release(kind.answer);
    await until(() => frame.onItsWay() === 0, 'the call never ended');
    assert.equal(frame.sourceWriting(), false, 'the call has ended');
  });
}

for (const kind of CALL_KINDS) {
  test(`${kind.name} held during a pause is not counted by onItsWay, and sourceWriting is ${kind.sourceWrite}`, async () => {
    const { client, frame } = connect({ hold: [kind.answer] });
    await frame.pause('permissions');
    frame.handle(kind.call(1));
    await flush();
    assert.equal(frame.onItsWay(), 0, 'a held call is not on its way');
    assert.equal(
      frame.sourceWriting(),
      kind.sourceWrite,
      'sourceWriting follows the kind of call while it is held',
    );
    assert.deepEqual(
      client.calls.map((call) => call.name),
      [],
      'the client is not called while the call is held',
    );
    frame.resume();
    await until(
      () => client.heldCount(kind.answer) === 1,
      'the call was not sent when the pause ended',
    );
    assert.equal(
      frame.onItsWay(),
      1,
      'the call is on its way once the pause ends',
    );
    client.release(kind.answer);
    await until(() => frame.onItsWay() === 0, 'the call never ended');
  });
}

test('a committed source.write tells the workspace its revision before its receipt is posted, and the frame gets the receipt unchanged', async () => {
  const { events, frame, sent } = connect({ onSaved: true });
  frame.handle(sourceWriteCall(1));
  await until(() => sent.length === 1, 'the save was never answered');
  assert.deepEqual(
    events,
    [
      { event: 'saved', revision: SAVED },
      { event: 'sent', message: receiptReply(1) },
    ],
    'the revision is reported, then the receipt is posted',
  );
});

test('after a save, the next call is held until the saved adoption, then goes out under the new generation', async () => {
  const { client, frame, sent } = connect({ onSaved: true });
  frame.handle(sourceWriteCall(1));
  await until(() => sent.length === 1, 'the save was never answered');
  frame.handle(metadataCall(2));
  await flush();
  assert.deepEqual(
    client.calls.map((call) => call.name),
    ['writeSource'],
    'the next call is held: only the save reached the client',
  );

  const adopting = frame.adopt({
    reason: 'saved',
    revision: SAVED,
    generation: 2,
  });
  await until(
    () => client.heldCount('createRuntimeView') === 1,
    'the adoption was never asked',
  );
  const request = client.calls.find(
    (call) => call.name === 'createRuntimeView',
  );
  assert.deepEqual(
    request.args,
    [ARTIFACT, SAVED, VIEW_ID, 2],
    'the adoption asks for the saved revision as generation 2',
  );
  assert.deepEqual(
    request.options.adopt,
    { generation: 1 },
    'the adoption replaces generation 1',
  );
  client.release(
    'createRuntimeView',
    runtimeView({ generation: 2, head: SAVED }),
  );
  await adopting;
  await until(() => sent.length === 3, 'the held call was never answered');
  assert.deepEqual(
    sent.slice(1),
    [adopted('saved', LEASE_MS), metadataReply(2, SAVED)],
    'the frame is told of the adoption before the held call is answered',
  );
  assert.deepEqual(
    client.calls
      .filter((call) => call.name === 'runtimeMetadata')
      .map((call) => call.view.generation),
    [2],
    'the held call is served under the new generation',
  );
});

test('a save that commits while the grant is unresolved keeps the frame refused: the next call is answered unavailable, not held', async () => {
  const { client, events, frame, sent } = connect({
    hold: ['writeSource'],
    onSaved: true,
  });
  frame.handle(sourceWriteCall(1));
  await until(
    () => client.heldCount('writeSource') === 1,
    'the save never reached the client',
  );
  frame.refuse();
  client.release('writeSource');
  await until(() => sent.length === 1, 'the save was never answered');
  frame.handle(metadataCall(2));
  assert.deepEqual(
    sent,
    [receiptReply(1), { id: 2, ok: false, code: 'unavailable' }],
    'the save is answered, then the next call is refused at once, not held',
  );
  assert.deepEqual(
    client.calls.map((call) => call.name),
    ['writeSource'],
    'the refused call never reaches the client',
  );
  assert.deepEqual(
    events.filter((event) => event.event === 'saved'),
    [{ event: 'saved', revision: SAVED }],
    'the workspace is still told of the save, so the saved adoption can follow the grant',
  );
});

test('the saved adoption that follows a refused save ends the refusal: the next call is served under the new generation', async () => {
  const { client, frame, sent } = connect({
    hold: ['writeSource'],
    onSaved: true,
  });
  frame.handle(sourceWriteCall(1));
  await until(
    () => client.heldCount('writeSource') === 1,
    'the save never reached the client',
  );
  frame.refuse();
  client.release('writeSource');
  await until(() => sent.length === 1, 'the save was never answered');
  const adopting = frame.adopt({
    reason: 'saved',
    revision: SAVED,
    generation: 2,
  });
  await until(
    () => client.heldCount('createRuntimeView') === 1,
    'the adoption was never asked',
  );
  client.release(
    'createRuntimeView',
    runtimeView({ generation: 2, head: SAVED }),
  );
  await adopting;
  frame.handle(metadataCall(2));
  await until(() => sent.length === 3, 'the next call was never answered');
  assert.deepEqual(
    sent.slice(1),
    [adopted('saved', LEASE_MS), metadataReply(2, SAVED)],
    'the adoption is announced, then the next call is answered, not refused',
  );
  assert.deepEqual(
    client.calls
      .filter((call) => call.name === 'runtimeMetadata')
      .map((call) => call.view.generation),
    [2],
    'the next call is served under the new generation',
  );
});

/**
 * The two saves that pause nothing (decision 0054, rule 8): a save the client
 * rejects, and a committed save on a frame that was not told of saves. In both,
 * the frame's next call is served at once.
 */
const SAVES_THAT_PAUSE_NOTHING = [
  {
    name: 'a source.write the client rejects with a revision conflict',
    options: { onSaved: true, fail: { writeSource: CONFLICT } },
    reply: { id: 1, ok: false, code: 'stale_view' },
  },
  {
    name: 'a committed source.write on a frame without onSaved',
    options: { onSaved: false },
    reply: receiptReply(1),
  },
];

for (const save of SAVES_THAT_PAUSE_NOTHING) {
  test(`${save.name} pauses nothing: the next call is served at once`, async () => {
    const { client, events, frame, sent } = connect(save.options);
    frame.handle(sourceWriteCall(1));
    await until(() => sent.length === 1, 'the save was never answered');
    frame.handle(metadataCall(2));
    await until(() => sent.length === 2, 'the next call was never answered');
    assert.deepEqual(
      sent,
      [save.reply, metadataReply(2, REVISION)],
      'the save is answered, then the next call',
    );
    assert.deepEqual(
      client.calls.map((call) => call.name),
      ['writeSource', 'runtimeMetadata'],
      'the next call reaches the client at once',
    );
    assert.deepEqual(
      events.filter((event) => event.event === 'saved'),
      [],
      'the workspace is told of no save',
    );
  });
}
