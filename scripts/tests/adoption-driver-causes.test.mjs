import assert from 'node:assert/strict';
import { test } from 'node:test';
import { setImmediate as nextTurn } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The adoption driver of a live frame decides when the frame adopts a new
// generation (decision 0054, rule 8) and when it restarts instead (rule 11).
// These cases drive it bundled as it ships, with an injected clock and timers,
// a fake frame and fake grants. The clock moves only when a case advances it.
// A save or a choice that a case makes before the next turn of the event loop
// is seen by the same evaluation; a pause or an adoption answers only when the
// case says so.

/**
 * A module of the web app, bundled as it ships and loaded. The driver imports
 * types only, so the bundle holds the driver alone.
 */
async function loadWebModule(relativePath) {
  const bundle = await build({
    entryPoints: [fileURLToPath(new URL(relativePath, import.meta.url))],
    bundle: true,
    format: 'esm',
    platform: 'neutral',
    write: false,
  });
  return import(
    `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
  );
}

const { IDLE_POLL_MS, createAdoptionDriver } = await loadWebModule(
  '../../apps/web/src/artifacts/adoption-driver.ts',
);

const ARTIFACT = `art_${'0'.repeat(32)}`;
/** The length of a lease, on the clock the driver reads (rule 8). */
const LEASE_MS = 900_000;
/** The end of the lease that an answered adoption grants. */
const RENEWED_DEADLINE = 2 * LEASE_MS;

/** A promise with the functions that settle it. */
function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

/** A view of the artifact at `revision`; its head defaults to that revision. */
function viewOf({ revision, headRevision = revision, generation = 1 }) {
  return {
    artifact_id: ARTIFACT,
    revision,
    head_revision: headRevision,
    generation,
    lease_ms: LEASE_MS,
  };
}

/**
 * A clock that moves only when advanced. Timers fire in due order, and each
 * callback is followed by a turn of the event loop, so the promise chain it
 * starts runs before the next timer.
 */
function createClock() {
  let time = 0;
  const timers = new Set();
  /** The timer due first by `target`; a tie keeps the order the timers were set. */
  function nextDue(target) {
    let next;
    for (const timer of timers) {
      if (timer.due <= target && (next === undefined || timer.due < next.due)) {
        next = timer;
      }
    }
    return next;
  }
  return {
    now: () => time,
    schedule(callback, delayMs) {
      const timer = { due: time + delayMs, callback };
      timers.add(timer);
      return () => {
        timers.delete(timer);
      };
    },
    async advance(ms) {
      const target = time + ms;
      let timer = nextDue(target);
      while (timer !== undefined) {
        timers.delete(timer);
        time = timer.due;
        timer.callback();
        await nextTurn();
        timer = nextDue(target);
      }
      time = target;
      await nextTurn();
    },
  };
}

/**
 * The connected frame as the driver calls it. A pause resolves at once unless
 * `state.holdPauses` is set. An adoption resolves only when its `answer` is
 * called, and it rejects when its signal aborts.
 */
function createFrame(start) {
  const state = { view: start, sourceWriting: false, holdPauses: false };
  const pauses = [];
  const adoptions = [];
  /** The calls the driver made to answer or end the frame: resume, refuse, fail. */
  const calls = [];
  const frame = {
    view: () => state.view,
    pause(reason) {
      const held = deferred();
      pauses.push({ reason, release: held.resolve });
      if (!state.holdPauses) {
        held.resolve();
      }
      return held.promise;
    },
    resume: () => {
      calls.push('resume');
    },
    refuse: () => {
      calls.push('refuse');
    },
    fail: () => {
      calls.push('fail');
    },
    adopt({ reason, revision, generation, signal }) {
      const answer = deferred();
      signal.addEventListener(
        'abort',
        () => {
          answer.reject(new Error('The adoption was aborted.'));
        },
        { once: true },
      );
      adoptions.push({
        reason,
        revision,
        generation,
        signal,
        answer(next, deadline) {
          state.view = next;
          answer.resolve({ view: next, deadline });
        },
      });
      return answer.promise;
    },
    // No call is on its way in these cases, so no lease waits for one.
    onItsWay: () => 0,
    sourceWriting: () => state.sourceWriting,
  };
  return { frame, state, pauses, adoptions, calls };
}

/** The grants, settled and at one version, so no grant change is pending. */
function createGrants() {
  const changes = [];
  return {
    grants: {
      subscribe: () => () => {},
      status: () => 'settled',
      revision: () => 1,
      set: (next, capabilities) => {
        changes.push({ view: next, capabilities });
        return Promise.resolve();
      },
    },
    changes,
  };
}

/**
 * Creates the driver of a frame at `start` with the choice `choice`, then lets
 * its first evaluation run, as the workspace does when it creates the driver.
 */
async function startDriver(start, choice) {
  const clock = createClock();
  const connected = createFrame(start);
  const grants = createGrants();
  const log = { closed: [], changes: [], restarts: 0 };
  let nextGeneration = start.generation + 1;
  const driver = createAdoptionDriver({
    frame: connected.frame,
    grants: grants.grants,
    deadline: LEASE_MS,
    choice,
    now: clock.now,
    schedule: clock.schedule,
    // The page stays visible in these cases, so a lapsed lease restarts at once.
    visibility: { visible: () => true, subscribe: () => () => {} },
    nextGeneration: () => {
      const generation = nextGeneration;
      nextGeneration += 1;
      return generation;
    },
    closeGeneration: (generation) => {
      log.closed.push(generation);
    },
    onChange: (state) => {
      log.changes.push(state);
    },
    onRestart: () => {
      log.restarts += 1;
    },
  });
  await nextTurn();
  return { driver, clock, connected, grants, log };
}

/** The adoptions the frame has been asked for, without their answers. */
function adoptionsOf(run) {
  return run.connected.adoptions.map(({ reason, revision, generation }) => ({
    reason,
    revision,
    generation,
  }));
}

/**
 * Starts a frame at revision 1 and has the workspace learn head 2, which asks
 * for a head adoption at revision 1 (rule 8). The choice is the followed
 * revision or a chosen one, as `following` says.
 */
async function startHeadAdoption(following) {
  const run = await startDriver(viewOf({ revision: 1 }), {
    following,
    revision: 1,
    latest: 1,
  });
  run.driver.choose({ following, revision: 1, latest: 2 });
  await nextTurn();
  assert.deepEqual(adoptionsOf(run), [
    { reason: 'head', revision: 1, generation: 2 },
  ]);
  return run;
}

/**
 * Starts a frame whose source write is on its way, and has the workspace learn
 * head 2. The head waits for the write, so no adoption starts while it runs.
 */
async function startWaitingForWrite() {
  const run = await startDriver(viewOf({ revision: 1 }), {
    following: true,
    revision: 1,
    latest: 1,
  });
  run.connected.state.sourceWriting = true;
  run.driver.choose({ following: true, revision: 2, latest: 2 });
  await nextTurn();
  await run.clock.advance(IDLE_POLL_MS * 4);
  assert.deepEqual(adoptionsOf(run), []);
  return run;
}

/** A followed revision and a chosen one: the head cause applies to both (rule 8). */
const HEAD_KINDS = [
  { name: 'a followed revision', following: true },
  { name: 'a chosen revision', following: false },
];

/** Changes of the choice that restart the frame (rule 11). */
const RESTARTING_CHANGES = [
  {
    name: 'following to a chosen revision',
    from: { following: true, revision: 1, latest: 1 },
    to: { following: false, revision: 1, latest: 1 },
  },
  {
    name: 'one chosen revision to another',
    from: { following: false, revision: 1, latest: 1 },
    to: { following: false, revision: 2, latest: 2 },
  },
  {
    name: 'a chosen revision to following',
    from: { following: false, revision: 1, latest: 1 },
    to: { following: true, revision: 1, latest: 1 },
  },
];

test('a save pins the revision it saved, not the head the workspace knows', async () => {
  const run = await startDriver(viewOf({ revision: 1 }), {
    following: true,
    revision: 1,
    latest: 1,
  });
  // Both arrive before the driver evaluates either, so the save is the cause
  // that applies, and it pins its own revision rather than the head.
  run.driver.choose({ following: true, revision: 3, latest: 3 });
  run.driver.saved(2);
  await nextTurn();
  assert.deepEqual(adoptionsOf(run), [
    { reason: 'saved', revision: 2, generation: 2 },
  ]);
  run.connected.adoptions[0].answer(
    viewOf({ revision: 2, headRevision: 3, generation: 2 }),
    RENEWED_DEADLINE,
  );
  await nextTurn();
  assert.equal(run.connected.adoptions.length, 1);
});

for (const { name, following } of HEAD_KINDS) {
  test(`another writer's head is adopted at the pinned revision for ${name}, with no grant change`, async () => {
    const run = await startHeadAdoption(following);
    assert.deepEqual(run.grants.changes, []);
    run.connected.adoptions[0].answer(
      viewOf({ revision: 1, headRevision: 2, generation: 2 }),
      RENEWED_DEADLINE,
    );
    await nextTurn();
    assert.deepEqual(
      run.log.changes.map(({ view }) => [view.revision, view.head_revision]),
      [[1, 2]],
    );
    assert.equal(run.log.restarts, 0);
  });
}

test('a head seen during a source write is covered by the save that ends the write', async () => {
  const run = await startWaitingForWrite();
  run.connected.state.sourceWriting = false;
  run.driver.saved(2);
  await nextTurn();
  assert.deepEqual(adoptionsOf(run), [
    { reason: 'saved', revision: 2, generation: 2 },
  ]);
  run.connected.adoptions[0].answer(
    viewOf({ revision: 2, headRevision: 2, generation: 2 }),
    RENEWED_DEADLINE,
  );
  await nextTurn();
  assert.equal(run.connected.adoptions.length, 1);
});

test('a head seen during a source write is adopted once the write ends without a save', async () => {
  const run = await startWaitingForWrite();
  run.connected.state.sourceWriting = false;
  await run.clock.advance(IDLE_POLL_MS);
  assert.deepEqual(adoptionsOf(run), [
    { reason: 'head', revision: 1, generation: 2 },
  ]);
});

test('a save that commits while a lease adoption pauses the frame is adopted as the saved revision', async () => {
  const run = await startDriver(viewOf({ revision: 1 }), {
    following: true,
    revision: 1,
    latest: 1,
  });
  run.connected.state.holdPauses = true;
  // A fifth of the lease remains, so the lease falls due and pauses the frame.
  await run.clock.advance(LEASE_MS - LEASE_MS / 5);
  assert.deepEqual(
    run.connected.pauses.map(({ reason }) => reason),
    ['lease'],
  );
  run.driver.saved(2);
  run.connected.pauses[0].release();
  await nextTurn();
  assert.deepEqual(adoptionsOf(run), [
    { reason: 'saved', revision: 2, generation: 2 },
  ]);
  run.connected.adoptions[0].answer(
    viewOf({ revision: 2, headRevision: 2, generation: 2 }),
    RENEWED_DEADLINE,
  );
  await nextTurn();
  assert.equal(run.connected.adoptions.length, 1);
});

test('one adoption runs at a time, and a head that arrives meanwhile follows it', async () => {
  const run = await startDriver(viewOf({ revision: 1 }), {
    following: true,
    revision: 1,
    latest: 1,
  });
  run.driver.saved(2);
  await nextTurn();
  assert.deepEqual(adoptionsOf(run), [
    { reason: 'saved', revision: 2, generation: 2 },
  ]);
  run.driver.choose({ following: true, revision: 3, latest: 3 });
  await nextTurn();
  assert.equal(
    run.connected.adoptions.length,
    1,
    'no second adoption starts while the first is pending',
  );
  run.connected.adoptions[0].answer(
    viewOf({ revision: 2, headRevision: 2, generation: 2 }),
    RENEWED_DEADLINE,
  );
  await nextTurn();
  assert.deepEqual(adoptionsOf(run), [
    { reason: 'saved', revision: 2, generation: 2 },
    { reason: 'head', revision: 2, generation: 3 },
  ]);
  run.connected.adoptions[1].answer(
    viewOf({ revision: 2, headRevision: 3, generation: 3 }),
    RENEWED_DEADLINE,
  );
  await nextTurn();
  assert.equal(run.connected.adoptions.length, 2);
});

for (const { name, from, to } of RESTARTING_CHANGES) {
  test(`a change from ${name} restarts the frame and adopts nothing`, async () => {
    const run = await startDriver(viewOf({ revision: 1 }), from);
    run.driver.choose(to);
    await nextTurn();
    assert.equal(run.log.restarts, 1);
    assert.deepEqual(adoptionsOf(run), []);
  });
}

test('a newer head of the followed artifact is adopted without a restart', async () => {
  const run = await startDriver(viewOf({ revision: 2 }), {
    following: true,
    revision: 2,
    latest: 2,
  });
  run.driver.choose({ following: true, revision: 3, latest: 3 });
  await nextTurn();
  assert.equal(run.log.restarts, 0);
  assert.deepEqual(adoptionsOf(run), [
    { reason: 'head', revision: 2, generation: 2 },
  ]);
});

test('disposing during an adoption aborts it and closes the generation it requested, reporting nothing', async () => {
  const run = await startHeadAdoption(true);
  const [request] = run.connected.adoptions;
  run.driver.dispose();
  assert.equal(request.signal.aborted, true);
  await nextTurn();
  assert.deepEqual(run.log.closed, [2]);
  assert.deepEqual(run.log.changes, []);
  assert.equal(run.log.restarts, 0);
  assert.deepEqual(run.connected.calls, []);
});

test('disposing after an adoption closes the adopted generation and never the first one', async () => {
  const run = await startHeadAdoption(true);
  run.connected.adoptions[0].answer(
    viewOf({ revision: 1, headRevision: 2, generation: 2 }),
    RENEWED_DEADLINE,
  );
  await nextTurn();
  assert.equal(run.log.changes.length, 1);
  run.driver.dispose();
  await nextTurn();
  assert.deepEqual(run.log.closed, [2]);
});
