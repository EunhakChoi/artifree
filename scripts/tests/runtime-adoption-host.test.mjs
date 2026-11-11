import assert from 'node:assert/strict';
import { afterEach, mock, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The workspace side of a frame whose view can be adopted (decision 0054).
// connectFrame is bundled as it ships, with its rpc, its change hints and the
// desk that createDesk makes. The client is a fake that answers as the server
// does, and the clock is fake: performance.now and the timers move only when a
// test moves them. The change hub is the page's DataChangeHub, wrapped to count
// the listeners of each artifact, so a listener that was never removed shows.
const bundle = await build({
  stdin: {
    contents: [
      "export { ACTIVATION_MS } from './offer-desk.ts';",
      "export { ADOPTION_WAIT_MS } from './frame-rpc.ts';",
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
const {
  ACTIVATION_MS,
  ADOPTION_WAIT_MS,
  DataChangeHub,
  connectFrame,
  createDesk,
} = await import(
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
);

/** The four reasons an adoption can have (decision 0054, rule 10). */
const REASONS = ['permissions', 'saved', 'head', 'lease'];
/** The reasons that withdraw a download prompt; a lease keeps it (rule 13). */
const WITHDRAWING = ['permissions', 'saved', 'head'];
/** The window a hint waits after its change (decision 0053, rule 17). */
const WINDOW_MS = 250;
/** The lease a view is issued with. No case below reaches its end. */
const LEASE_MS = 900_000;
/** The end of a frame's lease before any adoption, on the fake clock. */
const DEADLINE = 900_000;

const ARTIFACT = `art_${'a'.repeat(32)}`;
const VIEW_ID = 'd'.repeat(32);
const CURSOR = `${'f'.repeat(32)}.1`;
/** The source revision the views are pinned to, which is also their head. */
const REVISION = 1;
/** The data path the writes name. */
const PATH = 'notes/a';
/** The file a download offers, and the prompt the desk shows for it. */
const OFFER = { kind: 'file', path: 'a.txt', file_name: 'a.txt', size: 3 };
const PROMPT = { kind: 'file', path: 'a.txt', fileName: 'a.txt', size: 3 };
/** The bytes the client sends for the offer: three, as the offer says. */
const BYTES = Uint8Array.from([1, 2, 3]);
/** The server's metadata of the artifact, and the reply the frame gets. */
const METADATA = {
  artifact_id: ARTIFACT,
  title: 'Notes',
  current_revision: REVISION,
  metadata_revision: 1,
};
const METADATA_REPLY = {
  artifactId: ARTIFACT,
  title: 'Notes',
  currentRevision: REVISION,
  metadataRevision: 1,
};

afterEach(() => {
  mock.timers.reset();
  mock.restoreAll();
  delete globalThis.window;
  delete globalThis.document;
});

/**
 * Starts the fake clock at zero: performance.now reads it and setTimeout is
 * mocked on it. advance() moves both, so a timer fires when the clock reaches
 * its time.
 */
function startClock() {
  let now = 0;
  mock.timers.enable({ apis: ['setTimeout'] });
  mock.method(performance, 'now', () => now);
  return {
    advance(ms) {
      now += ms;
      mock.timers.tick(ms);
    },
  };
}

/**
 * The browser pieces a prompt and a save reach: the window's timers, which are
 * the fake ones, and the link a save is clicked on. The returned list holds the
 * file name of each save, in order.
 */
function installBrowser() {
  const saved = [];
  globalThis.window = {
    setTimeout: (...args) => setTimeout(...args),
    clearTimeout: (...args) => clearTimeout(...args),
  };
  globalThis.document = {
    body: { append: () => undefined },
    createElement: () => {
      const link = {
        click: () => {
          saved.push(link.download);
        },
        remove: () => undefined,
      };
      return link;
    },
  };
  return saved;
}

/** A promise that the test settles when it chooses. */
function deferred() {
  let resolve;
  const promise = new Promise((settle) => {
    resolve = settle;
  });
  return { promise, resolve };
}

/** The page's change hub, counting the listeners each artifact has. */
function countedHub() {
  const hub = new DataChangeHub();
  const counts = new Map();
  return {
    subscribe(artifactId, listener) {
      const remove = hub.subscribe(artifactId, listener);
      counts.set(artifactId, (counts.get(artifactId) ?? 0) + 1);
      let live = true;
      return () => {
        if (!live) return;
        live = false;
        counts.set(artifactId, counts.get(artifactId) - 1);
        remove();
      };
    },
    follow: hub.follow,
    listenerCount: (artifactId) => counts.get(artifactId) ?? 0,
  };
}

/** A runtime view of ARTIFACT as the server issues it, pinned to `head`. */
function runtimeView({
  generation,
  granted,
  leaseMs = LEASE_MS,
  head = REVISION,
}) {
  return {
    artifact_id: ARTIFACT,
    view_id: VIEW_ID,
    generation,
    granted,
    head_revision: head,
    revision: head,
    lease_ms: leaseMs,
    enabled: true,
    declared: ['data.read', 'data.write'],
    grant_version: 1,
    expires_at_ms: 1_000_000,
  };
}

/**
 * The client a frame and its workspace call, faked as the server answers. Each
 * call is recorded in `calls` with its name and what the tests check. An
 * adoption is answered when the test settles it: its answer waits in
 * `adoptions`, oldest first. Every other answer comes at once.
 */
function fakeClient() {
  const calls = [];
  const adoptions = [];
  const source = {
    setRuntimeData: async (_frame, _view, input) => {
      calls.push({ name: 'setRuntimeData', input });
      return {
        artifact_id: ARTIFACT,
        path: input.path,
        data_version: input.baseDataVersion + 1,
        changed: true,
        replayed: false,
      };
    },
  };
  return {
    calls,
    adoptions,
    createRuntimeView(artifactId, revision, viewId, generation, options) {
      calls.push({
        name: 'createRuntimeView',
        args: [artifactId, revision, viewId, generation],
        options,
      });
      // The client reports the dispatch, which is when the server starts the
      // lease.
      options.onDispatch?.();
      const answer = deferred();
      adoptions.push(answer);
      return answer.promise;
    },
    async runtimeMetadata(view) {
      calls.push({ name: 'runtimeMetadata', view });
      return METADATA;
    },
    async runtimeSourceOffer(view, target) {
      calls.push({ name: 'runtimeSourceOffer', view, target });
      return OFFER;
    },
    async runtimeSourceDownload(view) {
      calls.push({ name: 'runtimeSourceDownload', view });
      return BYTES;
    },
    runtimeCall: (_view, run) => run(source, {}),
  };
}

/**
 * A frame connected at generation 1 on the fake clock: its fake client, a real
 * desk, a counted hub, and the messages the frame is sent, in order.
 */
function connect({ granted = ['data.read'], deadline = DEADLINE } = {}) {
  const clock = startClock();
  const saved = installBrowser();
  const client = fakeClient();
  const desk = createDesk();
  const hub = countedHub();
  const sent = [];
  const frame = connectFrame({
    client,
    view: runtimeView({ generation: 1, granted }),
    desk,
    dataChanges: hub,
    deadline,
    send: (message) => {
      sent.push(message);
    },
  });
  return { clock, client, desk, frame, hub, saved, sent };
}

/** Waits, a turn at a time, until `condition` holds; the limit is a time. */
async function until(condition) {
  const deadline = Date.now() + 10_000;
  while (!condition()) {
    if (Date.now() > deadline) assert.fail('The condition never became true.');
    await flush();
  }
}

/** Lets every answer that is already settled reach the frame. */
function flush() {
  return new Promise((resolve) => {
    setImmediate(resolve);
  });
}

/** A data write to PATH that is write number `id`, on the base it names. */
function dataSet(id) {
  return {
    type: 'data.set',
    id,
    path: PATH,
    value: 'v',
    baseDataVersion: id - 1,
    retryId: `retry_${String(id).padStart(4, '0')}`,
  };
}

/** The change of a data write, as the change feed delivers it. */
function dataChanged(dataVersion, revision) {
  return {
    type: 'change',
    change: {
      artifact_id: ARTIFACT,
      change: 'data_changed',
      cursor: CURSOR,
      committed_at_ms: 1,
      revision,
      metadata_revision: 1,
      data_version: dataVersion,
    },
  };
}

/** The hint for `dataVersion`, as rule 16 gives it. */
function changed(dataVersion) {
  return { type: 'event', event: 'data.changed', dataVersion };
}

/** The event that tells a frame its view was adopted (rule 10). */
function adopted(reason, leaseMs) {
  return { type: 'event', event: 'view.adopted', reason, leaseMs };
}

for (const reason of REASONS) {
  test(`an adoption for ${reason} makes one request, and the frame is told before a held call is answered`, async () => {
    const { client, frame, sent } = connect();
    const adopting = frame.adopt({ reason, revision: REVISION, generation: 2 });
    await until(() => client.adoptions.length === 1);
    frame.handle({ type: 'metadata', id: 5 });
    await flush();
    assert.deepEqual(sent, [], 'a call received during the adoption is held');
    client.adoptions[0].resolve(
      runtimeView({ generation: 2, granted: ['data.read'] }),
    );
    await adopting;
    await until(() => sent.length === 2);
    const requests = client.calls.filter(
      (call) => call.name === 'createRuntimeView',
    );
    assert.equal(requests.length, 1, 'one adoption request is made');
    assert.deepEqual(requests[0].args, [ARTIFACT, REVISION, VIEW_ID, 2]);
    assert.deepEqual(requests[0].options.adopt, { generation: 1 });
    assert.deepEqual(
      sent,
      [
        adopted(reason, LEASE_MS),
        { id: 5, ok: true, metadata: METADATA_REPLY },
      ],
      'the frame is told of the adoption before the held call is answered',
    );
  });
}

// Rule 13: the hint follows the new view only while the new view holds
// data.read, and it is sent for a change at the new view's head.
const HINT_CASES = [
  {
    name: 'a view that holds data.read is sent the hint for a change at its head',
    granted: ['data.read'],
    listeners: 1,
    hints: [changed(5)],
  },
  {
    name: 'a view without data.read is sent no hint for the same change',
    granted: [],
    listeners: 1,
    hints: [],
  },
];
for (const hintCase of HINT_CASES) {
  test(`after an adoption, ${hintCase.name}`, async () => {
    const { clock, client, frame, hub, sent } = connect();
    const adopting = frame.adopt({
      reason: 'head',
      revision: REVISION,
      generation: 2,
    });
    await until(() => client.adoptions.length === 1);
    client.adoptions[0].resolve(
      runtimeView({ generation: 2, granted: hintCase.granted }),
    );
    await adopting;
    assert.equal(
      hub.listenerCount(ARTIFACT),
      hintCase.listeners,
      'the frame keeps one hub listener for its life, whatever the new view holds',
    );
    const before = sent.length;
    hub.follow(dataChanged(5, REVISION));
    clock.advance(WINDOW_MS - 1);
    assert.deepEqual(sent.slice(before), [], 'no hint is sent before 250 ms');
    clock.advance(1);
    assert.deepEqual(
      sent.slice(before),
      hintCase.hints,
      'the hint, if any, is sent once, at 250 ms',
    );
  });
}

/** When an adoption comes, in the cases below, into a change's window. */
const ADOPT_AT_MS = 100;
/**
 * A change waiting in its window when an adoption starts, and the view the
 * adoption brings: its grant and its head. The hint is sent only if the new
 * view holds data.read and pins the change's head (decision 0054, rule 13).
 */
const WAITING_CASES = [
  {
    name: 'a new view that holds data.read is sent the waiting version once',
    granted: ['data.read'],
    head: REVISION,
    hints: [changed(5)],
  },
  {
    name: 'a new view without data.read is sent no hint for the waiting version',
    granted: [],
    head: REVISION,
    hints: [],
  },
  {
    name: 'a new view with data.read that pins another head is sent no hint for the waiting version',
    granted: ['data.read'],
    head: REVISION + 1,
    hints: [],
  },
];
for (const waitingCase of WAITING_CASES) {
  test(`an adoption ${ADOPT_AT_MS} ms into the window of a change: ${waitingCase.name}`, async () => {
    const { clock, client, frame, hub, sent } = connect();
    hub.follow(dataChanged(5, REVISION));
    clock.advance(ADOPT_AT_MS);
    const adopting = frame.adopt({
      reason: 'head',
      revision: waitingCase.head,
      generation: 2,
    });
    await until(() => client.adoptions.length === 1);
    client.adoptions[0].resolve(
      runtimeView({
        generation: 2,
        granted: waitingCase.granted,
        head: waitingCase.head,
      }),
    );
    await adopting;
    const before = sent.length;
    clock.advance(WINDOW_MS - ADOPT_AT_MS - 1);
    assert.deepEqual(sent.slice(before), [], 'no hint is sent before 250 ms');
    clock.advance(1);
    assert.deepEqual(
      sent.slice(before),
      waitingCase.hints,
      'the waiting version is sent at 250 ms only if the new view allows it',
    );
    clock.advance(WINDOW_MS);
    assert.deepEqual(
      sent.slice(before),
      waitingCase.hints,
      'the waiting version is not sent again',
    );
  });
}

test('a version already told before an adoption is not told again after it, and a higher one is', async () => {
  const { clock, client, frame, hub, sent } = connect();
  hub.follow(dataChanged(5, REVISION));
  clock.advance(WINDOW_MS);
  assert.deepEqual(sent, [changed(5)], 'version 5 is told before the adoption');
  const adopting = frame.adopt({
    reason: 'head',
    revision: REVISION,
    generation: 2,
  });
  await until(() => client.adoptions.length === 1);
  client.adoptions[0].resolve(
    runtimeView({ generation: 2, granted: ['data.read'] }),
  );
  await adopting;
  const before = sent.length;
  // The hub replays version 5 to the frame, which is now bound to a new view.
  hub.follow(dataChanged(5, REVISION));
  clock.advance(WINDOW_MS);
  assert.deepEqual(
    sent.slice(before),
    [],
    'the replayed version 5 is not told again',
  );
  hub.follow(dataChanged(6, REVISION));
  clock.advance(WINDOW_MS);
  assert.deepEqual(
    sent.slice(before),
    [changed(6)],
    'the higher version 6 is told once',
  );
});

test('a download prompt stays open through a lease adoption and downloads under the new generation', async () => {
  const { clock, client, desk, frame, saved, sent } = connect();
  frame.handle({ type: 'source.download', id: 1, kind: 'file', path: 'a.txt' });
  await until(() => desk.snapshot().offer !== undefined);
  const adopting = frame.adopt({
    reason: 'lease',
    revision: REVISION,
    generation: 2,
  });
  await until(() => client.adoptions.length === 1);
  client.adoptions[0].resolve(
    runtimeView({ generation: 2, granted: ['data.read'] }),
  );
  await adopting;
  assert.deepEqual(desk.snapshot().offer, PROMPT, 'the offer is still open');
  clock.advance(ACTIVATION_MS);
  assert.equal(desk.accept(true), true, 'the press counts after the delay');
  await until(() => sent.length === 2);
  const downloads = client.calls.filter(
    (call) => call.name === 'runtimeSourceDownload',
  );
  assert.deepEqual(
    downloads.map((call) => call.view.generation),
    [2],
    'the bytes are fetched under the new generation',
  );
  assert.deepEqual(
    sent,
    [
      adopted('lease', LEASE_MS),
      { id: 1, ok: true, receipt: { started: true, kind: 'file', bytes: 3 } },
    ],
    'the frame receives the receipt of the download',
  );
  assert.deepEqual(
    saved,
    ['a.txt'],
    'the bytes are saved under the offer name',
  );
});

for (const reason of WITHDRAWING) {
  test(`a ${reason} pause withdraws a download prompt and answers the download unavailable`, async () => {
    const { desk, frame, sent } = connect();
    frame.handle({
      type: 'source.download',
      id: 1,
      kind: 'file',
      path: 'a.txt',
    });
    await until(() => desk.snapshot().offer !== undefined);
    await frame.pause(reason);
    assert.equal(desk.snapshot().offer, undefined, 'the offer is closed');
    assert.deepEqual(
      sent,
      [{ id: 1, ok: false, code: 'unavailable' }],
      'the frame is told the download is unavailable',
    );
    await flush();
    assert.deepEqual(
      sent,
      [{ id: 1, ok: false, code: 'unavailable' }],
      'the download is not answered again',
    );
  });
}

test('an old-generation answer that arrives after the adoption is sent nothing', async () => {
  const { clock, client, frame, sent } = connect();
  const late = deferred();
  client.runtimeMetadata = async (view) => {
    client.calls.push({ name: 'runtimeMetadata', view });
    return late.promise;
  };
  frame.handle({ type: 'metadata', id: 3 });
  await until(() =>
    client.calls.some((call) => call.name === 'runtimeMetadata'),
  );
  const adopting = frame.adopt({
    reason: 'permissions',
    revision: REVISION,
    generation: 2,
  });
  clock.advance(ADOPTION_WAIT_MS);
  await until(() => client.adoptions.length === 1);
  const unavailable = { id: 3, ok: false, code: 'unavailable' };
  assert.deepEqual(
    sent,
    [unavailable],
    'the call that has not ended is abandoned after 5 s',
  );
  client.adoptions[0].resolve(
    runtimeView({ generation: 2, granted: ['data.read'] }),
  );
  await adopting;
  const event = adopted('permissions', LEASE_MS);
  assert.deepEqual(sent, [unavailable, event]);
  late.resolve(METADATA);
  await flush();
  assert.deepEqual(
    sent,
    [unavailable, event],
    'the late answer of generation 1 is not sent',
  );
});

test('an adoption does not refill the data write budget, so the eleventh write is refused as rate_limited', async () => {
  const { client, frame, sent } = connect({
    granted: ['data.read', 'data.write'],
  });
  for (let id = 1; id <= 10; id += 1) {
    frame.handle(dataSet(id));
    await until(() => sent.length === id);
  }
  assert.deepEqual(
    sent.map((message) => message.ok),
    Array(10).fill(true),
    'the first ten writes are answered ok under generation 1',
  );
  const adopting = frame.adopt({
    reason: 'permissions',
    revision: REVISION,
    generation: 2,
  });
  await until(() => client.adoptions.length === 1);
  client.adoptions[0].resolve(
    runtimeView({ generation: 2, granted: ['data.read', 'data.write'] }),
  );
  await adopting;
  frame.handle(dataSet(11));
  await until(() => sent.length === 12);
  assert.deepEqual(
    sent.slice(10),
    [
      adopted('permissions', LEASE_MS),
      { id: 11, ok: false, code: 'rate_limited' },
    ],
    'the eleventh write is refused without a token',
  );
  assert.equal(
    client.calls.filter((call) => call.name === 'setRuntimeData').length,
    10,
    'only the first ten writes reached the server',
  );
});

test('after an adoption the new lease governs the calls, so a call after the old lease is answered', async () => {
  const { clock, client, frame, sent } = connect({ deadline: 1_000 });
  clock.advance(900);
  const adopting = frame.adopt({
    reason: 'head',
    revision: REVISION,
    generation: 2,
  });
  await until(() => client.adoptions.length === 1);
  client.adoptions[0].resolve(
    runtimeView({ generation: 2, granted: ['data.read'], leaseMs: 60_000 }),
  );
  await adopting;
  // The old lease ended at 1000 ms; the adoption moved the end to 60900 ms.
  clock.advance(600);
  frame.handle({ type: 'metadata', id: 9 });
  await until(() => sent.length === 2);
  assert.deepEqual(
    sent,
    [adopted('head', 60_000), { id: 9, ok: true, metadata: METADATA_REPLY }],
    'the call at 1500 ms is answered under the new lease',
  );
});

test('without an adoption, a call after the lease the frame was given is not answered', async () => {
  const { clock, frame, sent } = connect({ deadline: 1_000 });
  clock.advance(1_500);
  frame.handle({ type: 'metadata', id: 9 });
  await flush();
  assert.deepEqual(sent, [], 'the lease of 1000 ms has ended');
});

/** How many adoption requests the client has been asked to make. */
function adoptionRequests(client) {
  const requests = client.calls.filter(
    (call) => call.name === 'createRuntimeView',
  );
  return requests.length;
}

test('a second adoption while one is on its way is rejected without asking the server', async () => {
  const { client, frame } = connect();
  const first = frame.adopt({
    reason: 'head',
    revision: REVISION,
    generation: 2,
  });
  await until(() => client.adoptions.length === 1);
  // The rejection is attached before the count is read, so a second adoption
  // that is wrongly accepted fails the count and does not leave the test
  // waiting on a request that is never answered.
  const second = assert.rejects(
    frame.adopt({ reason: 'head', revision: REVISION, generation: 2 }),
    { name: 'Error', message: 'An adoption is already on its way.' },
  );
  await flush();
  assert.equal(
    adoptionRequests(client),
    1,
    'the second adoption does not ask the server',
  );
  await second;
  const view = runtimeView({ generation: 2, granted: ['data.read'] });
  client.adoptions[0].resolve(view);
  assert.deepEqual((await first).view, view, 'the first adoption succeeds');
  // The first adoption has ended, so the flag is released and a later one is
  // accepted.
  const later = frame.adopt({
    reason: 'head',
    revision: REVISION,
    generation: 3,
  });
  await until(() => client.adoptions.length === 2);
  client.adoptions[1].resolve(
    runtimeView({ generation: 3, granted: ['data.read'] }),
  );
  assert.equal(
    (await later).view.generation,
    3,
    'a later adoption is accepted',
  );
});

for (const end of ['close', 'fail']) {
  test(`an adoption after the frame ended is rejected without asking the server, after ${end}()`, async () => {
    const { client, frame } = connect();
    frame[end]();
    // As in the case above, the rejection is attached before the count is read.
    const rejection = assert.rejects(
      frame.adopt({ reason: 'head', revision: REVISION, generation: 2 }),
      { name: 'Error', message: 'The frame has ended.' },
    );
    await flush();
    assert.equal(
      adoptionRequests(client),
      0,
      'the server is not asked once the frame has ended',
    );
    await rejection;
  });
}

test('an adoption that waits for a write which close() interrupts rejects without asking the server', async () => {
  const { client, frame, sent } = connect();
  const write = deferred();
  client.runtimeCall = () => {
    client.calls.push({ name: 'runtimeCall' });
    return write.promise;
  };
  frame.handle(dataSet(1));
  await until(() => client.calls.some((call) => call.name === 'runtimeCall'));
  const adopting = frame.adopt({
    reason: 'head',
    revision: REVISION,
    generation: 2,
  });
  // The rejection is attached before the count is read, as in the cases above.
  const rejection = assert.rejects(adopting, {
    name: 'Error',
    message: 'The frame has ended.',
  });
  frame.close();
  // The write ends only now: the adoption waits for it, then finds the frame
  // ended.
  write.resolve({
    artifact_id: ARTIFACT,
    path: PATH,
    data_version: 2,
    changed: true,
    replayed: false,
  });
  await flush();
  assert.equal(
    adoptionRequests(client),
    0,
    'the server is not asked once the frame has ended',
  );
  await rejection;
  assert.deepEqual(sent, [], 'the frame is told nothing');
});

test('resume() and refuse() throw while an adoption is on its way', async () => {
  const { client, frame, sent } = connect();
  const adopting = frame.adopt({
    reason: 'head',
    revision: REVISION,
    generation: 2,
  });
  await until(() => client.adoptions.length === 1);
  assert.throws(() => frame.resume(), {
    name: 'Error',
    message: 'An adoption is on its way.',
  });
  assert.throws(() => frame.refuse(), {
    name: 'Error',
    message: 'An adoption is on its way.',
  });
  client.adoptions[0].resolve(
    runtimeView({ generation: 2, granted: ['data.read'] }),
  );
  await adopting;
  assert.deepEqual(
    sent,
    [adopted('head', LEASE_MS)],
    'the adoption completes as if the two calls had not been made',
  );
});

test('a pause asked for during an adoption resolves after it and holds later calls under the new generation', async () => {
  const { client, frame, sent } = connect();
  const adopting = frame.adopt({
    reason: 'head',
    revision: REVISION,
    generation: 2,
  });
  await until(() => client.adoptions.length === 1);
  let paused = false;
  const pausing = frame.pause('lease').then(() => {
    paused = true;
  });
  await flush();
  assert.equal(paused, false, 'the pause waits for the adoption on its way');
  client.adoptions[0].resolve(
    runtimeView({ generation: 2, granted: ['data.read'] }),
  );
  await adopting;
  await pausing;
  frame.handle({ type: 'metadata', id: 7 });
  await flush();
  assert.deepEqual(
    client.calls.filter((call) => call.name === 'runtimeMetadata'),
    [],
    'a call received after the pause is held, not served',
  );
  frame.resume();
  await until(() => sent.length === 2);
  assert.deepEqual(
    client.calls
      .filter((call) => call.name === 'runtimeMetadata')
      .map((call) => call.view.generation),
    [2],
    'the held call is served under generation 2 once the pause ends',
  );
  assert.deepEqual(
    sent,
    [adopted('head', LEASE_MS), { id: 7, ok: true, metadata: METADATA_REPLY }],
    'the frame is told of the adoption, then receives the held call answer',
  );
});
