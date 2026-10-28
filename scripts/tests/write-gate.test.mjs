import assert from 'node:assert/strict';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The rule is web code without DOM or React imports; bundle it as it ships.
const bundle = await build({
  entryPoints: [
    fileURLToPath(
      new URL('../../apps/web/src/artifacts/write-gate.ts', import.meta.url),
    ),
  ],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const { prepare, raise, waitFor } = await import(
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
);

const page = {
  resets: 2,
  shownResets: 2,
  known: undefined,
  floor: undefined,
  sending: false,
};

test('A write waits until the page shows every version it learned of', () => {
  assert.equal(waitFor(5, page), undefined);
  // A notice, a list, or the page's own write reported a newer version.
  assert.equal(waitFor(5, { ...page, known: 6 }), 'newer');
  assert.equal(waitFor(6, { ...page, known: 6 }), undefined);
  // A conflict proved one.
  const refused = raise(undefined, 7, 2);
  assert.equal(waitFor(6, { ...page, floor: refused }), 'refused');
  assert.equal(waitFor(7, { ...page, floor: refused }), undefined);
});

test('One write of a gate is sent at a time', () => {
  assert.equal(waitFor(5, { ...page, sending: true }), 'sending');
});

test('Nothing is prepared from a read that predates a possible replacement', () => {
  assert.equal(waitFor(5, { ...page, shownResets: 1 }), 'replaced');
  // What was learned before the replacement no longer holds: replaced data
  // can be older than it, and the page could never reach it.
  const before = raise(undefined, 9, 1);
  assert.equal(waitFor(5, { ...page, floor: before }), undefined);
  assert.deepEqual(raise(before, 3, 2), { version: 3, resets: 2 });
});

test('A floor only rises', () => {
  const floor = raise(undefined, 6, 2);
  assert.equal(raise(floor, 5, 2), floor, 'A lower version is ignored.');
  assert.equal(raise(floor, 6, 2), floor);
  assert.deepEqual(raise(floor, 8, 2), { version: 8, resets: 2 });
});

test('A dialog prepares from what it opened on until the page shows newer', () => {
  const opened = { base: 4, title: 'Old' };
  const shown = { base: 6, title: 'New' };
  const waits = (known, floor) => (version) =>
    waitFor(version, { ...page, known, floor });
  // Nothing newer learned: the dialog keeps its base.
  assert.deepEqual(prepare(opened, shown, waits(undefined), false), {
    from: opened,
    behind: false,
  });
  // A newer version is known and shown: the next request starts from it.
  assert.deepEqual(prepare(opened, shown, waits(6), false), {
    from: shown,
    behind: true,
  });
  // Known but not shown yet: it waits on what it opened on, and says so.
  assert.deepEqual(prepare(opened, shown, waits(7), false), {
    from: opened,
    behind: true,
  });
  // A conflict proved a newer version than either.
  assert.deepEqual(
    prepare(opened, shown, waits(undefined, raise(undefined, 8, 2)), false),
    { from: opened, behind: true },
  );
  // A request whose answer was lost is sent again as it was.
  assert.deepEqual(prepare(opened, shown, waits(6), true), {
    from: opened,
    behind: false,
  });
});
