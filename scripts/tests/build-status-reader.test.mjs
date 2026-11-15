import assert from 'node:assert/strict';
import { test } from 'node:test';
import { setImmediate as nextTurn } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The head build status reads of the preview's build choice (decision 0058,
// rule 16, R03eb): a change during a read owes one more read after it, never an
// abort, and every answer is shown, so a stream of changes cannot keep the
// status from being read. A `profile_unknown` answer owes no read until a
// reset, and a reset drops what was read. The reader is bundled as it ships,
// with the client's error class from the same bundle so `instanceof` holds.
const bundle = await build({
  stdin: {
    contents:
      "export { BuildStatusReader } from './build-status-reader.ts'; export { ArtifreeApiError } from '@artifree/client';",
    resolveDir: fileURLToPath(
      new URL('../../apps/web/src/artifacts/', import.meta.url),
    ),
  },
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const { ArtifreeApiError, BuildStatusReader } = await import(
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
);

/** A head status of revision `head`, built. */
const built = (head) => ({
  profile: 'react-v1',
  head_revision: head,
  status: 'built',
  latest_build_revision: head,
});

/**
 * A reader on a port whose reads stay open until the case answers them, in
 * the order they were made.
 */
function harness() {
  const reads = [];
  const reader = new BuildStatusReader(
    (signal) =>
      new Promise((resolve, reject) => {
        reads.push({ signal, resolve, reject });
      }),
  );
  const seen = [];
  reader.subscribe(() => seen.push(reader.snapshot()));
  return { reader, reads, seen };
}

test('changes during a read owe one read after it, and the first is not aborted', async () => {
  const { reader, reads, seen } = harness();
  reader.owe();
  reader.owe();
  reader.owe();
  reader.owe();
  assert.equal(reads.length, 1);
  assert.equal(reads[0].signal.aborted, false);
  reads[0].resolve(built(4));
  await nextTurn();
  assert.equal(reads.length, 2);
  reads[1].resolve(built(5));
  await nextTurn();
  assert.equal(reads.length, 2);
  assert.deepEqual(seen, [
    { kind: 'read', status: built(4) },
    { kind: 'read', status: built(5) },
  ]);
});

test('a failed read is shown in words, and a profile_unknown owes no read until a reset', async () => {
  const { reader, reads } = harness();
  reader.owe();
  reads[0].reject(new Error('Down.'));
  await nextTurn();
  assert.deepEqual(reader.snapshot(), { kind: 'failed', message: 'Down.' });
  reader.owe();
  reads[1].reject(
    new ArtifreeApiError('Unknown profile.', {
      status: 422,
      code: 'profile_unknown',
    }),
  );
  await nextTurn();
  assert.deepEqual(reader.snapshot(), { kind: 'profile_unknown' });
  reader.owe();
  assert.equal(reads.length, 2);
  reader.reset();
  assert.equal(reads.length, 3);
});

test('a reset drops the status and the read in flight, whose answer is never shown', async () => {
  const { reader, reads } = harness();
  reader.owe();
  reads[0].resolve(built(4));
  await nextTurn();
  reader.owe();
  reader.owe();
  reader.reset();
  assert.deepEqual(reader.snapshot(), { kind: 'unread' });
  assert.equal(reads[1].signal.aborted, true);
  assert.equal(reads.length, 3);
  reads[1].resolve(built(9));
  await nextTurn();
  assert.deepEqual(reader.snapshot(), { kind: 'unread' });
  reads[2].resolve(built(5));
  await nextTurn();
  assert.deepEqual(reader.snapshot(), { kind: 'read', status: built(5) });
  assert.equal(reads.length, 3, 'the read owed before the reset was dropped');
});

test('a stopped reader shows no late answer and makes no owed read', async () => {
  const { reader, reads, seen } = harness();
  reader.owe();
  reader.owe();
  reader.stop();
  assert.equal(reads[0].signal.aborted, true);
  reads[0].resolve(built(4));
  await nextTurn();
  assert.equal(reads.length, 1);
  assert.deepEqual(seen, []);
  // A later mount owes its own read, and the read owed before the stop does
  // not follow it.
  reader.owe();
  reads[1].resolve(built(5));
  await nextTurn();
  assert.equal(reads.length, 2);
  assert.deepEqual(seen, [{ kind: 'read', status: built(5) }]);
});
