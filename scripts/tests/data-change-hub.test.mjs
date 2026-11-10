import assert from 'node:assert/strict';
import { afterEach, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The change hub is web code whose only import is a type; bundle it as it ships.
const bundle = await build({
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
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
);

const ARTIFACT = `art_${'a'.repeat(32)}`;
const OTHER = `art_${'b'.repeat(32)}`;
const CURSOR = `${'f'.repeat(32)}.1`;

/**
 * Every `ChangeKind` of the contract except `data_changed`, plus `unknown`,
 * the value a client gives a kind it does not know.
 */
const OTHER_KINDS = [
  'created',
  'revised',
  'metadata_updated',
  'deleted',
  'recovered',
  'purged',
  'unknown',
];

/** A committed change as the change feed delivers it. */
function change(artifactId, kind, extra = {}) {
  return {
    type: 'change',
    change: {
      artifact_id: artifactId,
      change: kind,
      cursor: CURSOR,
      committed_at_ms: 1,
      revision: 2,
      metadata_revision: 1,
      ...extra,
    },
  };
}

/** The change a data write makes, with the version it left the data space at. */
function dataChanged(artifactId, dataVersion) {
  return change(artifactId, 'data_changed', { data_version: dataVersion });
}

/** Subscribes a listener that records each version it hears. */
function recorder(hub, artifactId) {
  const heard = [];
  const remove = hub.subscribe(artifactId, (version) => heard.push(version));
  return { heard, remove };
}

// Captured once, before any test replaces it.
const realConsoleError = console.error;

/**
 * Replaces console.error for the running test and returns the argument list
 * of each call; afterEach puts the real one back.
 */
function captureConsoleError() {
  const calls = [];
  console.error = (...args) => {
    calls.push(args);
  };
  return calls;
}

afterEach(() => {
  console.error = realConsoleError;
});

test('delivers a data_changed change to the listeners of its artifact only, with its version', () => {
  const hub = new DataChangeHub();
  const own = recorder(hub, ARTIFACT);
  const other = recorder(hub, OTHER);
  hub.follow(dataChanged(ARTIFACT, 5));
  assert.deepEqual(own.heard, [5]);
  assert.deepEqual(other.heard, []);
});

test('passes the change revision to a listener as its second argument', () => {
  const hub = new DataChangeHub();
  const heard = [];
  hub.subscribe(ARTIFACT, (dataVersion, revision) => {
    heard.push({ dataVersion, revision });
  });
  hub.follow(
    change(ARTIFACT, 'data_changed', { data_version: 5, revision: 9 }),
  );
  assert.deepEqual(heard, [{ dataVersion: 5, revision: 9 }]);
});

// The kind alone must decide: the second form carries a version, which the
// client never sends with a kind other than data_changed.
const ignored = [
  ...OTHER_KINDS.map((kind) => [
    `ignores a change of kind '${kind}'`,
    change(ARTIFACT, kind),
  ]),
  ...OTHER_KINDS.map((kind) => [
    `ignores a change of kind '${kind}' that carries a version`,
    change(ARTIFACT, kind, { data_version: 7 }),
  ]),
  [
    "ignores a change of kind 'data_changed' without a version",
    change(ARTIFACT, 'data_changed'),
  ],
  ["ignores a 'ready' event", { type: 'ready', cursor: CURSOR }],
  ["ignores a 'reset' event", { type: 'reset', cursor: CURSOR }],
];

for (const [name, event] of ignored) {
  test(name, () => {
    const hub = new DataChangeHub();
    const listener = recorder(hub, ARTIFACT);
    hub.follow(event);
    assert.deepEqual(listener.heard, []);
  });
}

test('calls each of two listeners of the artifact', () => {
  const hub = new DataChangeHub();
  const first = recorder(hub, ARTIFACT);
  const second = recorder(hub, ARTIFACT);
  hub.follow(dataChanged(ARTIFACT, 5));
  assert.deepEqual(first.heard, [5]);
  assert.deepEqual(second.heard, [5]);
});

test('calls a listener registered twice once per registration', () => {
  const hub = new DataChangeHub();
  const heard = [];
  const listener = (version) => heard.push(version);
  hub.subscribe(ARTIFACT, listener);
  hub.subscribe(ARTIFACT, listener);
  hub.follow(dataChanged(ARTIFACT, 5));
  assert.deepEqual(heard, [5, 5]);
});

test('a remover drops only its own registration, and calling it again changes nothing', () => {
  const hub = new DataChangeHub();
  const heard = [];
  const listener = (version) => heard.push(version);
  const removeFirst = hub.subscribe(ARTIFACT, listener);
  const removeSecond = hub.subscribe(ARTIFACT, listener);
  removeFirst();
  removeFirst();
  hub.follow(dataChanged(ARTIFACT, 5));
  assert.deepEqual(heard, [5]);
  removeSecond();
  removeSecond();
  hub.follow(dataChanged(ARTIFACT, 6));
  assert.deepEqual(heard, [5]);
});

test('a new subscription works after every registration of the artifact is removed', () => {
  const hub = new DataChangeHub();
  const gone = recorder(hub, ARTIFACT);
  gone.remove();
  gone.remove();
  hub.follow(dataChanged(ARTIFACT, 5));
  const later = recorder(hub, ARTIFACT);
  hub.follow(dataChanged(ARTIFACT, 6));
  assert.deepEqual(gone.heard, []);
  assert.deepEqual(later.heard, [6]);
});

test('a listener that removes another during a change does not break the dispatch', () => {
  const hub = new DataChangeHub();
  const first = [];
  const second = [];
  const third = [];
  let removeSecond;
  hub.subscribe(ARTIFACT, (version) => {
    first.push(version);
    removeSecond();
  });
  removeSecond = hub.subscribe(ARTIFACT, (version) => second.push(version));
  hub.subscribe(ARTIFACT, (version) => third.push(version));
  // The dispatch iterates a copy, so the listener removed during the first
  // change still hears it; the removal applies from the next change on.
  hub.follow(dataChanged(ARTIFACT, 5));
  hub.follow(dataChanged(ARTIFACT, 6));
  assert.deepEqual(first, [5, 6]);
  assert.deepEqual(second, [5]);
  assert.deepEqual(third, [5, 6]);
});

test('a listener that throws does not stop a later listener of the artifact', () => {
  const hub = new DataChangeHub();
  captureConsoleError();
  hub.subscribe(ARTIFACT, () => {
    throw new Error('listener failed');
  });
  const later = recorder(hub, ARTIFACT);
  hub.follow(dataChanged(ARTIFACT, 5));
  assert.deepEqual(later.heard, [5]);
});

test('follow() does not throw when a listener throws', () => {
  const hub = new DataChangeHub();
  captureConsoleError();
  hub.subscribe(ARTIFACT, () => {
    throw new Error('listener failed');
  });
  assert.doesNotThrow(() => hub.follow(dataChanged(ARTIFACT, 5)));
});

test('reports a listener that throws once to console.error, with its error', () => {
  const hub = new DataChangeHub();
  const failure = new Error('listener failed');
  hub.subscribe(ARTIFACT, () => {
    throw failure;
  });
  const reported = captureConsoleError();
  hub.follow(dataChanged(ARTIFACT, 5));
  assert.equal(reported.length, 1);
  assert.equal(reported[0][0], 'A data change listener failed.');
  assert.equal(reported[0][1], failure);
});
