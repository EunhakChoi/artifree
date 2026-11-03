import assert from 'node:assert/strict';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The frame's side of the protocol is the code the server embeds in an
// opted-in document. Run the bundle as shipped, against a stand-in window
// and a parent that answers on the real message port.
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

const DIGEST = 'e'.repeat(64);
const FILE = {
  path: 'a.txt',
  size: 3,
  sha256: DIGEST,
  mediaType: 'text/plain',
};

/** Starts a runtime; `port` is the parent's end, `seen` what it received. */
function boot(leaseMs = 60_000) {
  const listeners = new Map();
  let connect;
  globalThis.window = {
    artifree: undefined,
    addEventListener: (type, listener) => listeners.set(type, listener),
    setTimeout: (...args) => setTimeout(...args),
    parent: {
      postMessage: (message, _origin, transfer) => {
        connect = { message, port: transfer[0] };
      },
    },
  };
  start({
    view_id: 'a'.repeat(32),
    generation: 1,
    initialization: 'b'.repeat(64),
    lease_ms: leaseMs,
  });
  const { port } = connect;
  const seen = [];
  port.onmessage = (event) => seen.push(event.data);
  return {
    runtime: globalThis.window.artifree,
    connect: connect.message,
    seen,
    reply: (message) => port.postMessage(message),
    hide: () => listeners.get('pagehide')(),
    // Messages posted so far have reached the other port.
    settle: () => new Promise((resolve) => setImmediate(resolve)),
  };
}

async function failure(promise) {
  return promise.then(
    () => assert.fail('The request must fail.'),
    (error) => error,
  );
}

test('The frame announces its view and generation, never anything else', () => {
  const view = boot();
  assert.deepEqual(view.connect, {
    type: 'artifree-runtime-connect',
    viewId: 'a'.repeat(32),
    generation: 1,
    initialization: 'b'.repeat(64),
  });
  assert.ok(Object.isFrozen(view.runtime));
  assert.deepEqual(Object.keys(view.runtime).sort(), [
    'currentMetadata',
    'listSource',
    'readSource',
    'requestDownload',
    'saveSource',
  ]);
  view.hide();
});

test('Requests that are wrong are refused without reaching the parent', async () => {
  const view = boot();
  const bad = [
    view.runtime.readSource(''),
    view.runtime.readSource('naïve.txt'),
    view.runtime.readSource('a.txt', { offset: -1 }),
    view.runtime.readSource('a.txt', { offset: 1.5 }),
    view.runtime.readSource('a.txt', { length: 0 }),
    // A bad key is refused even when the changes are fine.
    view.runtime.saveSource({
      key: 'short',
      changes: [{ op: 'delete', path: 'a.txt' }],
    }),
    view.runtime.saveSource({
      key: 'has a space in it',
      changes: [{ op: 'delete', path: 'a.txt' }],
    }),
    view.runtime.saveSource({
      key: 'frame-key-0001',
      changes: [],
    }),
    view.runtime.saveSource({
      key: 'frame-key-0001',
      changes: [{ op: 'put', path: 'a.txt' }],
    }),
    view.runtime.saveSource({
      key: 'frame-key-0001',
      changes: [{ op: 'put', path: 'a.txt', content: 'x', extra: 1 }],
    }),
    view.runtime.saveSource({
      key: 'frame-key-0001',
      changes: Array.from({ length: 513 }, (_, index) => ({
        op: 'delete',
        path: `f${index}.txt`,
      })),
    }),
    view.runtime.requestDownload({ path: 'a b.txt' }),
  ];
  for (const promise of bad) {
    const error = await failure(promise);
    assert.equal(error.code, 'invalid_request');
    assert.equal(error.name, 'RuntimeRequestError');
  }
  await view.settle();
  assert.deepEqual(view.seen, []);
  view.hide();
});

test('No public method throws: any argument is refused as a rejected request', async () => {
  const view = boot();
  const explode = () => {
    throw new Error('hostile');
  };
  const hostile = new Proxy(
    {},
    { get: explode, ownKeys: explode, getPrototypeOf: explode },
  );
  const hostileArray = new Proxy([], { get: explode });
  class Instance {}
  const bad = [
    // Each of these is a value an untyped caller might pass in place of an
    // object; none may throw before the promise is returned.
    ...[
      null,
      [],
      5,
      'x',
      true,
      Symbol('s'),
      () => undefined,
      new Instance(),
      hostile,
    ].flatMap((value) => [
      () => view.runtime.saveSource(value),
      () => view.runtime.requestDownload(value),
      () => view.runtime.readSource('a.txt', value),
    ]),
    // Not a plain object, even with the right members.
    () =>
      view.runtime.saveSource(
        Object.assign(new Instance(), {
          key: 'frame-key-0001',
          changes: [{ op: 'delete', path: 'a.txt' }],
        }),
      ),
    () =>
      view.runtime.saveSource(
        Object.assign([], {
          key: 'frame-key-0001',
          changes: [{ op: 'delete', path: 'a.txt' }],
        }),
      ),
    () => view.runtime.requestDownload(Object.assign([], { path: 'a.txt' })),
    () => view.runtime.saveSource(undefined),
    () => view.runtime.readSource(),
    () => view.runtime.readSource(null),
    () => view.runtime.readSource({ toString: explode }),
    () =>
      view.runtime.readSource('a.txt', {
        get offset() {
          return explode();
        },
      }),
    () =>
      view.runtime.readSource('a.txt', {
        offset: 0,
        get length() {
          return explode();
        },
      }),
    () =>
      view.runtime.requestDownload({
        get path() {
          return explode();
        },
      }),
    () =>
      view.runtime.saveSource({
        get key() {
          return explode();
        },
        changes: [],
      }),
    () =>
      view.runtime.saveSource({
        key: 'frame-key-0001',
        get changes() {
          return explode();
        },
      }),
    () =>
      view.runtime.saveSource({ key: 'frame-key-0001', changes: hostileArray }),
    () => view.runtime.saveSource({ key: 'frame-key-0001', changes: [null] }),
    () => view.runtime.saveSource({ key: 'frame-key-0001', changes: [[]] }),
    () =>
      view.runtime.saveSource({ key: 'frame-key-0001', changes: [hostile] }),
    () =>
      view.runtime.saveSource({
        key: 'frame-key-0001',
        changes: [
          {
            op: 'put',
            path: 'a.txt',
            get content() {
              return explode();
            },
          },
        ],
      }),
    () =>
      view.runtime.saveSource({
        key: 'frame-key-0001',
        changes: [
          {
            get op() {
              return explode();
            },
            path: 'a.txt',
          },
        ],
      }),
  ];
  for (const attempt of bad) {
    let promise;
    assert.doesNotThrow(() => {
      promise = attempt();
    }, attempt.toString());
    const error = await failure(promise);
    assert.equal(error.code, 'invalid_request', attempt.toString());
    assert.equal(error.name, 'RuntimeRequestError');
  }
  // The methods that take nothing ignore what they are given and still
  // answer asynchronously.
  for (const argument of [null, 5, hostile]) {
    assert.doesNotThrow(() => {
      void view.runtime.currentMetadata(argument).catch(() => undefined);
      void view.runtime.listSource(argument).catch(() => undefined);
    });
  }
  await view.settle();
  // Only the two argument-free requests per round reached the parent.
  assert.deepEqual(
    view.seen.map(({ type }) => type),
    [
      'metadata',
      'source.list',
      'metadata',
      'source.list',
      'metadata',
      'source.list',
    ],
  );
  view.hide();
});

test('A save is copied when it is checked, so later changes cannot alter it', async () => {
  const view = boot();
  const bytes = new Uint8Array([1, 2, 3]);
  const change = { op: 'put', path: 'a.bin', content: bytes };
  const request = { key: 'frame-key-0001', changes: [change] };
  const pending = view.runtime.saveSource(request);
  change.path = 'changed.bin';
  bytes[0] = 9;
  await view.settle();
  assert.equal(view.seen.length, 1);
  assert.equal(view.seen[0].changes[0].path, 'a.bin');
  assert.deepEqual([...view.seen[0].changes[0].content], [1, 2, 3]);
  view.hide();
  await failure(pending);
});

test('Valid requests carry exactly their fields to the parent', async () => {
  const view = boot();
  const calls = [
    view.runtime.currentMetadata(),
    view.runtime.listSource(),
    view.runtime.readSource('a.txt', { offset: 1, length: 2 }),
    view.runtime.readSource('a.txt'),
    view.runtime.saveSource({
      key: 'frame-key-0001',
      changes: [
        { op: 'put', path: 'a.txt', content: 'héllo' },
        { op: 'put', path: 'b.bin', content: new Uint8Array([0, 255]) },
        { op: 'delete', path: 'c.txt' },
      ],
    }),
    view.runtime.requestDownload(),
    view.runtime.requestDownload({ path: 'a.txt' }),
  ];
  await view.settle();
  assert.deepEqual(view.seen, [
    { type: 'metadata', id: 1 },
    { type: 'source.list', id: 2 },
    { type: 'source.read', id: 3, path: 'a.txt', offset: 1, length: 2 },
    { type: 'source.read', id: 4, path: 'a.txt' },
    {
      type: 'source.write',
      id: 5,
      key: 'frame-key-0001',
      changes: [
        { op: 'put', path: 'a.txt', content: 'héllo' },
        { op: 'put', path: 'b.bin', content: new Uint8Array([0, 255]) },
        { op: 'delete', path: 'c.txt' },
      ],
    },
    { type: 'download', id: 6 },
    { type: 'download', id: 7, path: 'a.txt' },
  ]);
  view.hide();
  for (const call of calls) await failure(call);
});

test('Answers are parsed strictly and matched to their request', async () => {
  const view = boot();
  const metadata = view.runtime.currentMetadata();
  const list = view.runtime.listSource();
  const read = view.runtime.readSource('a.txt');
  const write = view.runtime.saveSource({
    key: 'frame-key-0001',
    changes: [{ op: 'put', path: 'a.txt', content: 'abc' }],
  });
  const download = view.runtime.requestDownload();
  view.reply({
    id: 1,
    ok: true,
    metadata: {
      artifactId: 'art',
      title: 'T',
      currentRevision: 2,
      metadataRevision: 1,
    },
  });
  view.reply({
    id: 2,
    ok: true,
    result: { artifactId: 'art', revision: 2, files: [FILE] },
  });
  view.reply({
    id: 3,
    ok: true,
    result: {
      path: 'a.txt',
      revision: 2,
      offset: 0,
      size: 3,
      sha256: DIGEST,
      nextOffset: null,
      bytes: new Uint8Array([97, 98, 99]),
    },
  });
  view.reply({
    id: 4,
    ok: true,
    result: { revision: 3, createdAtMs: 9, replayed: true, files: [FILE] },
  });
  view.reply({
    id: 5,
    ok: true,
    result: { kind: 'archive', name: 'a-r2.zip', size: 22, revision: 2 },
  });
  assert.equal((await metadata).currentRevision, 2);
  assert.deepEqual(await list, {
    artifactId: 'art',
    revision: 2,
    files: [FILE],
  });
  assert.deepEqual(Array.from((await read).bytes), [97, 98, 99]);
  assert.deepEqual(await write, {
    revision: 3,
    createdAtMs: 9,
    replayed: true,
    files: [FILE],
  });
  assert.deepEqual(await download, {
    status: 'started',
    kind: 'archive',
    name: 'a-r2.zip',
    size: 22,
    revision: 2,
  });
  view.hide();
});

test('A malformed or mismatched answer rejects as refused and never resolves', async () => {
  const view = boot();
  const cases = [
    [view.runtime.listSource(), { artifactId: 'a', revision: 0, files: [] }],
    [
      view.runtime.listSource(),
      { artifactId: 'a', revision: 1, files: [{ ...FILE, sha256: 'xyz' }] },
    ],
    [
      view.runtime.readSource('a.txt'),
      {
        path: 'a.txt',
        revision: 1,
        offset: 0,
        size: 3,
        sha256: DIGEST,
        nextOffset: null,
        bytes: 'abc',
      },
    ],
    [
      view.runtime.saveSource({
        key: 'frame-key-0001',
        changes: [{ op: 'delete', path: 'a.txt' }],
      }),
      { revision: 1, createdAtMs: 1, replayed: false, files: [] },
    ],
    [
      view.runtime.requestDownload(),
      { kind: 'html', name: 'a', size: 1, revision: 1 },
    ],
  ];
  cases.forEach(([, result], index) =>
    view.reply({ id: index + 1, ok: true, result }),
  );
  for (const [promise] of cases) {
    const error = await failure(promise);
    assert.equal(error.code, 'refused');
  }
  // The answer of the wrong kind is parsed as the kind that was asked.
  const metadata = view.runtime.currentMetadata();
  view.reply({
    id: 6,
    ok: true,
    result: { artifactId: 'a', revision: 1, files: [] },
  });
  assert.equal(
    (await failure(metadata)).message,
    'The runtime metadata request was refused.',
  );
  view.hide();
});

test('A refusal carries the code and the current revision, never free text', async () => {
  const view = boot();
  const save = () =>
    view.runtime.saveSource({
      key: 'frame-key-0001',
      changes: [{ op: 'delete', path: 'a.txt' }],
    });
  const conflict = save();
  view.reply({
    id: 1,
    ok: false,
    code: 'revision_conflict',
    currentRevision: 7,
    message: 'attacker-controlled text',
  });
  const error = await failure(conflict);
  assert.deepEqual(
    [error.code, error.currentRevision],
    ['revision_conflict', 7],
  );
  assert.ok(!error.message.includes('attacker'));
  for (const [index, code] of [
    [2, 'not_granted'],
    [3, 'declined'],
    [4, 'something-new'],
    [5, 'constructor'],
    [6, '__proto__'],
    [7, undefined],
  ]) {
    const call = save();
    view.reply({ id: index, ok: false, code });
    const failed = await failure(call);
    const known = ['not_granted', 'declined'].includes(code);
    assert.equal(failed.code, known ? code : 'refused', String(code));
    assert.equal(failed.currentRevision, undefined);
  }
  view.hide();
});

test('Answers for unknown ids and non-records are ignored', async () => {
  const view = boot();
  const call = view.runtime.listSource();
  for (const stray of [
    null,
    'x',
    7,
    [],
    { ok: true },
    { id: 'one', ok: true },
    { id: 99, ok: true },
    { id: 1.5, ok: true },
  ])
    view.reply(stray);
  let settled = false;
  call.then(
    () => (settled = true),
    () => (settled = true),
  );
  await view.settle();
  assert.equal(settled, false);
  view.reply({
    id: 1,
    ok: true,
    result: { artifactId: 'a', revision: 1, files: [] },
  });
  assert.equal((await call).revision, 1);
  // An answer is used once.
  view.reply({ id: 1, ok: false });
  view.hide();
});

test('Closing the page rejects what is pending and refuses what follows', async () => {
  const view = boot();
  const pending = view.runtime.listSource();
  view.hide();
  assert.equal((await failure(pending)).code, 'view_unavailable');
  assert.equal(
    (await failure(view.runtime.listSource())).code,
    'view_unavailable',
  );
  assert.equal(
    (await failure(view.runtime.currentMetadata())).code,
    'view_unavailable',
  );
});

test('An expired lease and a flood of requests are refused locally', async () => {
  const expired = boot(0);
  assert.equal(
    (await failure(expired.runtime.listSource())).code,
    'view_unavailable',
  );
  expired.hide();
  const busy = boot();
  const calls = Array.from({ length: 64 }, () => busy.runtime.listSource());
  assert.equal((await failure(busy.runtime.listSource())).code, 'busy');
  busy.hide();
  for (const call of calls) await failure(call);
});

test('A second runtime in the same window is refused', () => {
  const view = boot();
  assert.throws(
    () =>
      start({
        view_id: 'a'.repeat(32),
        generation: 2,
        initialization: 'b'.repeat(64),
        lease_ms: 1,
      }),
    /already installed/,
  );
  view.hide();
});

const PART = {
  path: 'a.txt',
  revision: 2,
  offset: 0,
  size: 3,
  sha256: DIGEST,
  nextOffset: null,
  bytes: new Uint8Array([97, 98, 99]),
};

test('A part whose numbers disagree with its bytes is refused, whole ones are not', async () => {
  const MIB = 1024 * 1024;
  const answers = async (request, results) => {
    const view = boot();
    const outcomes = results.map((result, index) => {
      const promise = request(view.runtime, index);
      return [promise, result];
    });
    outcomes.forEach(([, result], index) =>
      view.reply({ id: index + 1, ok: true, result }),
    );
    const settled = await Promise.all(
      outcomes.map(([promise]) =>
        promise.then(
          () => 'accepted',
          (error) => error.code,
        ),
      ),
    );
    view.hide();
    return settled;
  };
  const bytes = (length) => new Uint8Array(length);
  // What a caller who joins parts at nextOffset would be fooled by.
  assert.deepEqual(
    await answers(
      // The last one asks for more than a part may hold, so only the size
      // of the part itself can refuse it.
      (runtime, index) =>
        runtime.readSource('a.txt', index === 7 ? { length: 2 * MIB } : {}),
      [
        // Empty content, but the next part is said to start at 1 MiB.
        { ...PART, size: 2 * MIB, bytes: bytes(0), nextOffset: MIB },
        // Claims completion before the advertised size.
        { ...PART, size: 10, nextOffset: null },
        // Says more follows but ends where the size does.
        { ...PART, nextOffset: 3 },
        // The bytes do not reach the next offset.
        { ...PART, size: 10, nextOffset: 4 },
        // No progress, past the end, and starting past the end.
        { ...PART, size: 10, bytes: bytes(0), nextOffset: 0 },
        { ...PART, size: 3, nextOffset: 5 },
        { ...PART, offset: 4, size: 3, bytes: bytes(0) },
        // More bytes than a part may hold.
        { ...PART, size: MIB + 1, bytes: bytes(MIB + 1), nextOffset: null },
      ],
    ),
    Array(8).fill('refused'),
  );
  assert.deepEqual(
    await answers(
      // The final part is the one asked for from offset 7.
      (runtime, index) =>
        runtime.readSource('a.txt', index === 1 ? { offset: 7 } : {}),
      [
        // A middle part, a final part, the whole file, and an empty file.
        { ...PART, size: 10, nextOffset: 3 },
        { ...PART, offset: 7, size: 10, nextOffset: null },
        PART,
        { ...PART, size: 0, bytes: bytes(0), nextOffset: null },
        // The largest part there is.
        { ...PART, size: 2 * MIB, bytes: bytes(MIB), nextOffset: MIB },
      ],
    ),
    Array(5).fill('accepted'),
  );
});

test('A read is answered with the file and the bytes that were asked for', async () => {
  const outcome = async (ask, result) => {
    const view = boot();
    const promise = ask(view.runtime);
    view.reply({ id: 1, ok: true, result });
    const settled = await promise.then(
      () => 'accepted',
      (error) => error.code,
    );
    view.hide();
    return settled;
  };
  const asks = (runtime) =>
    runtime.readSource('a.txt', { offset: 1, length: 2 });
  const part = { ...PART, offset: 1, size: 3, nextOffset: null };
  const two = new Uint8Array([98, 99]);
  assert.equal(await outcome(asks, { ...part, bytes: two }), 'accepted');
  assert.equal(
    await outcome(asks, { ...part, path: 'b.txt', bytes: two }),
    'refused',
    'another file',
  );
  assert.equal(
    await outcome(asks, { ...part, offset: 0, size: 2, bytes: two }),
    'refused',
    'another offset',
  );
  assert.equal(
    await outcome(asks, {
      ...part,
      offset: 1,
      size: 4,
      bytes: new Uint8Array(3),
    }),
    'refused',
    'more than the length asked for',
  );
});

test('A listing names each file once and a save receipt only what the save wrote', async () => {
  const outcome = async (ask, result) => {
    const view = boot();
    const promise = ask(view.runtime);
    view.reply({ id: 1, ok: true, result });
    const settled = await promise.then(
      () => 'accepted',
      (error) => error.code,
    );
    view.hide();
    return settled;
  };
  const list = (runtime) => runtime.listSource();
  assert.equal(
    await outcome(list, { artifactId: 'a', revision: 1, files: [FILE, FILE] }),
    'refused',
  );
  const save = (runtime) =>
    runtime.saveSource({
      key: 'frame-key-0001',
      changes: [
        { op: 'put', path: 'a.txt', content: 'héllo' },
        { op: 'put', path: 'b.bin', content: new Uint8Array([0, 255]) },
        { op: 'delete', path: 'c.txt' },
      ],
    });
  const receipt = (files) => ({
    revision: 3,
    createdAtMs: 9,
    replayed: false,
    files,
  });
  const a = { ...FILE, path: 'a.txt', size: 6 };
  const b = { ...FILE, path: 'b.bin', size: 2 };
  assert.equal(await outcome(save, receipt([a, b])), 'accepted');
  for (const [name, files] of [
    ['a file that was not written', [a, b, { ...FILE, path: 'c.txt' }]],
    ['a file missing', [a]],
    ['a size that is not the bytes sent', [{ ...a, size: 5 }, b]],
    ['a file listed twice', [a, a]],
    ['nothing', []],
  ])
    assert.equal(await outcome(save, receipt(files)), 'refused', name);
});

test('A download is answered with the kind that was asked for', async () => {
  const outcome = async (request, kind) => {
    const view = boot();
    const promise = view.runtime.requestDownload(request);
    view.reply({
      id: 1,
      ok: true,
      result: { kind, name: 'x', size: 1, revision: 1 },
    });
    const settled = await promise.then(
      (done) => done.kind,
      (error) => error.code,
    );
    view.hide();
    return settled;
  };
  assert.equal(await outcome({ path: 'a.txt' }, 'file'), 'file');
  assert.equal(await outcome({}, 'archive'), 'archive');
  assert.equal(await outcome({ path: 'a.txt' }, 'archive'), 'refused');
  assert.equal(await outcome({}, 'file'), 'refused');
});
