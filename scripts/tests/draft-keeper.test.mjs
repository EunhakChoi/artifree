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
const { discardKept, draftKeeper } = await import(
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
    remove(listed) {
      writes.push(`remove ${listed.owner}`);
      return later(() => replace(listed));
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
  const keeper = draftKeeper(store, 'a');
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
  const keeper = draftKeeper(store, 'a');
  const first = keeper.keep(record(1));
  await tick();
  store.finish(new Error('The quota is exceeded.'));
  assert.equal(await first, 'The quota is exceeded.');
  // A write that fails while a later one waits is cleared by that one.
  void keeper.keep(record(2));
  const third = keeper.keep(record(3));
  const settled = keeper.settled();
  await tick();
  store.finish(new Error('The quota is exceeded.'));
  await tick();
  store.finish();
  await settled;
  assert.equal(await third, undefined);
  assert.deepEqual(store.writes, ['put 1', 'put 2', 'put 3']);
  assert.equal(store.kept.get('this:a').version, 3);
  await keeper.settled();
});

test('A view that replaces another writes after the writes it asked for', async () => {
  const store = slowStore();
  // The first view saves and is replaced while its write is in flight.
  const first = draftKeeper(store, 'a');
  void first.keep(record(1));
  void first.keep(undefined);
  // The view that replaced it starts a new draft of the same artifact.
  const last = draftKeeper(store, 'a').keep(record(2));
  await tick();
  assert.deepEqual(store.writes, ['put 1'], 'one write at a time');
  store.finish();
  await tick();
  store.finish();
  await last;
  assert.deepEqual(store.writes, ['put 1', 'put 2']);
  assert.equal(store.kept.get('this:a').version, 2, 'the new draft stays');
});

test('A discard is ordered after the writes asked for before it', async () => {
  const store = slowStore();
  const keeper = draftKeeper(store, 'a');
  // An earlier view's write is in flight and another waits behind it.
  void keeper.keep(record(1));
  void keeper.keep(record(2));
  const mine = discardKept(store, {
    owner: 'tab',
    artifactId: 'a',
    mine: true,
  });
  await tick();
  store.finish();
  await tick();
  store.finish();
  await mine;
  assert.deepEqual(store.writes, ['put 1', 'forget']);
  assert.equal(store.kept.has('this:a'), false, 'a late put never undoes it');

  const other = { owner: 'closed', artifactId: 'a', mine: false };
  store.kept.set('closed:a', other);
  void keeper.keep(record(3));
  const theirs = discardKept(store, other);
  await tick();
  assert.deepEqual(store.writes.slice(2), ['put 3'], 'removed after it');
  store.finish();
  await tick();
  store.finish();
  await theirs;
  assert.deepEqual(store.writes.slice(2), ['put 3', 'remove closed']);
  assert.equal(store.kept.has('closed:a'), false);
});

test('A continued draft of another tab stays until a write of this tab succeeds', async () => {
  const store = slowStore();
  const other = { owner: 'closed', artifactId: 'a', version: 4, mine: false };
  store.kept.set('closed:a', other);
  const keeper = draftKeeper(store, 'a');
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
