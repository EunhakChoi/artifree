import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { afterEach, beforeEach, mock, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The frame script is bundled as the server embeds it, then run against a
// stand-in window and a real MessageChannel. The host side is the test.
const bundle = await build({
  entryPoints: [
    fileURLToPath(
      new URL('../../packages/runtime-sdk/src/bootstrap.ts', import.meta.url),
    ),
  ],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const { start } = await import(
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
);
// The workspace host's parser, bundled as it ships, to check that whatever the
// frame sends the host accepts.
const hostBundle = await build({
  entryPoints: [
    fileURLToPath(
      new URL('../../apps/web/src/artifacts/frame-rpc.ts', import.meta.url),
    ),
  ],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const { parseCall } = await import(
  `data:text/javascript;base64,${Buffer.from(hostBundle.outputFiles[0].text).toString('base64')}`
);
// The frame's own copy of the package path rules.
const rulesBundle = await build({
  entryPoints: [
    fileURLToPath(
      new URL(
        '../../packages/runtime-sdk/src/package-path.ts',
        import.meta.url,
      ),
    ),
  ],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const { FRAME_PATH_RULES } = await import(
  `data:text/javascript;base64,${Buffer.from(rulesBundle.outputFiles[0].text).toString('base64')}`
);
// The frame's own SHA-256, with which it checks a save receipt.
const digestBundle = await build({
  entryPoints: [
    fileURLToPath(
      new URL('../../packages/runtime-sdk/src/sha256.ts', import.meta.url),
    ),
  ],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const { sha256Hex } = await import(
  `data:text/javascript;base64,${Buffer.from(digestBundle.outputFiles[0].text).toString('base64')}`
);
const { scaffoldDescription } =
  await import('../../packages/contracts/dist/index.js');
const pathCases = JSON.parse(
  await readFile(
    new URL('../../contracts/fixtures/package-paths.json', import.meta.url),
    'utf8',
  ),
).cases;
const vectors = JSON.parse(
  await readFile(
    new URL('./fixtures/source-part-vectors.json', import.meta.url),
    'utf8',
  ),
);

const CONFIG = {
  view_id: 'd'.repeat(32),
  generation: 1,
  initialization: 'c'.repeat(64),
  lease_ms: 900_000,
};
const DIGEST = vectors.digest;

async function settle() {
  for (let round = 0; round < 20; round += 1) await Promise.resolve();
  // A message crosses the port on the event loop, not in a microtask.
  for (let round = 0; round < 5; round += 1)
    await new Promise((resolve) => setImmediate(resolve));
}

let current;

/** Starts the script on a stand-in window; the test plays the workspace. */
function frame(config = CONFIG) {
  const listeners = {};
  const received = [];
  const host = { port: undefined, connect: undefined };
  const window = {
    artifree: undefined,
    addEventListener: (type, listener) => {
      listeners[type] = listener;
    },
    setTimeout: (run, ms) => setTimeout(run, ms),
    parent: {
      postMessage: (message, _origin, ports) => {
        host.connect = message;
        host.port = ports[0];
        host.port.onmessage = (event) => received.push(event.data);
      },
    },
  };
  globalThis.window = window;
  start(config);
  current = { window, listeners, host };
  return {
    api: window.artifree,
    received,
    host,
    /** Answers call `id` as the workspace. */
    reply: (message) => host.port.postMessage(message),
    hangUp: () => listeners.pagehide(),
  };
}

afterEach(() => {
  current?.listeners.pagehide?.();
  current = undefined;
  delete globalThis.window;
  mock.timers.reset();
});

/** The id the next call was sent under, after the port delivered it. */
async function nextCall(f, index) {
  await settle();
  return f.received[index];
}

async function code(promise) {
  const error = await promise.then(
    () => assert.fail('The call must be refused.'),
    (rejection) => rejection,
  );
  assert.equal(error.name, 'RuntimeRequestFailure');
  return error;
}

test('The script announces the view with one port and nothing else', () => {
  const f = frame();
  assert.deepEqual(f.host.connect, {
    type: 'artifree-runtime-connect',
    viewId: CONFIG.view_id,
    generation: 1,
    initialization: CONFIG.initialization,
  });
  assert.ok(Object.isFrozen(f.api));
  assert.ok(Object.isFrozen(f.api.source));
  assert.deepEqual(Object.keys(f.api.source).sort(), [
    'list',
    'read',
    'requestDownload',
    'write',
  ]);
  assert.throws(() => start(CONFIG), /already installed/);
});

test('Calls send exactly their fields, and a detached method works', async () => {
  const f = frame();
  const { list, read, requestDownload } = f.api.source;
  const calls = [
    list(),
    read({ path: 'a.txt', offset: 3, length: 4 }),
    requestDownload({ path: 'a.txt' }),
    requestDownload(),
    requestDownload(undefined),
    f.api.currentMetadata(),
  ];
  await settle();
  assert.deepEqual(f.received, [
    { type: 'source.list', id: 1 },
    { type: 'source.read', id: 2, path: 'a.txt', offset: 3, length: 4 },
    { type: 'source.download', id: 3, kind: 'file', path: 'a.txt' },
    { type: 'source.download', id: 4, kind: 'archive', path: null },
    { type: 'source.download', id: 5, kind: 'archive', path: null },
    { type: 'metadata', id: 6 },
  ]);
  f.hangUp();
  await Promise.allSettled(calls);
});

function hostileObjects() {
  const revocable = Proxy.revocable({}, {});
  revocable.revoke();
  const withGetter = {
    get path() {
      throw new Error('getter ran');
    },
    offset: 0,
    length: 1,
  };
  const trapping = new Proxy(
    { path: 'a.txt', offset: 0, length: 1 },
    {
      getPrototypeOf() {
        throw new Error('trap ran');
      },
      ownKeys() {
        throw new Error('trap ran');
      },
      getOwnPropertyDescriptor() {
        throw new Error('trap ran');
      },
      get() {
        throw new Error('trap ran');
      },
    },
  );
  class Shaped {
    constructor() {
      this.path = 'a.txt';
      this.offset = 0;
      this.length = 1;
    }
  }
  const symbolic = { path: 'a.txt', offset: 0, length: 1, [Symbol('x')]: 1 };
  const hidden = { offset: 0, length: 1 };
  Object.defineProperty(hidden, 'path', { value: 'a.txt', enumerable: false });
  return [
    null,
    undefined,
    [],
    ['a', 0, 1],
    'a',
    5,
    true,
    Symbol('s'),
    () => undefined,
    revocable.proxy,
    trapping,
    withGetter,
    new Shaped(),
    symbolic,
    hidden,
    { path: 'a.txt', offset: 0, length: 1, extra: 1 },
    { path: 5, offset: 0, length: 1 },
    { path: '', offset: 0, length: 1 },
    { path: 'a'.repeat(1025), offset: 0, length: 1 },
    { path: 'a.txt', offset: -1, length: 1 },
    { path: 'a.txt', offset: 1.5, length: 1 },
    { path: 'a.txt', offset: Number.NaN, length: 1 },
    { path: 'a.txt', offset: Infinity, length: 1 },
    { path: 'a.txt', offset: 2 ** 53, length: 1 },
    { path: 'a.txt', offset: '0', length: 1 },
    { path: 'a.txt', offset: 0, length: 0 },
    { path: 'a.txt', offset: 0, length: 1_048_577 },
    { path: 'a.txt', offset: 0, length: -1 },
    { path: 'a.txt', length: 1 },
  ];
}

test('Hostile arguments are refused without throwing, sending or leaking a slot', async () => {
  const f = frame();
  const { read, requestDownload } = f.api.source;
  for (const argument of hostileObjects()) {
    let promise;
    assert.doesNotThrow(() => {
      promise = read(argument);
    }, 'read');
    assert.equal((await code(promise)).code, 'invalid_request');
  }
  for (const argument of hostileObjects()) {
    if (argument === undefined) continue;
    let promise;
    assert.doesNotThrow(() => {
      promise = requestDownload(argument);
    }, 'requestDownload');
    assert.equal((await code(promise)).code, 'invalid_request');
  }
  for (const argument of [
    { path: 5 },
    { path: '' },
    { path: 'a.txt', extra: 1 },
    {},
  ]) {
    assert.equal(
      (await code(requestDownload(argument))).code,
      'invalid_request',
    );
  }
  await settle();
  assert.deepEqual(f.received, [], 'nothing was sent for a refused call');
  // Every slot is still free: 64 calls wait, the 65th is told to slow down.
  const waiting = Array.from({ length: 64 }, () => f.api.source.list());
  await assert.rejects(f.api.source.list(), { code: 'rate_limited' });
  f.hangUp();
  for (const promise of waiting)
    await assert.rejects(promise, { code: 'expired' });
});

test('A path the package rules refuse fails at once, and the host accepts what is sent', async () => {
  assert.ok(pathCases.length >= 30);
  for (const { path, error } of pathCases) {
    const f = frame();
    const calls = [
      f.api.source.read({ path, offset: 0, length: 1 }),
      f.api.source.requestDownload({ path }),
    ];
    const outcomes = Promise.allSettled(calls);
    await settle();
    if (error === null) {
      // Sent, and the host's parser reads each message as a call.
      assert.equal(f.received.length, 2, JSON.stringify(path));
      for (const message of f.received)
        assert.notEqual(parseCall(message), undefined, JSON.stringify(path));
      current.listeners.pagehide();
      await outcomes;
    } else {
      // Refused before sending, so no waiting slot is held until a timeout.
      assert.deepEqual(f.received, [], JSON.stringify(path));
      for (const outcome of await outcomes) {
        assert.equal(outcome.status, 'rejected', path);
        assert.equal(outcome.reason.code, 'invalid_request', path);
      }
    }
    // Close the port, or the test process never ends.
    current.listeners.pagehide();
    delete globalThis.window;
  }
});

test("The frame's copy of the path rules is the build's", () => {
  const rules = scaffoldDescription.package_rules;
  assert.equal(FRAME_PATH_RULES.maxPathBytes, rules.max_path_bytes);
  assert.equal(FRAME_PATH_RULES.maxPathSegments, rules.max_path_segments);
  assert.equal(FRAME_PATH_RULES.maxSegmentBytes, rules.max_segment_bytes);
  assert.deepEqual(
    [...FRAME_PATH_RULES.extensions].sort(),
    Object.keys(rules.media_types).sort(),
  );
});

test('Whatever the frame sends, the host reads as a call', async () => {
  const f = frame();
  const { list, read, requestDownload } = f.api.source;
  const calls = [
    list(),
    f.api.currentMetadata(),
    read({ path: 'a/b.txt', offset: 0, length: 1 }),
    read({ path: 'a/b.txt', offset: 2 ** 53 - 1, length: 1_048_576 }),
    requestDownload(),
    requestDownload({ path: 'a/b.txt' }),
  ];
  await settle();
  assert.equal(f.received.length, calls.length);
  for (const message of f.received)
    assert.deepEqual(
      parseCall(message)?.type,
      message.type,
      JSON.stringify(message),
    );
  f.hangUp();
  await Promise.allSettled(calls);
  // Each value the frame refuses is one the host would refuse too.
  for (const message of [
    { type: 'source.read', id: 1, path: 'a.txt', offset: 0, length: 0 },
    { type: 'source.read', id: 1, path: 'a.txt', offset: 0, length: 1_048_577 },
    { type: 'source.read', id: 1, path: 'a.txt', offset: -1, length: 1 },
    { type: 'source.read', id: 1, path: 'a.txt', offset: 2 ** 53, length: 1 },
    { type: 'source.download', id: 1, kind: 'file', path: '../a.txt' },
    { type: 'source.download', id: 1, kind: 'archive', path: 'a.txt' },
  ])
    assert.equal(parseCall(message), undefined, JSON.stringify(message));
});

test('A call with a strange this or arguments still behaves', async () => {
  const f = frame();
  const { list, read } = f.api.source;
  const calls = [
    list.call(null),
    list.call(undefined, 1, 2, 3),
    read.call({ path: 'x' }, { path: 'a.txt', offset: 0, length: 1 }),
  ];
  await settle();
  assert.equal(f.received.length, 3);
  f.hangUp();
  await Promise.allSettled(calls);
});

test('Parts are checked against the request and the bytes received', async () => {
  for (const testCase of vectors.cases) {
    const f = frame();
    const result = f.api.source.read(testCase.request);
    const call = await nextCall(f, 0);
    const reply = {
      path: testCase.request.path,
      offset: testCase.request.offset,
      sha256: DIGEST,
      ...testCase.reply,
    };
    const { next_offset: next, ...rest } = reply;
    f.reply({
      id: call.id,
      ok: true,
      part: {
        ...rest,
        ...(next === undefined ? {} : { nextOffset: next }),
        bytes: new Uint8Array(testCase.bytes).buffer,
      },
    });
    if (testCase.valid) {
      const part = await result;
      assert.equal(part.bytes.length, testCase.bytes, testCase.name);
      assert.ok(part.bytes instanceof Uint8Array);
    } else {
      const error = await code(result);
      assert.equal(error.code, 'unavailable', testCase.name);
      assert.equal(error.message, 'The runtime request is unavailable.');
    }
    current.listeners.pagehide();
    delete globalThis.window;
  }
});

test('A part whose bytes are not an ArrayBuffer is refused', async () => {
  for (const bytes of [
    'AAAA',
    [1, 2, 3, 4],
    new Uint8Array(4),
    null,
    undefined,
    { byteLength: 4 },
  ]) {
    const f = frame();
    const result = f.api.source.read({ path: 'a.txt', offset: 0, length: 4 });
    const call = await nextCall(f, 0);
    f.reply({
      id: call.id,
      ok: true,
      part: {
        path: 'a.txt',
        offset: 0,
        nextOffset: 4,
        size: 10,
        sha256: DIGEST,
        bytes,
      },
    });
    assert.equal((await code(result)).code, 'unavailable');
    current.listeners.pagehide();
    delete globalThis.window;
  }
});

test('Failures come from a closed set and carry no workspace text', async () => {
  const f = frame();
  const cases = [
    ['invalid_request', 'The runtime request was not valid.'],
    ['denied', 'The runtime request was not permitted.'],
    ['stale_view', 'The runtime view is out of date.'],
    ['not_found', 'The requested source was not found.'],
    ['rate_limited', 'Too many runtime requests.'],
    ['declined', 'The user declined the request.'],
    ['unavailable', 'The runtime request is unavailable.'],
    ['expired', 'The runtime view has expired.'],
    ['<script>alert(1)</script>', 'The runtime request was not permitted.'],
    [undefined, 'The runtime request was not permitted.'],
    [{}, 'The runtime request was not permitted.'],
    ['__proto__', 'The runtime request was not permitted.'],
    ['constructor', 'The runtime request was not permitted.'],
  ];
  for (const [index, [sent, message]] of cases.entries()) {
    const result = f.api.source.list();
    const call = await nextCall(f, index);
    f.reply({ id: call.id, ok: false, code: sent, message: 'SECRET detail' });
    const error = await code(result);
    assert.equal(error.message, message);
    assert.ok(!error.message.includes('SECRET'));
    assert.ok(!('SECRET' in error));
  }
  // The first wording of a metadata refusal is kept.
  const metadata = f.api.currentMetadata();
  const call = await nextCall(f, cases.length);
  f.reply({ id: call.id, ok: false, code: 'denied' });
  assert.equal(
    (await code(metadata)).message,
    'The runtime metadata request was refused.',
  );
});

test('Replies that are not for a waiting call are ignored', async () => {
  const f = frame();
  const result = f.api.source.list();
  const call = await nextCall(f, 0);
  for (const stray of [
    null,
    'x',
    [],
    { ok: true },
    { id: 99, ok: true, listing: { revision: 1, files: [] } },
    { id: -1, ok: true },
    { id: 1.5, ok: true },
    { id: String(call.id), ok: true, listing: { revision: 1, files: [] } },
  ])
    f.reply(stray);
  await settle();
  f.reply({ id: call.id, ok: true, listing: { revision: 1, files: [] } });
  assert.deepEqual(await result, { revision: 1, files: [] });
  // A second answer to the same call changes nothing.
  f.reply({ id: call.id, ok: false, code: 'denied' });
  await settle();
});

const file = (path, size = 3) => ({
  path,
  size,
  sha256: DIGEST,
  mediaType: 'text/plain; charset=utf-8',
});

test('A listing names each path once and every number is checked', async () => {
  const answers = [
    [{ revision: 3, files: [file('a.txt'), file('b.txt', 0)] }, true],
    [{ revision: 3, files: [] }, true],
    [{ revision: 3, files: [file('a.txt'), file('a.txt')] }, false],
    [{ revision: 0, files: [] }, false],
    [{ revision: 3, files: [{ ...file('a'), size: -1 }] }, false],
    [{ revision: 3, files: [{ ...file('a'), size: 1.5 }] }, false],
    [{ revision: 3, files: [{ ...file('a'), sha256: 'A'.repeat(64) }] }, false],
    [{ revision: 3, files: [{ ...file('a'), path: '' }] }, false],
    [{ revision: 3, files: [{ ...file('a'), mediaType: 5 }] }, false],
    [
      {
        revision: 3,
        files: Array.from({ length: 257 }, (_, i) => file(`f${i}`)),
      },
      false,
    ],
    [{ revision: 3, files: 'a' }, false],
    [undefined, false],
  ];
  for (const [listing, valid] of answers) {
    const f = frame();
    const result = f.api.source.list();
    const call = await nextCall(f, 0);
    f.reply({ id: call.id, ok: true, listing });
    if (valid) assert.equal((await result).files.length, listing.files.length);
    else assert.equal((await code(result)).code, 'unavailable');
    current.listeners.pagehide();
    delete globalThis.window;
  }
});

test('A download receipt must say the bytes were handed over', async () => {
  const answers = [
    [{ started: true, kind: 'file', bytes: 12 }, true],
    [{ started: true, kind: 'archive', bytes: 0 }, true],
    [{ started: false, kind: 'file', bytes: 12 }, false],
    [{ started: 'yes', kind: 'file', bytes: 12 }, false],
    [{ started: true, kind: 'folder', bytes: 12 }, false],
    [{ started: true, kind: 'file', bytes: -1 }, false],
    [{ started: true, kind: 'file', bytes: 1.5 }, false],
    [{ started: true, kind: 'file' }, false],
    [undefined, false],
  ];
  for (const [receipt, valid] of answers) {
    const f = frame();
    const result = f.api.source.requestDownload({ path: 'a.txt' });
    const call = await nextCall(f, 0);
    f.reply({ id: call.id, ok: true, receipt });
    if (valid) assert.deepEqual(await result, receipt);
    else assert.equal((await code(result)).code, 'unavailable');
    current.listeners.pagehide();
    delete globalThis.window;
  }
});

test('A read gives up after ten seconds and tells the workspace; a download waits for a person', async () => {
  mock.timers.enable({ apis: ['setTimeout'] });
  const f = frame();
  const read = f.api.source.read({ path: 'a.txt', offset: 0, length: 1 });
  const download = f.api.source.requestDownload({ path: 'a.txt' });
  await settle();
  mock.timers.tick(10_000);
  assert.equal((await code(read)).code, 'unavailable');
  await settle();
  assert.deepEqual(f.received.at(-1), { type: 'cancel', id: 1 });
  mock.timers.tick(110_000);
  assert.equal((await code(download)).code, 'unavailable');
  await settle();
  assert.deepEqual(f.received.at(-1), { type: 'cancel', id: 2 });
});

test('A call still waiting when the lease ends is expired, not merely unavailable', async () => {
  mock.timers.enable({ apis: ['setTimeout'] });
  const f = frame({ ...CONFIG, lease_ms: 5_000 });
  const waiting = f.api.source.list();
  await settle();
  const real = performance.now.bind(performance);
  mock.method(performance, 'now', () => real() + 6_000);
  mock.timers.tick(5_000);
  assert.equal((await code(waiting)).code, 'expired');
  mock.restoreAll();
});

test('A view that ended or whose lease ran out refuses every call', async () => {
  const f = frame();
  const waiting = f.api.source.list();
  f.hangUp();
  await assert.rejects(waiting, { code: 'expired' });
  await assert.rejects(f.api.source.list(), { code: 'expired' });
  await assert.rejects(f.api.currentMetadata(), { code: 'expired' });
  await assert.rejects(
    f.api.source.read({ path: 'a.txt', offset: 0, length: 1 }),
    {
      code: 'expired',
    },
  );
  delete globalThis.window;
  const short = frame({ ...CONFIG, lease_ms: 0 });
  await assert.rejects(short.api.source.list(), { code: 'expired' });
});

beforeEach(() => {
  delete globalThis.window;
});

// ---- saving a file (decision 0033) ----

const HELLO_SHA256 =
  '2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824';
const SAVE = { path: 'a.txt', content: 'hello', retryId: 'retry-0001' };

test('The frame hashes like the platform, for every length around a block', () => {
  assert.equal(sha256Hex(new TextEncoder().encode('hello')), HELLO_SHA256);
  const lengths = [
    ...Array.from({ length: 200 }, (_, length) => length),
    1000,
    65_535,
    1_048_576,
  ];
  for (const length of lengths) {
    const bytes = Uint8Array.from({ length }, (_, index) => (index * 7) % 256);
    assert.equal(
      sha256Hex(bytes),
      createHash('sha256').update(bytes).digest('hex'),
      `${length} bytes`,
    );
  }
});

test('A save sends its fields once, with a private copy that it transfers', async () => {
  const f = frame();
  const mine = Uint8Array.from([1, 2, 3]);
  const text = f.api.source.write(SAVE);
  const bytes = f.api.source.write({
    path: 'b/c.png',
    content: mine,
    retryId: 'retry_0002',
  });
  const outcomes = Promise.allSettled([text, bytes]);
  await settle();
  assert.equal(f.received.length, 2);
  const [first, second] = f.received;
  assert.deepEqual(
    { ...first, bytes: [...new Uint8Array(first.bytes)] },
    {
      type: 'source.write',
      id: 1,
      retryId: 'retry-0001',
      path: 'a.txt',
      bytes: [...new TextEncoder().encode('hello')],
    },
  );
  assert.deepEqual([...new Uint8Array(second.bytes)], [1, 2, 3]);
  // The caller's own array is untouched: the transfer took a copy.
  assert.deepEqual([...mine], [1, 2, 3]);
  assert.equal(mine.buffer.byteLength, 3);
  // The host reads both as calls.
  for (const message of f.received)
    assert.equal(parseCall(message)?.type, 'source.write');
  f.hangUp();
  await outcomes;
});

function receiptFor(overrides = {}) {
  return {
    path: 'a.txt',
    revision: 2,
    size: 5,
    sha256: HELLO_SHA256,
    ...overrides,
  };
}

test('A save receipt is judged by the bytes that were sent', async () => {
  const answers = [
    [receiptFor(), true],
    [receiptFor({ revision: 7 }), true],
    [receiptFor({ path: 'b.txt' }), false],
    [receiptFor({ size: 6 }), false],
    [receiptFor({ size: '5' }), false],
    [receiptFor({ sha256: 'f'.repeat(64) }), false],
    [receiptFor({ sha256: HELLO_SHA256.toUpperCase() }), false],
    [receiptFor({ sha256: HELLO_SHA256.slice(1) }), false],
    [receiptFor({ revision: 1 }), false],
    [receiptFor({ revision: 0 }), false],
    [receiptFor({ revision: 2.5 }), false],
    [receiptFor({ revision: 2 ** 53 }), false],
    [{ ...receiptFor(), revision: undefined }, false],
    [null, false],
    [[receiptFor()], false],
    [undefined, false],
  ];
  for (const [receipt, valid] of answers) {
    const f = frame();
    const result = f.api.source.write(SAVE);
    const call = await nextCall(f, 0);
    f.reply({ id: call.id, ok: true, written: receipt });
    if (valid) assert.deepEqual(await result, receipt);
    else assert.equal((await code(result)).code, 'unavailable');
    current.listeners.pagehide();
    delete globalThis.window;
  }
});

test('A save receipt for other bytes is refused, whatever the digest of its own says', async () => {
  const f = frame();
  const result = f.api.source.write({ ...SAVE, content: 'goodbye' });
  const call = await nextCall(f, 0);
  // Right shape, and the digest and size of "hello", not of "goodbye".
  f.reply({ id: call.id, ok: true, written: receiptFor() });
  assert.equal((await code(result)).code, 'unavailable');
});

test('Hostile save arguments are refused without throwing, sending or leaking a slot', async () => {
  const f = frame();
  const { write } = f.api.source;
  const throwingLength = Uint8Array.from([1]);
  Object.defineProperty(throwingLength, 'length', {
    get() {
      throw new Error('getter ran');
    },
  });
  const bad = [
    ...hostileObjects(),
    { ...SAVE, extra: 1 },
    { path: SAVE.path, content: SAVE.content },
    { path: SAVE.path, retryId: SAVE.retryId },
    { content: SAVE.content, retryId: SAVE.retryId },
    { ...SAVE, path: '../a.txt' },
    { ...SAVE, path: 5 },
    { ...SAVE, content: 5 },
    { ...SAVE, content: null },
    { ...SAVE, content: ['h'] },
    { ...SAVE, content: new Uint16Array(2) },
    { ...SAVE, content: new ArrayBuffer(2) },
    { ...SAVE, content: throwingLength },
    { ...SAVE, content: 'a'.repeat(1_048_577) },
    { ...SAVE, content: new Uint8Array(1_048_577) },
    { ...SAVE, retryId: '1234567' },
    { ...SAVE, retryId: 'r'.repeat(37) },
    { ...SAVE, retryId: 'retry:001' },
    { ...SAVE, retryId: 'retry 001' },
    { ...SAVE, retryId: 'retry-001-ü' },
    { ...SAVE, retryId: 7 },
    { ...SAVE, retryId: undefined },
    {
      get path() {
        throw new Error('getter ran');
      },
      content: 'x',
      retryId: SAVE.retryId,
    },
  ];
  for (const argument of bad) {
    let promise;
    assert.doesNotThrow(() => {
      promise = write(argument);
    }, 'write');
    assert.equal((await code(promise)).code, 'invalid_request');
  }
  await settle();
  assert.deepEqual(f.received, [], 'nothing was sent for a refused save');
  // Every slot is still free, and the limits themselves are accepted.
  const edge = [
    write({
      ...SAVE,
      content: new Uint8Array(1_048_576),
      retryId: 'r'.repeat(36),
    }),
    write({ ...SAVE, content: 'a'.repeat(1_048_576), retryId: 'r'.repeat(8) }),
    write({ ...SAVE, content: '' }),
  ];
  await settle();
  assert.equal(f.received.length, 3);
  for (const message of f.received)
    assert.notEqual(parseCall(message), undefined);
  f.hangUp();
  for (const promise of edge)
    await assert.rejects(promise, { code: 'expired' });
});

test('A save path is judged by the shared fixture, and the host accepts what is sent', async () => {
  for (const { path, error } of pathCases) {
    const f = frame();
    const outcome = f.api.source.write({ ...SAVE, path }).then(
      () => 'resolved',
      (rejection) => rejection.code,
    );
    await settle();
    if (error === null) {
      assert.equal(f.received.length, 1, JSON.stringify(path));
      assert.notEqual(
        parseCall(f.received[0]),
        undefined,
        JSON.stringify(path),
      );
    } else {
      assert.deepEqual(f.received, [], JSON.stringify(path));
      assert.equal(await outcome, 'invalid_request', path);
    }
    current.listeners.pagehide();
    delete globalThis.window;
    await outcome;
  }
});

test('A save that is refused or unanswered says so from the closed set, and may be retried', async () => {
  mock.timers.enable({ apis: ['setTimeout'] });
  const f = frame();
  const refused = f.api.source.write(SAVE);
  const call = await nextCall(f, 0);
  f.reply({ id: call.id, ok: false, code: 'stale_view', detail: 'SECRET' });
  const failure = await code(refused);
  assert.equal(failure.code, 'stale_view');
  assert.ok(!failure.message.includes('SECRET'));
  // An unanswered save gives up after thirty seconds and tells the workspace;
  // the same retry identity then asks again.
  const waiting = f.api.source.write(SAVE);
  await settle();
  mock.timers.tick(30_000);
  assert.equal((await code(waiting)).code, 'unavailable');
  await settle();
  assert.deepEqual(f.received.at(-1), { type: 'cancel', id: 2 });
  const again = f.api.source.write(SAVE);
  await settle();
  const sent = f.received.at(-1);
  assert.equal(sent.type, 'source.write');
  assert.equal(sent.retryId, SAVE.retryId);
  f.hangUp();
  await assert.rejects(again, { code: 'expired' });
});
