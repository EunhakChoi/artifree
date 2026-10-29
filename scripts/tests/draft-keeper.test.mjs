import assert from 'node:assert/strict';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The keeper is web code without DOM or React imports; bundle it as it
// ships. The store below stands in for IndexedDB at its interface.
const bundle = await build({
  entryPoints: [
    fileURLToPath(
      new URL('../../apps/web/src/artifacts/draft-keeper.ts', import.meta.url),
    ),
  ],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const { createDraftKeeper } = await import(
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
);

/**
 * A store whose writes finish only when released, in any order. Like one
 * IndexedDB transaction, a write and the removal of the draft it replaces
 * apply together or not at all. This tab's drafts are keyed `this:<id>`.
 */
function slowStore() {
  const kept = new Map();
  const writes = [];
  const pending = [];
  const later = (apply) =>
    new Promise((resolve, reject) => {
      pending.push({ resolve, reject, apply });
    });
  const replace = (replacing) => {
    if (replacing !== undefined)
      kept.delete(`${replacing.owner}:${replacing.artifactId}`);
  };
  return {
    kept,
    writes,
    pending,
    put(record, replacing) {
      writes.push(`put ${record.version}`);
      return later(() => {
        kept.set(`this:${record.artifactId}`, record);
        replace(replacing);
      });
    },
    forget(artifactId, replacing) {
      writes.push('forget');
      return later(() => {
        kept.delete(`this:${artifactId}`);
        replace(replacing);
      });
    },
    /** Finishes the oldest write, or fails it with `error`. */
    finish(error) {
      const { resolve, reject, apply } = pending.shift();
      if (error === undefined) {
        apply();
        resolve();
      } else reject(error);
    },
  };
}
const record = (version) => ({ artifactId: 'a', version });
const tick = () => new Promise((resolve) => setImmediate(resolve));

test('Only the last state asked for is written, after the write in flight', async () => {
  const store = slowStore();
  const keeper = createDraftKeeper(store, 'a', () => {});
  void keeper.keep(record(1));
  void keeper.keep(record(2));
  const last = keeper.keep(undefined);
  await tick();
  assert.deepEqual(store.writes, ['put 1'], 'one write at a time');
  store.finish();
  await tick();
  assert.deepEqual(store.writes, ['put 1', 'forget'], 'version 2 is skipped');
  store.finish();
  await last;
  assert.equal(
    store.kept.has('this:a'),
    false,
    'a late put never undoes the end',
  );
});

test('A failed write is reported until one succeeds, and settled waits for both', async () => {
  const store = slowStore();
  const results = [];
  const keeper = createDraftKeeper(store, 'a', (failure) =>
    results.push(failure),
  );
  void keeper.keep(record(1));
  await tick();
  store.finish(new Error('The quota is exceeded.'));
  await tick();
  void keeper.keep(record(2));
  const settled = keeper.settled();
  await tick();
  store.finish();
  await settled;
  assert.deepEqual(results, ['The quota is exceeded.', undefined]);
  assert.equal(store.kept.get('this:a').version, 2);
  await keeper.settled();
});

test('A continued draft of another tab stays until a write of this tab succeeds', async () => {
  const store = slowStore();
  const other = { owner: 'closed', artifactId: 'a', version: 4, mine: false };
  store.kept.set('closed:a', other);
  const keeper = createDraftKeeper(store, 'a', () => {});
  void keeper.keep(record(4), other);
  await tick();
  store.finish(new Error('The quota is exceeded.'));
  await tick();
  assert.equal(store.kept.get('closed:a'), other, 'the only copy is kept');
  const next = keeper.keep(record(5));
  await tick();
  store.finish();
  await next;
  assert.equal(store.kept.has('closed:a'), false, 'removed with the write');
  assert.equal(store.kept.get('this:a').version, 5);
});
