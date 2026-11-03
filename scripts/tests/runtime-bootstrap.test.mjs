import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
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
    { path: 'a', offset: 0, length: 1 },
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
      this.path = 'a';
      this.offset = 0;
      this.length = 1;
    }
  }
  const symbolic = { path: 'a', offset: 0, length: 1, [Symbol('x')]: 1 };
  const hidden = { offset: 0, length: 1 };
  Object.defineProperty(hidden, 'path', { value: 'a', enumerable: false });
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
    { path: 'a', offset: 0, length: 1, extra: 1 },
    { path: 5, offset: 0, length: 1 },
    { path: '', offset: 0, length: 1 },
    { path: 'a'.repeat(1025), offset: 0, length: 1 },
    { path: 'a', offset: -1, length: 1 },
    { path: 'a', offset: 1.5, length: 1 },
    { path: 'a', offset: Number.NaN, length: 1 },
    { path: 'a', offset: Infinity, length: 1 },
    { path: 'a', offset: 2 ** 53, length: 1 },
    { path: 'a', offset: '0', length: 1 },
    { path: 'a', offset: 0, length: 0 },
    { path: 'a', offset: 0, length: 1_048_577 },
    { path: 'a', offset: 0, length: -1 },
    { path: 'a', length: 1 },
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
    { path: 'a', extra: 1 },
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

test('A call with a strange this or arguments still behaves', async () => {
  const f = frame();
  const { list, read } = f.api.source;
  const calls = [
    list.call(null),
    list.call(undefined, 1, 2, 3),
    read.call({ path: 'x' }, { path: 'a', offset: 0, length: 1 }),
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
  await assert.rejects(f.api.source.read({ path: 'a', offset: 0, length: 1 }), {
    code: 'expired',
  });
  delete globalThis.window;
  const short = frame({ ...CONFIG, lease_ms: 0 });
  await assert.rejects(short.api.source.list(), { code: 'expired' });
});

beforeEach(() => {
  delete globalThis.window;
});
