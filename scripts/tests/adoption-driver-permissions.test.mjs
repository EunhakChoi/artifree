import assert from 'node:assert/strict';
import { afterEach, describe, mock, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The adoption driver of one live frame, bundled from its source (decision 0054,
// rules 8 and 9). The frame, the grants and the timers are fakes that record
// what the driver asks of them. The clock moves in steps and the promise
// callbacks settle after each step, so the driver sees every moment a timer
// falls on before a test looks at the outcome.
const bundle = await build({
  entryPoints: [
    fileURLToPath(
      new URL(
        '../../apps/web/src/artifacts/adoption-driver.ts',
        import.meta.url,
      ),
    ),
  ],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const { createAdoptionDriver } = await import(
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
);

/** The lease a view is issued with, and the deadline of the first one. */
const LEASE_MS = 900_000;
const DEADLINE = 900_000;
/** A fifth of the lease before the deadline: when a lease adoption falls due (rule 8). */
const LEASE_DUE = 720_000;
/** The deadline of a lease adopted at LEASE_DUE, and the moment that lease falls due. */
const RENEWED_DEADLINE = 1_620_000;
const RENEWED_DUE = 1_440_000;
/** The last 10 seconds of a lease, from which a lease adoption proceeds regardless (rule 8). */
const LAST_MOMENT = 890_000;
/** The 10 seconds an adoption has, within which a 503 may be repeated (rule 8). */
const BOUND_MS = 10_000;
/** The wait before a 503 is repeated. */
const REPEAT_MS = 500;
/** The wait before a deferred lease adoption looks again. */
const POLL_MS = 250;
/** The steps the clock moves in: every timer of these cases falls on a step. */
const STEP_MS = 250;
const ARTIFACT = 'art_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa';
const REVISION = 1;
/** The revision the frame's own save makes, after REVISION. */
const SAVED_REVISION = 2;
const CAPABILITIES = ['assets.read'];

afterEach(() => {
  mock.timers.reset();
});

/** Resolves once every callback that promises have queued has run. */
function settle() {
  return new Promise((resolve) => {
    setImmediate(resolve);
  });
}

/** A promise with the function that settles it. */
function deferred() {
  let resolve;
  const promise = new Promise((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

/**
 * A clock that starts at zero, whose timers are node's mocked setTimeout. The
 * timers fire as the test moves the clock past them. `advance` moves the clock
 * and the timers together, `jumpTo` moves the clock alone, as a jump of the
 * system clock does, and `runTimers` fires the timers without moving the clock.
 */
function startClock() {
  let time = 0;
  mock.timers.enable({ apis: ['setTimeout'] });
  async function run(ms, moveClock) {
    let remaining = ms;
    while (remaining > 0) {
      const step = Math.min(STEP_MS, remaining);
      if (moveClock) {
        time += step;
      }
      mock.timers.tick(step);
      await settle();
      remaining -= step;
    }
  }
  return {
    now: () => time,
    schedule(callback, delayMs) {
      let cancelled = false;
      const timer = setTimeout(() => {
        if (!cancelled) {
          callback();
        }
      }, delayMs);
      return () => {
        cancelled = true;
        clearTimeout(timer);
      };
    },
    advance: (ms) => run(ms, true),
    runTimers: (ms) => run(ms, false),
    jumpTo(at) {
      time = at;
    },
  };
}

/** An error shaped as the client's ArtifreeApiError, which the driver recognizes by name and status. */
function apiError(status) {
  return Object.assign(new Error(`The server answered ${status}.`), {
    name: 'ArtifreeApiError',
    status,
  });
}

/** The server is busy: the request is refused with 503, which the driver repeats (rule 8). */
function unavailable() {
  return Promise.reject(apiError(503));
}

/** The view of the artifact at `revision`, in `generation`, with the lease a view is issued with. */
function viewOf(generation, revision = REVISION) {
  return {
    artifact_id: ARTIFACT,
    revision,
    generation,
    head_revision: REVISION,
    lease_ms: LEASE_MS,
  };
}

/** The adoption requests as the tests compare them: what was asked, and when. */
function attemptsOf(requests) {
  return requests.map(({ reason, revision, generation, at }) => ({
    reason,
    revision,
    generation,
    at,
  }));
}

/**
 * The connected frame as the driver uses it. The frame's pause accepts a repeat
 * while it is in effect, and the driver makes one when it adopts after a change,
 * so the log records a pause only when it begins. Each adoption request is
 * recorded with the moment it was made and answered by the next queued outcome,
 * or by `fallback`.
 */
function createFrame(clock, log) {
  const succeed = (request) =>
    Promise.resolve({
      view: viewOf(request.generation, request.revision),
      deadline: clock.now() + LEASE_MS,
    });
  const state = {
    current: viewOf(0),
    mode: 'open',
    callsOnTheirWay: 0,
    sourceWriting: false,
    writeOnItsWay: undefined,
    answers: [],
    fallback: succeed,
    requests: [],
  };
  const driver = {
    view: () => state.current,
    pause: async (reason) => {
      if (state.mode !== 'paused') {
        state.mode = 'paused';
        log.push(`pause:${reason}`);
      }
      // A write on its way holds the pause until the write has ended (rule 9).
      if (state.writeOnItsWay !== undefined) {
        await state.writeOnItsWay;
      }
    },
    resume: () => {
      state.mode = 'open';
      log.push('resume');
    },
    refuse: () => {
      state.mode = 'refusing';
      log.push('refuse');
    },
    adopt: (request) => {
      log.push(`adopt:${request.reason}`);
      state.requests.push({
        reason: request.reason,
        revision: request.revision,
        generation: request.generation,
        at: clock.now(),
        signal: request.signal,
      });
      const answer = state.answers.shift() ?? state.fallback;
      return answer(request).then((adopted) => {
        state.current = adopted.view;
        state.mode = 'open';
        return adopted;
      });
    },
    fail: () => {
      log.push('fail');
    },
    onItsWay: () => state.callsOnTheirWay,
    sourceWriting: () => state.sourceWriting,
  };
  return { driver, state };
}

/**
 * The grants the frame's calls are checked against. A change is recorded in the
 * log as `set`. By default it is made: the grant's version moves and its
 * listeners are told before the change answers, so the driver must tolerate a
 * notice that arrives during its own change.
 */
function createGrants(log) {
  const versions = new Map();
  const listeners = new Set();
  const key = (artifactId, revision) => `${artifactId}/${revision}`;
  const notify = () => {
    for (const listener of listeners) {
      listener();
    }
  };
  const change = (revision) => {
    const next = (versions.get(key(ARTIFACT, revision)) ?? 0) + 1;
    versions.set(key(ARTIFACT, revision), next);
    notify();
  };
  const control = {
    status: 'settled',
    sets: [],
    answerSet: (view) => {
      change(view.revision);
      return Promise.resolve();
    },
    change,
    notify,
  };
  const driver = {
    subscribe: (listener) => {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    status: () => control.status,
    revision: (artifactId, revision) =>
      versions.get(key(artifactId, revision)) ?? 0,
    set: (view, capabilities) => {
      log.push('set');
      control.sets.push({
        artifactId: view.artifact_id,
        revision: view.revision,
        capabilities,
      });
      return control.answerSet(view, capabilities);
    },
  };
  return { driver, control };
}

/**
 * The page's visibility as the driver reads it. The page is visible until a
 * case hides it. `hide` and `show` change it and tell the listeners, as a change
 * of the tab does.
 */
function createVisibility() {
  const state = { visible: true };
  const listeners = new Set();
  const change = (visible) => {
    state.visible = visible;
    for (const listener of listeners) {
      listener();
    }
  };
  const driver = {
    visible: () => state.visible,
    subscribe: (listener) => {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
  };
  return {
    driver,
    control: {
      listeners,
      hide: () => change(false),
      show: () => change(true),
    },
  };
}

/**
 * A driver over the fakes, on a frame that follows the latest revision. Its
 * first evaluation has run when this resolves.
 */
async function setUp() {
  const clock = startClock();
  const log = [];
  const changes = [];
  const restarts = [];
  const frame = createFrame(clock, log);
  const grants = createGrants(log);
  const visibility = createVisibility();
  let generations = 0;
  const driver = createAdoptionDriver({
    frame: frame.driver,
    grants: grants.driver,
    deadline: DEADLINE,
    choice: { following: true, revision: REVISION, latest: REVISION },
    now: clock.now,
    schedule: clock.schedule,
    visibility: visibility.driver,
    nextGeneration: () => {
      generations += 1;
      return generations;
    },
    closeGeneration: (generation) => {
      log.push(`close:${generation}`);
    },
    onChange: (state) => {
      changes.push(state);
    },
    onRestart: () => {
      restarts.push(clock.now());
      log.push('restart');
    },
  });
  await settle();
  return {
    clock,
    driver,
    frame: frame.state,
    grants: grants.control,
    visibility: visibility.control,
    log,
    changes,
    restarts,
  };
}

describe('a permission change', () => {
  test('pauses the frame before the grant is sent and adopts after it', async () => {
    const world = await setUp();
    // A later moment, so that the renewed deadline differs from the first one.
    await world.clock.advance(1_000);

    await world.driver.changePermissions(CAPABILITIES, world.frame.current);

    assert.deepEqual(world.log, [
      'pause:permissions',
      'set',
      'adopt:permissions',
    ]);
    assert.deepEqual(world.grants.sets, [
      { artifactId: ARTIFACT, revision: REVISION, capabilities: CAPABILITIES },
    ]);
    assert.deepEqual(attemptsOf(world.frame.requests), [
      { reason: 'permissions', revision: REVISION, generation: 1, at: 1_000 },
    ]);
    assert.deepEqual(world.changes, [
      {
        view: viewOf(1),
        deadline: 1_000 + LEASE_MS,
        documentRevision: REVISION,
      },
    ]);
  });

  test('a refused change with the grant as it was resumes the frame and adopts nothing', async () => {
    const world = await setUp();
    const refusal = apiError(409);
    world.grants.answerSet = () => Promise.reject(refusal);

    await assert.rejects(
      world.driver.changePermissions(CAPABILITIES, world.frame.current),
      (error) => error === refusal,
    );

    assert.deepEqual(world.log, ['pause:permissions', 'set', 'resume']);
    assert.deepEqual(world.changes, []);
    assert.deepEqual(world.frame.requests, []);
  });

  test('a refused change does not resume the frame over a save that committed meanwhile', async () => {
    const world = await setUp();
    const refusal = apiError(409);
    world.grants.answerSet = () => Promise.reject(refusal);
    // The frame's write is on its way when the change pauses, so the pause waits for it.
    const write = deferred();
    world.frame.writeOnItsWay = write.promise;

    const change = world.driver.changePermissions(
      CAPABILITIES,
      world.frame.current,
    );
    await settle();
    // The write commits during that wait, so its saved adoption is queued behind the change.
    world.driver.saved(SAVED_REVISION);
    write.resolve();

    await assert.rejects(change, (error) => error === refusal);
    await settle();
    // The calls stay held until the saved adoption releases them under its own generation (rule 9).
    assert.deepEqual(world.log, ['pause:permissions', 'set', 'adopt:saved']);
    assert.deepEqual(attemptsOf(world.frame.requests), [
      { reason: 'saved', revision: SAVED_REVISION, generation: 1, at: 0 },
    ]);
  });

  test('an unknown outcome refuses the calls, and the frame adopts once the grant settles', async () => {
    const world = await setUp();
    const failure = new TypeError('The network request failed.');
    world.grants.answerSet = () => {
      world.grants.status = 'unknown';
      return Promise.reject(failure);
    };

    await assert.rejects(
      world.driver.changePermissions(CAPABILITIES, world.frame.current),
      (error) => error === failure,
    );
    assert.deepEqual(world.log, ['pause:permissions', 'set', 'refuse']);

    // The grant is still unknown, so a notice of it adopts nothing.
    world.grants.notify();
    await settle();
    assert.deepEqual(world.log, ['pause:permissions', 'set', 'refuse']);

    world.grants.status = 'settled';
    world.grants.notify();
    await settle();
    assert.deepEqual(world.log, [
      'pause:permissions',
      'set',
      'refuse',
      'pause:permissions',
      'adopt:permissions',
    ]);
    assert.deepEqual(attemptsOf(world.frame.requests), [
      { reason: 'permissions', revision: REVISION, generation: 1, at: 0 },
    ]);
  });

  const otherPreview = [
    {
      name: 'a grant change by another preview adopts once the grant is settled',
      status: 'settled',
      expectedLog: ['pause:permissions', 'adopt:permissions'],
      expectedRequests: [
        { reason: 'permissions', revision: REVISION, generation: 1, at: 0 },
      ],
    },
    {
      name: 'a grant change by another preview refuses the calls, and adopts nothing, while the grant is pending',
      status: 'pending',
      expectedLog: ['refuse'],
      expectedRequests: [],
    },
  ];
  for (const { name, status, expectedLog, expectedRequests } of otherPreview) {
    test(name, async () => {
      const world = await setUp();
      world.grants.status = status;
      world.grants.change(REVISION);
      await settle();
      assert.deepEqual(world.log, expectedLog);
      assert.deepEqual(attemptsOf(world.frame.requests), expectedRequests);
    });
  }
});

describe('a lease', () => {
  test('falls due a fifth of the lease before its deadline, and adopts the revision the frame shows', async () => {
    const world = await setUp();

    await world.clock.advance(LEASE_DUE - 1);
    assert.deepEqual(world.frame.requests, []);

    await world.clock.advance(1);
    assert.deepEqual(attemptsOf(world.frame.requests), [
      { reason: 'lease', revision: REVISION, generation: 1, at: LEASE_DUE },
    ]);
    assert.deepEqual(world.changes, [
      {
        view: viewOf(1),
        deadline: RENEWED_DEADLINE,
        documentRevision: REVISION,
      },
    ]);

    // The renewed lease falls due in the same way, a fifth before its own deadline.
    await world.clock.advance(RENEWED_DUE - LEASE_DUE - 1);
    assert.equal(world.frame.requests.length, 1);

    await world.clock.advance(1);
    assert.deepEqual(attemptsOf(world.frame.requests)[1], {
      reason: 'lease',
      revision: REVISION,
      generation: 2,
      at: RENEWED_DUE,
    });
  });

  test('waits for a call on its way, then adopts within a poll of the call answering', async () => {
    const world = await setUp();
    world.frame.callsOnTheirWay = 1;

    await world.clock.advance(LEASE_DUE + 1_000);
    assert.deepEqual(world.frame.requests, []);

    world.frame.callsOnTheirWay = 0;
    await world.clock.advance(POLL_MS);
    assert.deepEqual(attemptsOf(world.frame.requests), [
      { reason: 'lease', revision: REVISION, generation: 1, at: 721_250 },
    ]);
  });

  test('in its last 10 seconds, adopts even while a call is still on its way', async () => {
    const world = await setUp();
    world.frame.callsOnTheirWay = 1;

    await world.clock.advance(LAST_MOMENT - POLL_MS);
    assert.deepEqual(world.frame.requests, []);

    await world.clock.advance(POLL_MS);
    assert.deepEqual(attemptsOf(world.frame.requests), [
      { reason: 'lease', revision: REVISION, generation: 1, at: LAST_MOMENT },
    ]);
  });

  test('restarts the frame without adopting when the lease ended before its timer ran', async () => {
    const world = await setUp();
    // The clock jumps past the deadline, as it does after a sleep, before the
    // lease timer, which was set for a fifth of the lease before it, runs.
    world.clock.jumpTo(DEADLINE + 1);

    await world.clock.runTimers(LEASE_DUE);

    assert.deepEqual(world.frame.requests, []);
    assert.deepEqual(world.log, ['fail', 'restart']);
    assert.deepEqual(world.restarts, [DEADLINE + 1]);
  });
});

describe('a lease that ends while the page is hidden', () => {
  test('restarts the frame when the page is next shown, not while it is hidden', async () => {
    const world = await setUp();
    world.visibility.hide();
    // The clock jumps past the deadline while the page is hidden, and the lease
    // timer, set for a fifth of the lease before it, runs late.
    world.clock.jumpTo(DEADLINE + 1);
    await world.clock.runTimers(LEASE_DUE);
    await world.clock.advance(1_000);

    assert.deepEqual(world.frame.requests, []);
    assert.deepEqual(world.restarts, []);

    world.visibility.show();
    await settle();

    assert.deepEqual(world.frame.requests, []);
    assert.deepEqual(world.restarts, [DEADLINE + 1 + 1_000]);
    assert.equal(world.log.at(-1), 'restart');
    assert.equal(
      world.visibility.listeners.size,
      0,
      'the restarted driver stops listening to the page',
    );

    world.visibility.hide();
    world.visibility.show();
    await settle();
    assert.equal(world.restarts.length, 1, 'the frame restarts once');
  });

  test('adopts nothing for another cause once its lease has ended while the page is hidden', async () => {
    const world = await setUp();
    world.visibility.hide();
    world.clock.jumpTo(DEADLINE + 1);
    await world.clock.runTimers(LEASE_DUE);

    // Another writer's head arrives while the lapsed frame waits to be shown.
    world.driver.choose({
      following: true,
      revision: REVISION,
      latest: REVISION + 1,
    });
    await settle();

    assert.deepEqual(world.frame.requests, []);
    assert.deepEqual(world.restarts, []);

    world.visibility.show();
    await settle();

    assert.deepEqual(world.frame.requests, []);
    assert.equal(world.restarts.length, 1);
  });

  test('a page shown again before its lease ends is not restarted', async () => {
    const world = await setUp();
    world.visibility.hide();
    await world.clock.advance(1_000);
    world.visibility.show();
    await settle();

    assert.deepEqual(world.restarts, []);
    assert.deepEqual(world.frame.requests, []);
  });

  test('disposing the driver releases its visibility listener', async () => {
    const world = await setUp();
    assert.equal(world.visibility.listeners.size, 1);

    world.driver.dispose();

    assert.equal(world.visibility.listeners.size, 0);
  });
});

describe('the 10 second bound of an adoption', () => {
  test('repeats a 503 every 500 ms until the adoption succeeds', async () => {
    const world = await setUp();
    world.frame.answers.push(unavailable, unavailable);

    const change = world.driver.changePermissions(
      CAPABILITIES,
      world.frame.current,
    );
    await settle();
    await world.clock.advance(2 * REPEAT_MS);
    await change;

    assert.deepEqual(attemptsOf(world.frame.requests), [
      { reason: 'permissions', revision: REVISION, generation: 1, at: 0 },
      {
        reason: 'permissions',
        revision: REVISION,
        generation: 2,
        at: REPEAT_MS,
      },
      {
        reason: 'permissions',
        revision: REVISION,
        generation: 3,
        at: 2 * REPEAT_MS,
      },
    ]);
    assert.deepEqual(world.log, [
      'pause:permissions',
      'set',
      'adopt:permissions',
      'adopt:permissions',
      'adopt:permissions',
    ]);
    assert.deepEqual(world.changes, [
      {
        view: viewOf(3),
        deadline: 2 * REPEAT_MS + LEASE_MS,
        documentRevision: REVISION,
      },
    ]);
  });

  test('a 503 that repeats to the bound fails the adoption at the last repeat the bound allows, and restarts the frame', async () => {
    const world = await setUp();
    world.frame.fallback = unavailable;

    const change = world.driver.changePermissions(
      CAPABILITIES,
      world.frame.current,
    );
    await settle();
    await world.clock.advance(BOUND_MS);
    await change;

    // A repeat starts every 500 ms; the one that would start at 10 s does not.
    assert.deepEqual(
      attemptsOf(world.frame.requests).map(({ at }) => at),
      [
        0, 500, 1_000, 1_500, 2_000, 2_500, 3_000, 3_500, 4_000, 4_500, 5_000,
        5_500, 6_000, 6_500, 7_000, 7_500, 8_000, 8_500, 9_000, 9_500,
      ],
    );
    // No repeat fits after 9.5 s, so the adoption fails there and does not wait out the bound.
    assert.deepEqual(world.restarts, [9_500]);
    assert.deepEqual(world.log.slice(-2), ['fail', 'restart']);
  });

  test('an adoption that never settles is aborted at the bound, its generation is closed and the frame restarts', async () => {
    const world = await setUp();
    const aborts = [];
    world.frame.fallback = (request) =>
      new Promise((_resolve, reject) => {
        request.signal.addEventListener(
          'abort',
          () => {
            aborts.push(world.clock.now());
            reject(
              Object.assign(new Error('The request was aborted.'), {
                name: 'AbortError',
              }),
            );
          },
          { once: true },
        );
      });

    const change = world.driver.changePermissions(
      CAPABILITIES,
      world.frame.current,
    );
    await settle();
    await world.clock.advance(BOUND_MS - POLL_MS);
    assert.deepEqual(aborts, []);

    await world.clock.advance(POLL_MS);
    await change;

    assert.deepEqual(aborts, [BOUND_MS]);
    assert.deepEqual(world.log, [
      'pause:permissions',
      'set',
      'adopt:permissions',
      'close:1',
      'fail',
      'restart',
    ]);
    assert.deepEqual(world.restarts, [BOUND_MS]);
  });

  const refusals = [
    { kind: 'a conflict (409)', error: () => apiError(409) },
    { kind: 'a server failure (500)', error: () => apiError(500) },
    {
      // No answer came, so the server may have made the generation: it is closed.
      kind: 'a network failure',
      error: () => new TypeError('The network request failed.'),
      closes: ['close:1'],
    },
  ];
  for (const { kind, error, closes = [] } of refusals) {
    test(`an adoption refused by ${kind} fails at once and is not repeated`, async () => {
      const world = await setUp();
      const refusal = error();
      world.frame.fallback = () => Promise.reject(refusal);

      await world.driver.changePermissions(CAPABILITIES, world.frame.current);

      assert.deepEqual(attemptsOf(world.frame.requests), [
        { reason: 'permissions', revision: REVISION, generation: 1, at: 0 },
      ]);
      assert.deepEqual(world.log, [
        'pause:permissions',
        'set',
        'adopt:permissions',
        ...closes,
        'fail',
        'restart',
      ]);
      assert.deepEqual(world.restarts, [0]);
    });
  }
});

describe('a change chosen against a grant that was replaced', () => {
  test('a change chosen before an adoption is refused after it, and no grant is sent', async () => {
    const world = await setUp();
    const shown = world.frame.current;
    // The frame's own save is adopted first: its generation holds no grant.
    world.driver.saved(SAVED_REVISION);
    await settle();
    assert.equal(world.frame.current.generation, 1);

    await assert.rejects(
      world.driver.changePermissions(CAPABILITIES, shown),
      /permissions shown were replaced/,
    );
    assert.deepEqual(world.grants.sets, []);
    assert.deepEqual(world.log, ['pause:saved', 'adopt:saved']);
  });

  test('a change chosen while an adoption is on its way is refused once it lands', async () => {
    const world = await setUp();
    const shown = world.frame.current;
    const landing = deferred();
    world.frame.answers.push((request) =>
      landing.promise.then(() => ({
        view: viewOf(request.generation, request.revision),
        deadline: world.clock.now() + LEASE_MS,
      })),
    );
    world.driver.saved(SAVED_REVISION);
    await settle();
    const change = world.driver.changePermissions(CAPABILITIES, shown);
    landing.resolve();

    await assert.rejects(change, /permissions shown were replaced/);
    assert.deepEqual(world.grants.sets, []);
  });
});

describe('a grant changed by another preview while an adoption is on its way', () => {
  test('is adopted afterwards, since the generation made meanwhile does not hold it', async () => {
    const world = await setUp();
    const landing = deferred();
    world.frame.answers.push((request) =>
      landing.promise.then(() => ({
        view: viewOf(request.generation, request.revision),
        deadline: world.clock.now() + LEASE_MS,
      })),
    );
    world.grants.change(REVISION);
    await settle();
    assert.equal(world.frame.requests.length, 1);
    // A second change lands while the first adoption's request is on its way.
    world.grants.change(REVISION);
    landing.resolve();
    await settle();

    assert.deepEqual(attemptsOf(world.frame.requests), [
      { reason: 'permissions', revision: REVISION, generation: 1, at: 0 },
      { reason: 'permissions', revision: REVISION, generation: 2, at: 0 },
    ]);
  });
});

describe('a lease that ends while the grant is unresolved', () => {
  test('restarts the frame at the end of the lease, and adopts nothing', async () => {
    const world = await setUp();
    world.grants.status = 'unknown';
    world.grants.notify();
    await settle();
    assert.deepEqual(world.log, ['refuse']);

    await world.clock.advance(DEADLINE - 250);
    assert.deepEqual(world.restarts, []);
    await world.clock.advance(250);

    assert.deepEqual(world.frame.requests, []);
    assert.deepEqual(world.log, ['refuse', 'fail', 'restart']);
    assert.deepEqual(world.restarts, [DEADLINE]);
  });
});
