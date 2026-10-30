import assert from 'node:assert/strict';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The page's drafts are web code without DOM or React imports; bundle the
// module as it ships. The store below stands in for IndexedDB at its
// interface.
const bundle = await build({
  entryPoints: [
    fileURLToPath(
      new URL('../../apps/web/src/artifacts/page-drafts.ts', import.meta.url),
    ),
  ],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const { createPageDrafts } = await import(
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
);

/**
 * A store whose writes finish only when released, oldest first. As in one
 * IndexedDB transaction, a write and the removal of the draft it replaces
 * apply together or not at all. This page's drafts are keyed `mine:<id>`,
 * another tab's `<owner>:<id>`.
 */
function slowStore() {
  const kept = new Map();
  const writes = [];
  const pending = [];
  const later = (apply) =>
    new Promise((resolve, reject) => {
      pending.push({ resolve, reject, apply });
    });
  const removeOther = (other) => {
    if (other !== undefined) kept.delete(`${other.owner}:${other.artifactId}`);
  };
  return {
    kept,
    writes,
    async list() {
      return [...kept.entries()].map(([key, record]) => ({
        ...record,
        mine: key.startsWith('mine:'),
      }));
    },
    put(record, replacing) {
      writes.push(`put ${record.artifactId} ${record.version}`);
      return later(() => {
        kept.set(`mine:${record.artifactId}`, record);
        removeOther(replacing);
      });
    },
    forget(artifactId, replacing) {
      writes.push(`forget ${artifactId}`);
      return later(() => {
        kept.delete(`mine:${artifactId}`);
        removeOther(replacing);
      });
    },
    remove(record) {
      writes.push(`remove ${record.owner}`);
      return later(() => removeOther(record));
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
const record = (version, artifactId = 'a') => ({
  artifactId,
  version,
  keptAt: version,
});
const tick = () => new Promise((resolve) => setImmediate(resolve));
const QUOTA = 'The quota is exceeded.';

test('Only the last state asked for is written, after the write in flight', async () => {
  const store = slowStore();
  const drafts = createPageDrafts(store);
  void drafts.keep('a', record(1));
  await tick();
  void drafts.keep('a', record(2));
  const last = drafts.keep('a', undefined);
  await tick();
  assert.deepEqual(store.writes, ['put a 1'], 'one write at a time');
  store.finish();
  await tick();
  store.finish();
  assert.equal(await last, undefined);
  assert.deepEqual(store.writes, ['put a 1', 'forget a'], 'state 2 skipped');
  assert.equal(store.kept.has('mine:a'), false, 'a late put never undoes it');
});

test('Writes for different artifacts do not wait for each other', async () => {
  const store = slowStore();
  const drafts = createPageDrafts(store);
  void drafts.keep('a', record(1));
  void drafts.keep('b', record(1, 'b'));
  await tick();
  assert.deepEqual(store.writes, ['put a 1', 'put b 1']);
});

test('A failed write is reported until a later write succeeds', async () => {
  const store = slowStore();
  const drafts = createPageDrafts(store);
  const first = drafts.keep('a', record(1));
  await tick();
  store.finish(new Error(QUOTA));
  assert.equal(await first, QUOTA);
  // A write that fails while a later one waits is cleared by that one.
  void drafts.keep('a', record(2));
  await tick();
  const third = drafts.keep('a', record(3));
  await tick();
  store.finish(new Error(QUOTA));
  await tick();
  store.finish();
  assert.equal(await third, undefined);
  assert.equal(store.kept.get('mine:a').version, 3);
});

test('Another tab’s draft that this one continues stays until a write succeeds', async () => {
  const store = slowStore();
  const drafts = createPageDrafts(store);
  const other = { owner: 'closed', artifactId: 'a', version: 4, keptAt: 4 };
  store.kept.set('closed:a', other);
  const adopted = drafts.keep('a', record(4), { ...other, mine: false });
  await tick();
  store.finish(new Error(QUOTA));
  assert.equal(await adopted, QUOTA);
  assert.equal(store.kept.get('closed:a'), other, 'the only copy stays');
  const next = drafts.keep('a', record(5));
  await tick();
  store.finish();
  assert.equal(await next, undefined);
  assert.equal(store.kept.has('closed:a'), false, 'removed with that write');
  assert.equal(store.kept.get('mine:a').version, 5);
});

test('A discard is ordered after the writes asked for before it', async () => {
  const store = slowStore();
  const drafts = createPageDrafts(store);
  // One write is in flight and another waits behind it, as when a view
  // that replaced an earlier one discards its kept draft.
  void drafts.keep('a', record(1));
  await tick();
  void drafts.keep('a', record(2));
  const mine = drafts.discard({ owner: 'me', artifactId: 'a', mine: true });
  await tick();
  store.finish();
  await tick();
  store.finish();
  await mine;
  assert.deepEqual(store.writes, ['put a 1', 'forget a']);
  assert.equal(store.kept.has('mine:a'), false);

  const other = { owner: 'closed', artifactId: 'a', mine: false };
  store.kept.set('closed:a', other);
  void drafts.keep('a', record(3));
  const theirs = drafts.discard(other);
  await tick();
  assert.deepEqual(store.writes.slice(2), ['put a 3'], 'not before it');
  store.finish();
  await tick();
  store.finish();
  await theirs;
  assert.deepEqual(store.writes.slice(2), ['put a 3', 'remove closed']);
  assert.equal(store.kept.has('closed:a'), false);
});

test('A discard that fails says so', async () => {
  const store = slowStore();
  const drafts = createPageDrafts(store);
  store.kept.set('mine:a', record(1));
  const mine = drafts.discard({ owner: 'me', artifactId: 'a', mine: true });
  await tick();
  store.finish(new Error(QUOTA));
  await assert.rejects(mine, { message: QUOTA });
  assert.equal(store.kept.has('mine:a'), true);
});

test('A new view and later write wait for another tab’s pending discard', async () => {
  const store = slowStore();
  const drafts = createPageDrafts(store);
  const other = { owner: 'closed', artifactId: 'a', mine: false, keptAt: 1 };
  store.kept.set('closed:a', other);
  const discarded = drafts.discard(other);
  await tick();
  let listed = false;
  const list = drafts.list().then((items) => {
    listed = true;
    return items;
  });
  const later = drafts.keep('a', record(2));
  await tick();
  assert.equal(listed, false, 'the new view must wait for the discard');
  assert.deepEqual(
    store.writes,
    ['remove closed'],
    'the later write must wait',
  );
  store.finish();
  await discarded;
  const items = await list;
  assert.equal(
    items.some((item) => item.owner === 'closed'),
    false,
  );
  await tick();
  assert.deepEqual(store.writes, ['remove closed', 'put a 2']);
  store.finish();
  assert.equal(await later, undefined);
  assert.equal(store.kept.get('mine:a').version, 2);
});

test('A discard separates keeps asked for before and after it', async () => {
  const store = slowStore();
  const drafts = createPageDrafts(store);
  const other = { owner: 'closed', artifactId: 'a', mine: false, keptAt: 1 };
  store.kept.set('closed:a', other);
  const first = drafts.keep('a', record(1));
  await tick();
  const discarded = drafts.discard(other);
  const later = drafts.keep('a', record(2));
  await tick();
  assert.deepEqual(store.writes, ['put a 1']);
  store.finish();
  await tick();
  assert.deepEqual(store.writes, ['put a 1', 'remove closed']);
  await first;
  store.finish();
  await discarded;
  await tick();
  assert.deepEqual(store.writes, ['put a 1', 'remove closed', 'put a 2']);
  store.finish();
  assert.equal(await later, undefined);
  assert.equal(store.kept.get('mine:a').version, 2);
});

test('A failed other-tab discard remains visible and can be retried', async () => {
  const store = slowStore();
  const drafts = createPageDrafts(store);
  const other = { owner: 'closed', artifactId: 'a', mine: false, keptAt: 1 };
  store.kept.set('closed:a', other);
  const first = drafts.discard(other);
  const refused = assert.rejects(first, { message: QUOTA });
  await tick();
  const list = drafts.list();
  store.finish(new Error(QUOTA));
  await refused;
  assert.equal(
    (await list).some((item) => item.owner === 'closed'),
    true,
  );
  const retry = drafts.discard(other);
  await tick();
  store.finish();
  await retry;
  assert.equal(store.kept.has('closed:a'), false);
});

test('Discarding a draft after failed adoption allows a fresh draft', async () => {
  const store = slowStore();
  const drafts = createPageDrafts(store);
  const other = { owner: 'closed', artifactId: 'a', mine: false, keptAt: 1 };
  store.kept.set('closed:a', other);
  const put = store.put;
  store.put = (next, replacing) => {
    if (
      replacing !== undefined &&
      !store.kept.has(`${replacing.owner}:${replacing.artifactId}`)
    )
      return Promise.reject(new Error('This draft changed.'));
    return put(next, replacing);
  };
  const adoption = drafts.keep('a', record(1), other);
  await tick();
  store.finish(new Error(QUOTA));
  assert.equal(await adoption, QUOTA);
  const discarded = drafts.discard({ ...other });
  await tick();
  store.finish();
  await discarded;
  let failure;
  const fresh = drafts.keep('a', record(2));
  void fresh.then((result) => {
    failure = result;
  });
  await tick();
  assert.equal(
    failure,
    undefined,
    'the removed draft must not reject a new one',
  );
  store.finish();
  assert.equal(await fresh, undefined);
  assert.equal(store.kept.get('mine:a').version, 2);
});

test('The list waits for the writes asked for and offers this page’s drafts first', async () => {
  const store = slowStore();
  const drafts = createPageDrafts(store);
  store.kept.set('closed:a', { owner: 'closed', artifactId: 'a', keptAt: 9 });
  store.kept.set('closed:b', { owner: 'closed', artifactId: 'b', keptAt: 7 });
  void drafts.keep('a', record(1));
  const listed = drafts.list();
  await tick();
  store.finish();
  const records = await listed;
  assert.deepEqual(
    records.map(
      (item) => `${item.mine ? 'mine' : item.owner}:${item.artifactId}`,
    ),
    ['mine:a', 'closed:a', 'closed:b'],
    'its own first, though older; then the newest',
  );
});
