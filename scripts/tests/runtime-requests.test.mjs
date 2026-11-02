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
const { HostRefusal, base64Bytes, frameErrorCode, parseFrameMessage } =
  await import(
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
