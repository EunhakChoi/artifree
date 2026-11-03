import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';
import { ArtifreeClient, isRetryId } from '../../packages/client/dist/index.js';

// The workspace host's rules are web code without DOM or React imports;
// bundle them as they ship and drive them with an injected clock.
const bundle = await build({
  stdin: {
    contents: [
      "export * from './frame-budget.ts';",
      "export * from './offer-desk.ts';",
      "export * from './frame-rpc.ts';",
      "export * from './save-blob.ts';",
    ].join('\n'),
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
  ACTIVATION_MS,
  BlobSaver,
  DECLINE_COOLDOWN_MS,
  FRAME_BURST,
  FRAME_PENDING_CAP,
  FrameRpc,
  OBJECT_URL_LIFETIME_MS,
  OfferDesk,
  TokenBucket,
  failureCode,
  parseCall,
} = await import(
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
);

/** A clock the test moves; timers run when it passes them. */
function clock() {
  let now = 1000;
  const timers = [];
  return {
    now: () => now,
    after(ms, run) {
      const timer = { at: now + ms, run, live: true };
      timers.push(timer);
      return () => {
        timer.live = false;
      };
    },
    advance(ms) {
      now += ms;
      for (const timer of timers)
        if (timer.live && timer.at <= now) {
          timer.live = false;
          timer.run();
        }
    },
    back(ms) {
      now -= ms;
    },
    live: () => timers.filter((timer) => timer.live).length,
  };
}

/** Waits, a turn at a time, for a condition made by real asynchronous work. */
async function until(condition) {
  for (let turn = 0; turn < 1000; turn += 1) {
    if (condition()) return;
    await new Promise((resolve) => setImmediate(resolve));
  }
  assert.fail('The condition never became true.');
}

async function settle() {
  for (let round = 0; round < 20; round += 1) await Promise.resolve();
}

test('The frame budget refills with time and never goes negative or over the burst', () => {
  const time = clock();
  const bucket = new TokenBucket(time.now, 3, 60);
  assert.deepEqual(
    [1, 2, 3, 4, 5].map(() => bucket.take()),
    [true, true, true, false, false],
  );
  time.advance(999);
  assert.equal(bucket.take(), false);
  time.advance(1);
  assert.equal(bucket.take(), true);
  assert.equal(bucket.take(), false);
  time.back(60_000);
  assert.equal(bucket.take(), false, 'a clock that steps back refills nothing');
  time.advance(3_600_000);
  assert.deepEqual(
    [1, 2, 3, 4].map(() => bucket.take()),
    [true, true, true, false],
  );
});

const offer = {
  kind: 'file',
  path: 'a.txt',
  fileName: 'a.txt',
  size: 3,
};

/** A signal that counts its listeners, so a leak is visible. */
function countingSignal() {
  const listeners = new Set();
  return {
    aborted: false,
    addEventListener: (_type, listener) => listeners.add(listener),
    removeEventListener: (_type, listener) => listeners.delete(listener),
    abort() {
      this.aborted = true;
      for (const listener of listeners) listener();
    },
    listeners: () => listeners.size,
  };
}

test('An offer counts only after the delay, only when trusted, and only once', async () => {
  const time = clock();
  const desk = new OfferDesk(time);
  const signal = countingSignal();
  const answer = desk.ask(offer, signal);
  assert.deepEqual(desk.snapshot(), { offer, armed: false });
  assert.equal(desk.accept(true), false, 'before the delay');
  time.advance(ACTIVATION_MS - 1);
  assert.equal(desk.accept(true), false, 'one tick before the delay');
  assert.equal(desk.snapshot().armed, false);
  time.advance(1);
  assert.equal(desk.snapshot().armed, true);
  assert.equal(desk.accept(false), false, 'a press the browser did not make');
  assert.notEqual(desk.snapshot().offer, undefined);
  // Two presses in one turn: the second finds nothing open.
  assert.deepEqual([desk.accept(true), desk.accept(true)], [true, false]);
  assert.equal(await answer, 'accepted');
  assert.equal(desk.snapshot().offer, undefined);
  assert.equal(time.live(), 0, 'no timer is left');
  assert.equal(signal.listeners(), 0, 'no listener is left');
});

test('Declining works at once, starts a cooldown, and the cooldown ends', async () => {
  const time = clock();
  const desk = new OfferDesk(time);
  const first = desk.ask(offer, countingSignal());
  assert.equal(desk.decline(), true);
  assert.equal(await first, 'declined');
  assert.equal(desk.decline(), false);
  assert.equal(await desk.ask(offer, countingSignal()), 'cooling');
  assert.equal(desk.snapshot().offer, undefined, 'nothing was shown');
  time.advance(DECLINE_COOLDOWN_MS - 1);
  assert.equal(await desk.ask(offer, countingSignal()), 'cooling');
  time.advance(1);
  const next = desk.ask(offer, countingSignal());
  assert.notEqual(desk.snapshot().offer, undefined);
  desk.withdraw();
  assert.equal(await next, 'withdrawn');
});

test('Only one offer is open at a time and a second does not disturb the first', async () => {
  const time = clock();
  const desk = new OfferDesk(time);
  const first = desk.ask(offer, countingSignal());
  assert.equal(
    await desk.ask({ ...offer, path: 'b.txt' }, countingSignal()),
    'busy',
  );
  assert.equal(desk.snapshot().offer, offer);
  time.advance(ACTIVATION_MS);
  assert.equal(desk.accept(true), true);
  assert.equal(await first, 'accepted');
});

test('An offer is withdrawn when the frame gives up, goes away, or was already gone', async () => {
  const time = clock();
  const desk = new OfferDesk(time);
  // Already aborted: nothing is shown and nothing is left behind.
  const gone = countingSignal();
  gone.abort();
  assert.equal(await desk.ask(offer, gone), 'withdrawn');
  assert.equal(desk.snapshot().offer, undefined);
  assert.equal(time.live(), 0);
  assert.equal(gone.listeners(), 0);
  // The frame gives up while the prompt is open.
  const giveUp = countingSignal();
  const first = desk.ask(offer, giveUp);
  giveUp.abort();
  assert.equal(await first, 'withdrawn');
  assert.equal(desk.snapshot().offer, undefined);
  assert.equal(time.live(), 0);
  assert.equal(giveUp.listeners(), 0);
  // A withdrawal is not a decline: the next offer is not refused.
  const signal = countingSignal();
  const second = desk.ask(offer, signal);
  assert.notEqual(desk.snapshot().offer, undefined);
  // The frame goes away.
  desk.withdraw();
  assert.equal(await second, 'withdrawn');
  assert.equal(time.live(), 0);
  assert.equal(signal.listeners(), 0);
  desk.withdraw();
});

test('Listeners hear every change and can leave', async () => {
  const time = clock();
  const desk = new OfferDesk(time);
  let heard = 0;
  const leave = desk.subscribe(() => {
    heard += 1;
  });
  const answer = desk.ask(offer, countingSignal());
  time.advance(ACTIVATION_MS);
  desk.decline();
  await answer;
  assert.equal(heard, 3, 'shown, armed, closed');
  leave();
  desk.ask(offer, countingSignal()).catch(() => undefined);
  assert.equal(heard, 3);
});

// ---- the rpc ----

const view = { artifact_id: 'art', view_id: 'v', generation: 1 };

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
  let reject;
  const promise = new Promise((ok, fail) => {
    resolve = ok;
    reject = fail;
  });
  return { promise, resolve, reject };
}

/** A rpc whose client, prompt and saver the test controls. */
function rpcWith(overrides = {}) {
  const time = clock();
  const log = [];
  const sent = [];
  const client = {
    async runtimeMetadata() {
      log.push('metadata');
      return {
        artifact_id: 'art',
        title: 't',
        current_revision: 1,
        metadata_revision: 1,
      };
    },
    async runtimeSourceList() {
      log.push('list');
      return {
        revision: 1,
        files: [
          {
            path: 'a.txt',
            size: 3,
            sha256: 'f'.repeat(64),
            media_type: 'text/plain',
          },
        ],
      };
    },
    async runtimeSourceRead(_view, request) {
      log.push(`read ${request.path}`);
      return {
        path: request.path,
        offset: request.offset,
        nextOffset: null,
        size: 3,
        sha256: 'f'.repeat(64),
        bytes: Uint8Array.from([1, 2, 3]),
      };
    },
    async runtimeSourceOffer(_view, target) {
      log.push('offer');
      return target.kind === 'file'
        ? {
            kind: 'file',
            path: target.path,
            file_name: 'a.txt',
            size: 3,
            sha256: 'f'.repeat(64),
          }
        : {
            kind: 'archive',
            path: null,
            file_name: 'x-r1.zip',
            size: null,
            sha256: null,
          };
    },
    async runtimeSourceDownload() {
      log.push('download');
      return Uint8Array.from([9, 9, 9]);
    },
    ...overrides.client,
  };
  const saved = [];
  const bucket = new TokenBucket(time.now);
  const rpc = new FrameRpc({
    client: overrides.realClient ?? client,
    view: overrides.view ?? view,
    ask: async (shown, signal) => {
      log.push('ask');
      return (overrides.ask ?? (async () => 'accepted'))(shown, signal);
    },
    save: (bytes, name, type) => saved.push({ bytes: [...bytes], name, type }),
    take: overrides.take ?? (() => bucket.take()),
    now: time.now,
    deadline: overrides.deadline ?? time.now() + 900_000,
    send:
      overrides.send ??
      ((message, transfer) => sent.push({ message, transfer })),
  });
  return { rpc, sent, log, saved, time, client };
}

test('A message is a call only with exactly its fields and bounded values', () => {
  const good = [
    { type: 'metadata', id: 1 },
    { type: 'source.list', id: 2 },
    { type: 'cancel', id: 3 },
    { type: 'source.read', id: 4, path: 'a/b.txt', offset: 0, length: 1 },
    {
      type: 'source.read',
      id: 5,
      path: 'a.txt',
      offset: 2 ** 53 - 1,
      length: 1_048_576,
    },
    { type: 'source.download', id: 6, kind: 'file', path: 'a.txt' },
    { type: 'source.download', id: 7, kind: 'archive', path: null },
  ];
  for (const message of good)
    assert.deepEqual(parseCall(message)?.id, message.id);
  const read = {
    type: 'source.read',
    id: 1,
    path: 'a.txt',
    offset: 0,
    length: 1,
  };
  const download = {
    type: 'source.download',
    id: 1,
    kind: 'file',
    path: 'a.txt',
  };
  const bad = [
    null,
    undefined,
    'metadata',
    5,
    [],
    {},
    { type: 'metadata' },
    { type: 'metadata', id: 0 },
    { type: 'metadata', id: -1 },
    { type: 'metadata', id: 1.5 },
    { type: 'metadata', id: '1' },
    { type: 'metadata', id: Number.NaN },
    { type: 'metadata', id: 2 ** 53 },
    { type: 'metadata', id: 1, extra: true },
    { type: 'source.list', id: 1, path: 'a.txt' },
    { type: '__proto__', id: 1 },
    { type: 'constructor', id: 1 },
    { type: 'toString', id: 1 },
    { type: 7, id: 1 },
    { ...read, path: '../a.txt' },
    { ...read, path: 'a b.txt' },
    { ...read, path: 'a'.repeat(300) + '.txt' },
    { ...read, path: '' },
    { ...read, path: 5 },
    { ...read, offset: -1 },
    { ...read, offset: 0.5 },
    { ...read, offset: 2 ** 53 },
    { ...read, length: 0 },
    { ...read, length: 1_048_577 },
    { ...read, length: 1.5 },
    { ...read, length: '1' },
    { type: 'source.read', id: 1, path: 'a.txt', offset: 0 },
    { ...download, path: null },
    { ...download, kind: 'archive' },
    { ...download, kind: 'folder' },
    { ...download, path: '../x' },
    { type: 'source.download', id: 1, kind: 'archive' },
    { type: 'source.download', id: 1, path: 'a.txt' },
  ];
  for (const message of bad)
    assert.equal(parseCall(message), undefined, JSON.stringify(message));
  assert.equal(
    parseCall(Object.create({ type: 'metadata', id: 1 })),
    undefined,
  );
});

test('The host judges the shared path fixture as the domain does', async () => {
  const { cases } = JSON.parse(
    await readFile(
      new URL('../../contracts/fixtures/package-paths.json', import.meta.url),
      'utf8',
    ),
  );
  assert.ok(cases.length >= 30);
  for (const { path, error } of cases) {
    const read = parseCall({
      type: 'source.read',
      id: 1,
      path,
      offset: 0,
      length: 1,
    });
    const download = parseCall({
      type: 'source.download',
      id: 1,
      kind: 'file',
      path,
    });
    const write = parseCall({ ...save, path });
    assert.equal(read !== undefined, error === null, JSON.stringify(path));
    assert.equal(download !== undefined, error === null, JSON.stringify(path));
    assert.equal(write !== undefined, error === null, JSON.stringify(path));
  }
  // The paths a person might actually mistype.
  for (const path of ['../a.txt', 'a b.txt', 'a//b.txt', 'LICENSE', '/a.txt'])
    assert.equal(
      parseCall({ type: 'source.read', id: 1, path, offset: 0, length: 1 }),
      undefined,
      path,
    );
});

test('The frame is told a closed word for a failure, never the failure', () => {
  const table = [
    [new StatusError(404, 'not_found'), 'not_found'],
    [new StatusError(403, 'forbidden'), 'denied'],
    [new StatusError(409, 'revision_conflict'), 'stale_view'],
    [new StatusError(409, 'data_conflict'), 'unavailable'],
    [new StatusError(429, 'rate_limited'), 'rate_limited'],
    [new StatusError(400, 'invalid_request'), 'invalid_request'],
    [new StatusError(413, 'payload_too_large'), 'invalid_request'],
    [new StatusError(416, 'invalid_request'), 'invalid_request'],
    [new StatusError(422, 'invalid_request'), 'invalid_request'],
    [new StatusError(422, 'request_key_reused'), 'invalid_request'],
    [new StatusError(500, 'internal'), 'unavailable'],
    [new StatusError(503, 'unavailable'), 'unavailable'],
    [new StatusError(401, 'unauthenticated'), 'unavailable'],
    // Only the client's own marker says the connection ended; another status
    // 0 failure that shares its code is an ordinary bad request.
    [
      Object.assign(new StatusError(0, 'invalid_request'), {
        connectionReplaced: true,
      }),
      'expired',
    ],
    [new StatusError(0, 'invalid_request'), 'invalid_request'],
    [new StatusError(0, 'network_error'), 'unavailable'],
    [new Error('SECRET'), 'unavailable'],
    [{ status: 404, name: 'ArtifreeApiError' }, 'not_found'],
    [{ status: 404 }, 'unavailable'],
    ['SECRET', 'unavailable'],
    [null, 'unavailable'],
    [undefined, 'unavailable'],
  ];
  for (const [error, expected] of table)
    assert.equal(failureCode(error), expected);
});

test('Calls are answered from the server and a reply carries only what was read', async () => {
  const { rpc, sent } = rpcWith();
  rpc.handle({ type: 'metadata', id: 1 });
  rpc.handle({ type: 'source.list', id: 2 });
  rpc.handle({
    type: 'source.read',
    id: 3,
    path: 'a.txt',
    offset: 0,
    length: 8,
  });
  await settle();
  assert.deepEqual(
    sent.map(({ message }) => [message.id, message.ok]),
    [
      [1, true],
      [2, true],
      [3, true],
    ],
  );
  assert.deepEqual(sent[1].message.listing.files[0], {
    path: 'a.txt',
    size: 3,
    sha256: 'f'.repeat(64),
    mediaType: 'text/plain',
  });
  const part = sent[2].message.part;
  assert.ok(part.bytes instanceof ArrayBuffer);
  assert.deepEqual([...new Uint8Array(part.bytes)], [1, 2, 3]);
  assert.deepEqual(sent[2].transfer, [part.bytes]);
  assert.equal(rpc.pending, 0);
});

test('A flood costs the workspace a token each and the server only the burst', async () => {
  const { rpc, sent, log } = rpcWith();
  // Calls finish as the flood goes on, so the pending cap alone would not
  // hold it back: only the budget does.
  for (let id = 1; id <= 1000; id += 1) {
    rpc.handle({ type: 'source.list', id });
    if (id % 5 === 0) await settle();
  }
  for (let noise = 0; noise < 1000; noise += 1) rpc.handle({ junk: noise });
  await settle();
  const reached = log.filter((entry) => entry === 'list').length;
  assert.ok(reached > 0 && reached <= FRAME_BURST, String(reached));
  const refused = sent.filter(({ message }) => message.ok === false);
  assert.ok(refused.length >= 1000 - FRAME_BURST);
  assert.ok(refused.every(({ message }) => message.code === 'rate_limited'));
  assert.deepEqual(Object.keys(refused[0].message).sort(), [
    'code',
    'id',
    'ok',
  ]);
});

const save = {
  type: 'source.write',
  id: 1,
  retryId: 'retry-0001',
  path: 'a.txt',
  bytes: new ArrayBuffer(3),
};

test('A call the host cannot parse is still answered, at once, and holds no slot', async () => {
  const wrong = [
    { type: 'source.read', id: 1, path: '../a.txt', offset: 0, length: 1 },
    { type: 'source.read', id: 2, path: 'a b.txt', offset: 0, length: 1 },
    { type: 'source.read', id: 3, path: 'a//b.txt', offset: 0, length: 1 },
    { type: 'source.read', id: 4, path: 'LICENSE', offset: 0, length: 1 },
    {
      type: 'source.read',
      id: 5,
      path: `${'a'.repeat(241)}.txt`,
      offset: 0,
      length: 1,
    },
    { type: 'source.read', id: 6, path: 'a.txt', offset: -1, length: 1 },
    { type: 'source.read', id: 7, path: 'a.txt', offset: 0, length: 0 },
    { type: 'source.read', id: 8, path: 'a.txt', offset: 0, length: 2 ** 21 },
    {
      type: 'source.read',
      id: 9,
      path: 'a.txt',
      offset: 0,
      length: 1,
      extra: 1,
    },
    { type: 'source.read', id: 10, path: 'a.txt', offset: 0 },
    { type: 'source.download', id: 11, kind: 'file', path: 'LICENSE' },
    { type: 'source.download', id: 12, kind: 'file', path: null },
    { type: 'source.download', id: 13, kind: 'archive', path: 'a.txt' },
    { type: 'source.download', id: 14, kind: 'both', path: null },
    { type: 'source.download', id: 15 },
    { type: 'source.list', id: 16, path: 'a.txt' },
    { type: 'metadata', id: 17, extra: true },
    { type: 'cancel', id: 18, extra: true },
    { ...save, id: 19, retryId: 'short' },
    { ...save, id: 20, path: '../a.txt' },
    { ...save, id: 21, bytes: new ArrayBuffer(1_048_577) },
    { ...save, id: 22, bytes: new Uint8Array(3) },
    { ...save, id: 23, extra: 1 },
  ];
  const { rpc, sent, log } = rpcWith();
  for (const message of wrong) rpc.handle(message);
  await settle();
  assert.deepEqual(
    sent.map(({ message }) => message),
    wrong.map(({ id }) => ({ id, ok: false, code: 'invalid_request' })),
  );
  assert.equal(rpc.pending, 0, 'a refused call holds no slot');
  assert.deepEqual(log, [], 'nothing reached the server');
  // A message without a usable id, or of no known type, cannot be answered.
  sent.length = 0;
  for (const message of [
    { type: 'source.list' },
    { type: 'source.list', id: 0 },
    { type: 'source.list', id: -1 },
    { type: 'source.list', id: 1.5 },
    { type: 'source.list', id: '1' },
    { type: 'source.list', id: 2 ** 53 },
    { type: 'nothing', id: 1 },
    { id: 1 },
    'text',
    null,
    [],
  ])
    rpc.handle(message);
  assert.deepEqual(sent, []);
});

test('A call the host cannot parse costs a token, and answers only as the budget allows', async () => {
  let tokens = 2;
  const { rpc, sent } = rpcWith({ take: () => tokens-- > 0 });
  const bad = (id) => ({
    type: 'source.read',
    id,
    path: '../x',
    offset: 0,
    length: 1,
  });
  rpc.handle(bad(1));
  rpc.handle({ type: 'source.list', id: 2, junk: 1 });
  rpc.handle(bad(3));
  assert.deepEqual(
    sent.map(({ message }) => [message.id, message.code]),
    [
      [1, 'invalid_request'],
      [2, 'invalid_request'],
      [3, 'rate_limited'],
    ],
  );
});

test('After the lease a call that cannot be parsed is not answered either', () => {
  const time = clock();
  const { rpc, sent } = rpcWith({ deadline: time.now() - 1 });
  rpc.handle({
    type: 'source.read',
    id: 1,
    path: '../x',
    offset: 0,
    length: 1,
  });
  assert.deepEqual(sent, []);
});

test('Only a bounded number of calls wait at once, and finishing one frees a slot', async () => {
  const gates = [];
  const { rpc, sent } = rpcWith({
    take: () => true,
    client: {
      runtimeSourceList: () => {
        const gate = deferred();
        gates.push(gate);
        return gate.promise;
      },
    },
  });
  for (let id = 1; id <= FRAME_PENDING_CAP + 1; id += 1)
    rpc.handle({ type: 'source.list', id });
  await settle();
  assert.equal(rpc.pending, FRAME_PENDING_CAP);
  assert.deepEqual(
    sent.map(({ message }) => [message.id, message.code]),
    [[FRAME_PENDING_CAP + 1, 'rate_limited']],
  );
  gates[0].resolve({ revision: 1, files: [] });
  await settle();
  rpc.handle({ type: 'source.list', id: 99 });
  await settle();
  assert.equal(rpc.pending, FRAME_PENDING_CAP);
  assert.equal(gates.length, FRAME_PENDING_CAP + 1);
});

test('A failure is reported as its closed word and nothing the server said', async () => {
  for (const [error, code] of [
    [new StatusError(404, 'not_found'), 'not_found'],
    [new StatusError(403, 'forbidden'), 'denied'],
    [new StatusError(409, 'revision_conflict'), 'stale_view'],
    [new StatusError(429, 'rate_limited'), 'rate_limited'],
    [new Error('SECRET'), 'unavailable'],
  ]) {
    const { rpc, sent } = rpcWith({
      client: {
        runtimeSourceRead: async () => {
          throw error;
        },
      },
    });
    rpc.handle({
      type: 'source.read',
      id: 1,
      path: 'a.txt',
      offset: 0,
      length: 1,
    });
    await settle();
    assert.deepEqual(sent[0].message, { id: 1, ok: false, code });
    assert.ok(!JSON.stringify(sent).includes('SECRET'));
  }
});

test('After the lease ends the frame is told nothing, new call or late answer', async () => {
  const late = rpcWith();
  late.time.advance(900_000);
  late.rpc.handle({ type: 'source.list', id: 1 });
  await settle();
  assert.deepEqual(late.sent, []);
  assert.deepEqual(late.log, [], 'the server was not asked');
  const gate = deferred();
  const slow = rpcWith({ client: { runtimeSourceList: () => gate.promise } });
  slow.rpc.handle({ type: 'source.list', id: 1 });
  await settle();
  slow.time.advance(900_000);
  gate.resolve({ revision: 1, files: [] });
  await settle();
  assert.deepEqual(slow.sent, []);
});

test('A cancel aborts the call and no reply is sent for it', async () => {
  let seen;
  const gate = deferred();
  const { rpc, sent } = rpcWith({
    client: {
      runtimeSourceList: (_view, options) => {
        seen = options.signal;
        return gate.promise;
      },
    },
  });
  rpc.handle({ type: 'source.list', id: 7 });
  await settle();
  assert.equal(seen.aborted, false);
  rpc.handle({ type: 'cancel', id: 7 });
  rpc.handle({ type: 'cancel', id: 8 });
  assert.equal(seen.aborted, true);
  gate.resolve({ revision: 1, files: [] });
  await settle();
  assert.deepEqual(sent, []);
  assert.equal(rpc.pending, 0);
});

test('Closing aborts everything, and later messages and replies are ignored', async () => {
  const signals = [];
  const gate = deferred();
  const { rpc, sent } = rpcWith({
    client: {
      runtimeSourceList: (_view, options) => {
        signals.push(options.signal);
        return gate.promise;
      },
    },
  });
  rpc.handle({ type: 'source.list', id: 1 });
  rpc.handle({ type: 'source.list', id: 2 });
  await settle();
  rpc.close();
  assert.ok(signals.every((signal) => signal.aborted));
  gate.resolve({ revision: 1, files: [] });
  rpc.handle({ type: 'source.list', id: 3 });
  await settle();
  assert.deepEqual(sent, []);
  assert.equal(rpc.pending, 0);
});

test('Nothing a frame sends, or a broken port does, escapes the rpc', async () => {
  const { rpc } = rpcWith({
    send: () => {
      throw new Error('port closed');
    },
  });
  for (const message of [
    null,
    undefined,
    1,
    'x',
    [],
    {},
    { type: 'metadata', id: 1 },
  ])
    assert.doesNotThrow(() => rpc.handle(message));
  await settle();
  // A port that cannot be written to ends the calls.
  assert.equal(rpc.pending, 0);
  const hostile = new Proxy(
    {},
    {
      get() {
        throw new Error('trap');
      },
      ownKeys() {
        throw new Error('trap');
      },
      getPrototypeOf() {
        throw new Error('trap');
      },
    },
  );
  assert.doesNotThrow(() => rpc.handle(hostile));
});

test('A download asks the server before any prompt and fetches only after accept', async () => {
  const { rpc, sent, log, saved } = rpcWith();
  rpc.handle({ type: 'source.download', id: 1, kind: 'file', path: 'a.txt' });
  await settle();
  assert.deepEqual(log, ['offer', 'ask', 'download']);
  assert.deepEqual(saved, [
    { bytes: [9, 9, 9], name: 'a.txt', type: 'application/octet-stream' },
  ]);
  assert.deepEqual(sent[0].message, {
    id: 1,
    ok: true,
    receipt: { started: true, kind: 'file', bytes: 3 },
  });
  rpc.handle({ type: 'source.download', id: 2, kind: 'archive', path: null });
  await settle();
  assert.equal(saved[1].type, 'application/zip');
  assert.equal(saved[1].name, 'x-r1.zip');
  assert.equal(sent[1].message.receipt.kind, 'archive');
});

test('A refused offer never opens a prompt, whatever the page believed about its grant', async () => {
  for (const [error, code] of [
    [new StatusError(403, 'forbidden'), 'denied'],
    [new StatusError(409, 'revision_conflict'), 'stale_view'],
    [new StatusError(404, 'not_found'), 'not_found'],
  ]) {
    const { rpc, sent, log, saved } = rpcWith({
      client: {
        runtimeSourceOffer: async () => {
          log.push('offer');
          throw error;
        },
      },
    });
    // The workspace's own snapshot said the capability was granted.
    rpc.handle({ type: 'source.download', id: 1, kind: 'file', path: 'a.txt' });
    await settle();
    assert.deepEqual(sent[0].message, { id: 1, ok: false, code });
    assert.ok(!log.includes('ask'), 'no prompt');
    assert.ok(!log.includes('download'), 'no bytes');
    assert.deepEqual(saved, []);
  }
});

test('Each way a prompt ends has its own result and only accept saves', async () => {
  for (const [outcome, expected] of [
    ['declined', 'declined'],
    ['busy', 'rate_limited'],
    ['cooling', 'rate_limited'],
  ]) {
    const { rpc, sent, log, saved } = rpcWith({ ask: async () => outcome });
    rpc.handle({ type: 'source.download', id: 1, kind: 'archive', path: null });
    await settle();
    assert.deepEqual(sent[0].message, { id: 1, ok: false, code: expected });
    assert.ok(!log.includes('download'));
    assert.deepEqual(saved, []);
  }
  const { rpc, sent, saved } = rpcWith({ ask: async () => 'withdrawn' });
  rpc.handle({ type: 'source.download', id: 1, kind: 'archive', path: null });
  await settle();
  assert.deepEqual([sent, saved], [[], []]);
});

test('A prompt is withdrawn when the frame cancels, and nothing is saved', async () => {
  let shownSignal;
  const prompt = deferred();
  const { rpc, sent, saved, log } = rpcWith({
    ask: (_offer, signal) => {
      shownSignal = signal;
      return prompt.promise;
    },
  });
  rpc.handle({ type: 'source.download', id: 1, kind: 'file', path: 'a.txt' });
  await settle();
  assert.equal(shownSignal.aborted, false);
  rpc.handle({ type: 'cancel', id: 1 });
  assert.equal(shownSignal.aborted, true);
  prompt.resolve('accepted');
  await settle();
  assert.deepEqual([sent, saved], [[], []]);
  assert.ok(!log.includes('download'), 'the bytes were never fetched');
});

test('A download the server refuses after accept saves nothing', async () => {
  const { rpc, sent, saved } = rpcWith({
    client: {
      runtimeSourceDownload: async () => {
        throw new StatusError(409, 'revision_conflict');
      },
    },
  });
  rpc.handle({ type: 'source.download', id: 1, kind: 'file', path: 'a.txt' });
  await settle();
  assert.deepEqual(sent[0].message, { id: 1, ok: false, code: 'stale_view' });
  assert.deepEqual(saved, []);
});

// ---- the saver ----

test('An object URL lives about a minute, is replaced at once, and survives the frame', () => {
  const time = clock();
  const events = [];
  let created = 0;
  const saver = new BlobSaver({
    createObjectURL: (blob) => {
      created += 1;
      events.push(`create ${created} ${blob.type} ${blob.size}`);
      return `blob:${created}`;
    },
    revokeObjectURL: (url) => events.push(`revoke ${url}`),
    start: (url, name) => events.push(`start ${url} ${name}`),
    after: time.after,
  });
  saver.save(Uint8Array.from([1, 2, 3]), 'a.txt', 'application/octet-stream');
  assert.deepEqual(events, [
    'create 1 application/octet-stream 3',
    'start blob:1 a.txt',
  ]);
  time.advance(OBJECT_URL_LIFETIME_MS - 1);
  assert.equal(events.length, 2, 'not revoked before the minute');
  saver.save(Uint8Array.from([4]), 'b.zip', 'application/zip');
  assert.deepEqual(events.slice(2), [
    'revoke blob:1',
    'create 2 application/zip 1',
    'start blob:2 b.zip',
  ]);
  assert.equal(time.live(), 1, 'the replaced URL has no timer left');
  time.advance(1);
  assert.equal(events.length, 5, 'the first timer does not revoke the second');
  time.advance(OBJECT_URL_LIFETIME_MS);
  assert.equal(events.at(-1), 'revoke blob:2');
  assert.equal(time.live(), 0);
});

test('A save the browser could not start releases its URL and says so', () => {
  const time = clock();
  const revoked = [];
  const saver = new BlobSaver({
    createObjectURL: () => 'blob:1',
    revokeObjectURL: (url) => revoked.push(url),
    start: () => {
      throw new Error('blocked');
    },
    after: time.after,
  });
  assert.throws(
    () => saver.save(Uint8Array.of(1), 'a.txt', 'application/zip'),
    /blocked/,
  );
  assert.deepEqual(revoked, ['blob:1']);
  assert.equal(time.live(), 0);
});

// ---- saving a file (decision 0033) ----

test('A save is a call only with a retry identity, a path and bytes within the bound', () => {
  for (const bytes of [new ArrayBuffer(0), new ArrayBuffer(1_048_576)]) {
    for (const retryId of ['12345678', 'r'.repeat(36), 'a-B_9-zZ_0'])
      assert.equal(
        parseCall({ ...save, retryId, bytes })?.type,
        'source.write',
        `${retryId} ${bytes.byteLength}`,
      );
  }
  const { id: _id, ...withoutId } = save;
  const bad = [
    { ...save, retryId: '1234567' },
    { ...save, retryId: 'r'.repeat(37) },
    { ...save, retryId: 'retry:001' },
    { ...save, retryId: 'retry 001' },
    { ...save, retryId: 7 },
    { ...save, path: '../a.txt' },
    { ...save, path: 5 },
    { ...save, bytes: new ArrayBuffer(1_048_577) },
    { ...save, bytes: new Uint8Array(3) },
    { ...save, bytes: new DataView(new ArrayBuffer(3)) },
    { ...save, bytes: new SharedArrayBuffer(3) },
    { ...save, bytes: 'abc' },
    { ...save, bytes: [1, 2, 3] },
    { ...save, bytes: null },
    { ...save, extra: 1 },
    withoutId,
    { type: 'source.write', id: 1, path: 'a.txt', bytes: new ArrayBuffer(3) },
  ];
  for (const message of bad)
    assert.equal(parseCall(message), undefined, JSON.stringify(message));
});

test('The host and the client accept the same retry identities', () => {
  for (const retryId of [
    '',
    'a',
    '1234567',
    '12345678',
    'r'.repeat(36),
    'r'.repeat(37),
    'a-B_9-zZ_0',
    'a b c d e f g',
    'retry:0001',
    'retry.0001',
    'retry\n0001',
    'rétry-0001',
  ])
    assert.equal(
      parseCall({ ...save, retryId }) !== undefined,
      isRetryId(retryId),
      JSON.stringify(retryId),
    );
});

const SESSION = 'e'.repeat(32);
const savedView = {
  url: `http://127.0.0.1:7411/r/${'b'.repeat(64)}/`,
  enabled: true,
  session_id: SESSION,
  artifact_id: `art_${'0'.repeat(32)}`,
  revision: 3,
  head_revision: 3,
  sdk: 'artifree.app.v1',
  declared: ['source.write'],
  supported: ['source.write'],
  granted: ['source.write'],
  grant_version: 1,
  view_id: 'd'.repeat(32),
  generation: 1,
  initialization: 'c'.repeat(64),
  expires_at_ms: 1_000_000,
  lease_ms: 900_000,
};
const HELLO_SHA256 =
  '2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824';

/** A rpc over the real client, whose server `respond`s to a save. */
async function savingRpc(respond, overrides = {}) {
  const requests = [];
  const client = new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:7410',
    accessToken: 'token',
    fetch: async (url, init) => {
      if (url.endsWith('/v1/runtime-sessions'))
        return Response.json({ session_id: SESSION });
      requests.push({ url, init, body: JSON.parse(init.body) });
      return respond(requests.at(-1));
    },
  });
  await client.openRuntimeSession();
  return {
    ...rpcWith({ ...overrides, view: savedView, realClient: client }),
    requests,
  };
}

const hello = () => new TextEncoder().encode('hello').buffer;
const receipt = (overrides = {}) => ({
  path: 'a.txt',
  revision: 4,
  size: 5,
  sha256: HELLO_SHA256,
  ...overrides,
});

test('A save goes to the server with the bytes the frame sent, and the frame hears the receipt', async () => {
  const { rpc, sent, requests } = await savingRpc(() =>
    Response.json(receipt()),
  );
  rpc.handle({ ...save, id: 5, bytes: hello() });
  await until(() => sent.length === 1);
  assert.equal(requests.length, 1);
  assert.ok(
    requests[0].url.endsWith(
      `/revisions/3/runtime-views/${savedView.view_id}/1/source/write`,
    ),
  );
  assert.deepEqual(requests[0].body, {
    initialization: savedView.initialization,
    retry_id: 'retry-0001',
    path: 'a.txt',
    content: 'aGVsbG8=',
  });
  assert.deepEqual(
    sent.map(({ message }) => message),
    [{ id: 5, ok: true, written: receipt() }],
  );
  assert.equal(rpc.pending, 0);
});

test('A receipt for other bytes than the frame sent never reaches the frame as a success', async () => {
  const wrong = [
    receipt({ sha256: 'f'.repeat(64) }),
    receipt({ size: 6 }),
    receipt({ path: 'b.txt' }),
    // The view is revision 3, so its save is revision 4 and no other.
    receipt({ revision: 3 }),
    receipt({ revision: 5 }),
    { ...receipt(), extra: undefined, sha256: undefined },
  ];
  for (const body of wrong) {
    const { rpc, sent } = await savingRpc(() => Response.json(body));
    rpc.handle({ ...save, id: 1, bytes: hello() });
    await until(() => sent.length === 1);
    assert.deepEqual(
      sent.map(({ message }) => message),
      [{ id: 1, ok: false, code: 'unavailable' }],
      JSON.stringify(body),
    );
  }
});

test('A refused save is reported as its closed word and nothing the server said', async () => {
  const refusals = [
    [409, 'revision_conflict', 'stale_view'],
    [403, 'forbidden', 'denied'],
    [404, 'not_found', 'not_found'],
    [422, 'request_key_reused', 'invalid_request'],
    [422, 'invalid_request', 'invalid_request'],
    [429, 'rate_limited', 'rate_limited'],
    [503, 'unavailable', 'unavailable'],
  ];
  for (const [status, code, word] of refusals) {
    const { rpc, sent } = await savingRpc(() =>
      Response.json(
        { error: { code, message: 'SECRET detail', current_revision: 9 } },
        { status },
      ),
    );
    rpc.handle({ ...save, id: 1, bytes: hello() });
    await until(() => sent.length === 1);
    assert.deepEqual(
      sent.map(({ message }) => message),
      [{ id: 1, ok: false, code: word }],
      `${status} ${code}`,
    );
  }
});

test('A save the frame gives up on is aborted and gets no answer', async () => {
  const gate = deferred();
  let signal;
  const { rpc, sent } = await savingRpc(({ init }) => {
    signal = init.signal;
    return gate.promise;
  });
  rpc.handle({ ...save, id: 1, bytes: hello() });
  await until(() => signal !== undefined);
  assert.equal(rpc.pending, 1);
  assert.equal(signal.aborted, false);
  rpc.handle({ type: 'cancel', id: 1 });
  assert.equal(signal.aborted, true);
  gate.resolve(Response.json(receipt()));
  await until(() => rpc.pending === 0);
  assert.deepEqual(sent, []);
});

test('A save costs a token and holds a waiting slot like any other call', async () => {
  const gate = deferred();
  const { rpc, sent } = await savingRpc(() => gate.promise, {
    take: () => false,
  });
  rpc.handle({ ...save, id: 1, bytes: hello() });
  await settle();
  assert.deepEqual(
    sent.map(({ message }) => message),
    [{ id: 1, ok: false, code: 'rate_limited' }],
  );
  assert.equal(rpc.pending, 0);
  const full = await savingRpc(() => gate.promise);
  for (let id = 1; id <= FRAME_PENDING_CAP; id += 1)
    full.rpc.handle({ ...save, id, bytes: hello() });
  full.rpc.handle({ ...save, id: FRAME_PENDING_CAP + 1, bytes: hello() });
  await settle();
  assert.equal(full.rpc.pending, FRAME_PENDING_CAP);
  assert.deepEqual(
    full.sent.map(({ message }) => message),
    [{ id: FRAME_PENDING_CAP + 1, ok: false, code: 'rate_limited' }],
  );
  gate.resolve(Response.json(receipt()));
});
