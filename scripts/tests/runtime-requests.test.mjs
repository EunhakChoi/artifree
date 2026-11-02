import assert from 'node:assert/strict';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The workspace's side of the frame protocol is web code without DOM or
// React imports; bundle it as it ships.
const bundle = await build({
  entryPoints: [
    fileURLToPath(
      new URL(
        '../../apps/web/src/artifacts/runtime-requests.ts',
        import.meta.url,
      ),
    ),
  ],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const {
  FrameAdmission,
  HostRefusal,
  RequestBudget,
  base64Bytes,
  frameErrorCode,
  frameMessageId,
  parseFrameMessage,
} = await import(
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
);

const put = (path, content) => ({ op: 'put', path, content });
const save = (changes, key = 'frame-key-0001') => ({
  type: 'source.write',
  id: 1,
  key,
  changes,
});
const refused = (message) => {
  const parsed = parseFrameMessage(message);
  assert.equal(parsed?.ok, false, JSON.stringify(message));
  return parsed;
};

test('Each request kind is read into exactly its own fields', () => {
  assert.deepEqual(parseFrameMessage({ type: 'metadata', id: 3 }), {
    ok: true,
    request: { kind: 'metadata', id: 3 },
  });
  assert.deepEqual(parseFrameMessage({ type: 'source.list', id: 4 }), {
    ok: true,
    request: { kind: 'source.list', id: 4 },
  });
  assert.deepEqual(
    parseFrameMessage({
      type: 'source.read',
      id: 5,
      path: 'a/b.txt',
      offset: 10,
      length: 20,
    }),
    {
      ok: true,
      request: {
        kind: 'source.read',
        id: 5,
        path: 'a/b.txt',
        offset: 10,
        length: 20,
      },
    },
  );
  assert.deepEqual(
    parseFrameMessage({ type: 'source.read', id: 5, path: 'a.txt' }),
    { ok: true, request: { kind: 'source.read', id: 5, path: 'a.txt' } },
  );
  assert.deepEqual(parseFrameMessage({ type: 'download', id: 6 }), {
    ok: true,
    request: { kind: 'download', id: 6 },
  });
  assert.deepEqual(
    parseFrameMessage({ type: 'download', id: 6, path: 'index.html' }),
    { ok: true, request: { kind: 'download', id: 6, path: 'index.html' } },
  );
  const parsed = parseFrameMessage(
    save([
      put('notes.txt', 'héllo 🙂'),
      put('sprite.png', new Uint8Array([0, 255, 10])),
      { op: 'delete', path: 'old.txt' },
    ]),
  );
  assert.equal(parsed.ok, true);
  const { changes, key } = parsed.request;
  assert.equal(key, 'frame-key-0001');
  assert.deepEqual(Array.from(changes[0].bytes), [
    ...new TextEncoder().encode('héllo 🙂'),
  ]);
  assert.deepEqual(Array.from(changes[1].bytes), [0, 255, 10]);
  assert.deepEqual(changes[2], { op: 'delete', path: 'old.txt' });
});

test('A message without a usable id is dropped; any other defect is answered', () => {
  for (const message of [
    undefined,
    null,
    'metadata',
    7,
    [],
    {},
    { type: 'metadata' },
    { type: 'metadata', id: 0 },
    { type: 'metadata', id: -1 },
    { type: 'metadata', id: 1.5 },
    { type: 'metadata', id: '1' },
    { type: 'metadata', id: Number.MAX_SAFE_INTEGER + 1 },
    { type: 'metadata', id: NaN },
  ])
    assert.equal(parseFrameMessage(message), undefined, String(message));
  for (const type of [undefined, 'unknown', 'save', 7, null, 'source.rename'])
    assert.deepEqual(parseFrameMessage({ type, id: 9 }), {
      ok: false,
      id: 9,
      code: 'invalid_request',
    });
});

test('Unknown members and malformed fields are refused, not ignored', () => {
  for (const message of [
    { type: 'metadata', id: 1, extra: true },
    { type: 'source.list', id: 1, path: 'a.txt' },
    { type: 'source.read', id: 1 },
    { type: 'source.read', id: 1, path: '' },
    { type: 'source.read', id: 1, path: 'x'.repeat(241) },
    { type: 'source.read', id: 1, path: 'a b.txt' },
    { type: 'source.read', id: 1, path: 'naïve.txt' },
    { type: 'source.read', id: 1, path: 7 },
    { type: 'source.read', id: 1, path: 'a.txt', offset: -1 },
    { type: 'source.read', id: 1, path: 'a.txt', offset: 1.5 },
    { type: 'source.read', id: 1, path: 'a.txt', offset: '1' },
    { type: 'source.read', id: 1, path: 'a.txt', length: 0 },
    { type: 'source.read', id: 1, path: 'a.txt', length: Infinity },
    { type: 'source.read', id: 1, path: 'a.txt', base: 3 },
    { type: 'download', id: 1, path: '' },
    { type: 'download', id: 1, path: 'a b.txt' },
    { type: 'download', id: 1, path: 3 },
    { type: 'download', id: 1, revision: 2 },
  ]) {
    const parsed = refused(message);
    assert.equal(parsed.id, 1);
    assert.equal(parsed.code, 'invalid_request');
  }
});

test('A save is checked field by field before anything is sent', () => {
  for (const message of [
    save([put('a.txt', 'x')], 'short'),
    save([put('a.txt', 'x')], 'has space in it'),
    save([put('a.txt', 'x')], 'k'.repeat(101)),
    save([put('a.txt', 'x')], 7),
    { ...save([put('a.txt', 'x')]), base_revision: 3 },
    save('a.txt'),
    save({ 0: put('a.txt', 'x'), length: 1 }),
    save([]),
    save([null]),
    save(['a.txt']),
    save([{ op: 'put', path: 'a.txt' }]),
    save([{ op: 'put', path: 'a.txt', content: 7 }]),
    save([{ op: 'put', path: 'a.txt', content: [1, 2] }]),
    save([{ op: 'put', path: 'a.txt', content: 'x', mode: 'exec' }]),
    save([{ op: 'delete', path: 'a.txt', content: 'x' }]),
    save([{ op: 'rename', path: 'a.txt' }]),
    save([{ op: 'delete' }]),
    save([put('a.txt', 'x'), put('a.txt', 'y')]),
    save([put('a.txt', 'x'), { op: 'delete', path: 'a.txt' }]),
    save([put('../a.txt', 'x'), put('a b.txt', 'x')]),
    // Text that is not UTF-8 would be stored as other bytes: refused.
    save([put('a.txt', 'lone \ud800 surrogate')]),
    save([put('a.txt', 'lone \udc00 surrogate')]),
  ]) {
    const parsed = refused(message);
    assert.equal(parsed.code, 'invalid_request', JSON.stringify(message));
  }
  // Well-formed surrogate pairs are text.
  assert.equal(parseFrameMessage(save([put('a.txt', '🙂')])).ok, true);
});

test('Size limits are checked here, so an oversize save never leaves the workspace', () => {
  const many = Array.from({ length: 513 }, (_, index) =>
    put(`many/${index}.txt`, 'x'),
  );
  assert.equal(refused(save(many)).code, 'too_large');
  const exact = Array.from({ length: 512 }, (_, index) =>
    put(`many/${index}.txt`, 'x'),
  );
  assert.equal(parseFrameMessage(save(exact)).ok, true);
  const file = 5 * 1024 * 1024;
  assert.equal(
    parseFrameMessage(save([put('a.bin', new Uint8Array(file))])).ok,
    true,
  );
  assert.equal(
    refused(save([put('a.bin', new Uint8Array(file + 1))])).code,
    'too_large',
  );
  const five = Array.from({ length: 5 }, (_, index) =>
    put(`f${index}.bin`, new Uint8Array(file)),
  );
  assert.equal(parseFrameMessage(save(five)).ok, true);
  assert.equal(
    refused(save([...five, put('g.bin', new Uint8Array(1))])).code,
    'too_large',
  );
});

test('A save holds its own copy of the frame buffers', () => {
  const bytes = new Uint8Array([1, 2, 3]);
  const parsed = parseFrameMessage(save([put('a.bin', bytes)]));
  bytes[0] = 99;
  assert.deepEqual(Array.from(parsed.request.changes[0].bytes), [1, 2, 3]);
});

test('Only the closed set of codes crosses to a frame', () => {
  const expected = {
    forbidden: 'not_granted',
    revision_conflict: 'revision_conflict',
    request_key_reused: 'key_reused',
    payload_too_large: 'too_large',
    range_not_satisfiable: 'range',
    rate_limited: 'rate_limited',
    runtime_session_expired: 'view_unavailable',
    stale_view: 'view_unavailable',
    not_found: 'not_found',
    invalid_request: 'invalid_request',
    network_error: 'unavailable',
    unavailable: 'unavailable',
    internal: 'unavailable',
    invalid_response: 'unavailable',
    unauthenticated: 'refused',
    metadata_conflict: 'refused',
    something_new: 'refused',
    constructor: 'refused',
    __proto__: 'refused',
  };
  for (const [code, mapped] of Object.entries(expected))
    assert.equal(frameErrorCode(code), mapped, code);
  assert.equal(frameErrorCode('__proto__'), 'refused');
  assert.equal(new HostRefusal('declined').code, 'declined');
  assert.ok(new HostRefusal('busy') instanceof Error);
});

test('Server bytes are decoded exactly', () => {
  const all = Uint8Array.from({ length: 256 }, (_, index) => index);
  assert.deepEqual(base64Bytes(Buffer.from(all).toString('base64')), all);
  assert.deepEqual(base64Bytes(''), new Uint8Array());
  assert.throws(() => base64Bytes('not base64!'));
});

test('Text that starts with a byte order mark is saved as written', () => {
  const text = '\uFEFFhello — 日本語';
  const parsed = parseFrameMessage(save([put('bom.txt', text)]));
  assert.equal(parsed.ok, true);
  const bytes = parsed.request.changes[0].bytes;
  assert.deepEqual(
    Array.from(bytes),
    Array.from(Buffer.from(text, 'utf8')),
    'the mark is three bytes of the file, not dropped',
  );
  assert.deepEqual(Array.from(bytes.slice(0, 3)), [0xef, 0xbb, 0xbf]);
  // A mark alone, and a mark followed by nothing but a surrogate pair.
  assert.equal(parseFrameMessage(save([put('a.txt', '\uFEFF')])).ok, true);
  assert.equal(parseFrameMessage(save([put('a.txt', '\uFEFF🙂')])).ok, true);
  // Bytes that start with the mark are passed on untouched.
  const raw = new Uint8Array([0xef, 0xbb, 0xbf, 0x61]);
  const kept = parseFrameMessage(save([put('raw.bin', raw)]));
  assert.deepEqual(
    Array.from(kept.request.changes[0].bytes),
    [0xef, 0xbb, 0xbf, 0x61],
  );
  // A lone surrogate is still refused, also after a mark.
  assert.equal(
    refused(save([put('a.txt', '\uFEFF\ud800')])).code,
    'invalid_request',
  );
  assert.equal(refused(save([put('a.txt', '\ud800')])).code, 'invalid_request');
});

test('A frame is read only after its budget allows it', () => {
  assert.equal(frameMessageId({ id: 7, type: 'x' }), 7);
  for (const message of [
    null,
    5,
    'x',
    [],
    {},
    { id: 0 },
    { id: -1 },
    { id: 1.5 },
    { id: '1' },
  ])
    assert.equal(frameMessageId(message), undefined, JSON.stringify(message));
  const budget = new RequestBudget(3, 100, 1_000);
  assert.deepEqual(
    [1, 2, 3, 4].map(() => budget.take(1_000)),
    [true, true, true, false],
  );
  assert.equal(budget.take(1_099), false, 'less than one interval has passed');
  assert.equal(budget.take(1_100), true, 'one interval later there is one');
  assert.equal(budget.take(1_100), false);
  // A long pause refills the burst and no more than the burst.
  assert.deepEqual(
    [1, 2, 3, 4].map(() => budget.take(60_000)),
    [true, true, true, false],
  );
  // A clock that goes backwards neither refills nor breaks the budget.
  assert.equal(budget.take(10), false);
});

test('A frame is admitted one message at a time, budget first', () => {
  const admission = new FrameAdmission(0);
  const list = (id) => ({ type: 'source.list', id });
  // A message nothing can answer is dropped and costs nothing.
  for (let index = 0; index < 100; index += 1)
    assert.deepEqual(admission.admit({ type: 'source.list' }, 0, 0, false), {
      action: 'ignore',
    });
  // The burst is carried out; the next messages are refused by the budget
  // without being read, however large they claim to be.
  for (let id = 1; id <= 20; id += 1)
    assert.equal(admission.admit(list(id), 0, 0, false).action, 'run');
  const huge = save([put('a.bin', new Uint8Array(4 * 1024 * 1024))]);
  assert.deepEqual(admission.admit({ ...huge, id: 21 }, 0, 0, false), {
    action: 'answer',
    id: 21,
    code: 'rate_limited',
  });
  assert.deepEqual(admission.admit({ type: 'junk', id: 22 }, 0, 0, false), {
    action: 'answer',
    id: 22,
    code: 'rate_limited',
  });
  // A tenth of a second later there is one more.
  assert.equal(admission.admit(list(23), 100, 0, false).action, 'run');
});

test('Requests in flight are capped, and one save at a time', () => {
  const admission = new FrameAdmission(0);
  const answer = (message, inFlight, saving) =>
    admission.admit(message, 0, inFlight, saving);
  assert.equal(answer({ type: 'source.list', id: 1 }, 7, false).action, 'run');
  assert.deepEqual(answer({ type: 'source.list', id: 2 }, 8, false), {
    action: 'answer',
    id: 2,
    code: 'busy',
  });
  assert.equal(answer(save([put('a.txt', 'x')]), 0, false).action, 'run');
  assert.deepEqual(answer({ ...save([put('a.txt', 'x')]), id: 9 }, 1, true), {
    action: 'answer',
    id: 9,
    code: 'busy',
  });
  // A malformed request is answered as one, not as busy or run.
  assert.deepEqual(answer({ type: 'source.read', id: 10 }, 0, false), {
    action: 'answer',
    id: 10,
    code: 'invalid_request',
  });
});
