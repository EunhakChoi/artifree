import assert from 'node:assert/strict';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The assets panel's rules are web code without DOM or React imports; bundle
// them as they ship. The bundle carries its own copy of the client, so the
// errors below are built from that copy for `instanceof` to hold.
const bundle = await build({
  stdin: {
    contents:
      "export * from './asset-attempt.ts'; export { ArtifreeApiError } from '@artifree/client';",
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
  ArtifreeApiError,
  afterOutcome,
  classify,
  conflictVersion,
  downloadName,
  freezeDelete,
  freezeUpload,
  localUploadRefusal,
  OVERSIZE_TEXT,
} = await import(
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
);

const apiError = (status, code, body) =>
  new ArtifreeApiError('failed', { status, code, body });

test('A frozen upload is retried with the same envelope and key after its buffer changes', () => {
  const bytes = new Uint8Array([104, 105]);
  const attempt = freezeUpload('art', 3, 'text/plain', bytes);
  const { key } = attempt;
  bytes[0] = 0;

  const held = afterOutcome(undefined, attempt, 'unknown');

  assert.equal(held, attempt);
  assert.equal(held.key, key);
  assert.deepEqual(held.request, {
    base_asset_version: 3,
    content_type: 'text/plain',
    content_base64: 'aGk=',
  });
  assert.ok(Object.isFrozen(held.request));
});

for (const outcome of ['conflict', 'key_reused', 'refused', 'not_found']) {
  test(`A held unknown attempt survives a later ${outcome} outcome`, () => {
    const held = freezeDelete('art', 'b'.repeat(64), 2);
    const later = freezeDelete('art', 'b'.repeat(64), 5);
    assert.equal(afterOutcome(held, held, outcome), held);
    assert.equal(afterOutcome(held, later, outcome), held);
  });
}

test('A held unknown attempt is cleared only by its own success', () => {
  const held = freezeDelete('art', 'b'.repeat(64), 2);
  const other = freezeDelete('art', 'c'.repeat(64), 2);
  assert.equal(afterOutcome(held, other, 'done'), held);
  assert.equal(afterOutcome(held, held, 'done'), undefined);
});

for (const [name, error, expected] of [
  ['a local refusal (status 0)', apiError(0, 'invalid_request'), 'refused'],
  ['a 404', apiError(404, 'not_found'), 'not_found'],
  ['a 409 asset_conflict', apiError(409, 'asset_conflict'), 'conflict'],
  [
    'a 422 request_key_reused',
    apiError(422, 'request_key_reused'),
    'key_reused',
  ],
  ['a 422', apiError(422, 'asset_type_unsupported'), 'refused'],
  ['a 429 rate limit', apiError(429, 'rate_limited'), 'refused'],
  ['a 503', apiError(503, 'internal_error'), 'unknown'],
  ['a lost answer', apiError(0, 'network_error'), 'unknown'],
  ['an unreadable answer', apiError(200, 'invalid_response'), 'unknown'],
  ['a non-API error', new Error('boom'), 'unknown'],
]) {
  test(`classify reads ${name} as ${expected}`, () => {
    assert.equal(classify(error), expected);
  });
}

for (const [name, body, expected] of [
  ['a non-negative integer', { error: { current_asset_version: 7 } }, 7],
  ['a top-level field', { current_asset_version: 7 }, undefined],
  ['a missing field', { error: {} }, undefined],
  ['a string', { error: { current_asset_version: '7' } }, undefined],
  ['a negative number', { error: { current_asset_version: -1 } }, undefined],
  ['a fraction', { error: { current_asset_version: 1.5 } }, undefined],
  ['a null body', null, undefined],
]) {
  test(`conflictVersion reads ${name} as ${expected}`, () => {
    assert.equal(
      conflictVersion(apiError(409, 'asset_conflict', body)),
      expected,
    );
  });
}

test('A file over 5 MiB is refused locally', () => {
  assert.equal(localUploadRefusal(5_242_881), OVERSIZE_TEXT);
});

for (const size of [5_242_880, 0]) {
  test(`A ${size}-byte file is sent to the server`, () => {
    assert.equal(localUploadRefusal(size), undefined);
  });
}

test('A download is named after its asset ID and never after a path', () => {
  assert.equal(downloadName('a'.repeat(64)), `asset-${'a'.repeat(64)}`);
  assert.throws(() => downloadName('../x'), /safe download name/);
});
