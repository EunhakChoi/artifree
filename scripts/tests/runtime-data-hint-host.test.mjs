import assert from 'node:assert/strict';
import { afterEach, mock, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The change hint that a view with data.read gets (decision 0053, rules 16 and
// 17) is createDataHint in runtime-host.ts. These tests reach it through the
// host's own module, bundled as it ships, so a wiring that changes the window or
// the message fails here. The hint's timer is the global setTimeout: each test
// replaces it with node:test's fake clock and moves that clock with tick(), so
// nothing waits in real time.
const bundle = await build({
  entryPoints: [
    fileURLToPath(
      new URL('../../apps/web/src/artifacts/runtime-host.ts', import.meta.url),
    ),
  ],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  // lucide-react has no exports map, so its module field is read as the
  // browser build reads it. The neutral platform reads no field unless named.
  mainFields: ['module', 'main'],
  write: false,
});
const { createDataHint } = await import(
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
);

/** The event of a data change hint (rule 16). */
const DATA_CHANGED = 'data.changed';
/** The least time between two hints to one view, in milliseconds (rule 17). */
const WINDOW_MS = 250;
/** Offers that name no version: a version is a whole number of at least 1. */
const NOT_VERSIONS = [0, -1, 1.5, Number.NaN, Number.POSITIVE_INFINITY];

afterEach(() => {
  mock.timers.reset();
});

/**
 * The hint of one view, with the messages it sends in order. A test starts the
 * fake clock before it makes a hint, so the window runs on that clock.
 */
function viewHint() {
  const sent = [];
  const hint = createDataHint((message) => sent.push(message));
  return { hint, sent };
}

/** The hint for `dataVersion`, as rule 16 gives it: an event with no call id. */
function changed(dataVersion) {
  return { type: 'event', event: DATA_CHANGED, dataVersion };
}

test('one version is sent once, when its 250 ms window ends, with no call id', () => {
  mock.timers.enable({ apis: ['setTimeout'] });
  const { hint, sent } = viewHint();
  hint.offer(3);
  mock.timers.tick(WINDOW_MS - 1);
  assert.deepEqual(sent, [], 'nothing is sent before 250 ms');
  mock.timers.tick(1);
  assert.deepEqual(sent, [changed(3)], 'the version is sent at 250 ms');
  assert.equal('id' in sent[0], false, 'a hint names no call id');
  mock.timers.tick(1_000);
  assert.deepEqual(sent, [changed(3)], 'no further hint follows');
});

test('a replayed stretch of versions is one hint with the highest version, 250 ms after the first arrived', () => {
  mock.timers.enable({ apis: ['setTimeout'] });
  const { hint, sent } = viewHint();
  hint.offer(4);
  hint.offer(6);
  hint.offer(5);
  mock.timers.tick(100);
  hint.offer(8);
  // 249 ms in all: the window runs from the first offer, not from the last.
  mock.timers.tick(149);
  assert.deepEqual(sent, [], 'nothing is sent at 249 ms');
  mock.timers.tick(1);
  assert.deepEqual(sent, [changed(8)], 'the stretch is one hint, the highest');
  mock.timers.tick(1_000);
  assert.deepEqual(sent, [changed(8)], 'the stretch sends nothing more');
});

test('two hints to one view are at least 250 ms apart', () => {
  mock.timers.enable({ apis: ['setTimeout'] });
  const { hint, sent } = viewHint();
  hint.offer(8);
  mock.timers.tick(WINDOW_MS);
  assert.deepEqual(sent, [changed(8)], 'the first hint is sent at 250 ms');
  hint.offer(9);
  mock.timers.tick(WINDOW_MS - 1);
  assert.deepEqual(sent, [changed(8)], 'the next hint waits 250 ms more');
  mock.timers.tick(1);
  assert.deepEqual(sent, [changed(8), changed(9)], 'the next hint is sent');
});

test('a version no higher than the last one sent is not sent again, and a higher one is sent after its window', () => {
  mock.timers.enable({ apis: ['setTimeout'] });
  const { hint, sent } = viewHint();
  hint.offer(6);
  mock.timers.tick(WINDOW_MS);
  assert.deepEqual(sent, [changed(6)]);
  hint.offer(6);
  hint.offer(5);
  mock.timers.tick(1_000);
  assert.deepEqual(
    sent,
    [changed(6)],
    'a version equal to or below the last one sent is dropped',
  );
  hint.offer(7);
  mock.timers.tick(WINDOW_MS - 1);
  assert.deepEqual(sent, [changed(6)], 'the higher version waits its window');
  mock.timers.tick(1);
  assert.deepEqual(
    sent,
    [changed(6), changed(7)],
    'the higher version is sent',
  );
});

for (const dataVersion of NOT_VERSIONS)
  test(`an offer of ${dataVersion}, which names no version, sends nothing`, () => {
    mock.timers.enable({ apis: ['setTimeout'] });
    const { hint, sent } = viewHint();
    hint.offer(dataVersion);
    mock.timers.tick(1_000);
    assert.deepEqual(sent, [], 'no hint is sent for it');
  });

/**
 * The states a hint can be closed in. With a version waiting, a later offer
 * cannot open a window of its own; with none waiting, it can, so only the second
 * state tests that close stops offers as well as the window.
 */
const CLOSE_STATES = [
  ['a version is waiting', (hint) => hint.offer(3)],
  ['no version is waiting', () => {}],
];
for (const [name, arrange] of CLOSE_STATES)
  test(`close() sends nothing more when ${name}`, () => {
    mock.timers.enable({ apis: ['setTimeout'] });
    const { hint, sent } = viewHint();
    arrange(hint);
    mock.timers.tick(WINDOW_MS - 1);
    hint.close();
    mock.timers.tick(1_000);
    assert.deepEqual(sent, [], 'nothing is sent after close');
    hint.offer(4);
    mock.timers.tick(1_000);
    assert.deepEqual(sent, [], 'an offer after close is not sent');
  });

test('two views each send their own first hint for the same version', () => {
  mock.timers.enable({ apis: ['setTimeout'] });
  const first = viewHint();
  const second = viewHint();
  first.hint.offer(3);
  second.hint.offer(3);
  mock.timers.tick(WINDOW_MS);
  assert.deepEqual(first.sent, [changed(3)], 'the first view gets its hint');
  assert.deepEqual(
    second.sent,
    [changed(3)],
    'the second view is not held back by the first',
  );
});
