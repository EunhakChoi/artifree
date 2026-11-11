import assert from 'node:assert/strict';
import { afterEach, mock, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The change hub is web code whose only imports are types; bundle it as it
// ships and import only the hub.
const hubBundle = await build({
  entryPoints: [
    fileURLToPath(
      new URL(
        '../../apps/web/src/artifacts/artifact-changes.ts',
        import.meta.url,
      ),
    ),
  ],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const { ArtifactChangeHub } = await import(
  `data:text/javascript;base64,${Buffer.from(hubBundle.outputFiles[0].text).toString('base64')}`
);

// The reducer shares its module with React hooks and the client. Bundle the
// module as it ships and import only the pure reducer and its start state.
const reducerBundle = await build({
  entryPoints: [
    fileURLToPath(
      new URL(
        '../../apps/web/src/artifacts/use-live-workspace.ts',
        import.meta.url,
      ),
    ),
  ],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const { START, learnFrom } = await import(
  `data:text/javascript;base64,${Buffer.from(reducerBundle.outputFiles[0].text).toString('base64')}`
);

const ARTIFACT = `art_${'a'.repeat(32)}`;
const OTHER = `art_${'b'.repeat(32)}`;
const JOB = `job_${'c'.repeat(32)}`;
const CURSOR = `${'f'.repeat(32)}.1`;

/**
 * Every change kind of the contract, then `unknown` (what a client gives a kind
 * it does not know). Each row carries the one field its kind adds, if any.
 */
const CHANGES = [
  ['created', {}],
  ['revised', {}],
  ['metadata_updated', {}],
  ['deleted', {}],
  ['recovered', {}],
  ['purged', {}],
  ['data_changed', { data_version: 7 }],
  ['job_changed', { job_id: JOB }],
  ['unknown', {}],
];

/** The events that are not changes. The hub must ignore both. */
const NON_CHANGES = [
  ['ready', { type: 'ready', cursor: CURSOR }],
  ['reset', { type: 'reset', cursor: CURSOR }],
];

/** A committed change of `artifactId` as the change feed delivers it. */
function changeEvent(artifactId, kind, extra = {}) {
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

/** Subscribes a listener that records each change it hears, in order. */
function recorder(hub, artifactId) {
  const heard = [];
  const remove = hub.subscribe(artifactId, (change) => heard.push(change));
  return { heard, remove };
}

afterEach(() => {
  mock.restoreAll();
});

for (const [kind, extra] of CHANGES) {
  test(`delivers a change of kind '${kind}' to the listeners of its artifact only, with every field the feed sent`, () => {
    const hub = new ArtifactChangeHub();
    const own = recorder(hub, ARTIFACT);
    const other = recorder(hub, OTHER);
    const event = changeEvent(ARTIFACT, kind, extra);
    hub.follow(event);
    // For a job change this also checks that the job it names is passed on.
    assert.deepStrictEqual(own.heard, [event.change]);
    assert.deepStrictEqual(other.heard, []);
  });
}

for (const [type, event] of NON_CHANGES) {
  test(`ignores a '${type}' event: no listener of any artifact hears it, and follow does not throw`, () => {
    const hub = new ArtifactChangeHub();
    const own = recorder(hub, ARTIFACT);
    const other = recorder(hub, OTHER);
    assert.doesNotThrow(() => hub.follow(event));
    assert.deepStrictEqual(own.heard, []);
    assert.deepStrictEqual(other.heard, []);
  });
}

test('a change of an artifact nobody listens to reaches no listener and does not throw', () => {
  const hub = new ArtifactChangeHub();
  const listener = recorder(hub, ARTIFACT);
  assert.doesNotThrow(() =>
    hub.follow(changeEvent(OTHER, 'job_changed', { job_id: JOB })),
  );
  assert.deepStrictEqual(listener.heard, []);
});

test('the remover stops delivery of later changes to its listener', () => {
  const hub = new ArtifactChangeHub();
  const listener = recorder(hub, ARTIFACT);
  const first = changeEvent(ARTIFACT, 'revised');
  hub.follow(first);
  listener.remove();
  hub.follow(changeEvent(ARTIFACT, 'recovered'));
  assert.deepStrictEqual(listener.heard, [first.change]);
});

test('calling the remover again changes nothing and drops no other listener of the artifact', () => {
  const hub = new ArtifactChangeHub();
  const removed = recorder(hub, ARTIFACT);
  const kept = recorder(hub, ARTIFACT);
  removed.remove();
  assert.doesNotThrow(() => removed.remove());
  const event = changeEvent(ARTIFACT, 'recovered');
  hub.follow(event);
  assert.deepStrictEqual(removed.heard, []);
  assert.deepStrictEqual(kept.heard, [event.change]);
});

test('a listener that throws does not stop the next listener of the artifact', () => {
  const hub = new ArtifactChangeHub();
  mock.method(console, 'error', () => {});
  hub.subscribe(ARTIFACT, () => {
    throw new Error('listener failed');
  });
  const later = recorder(hub, ARTIFACT);
  const event = changeEvent(ARTIFACT, 'job_changed', { job_id: JOB });
  hub.follow(event);
  assert.deepStrictEqual(later.heard, [event.change]);
});

test('reports a listener that throws once to console.error, with its error', () => {
  const hub = new ArtifactChangeHub();
  const failure = new Error('listener failed');
  hub.subscribe(ARTIFACT, () => {
    throw failure;
  });
  const reported = mock.method(console, 'error', () => {});
  hub.follow(changeEvent(ARTIFACT, 'revised'));
  assert.equal(reported.mock.callCount(), 1);
  const [message, error] = reported.mock.calls[0].arguments;
  assert.equal(message, 'An artifact change listener failed.');
  assert.equal(error, failure);
});

/**
 * A state that knows `ARTIFACT` at revision 2 and metadata revision 1: what the
 * reducer makes of the first revision the page sees of the artifact.
 */
const KNOWS_ARTIFACT = learnFrom(START, {
  type: 'event',
  event: changeEvent(ARTIFACT, 'revised'),
});
const KNOWN_VERSION = { revision: 2, metadata: 1 };

/**
 * The state after a job change of `ARTIFACT` whose fields are `versions`
 * (overriding the defaults of a change), applied to `KNOWS_ARTIFACT`. It checks
 * first that `KNOWS_ARTIFACT` does know the artifact at `KNOWN_VERSION`, so a
 * broken setup cannot pass the identity checks below by accident.
 */
function afterJobChange(versions) {
  assert.deepStrictEqual(KNOWS_ARTIFACT.known, { [ARTIFACT]: KNOWN_VERSION });
  return learnFrom(KNOWS_ARTIFACT, {
    type: 'event',
    event: changeEvent(ARTIFACT, 'job_changed', { job_id: JOB, ...versions }),
  });
}

// Decision 0055, rule 11: the web's reducer leaves its artifact state as it is
// for a job_changed change, as for data_changed. The rows that name versions
// ahead of the page's show that a job change adds no news even then.
const JOB_CHANGE_VERSIONS = [
  ['at the versions the page knows', {}],
  ['with a newer revision', { revision: 3 }],
  ['with a newer metadata revision', { metadata_revision: 2 }],
];

for (const [name, versions] of JOB_CHANGE_VERSIONS) {
  test(`a job change of an artifact the page knows, ${name}, returns the same state object`, () => {
    assert.strictEqual(afterJobChange(versions), KNOWS_ARTIFACT);
  });
}

test('the reducer still counts a newer revision of a known artifact as news', () => {
  // The control: a change the reducer tracks returns a new state, so the
  // identity checks above do not pass merely because the reducer returns its
  // input.
  const next = learnFrom(KNOWS_ARTIFACT, {
    type: 'event',
    event: changeEvent(ARTIFACT, 'revised', { revision: 3 }),
  });
  assert.notStrictEqual(next, KNOWS_ARTIFACT);
  assert.equal(next.news, 2);
  assert.deepStrictEqual(next.known, {
    [ARTIFACT]: { revision: 3, metadata: 1 },
  });
});

test('counts each reset the hub follows, and neither a ready nor a change', () => {
  // Decision 0055, rule 15: a reset may replace the server's data, and a follower
  // tells it from a ready, which keeps the data, by this count.
  const hub = new ArtifactChangeHub();
  assert.equal(hub.resetCount(), 0);
  hub.follow({ type: 'ready', cursor: CURSOR });
  hub.follow(changeEvent(ARTIFACT, 'revised'));
  assert.equal(hub.resetCount(), 0);
  hub.follow({ type: 'reset', cursor: CURSOR });
  hub.follow({ type: 'reset', cursor: CURSOR });
  assert.equal(hub.resetCount(), 2);
});
