import assert from 'node:assert/strict';
import { afterEach, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// createRpc is where the host gives a frame its data write budget (decision
// 0053, rule 23). These tests reach that budget through createRpc itself, so a
// wiring that passes the general bucket, or drops the write constants, fails
// here. createRpc is bundled as it ships. It takes no clock: its budgets read
// performance.now(), which each test replaces with a clock the test moves and
// which is restored afterwards. A frame's message reaches the rpc through
// handle(), as the port handler in runtime-frame.tsx gives it. The client is a
// fake that answers as the real one would.
const bundle = await build({
  entryPoints: [
    fileURLToPath(
      new URL('../../apps/web/src/artifacts/runtime-host.ts', import.meta.url),
    ),
  ],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  // lucide-react has no exports map, so its module field is read as the
  // browser build reads it. The neutral platform reads no field unless named.
  mainFields: ['module', 'main'],
  write: false,
});
const { createDesk, createRpc } = await import(
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
);

const ARTIFACT = `art_${'0'.repeat(32)}`;
/** The frame transport that a source call is given. */
const FRAME = Object.freeze({ transport: 'frame' });
/** The data path every call names. */
const PATH = 'notes/a';
/** A retry identity of the length and alphabet rule 15 gives. */
const RETRY_ID = 'retry_0001';
/** The lease of the frame's view, on the same clock as the budgets. */
const LEASE_MS = 900_000;

/** A view that holds the data.read and data.write grants. */
const view = {
  url: `http://127.0.0.1:7411/r/${'b'.repeat(64)}/`,
  enabled: true,
  session_id: 'e'.repeat(32),
  artifact_id: ARTIFACT,
  revision: 3,
  head_revision: 3,
  sdk: 'artifree.app.v1',
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

/** What the client answers a read of PATH that holds a value. */
const FOUND = {
  artifact_id: ARTIFACT,
  path: PATH,
  data_version: 3,
  exists: true,
  value: '{"a":1}',
};
/** The receipt the client gives for removing the value at PATH: a change. */
const RECEIPT = {
  artifact_id: ARTIFACT,
  path: PATH,
  data_version: 4,
  changed: true,
  replayed: false,
};
/** What the client answers a metadata read of the artifact. */
const METADATA = {
  artifact_id: ARTIFACT,
  title: 'Notes',
  current_revision: 3,
  metadata_revision: 1,
};

const realNow = Object.getOwnPropertyDescriptor(performance, 'now');

/** Makes performance.now() read `clock`, the clock the budgets read. */
function installClock(clock) {
  Object.defineProperty(performance, 'now', {
    value: () => clock.ms,
    configurable: true,
    writable: true,
  });
}

afterEach(() => {
  if (realNow === undefined) delete performance.now;
  else Object.defineProperty(performance, 'now', realNow);
});

/**
 * Waits, a turn at a time, for a condition made by asynchronous work. A served
 * write is answered after the client, a refused one at once, so replies can
 * arrive in another order than the calls. A test therefore waits for each
 * reply before it makes the next call: then `sent[n]` answers call `n + 1`.
 */
async function until(condition) {
  const deadline = Date.now() + 10_000;
  while (!condition()) {
    if (Date.now() > deadline) assert.fail('The condition never became true.');
    await new Promise((resolve) => setImmediate(resolve));
  }
}

/**
 * The client the rpc is given. Each call that reaches it is named in `calls`,
 * in order, and answered as the server answers it.
 */
function fakeClient(calls) {
  const source = {
    readRuntimeData: async () => {
      calls.push('readRuntimeData');
      return FOUND;
    },
    removeRuntimeData: async () => {
      calls.push('removeRuntimeData');
      return RECEIPT;
    },
  };
  return {
    runtimeCall: (_view, run) => run(source, FRAME),
    runtimeMetadata: async () => {
      calls.push('runtimeMetadata');
      return METADATA;
    },
  };
}

/**
 * An rpc wired as runtime-host.ts wires it: the host's own createRpc, with the
 * fake client, the host's desk and a lease that outlasts the test. `sent` holds
 * what the frame was told, `calls` what reached the client, and `clock` is the
 * time the budgets read, which starts at zero.
 */
function wire() {
  const clock = { ms: 0 };
  installClock(clock);
  const calls = [];
  const sent = [];
  const rpc = createRpc({
    client: fakeClient(calls),
    view,
    desk: createDesk(),
    deadline: clock.ms + LEASE_MS,
    send: (message, transfer) => sent.push({ message, transfer }),
  });
  return { rpc, sent, calls, clock };
}

/** A data.remove of PATH under call id `id`, on the base the view holds. */
function removal(id) {
  return {
    type: 'data.remove',
    id,
    path: PATH,
    baseDataVersion: 3,
    retryId: RETRY_ID,
  };
}

test('ten data writes in a row reach the client, and the eleventh is refused as rate_limited without one', async () => {
  const { rpc, sent, calls } = wire();
  for (let id = 1; id <= 10; id += 1) rpc.handle(removal(id));
  await until(() => sent.length === 10);
  assert.deepEqual(
    sent.map(({ message }) => message.ok),
    Array(10).fill(true),
    'the first ten writes are answered ok',
  );
  assert.deepEqual(
    calls,
    Array(10).fill('removeRuntimeData'),
    'the first ten writes reached the client',
  );

  rpc.handle(removal(11));
  await until(() => sent.length === 11);
  assert.deepEqual(sent[10], {
    message: { id: 11, ok: false, code: 'rate_limited' },
    transfer: [],
  });
  assert.deepEqual(
    calls,
    Array(10).fill('removeRuntimeData'),
    'the eleventh write reached no client',
  );
});

/** Calls that are not writes, each with the reply the client's answer gives. */
const OTHER_CALLS = [
  [
    'a data read',
    { type: 'data.get', path: PATH },
    {
      ok: true,
      data: { path: PATH, dataVersion: 3, exists: true, value: '{"a":1}' },
    },
  ],
  [
    'a metadata read',
    { type: 'metadata' },
    {
      ok: true,
      metadata: {
        artifactId: ARTIFACT,
        title: 'Notes',
        currentRevision: 3,
        metadataRevision: 1,
      },
    },
  ],
];
for (const [name, call, reply] of OTHER_CALLS)
  test(`${name} is still served after the write budget is empty`, async () => {
    const { rpc, sent } = wire();
    for (let id = 1; id <= 10; id += 1) rpc.handle(removal(id));
    await until(() => sent.length === 10);
    rpc.handle(removal(11));
    await until(() => sent.length === 11);
    assert.deepEqual(
      sent[10].message,
      { id: 11, ok: false, code: 'rate_limited' },
      'the write budget is empty',
    );
    rpc.handle({ ...call, id: 12 });
    await until(() => sent.length === 12);
    assert.deepEqual(sent[11].message, { id: 12, ...reply });
  });

test('the write budget refills one write a second: none at 999 ms and one at 1,000 ms', async () => {
  const { rpc, sent, calls, clock } = wire();
  for (let id = 1; id <= 10; id += 1) rpc.handle(removal(id));
  await until(() => sent.length === 10);
  clock.ms = 999;
  rpc.handle(removal(11));
  await until(() => sent.length === 11);
  assert.deepEqual(
    sent[10].message,
    { id: 11, ok: false, code: 'rate_limited' },
    'no write has returned at 999 ms',
  );
  clock.ms = 1_000;
  rpc.handle(removal(12));
  await until(() => sent.length === 12);
  assert.deepEqual(sent[11].message, {
    id: 12,
    ok: true,
    written: { path: PATH, dataVersion: 4, changed: true },
  });
  rpc.handle(removal(13));
  await until(() => sent.length === 13);
  assert.deepEqual(
    sent[12].message,
    { id: 13, ok: false, code: 'rate_limited' },
    'only the one write has returned at 1,000 ms',
  );
  assert.equal(calls.length, 11, 'the refused writes reached no client');
});
