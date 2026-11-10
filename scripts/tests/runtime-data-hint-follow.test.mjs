import assert from 'node:assert/strict';
import { afterEach, mock, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The change hint of a view is followDataHints in runtime-host.ts (decision
// 0053, rules 16 and 17). It subscribes to the page's change hub for the view's
// artifact, offers the version of each change at the view's head revision, and
// sends the hint through createDataHint's window. These tests reach it through
// the host's module and the hub's module, both bundled as they ship. Both
// clocks are fake: the window's timer is node:test's setTimeout, and
// performance.now moves only when a test advances it.
const hostBundle = await build({
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
const { followDataHints } = await import(
  `data:text/javascript;base64,${Buffer.from(hostBundle.outputFiles[0].text).toString('base64')}`
);
const hubBundle = await build({
  entryPoints: [
    fileURLToPath(
      new URL('../../apps/web/src/artifacts/data-changes.ts', import.meta.url),
    ),
  ],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const { DataChangeHub } = await import(
  `data:text/javascript;base64,${Buffer.from(hubBundle.outputFiles[0].text).toString('base64')}`
);

/** The event of a data change hint (rule 16). */
const DATA_CHANGED = 'data.changed';
/** The window a hint waits after the first change it covers (rule 17). */
const WINDOW_MS = 250;
/** A lease end that no case below reaches. */
const FAR_DEADLINE = 60_000;

const ARTIFACT = `art_${'a'.repeat(32)}`;
const OTHER = `art_${'b'.repeat(32)}`;
const CURSOR = `${'f'.repeat(32)}.1`;
/** The source revision a view is issued at, and a later one. */
const HEAD_REVISION = 7;
const MOVED_REVISION = 8;

afterEach(() => {
  mock.timers.reset();
  mock.restoreAll();
});

/**
 * Starts the fake setTimeout and a performance.now that reads the same moment,
 * both at zero. advance() moves both. A hint reads the clock when its window
 * ends, at the end of the step that reaches it, so a deadline case steps to the
 * window's end exactly.
 */
function startClock() {
  let now = 0;
  mock.timers.enable({ apis: ['setTimeout'] });
  mock.method(performance, 'now', () => now);
  return {
    advance(ms) {
      now += ms;
      mock.timers.tick(ms);
    },
  };
}

/**
 * The page's change hub, with a count of the listeners each artifact has. The
 * hub keeps its listeners private, so the count shows a subscription that was
 * never removed.
 */
function countedHub() {
  const hub = new DataChangeHub();
  const listeners = new Map();
  return {
    dataChanges: {
      subscribe(artifactId, listener) {
        const remove = hub.subscribe(artifactId, listener);
        const registration = {};
        const live = listeners.get(artifactId) ?? new Set();
        listeners.set(artifactId, live);
        live.add(registration);
        return () => {
          live.delete(registration);
          remove();
        };
      },
    },
    follow: hub.follow,
    listenerCount: (artifactId) => listeners.get(artifactId)?.size ?? 0,
  };
}

/** The change a data write makes, as the change feed delivers it. */
function dataChanged(artifactId, dataVersion, revision = HEAD_REVISION) {
  return {
    type: 'change',
    change: {
      artifact_id: artifactId,
      change: 'data_changed',
      cursor: CURSOR,
      committed_at_ms: 1,
      revision,
      metadata_revision: 1,
      data_version: dataVersion,
    },
  };
}

/** The hint for `dataVersion`, as rule 16 gives it: an event with no call id. */
function changed(dataVersion) {
  return { type: 'event', event: DATA_CHANGED, dataVersion };
}

/**
 * A view of ARTIFACT issued at HEAD_REVISION, with the messages it is sent in
 * order and the function that ends its hints. followDataHints reads only the
 * three fields of a RuntimeView that this sets.
 */
function viewHints(feed, granted, deadline = FAR_DEADLINE) {
  const sent = [];
  const stop = followDataHints({
    view: { artifact_id: ARTIFACT, granted, head_revision: HEAD_REVISION },
    dataChanges: feed.dataChanges,
    deadline,
    send: (message) => {
      sent.push(message);
    },
  });
  return { sent, stop };
}

// Rule 17 sends no hint to a view without data.read, and rule 1 keeps
// data.write apart from it.
const WITHOUT_DATA_READ = [
  ['no grant', []],
  ['data.write but not data.read', ['data.write']],
];
for (const [name, granted] of WITHOUT_DATA_READ) {
  test(`a view with ${name} subscribes to nothing and is sent nothing`, () => {
    const clock = startClock();
    const feed = countedHub();
    const withoutRead = viewHints(feed, granted);
    assert.equal(feed.listenerCount(ARTIFACT), 0, 'no listener is registered');
    // The view with data.read on the same artifact shows that a change at the
    // head revision would reach a listener and be sent.
    const withRead = viewHints(feed, ['data.read']);
    assert.equal(
      feed.listenerCount(ARTIFACT),
      1,
      'only the view with data.read listens',
    );
    feed.follow(dataChanged(ARTIFACT, 5));
    clock.advance(WINDOW_MS);
    assert.deepEqual(
      withRead.sent,
      [changed(5)],
      'the view with data.read is sent its hint',
    );
    assert.deepEqual(
      withoutRead.sent,
      [],
      'the view without data.read is sent nothing',
    );
  });
}

test('a view with data.read is sent one data.changed hint with its version, 250 ms after the change', () => {
  const clock = startClock();
  const feed = countedHub();
  const { sent } = viewHints(feed, ['data.read']);
  feed.follow(dataChanged(ARTIFACT, 5));
  clock.advance(WINDOW_MS - 1);
  assert.deepEqual(sent, [], 'nothing is sent before 250 ms');
  clock.advance(1);
  assert.deepEqual(sent, [changed(5)], 'the hint is sent at 250 ms');
  clock.advance(1_000);
  assert.deepEqual(sent, [changed(5)], 'no further hint follows');
});

test("a change at a revision other than the view's head is not forwarded, while one at the head is", () => {
  const clock = startClock();
  const feed = countedHub();
  const { sent } = viewHints(feed, ['data.read']);
  // The source moved after the view was issued. Were the higher version
  // forwarded, it would be the one the window sends.
  feed.follow(dataChanged(ARTIFACT, 9, MOVED_REVISION));
  feed.follow(dataChanged(ARTIFACT, 6, HEAD_REVISION));
  clock.advance(WINDOW_MS);
  assert.deepEqual(sent, [changed(6)], 'only the change at the head is sent');
});

test("a change of another artifact is not forwarded, while one of the view's artifact is", () => {
  const clock = startClock();
  const feed = countedHub();
  const { sent } = viewHints(feed, ['data.read']);
  feed.follow(dataChanged(OTHER, 7));
  feed.follow(dataChanged(ARTIFACT, 5));
  clock.advance(WINDOW_MS);
  assert.deepEqual(sent, [changed(5)], "only the view's own artifact is sent");
});

/**
 * A change at time zero opens a window that ends at WINDOW_MS. The deadline
 * decides whether the hint is sent when that window ends.
 */
const DEADLINE_CASES = [
  ['is not sent when its window ends at the deadline', WINDOW_MS, []],
  ['is not sent when the deadline passed before its window ends', 100, []],
  [
    'is sent when its window ends one millisecond before the deadline',
    WINDOW_MS + 1,
    [changed(5)],
  ],
];
for (const [name, deadline, expected] of DEADLINE_CASES) {
  test(`a hint ${name}`, () => {
    const clock = startClock();
    const feed = countedHub();
    const { sent } = viewHints(feed, ['data.read'], deadline);
    feed.follow(dataChanged(ARTIFACT, 5));
    clock.advance(WINDOW_MS);
    assert.deepEqual(sent, expected, 'the deadline decides the send');
  });
}

test('stop() drops a waiting hint and removes its listener, so a later change is not sent', () => {
  const clock = startClock();
  const feed = countedHub();
  const stopped = viewHints(feed, ['data.read']);
  const kept = viewHints(feed, ['data.read']);
  feed.follow(dataChanged(ARTIFACT, 5));
  clock.advance(WINDOW_MS - 1);
  stopped.stop();
  assert.equal(
    feed.listenerCount(ARTIFACT),
    1,
    'only the view that did not stop is still subscribed',
  );
  clock.advance(1);
  assert.deepEqual(stopped.sent, [], 'the waiting hint is not sent');
  assert.deepEqual(kept.sent, [changed(5)], 'the other view is sent its hint');
  feed.follow(dataChanged(ARTIFACT, 6));
  clock.advance(WINDOW_MS);
  assert.deepEqual(stopped.sent, [], 'a later change is not sent');
  assert.deepEqual(
    kept.sent,
    [changed(5), changed(6)],
    'the other view is sent the later change',
  );
});

test("stop() called twice does not throw and leaves the other view's hint in place", () => {
  const clock = startClock();
  const feed = countedHub();
  const stopped = viewHints(feed, ['data.read']);
  const kept = viewHints(feed, ['data.read']);
  assert.doesNotThrow(() => {
    stopped.stop();
    stopped.stop();
  });
  assert.equal(
    feed.listenerCount(ARTIFACT),
    1,
    'the second stop removes no other listener',
  );
  feed.follow(dataChanged(ARTIFACT, 5));
  clock.advance(WINDOW_MS);
  assert.deepEqual(stopped.sent, [], 'the stopped view is sent nothing');
  assert.deepEqual(kept.sent, [changed(5)], "the other view's hint is sent");
});
