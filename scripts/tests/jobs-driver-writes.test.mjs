import assert from 'node:assert/strict';
import { test } from 'node:test';
import { setImmediate as nextTurn } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The writes of the Jobs tab's driver (decision 0055, rule 14), bundled as they
// ship. A case scripts the answer of each write in order. A write the case does
// not script is held, so its press stays sending. Request keys are k1, k2, ...
// in the order the driver asks for them. The clock stands still, since no case
// here depends on time.

// The driver and the client's error class come from one bundle, so the
// driver's `instanceof` checks hold for the errors these cases throw.
const bundle = await build({
  stdin: {
    contents:
      "export { JobsDriver } from './jobs-driver.ts'; export { ArtifreeApiError } from '@artifree/client';",
    resolveDir: fileURLToPath(
      new URL('../../apps/web/src/artifacts/', import.meta.url),
    ),
  },
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const { ArtifreeApiError, JobsDriver } = await import(
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
);

const ARTIFACT = `art_${'a'.repeat(32)}`;
const JOB = `job_${'c'.repeat(32)}`;

/** A job of `ARTIFACT` as the server answers it; `fields` replace its own. */
function job(fields = {}) {
  return {
    id: JOB,
    artifact_id: ARTIFACT,
    kind: 'verify_revision',
    revision: 3,
    state: 'queued',
    attempt: 0,
    max_attempts: 3,
    cancel_requested: false,
    created_at_ms: 1_000,
    ...fields,
  };
}

/** `GET /v1/background` of a server whose worker runs. */
const RUNNING = {
  worker: {
    dropped_lines: 0,
    mode: 'supervised',
    restarts: 0,
    state: 'running',
  },
};

/** A promise that never settles. */
function neverSettles() {
  return new Promise(() => {});
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
 * A failed request as the client raises it. `status` is 0 when no answer came.
 */
function apiError(status, code) {
  return new ArtifreeApiError(`The request failed with ${code}.`, {
    status,
    code,
  });
}

/** A write answer that succeeds with `answered`. */
function succeeds(answered, replayed = false) {
  return () => Promise.resolve({ job: answered, replayed });
}

/** A write answer that fails with `error`. */
function fails(error) {
  return () => Promise.reject(error);
}

/**
 * A write port. It records each call as its leading arguments (`target`) and
 * its key, which is the last argument. It answers the calls in order, each
 * answer being a function that returns the promise of the answer. A call past
 * the last answer is held.
 */
function writePort(...answers) {
  const calls = [];
  const port = (...args) => {
    calls.push({ target: args.slice(0, -1), key: args.at(-1) });
    const answer = answers.shift();
    return answer === undefined ? neverSettles() : answer();
  };
  return { port, calls };
}

/** Request keys k1, k2, ... in the order the driver asks for them. */
function keySource() {
  let count = 0;
  return () => {
    count += 1;
    return `k${count}`;
  };
}

/** A clock that stands still, whose timers never fire. */
const STILL_CLOCK = {
  now: () => 0,
  after: () => () => {},
};

/**
 * The driver's ports for a case. The first page answers with no jobs, the
 * worker runs, and a write the case does not override is held.
 */
function portsWith(overrides) {
  return {
    listJobs: async () => ({ jobs: [] }),
    getJob: async () => {
      throw new Error('No case in this file reads one job.');
    },
    getBackground: async () => RUNNING,
    enqueueJob: neverSettles,
    cancelJob: neverSettles,
    ...overrides,
  };
}

/** A driver of `ARTIFACT` on `ports`, with the request keys of a case. */
function driverOn(ports) {
  return new JobsDriver(ports, ARTIFACT, STILL_CLOCK, keySource());
}

/** A driver that has started, with its first reads answered. */
async function startedDriver(ports) {
  const driver = driverOn(ports);
  driver.start();
  await nextTurn();
  return driver;
}

/**
 * The two presses (rule 14). Each names the port its write goes through, the
 * calls that press, retry and forget it, how its slot reads (an absent cancel
 * is idle), the jobs the list shows before the press, and the job its answer
 * carries.
 */
const PRESSES = {
  verify: {
    port: 'enqueueJob',
    target: [{ kind: 'verify_revision', revision: 3 }],
    press: (driver) => driver.verify(3),
    retry: (driver) => driver.retryVerify(),
    forget: (driver) => driver.forgetVerify(),
    slot: (driver) => driver.snapshot().verify,
    listed: [],
    answer: job(),
  },
  cancel: {
    port: 'cancelJob',
    target: [JOB],
    press: (driver) => driver.cancel(JOB),
    retry: (driver) => driver.retryCancel(JOB),
    forget: (driver) => driver.forgetCancel(JOB),
    slot: (driver) => driver.snapshot().cancels.get(JOB) ?? { status: 'idle' },
    listed: [job({ state: 'running' })],
    answer: job({ state: 'running', cancel_requested: true }),
  },
};

/** The ports of a case of `spec`: its list shows `spec.listed`. */
function portsForPress(spec, writes) {
  return portsWith({
    listJobs: async () => ({ jobs: spec.listed }),
    [spec.port]: writes.port,
  });
}

// Fails when a new press replaces one whose outcome is unknown: the lost
// press may have made a job, and a new key would make a second one.
test('verify: a press while the outcome of the last one is unknown is ignored, and the retry keeps its key', async () => {
  const spec = PRESSES.verify;
  const writes = writePort(
    fails(apiError(0, 'network_error')),
    succeeds(spec.answer, true),
  );
  const driver = await startedDriver(portsForPress(spec, writes));

  spec.press(driver);
  await nextTurn();
  assert.equal(spec.slot(driver).status, 'unknown');
  spec.press(driver);
  await nextTurn();
  assert.equal(writes.calls.length, 1, 'the second press sent nothing');
  assert.equal(spec.slot(driver).status, 'unknown');

  spec.retry(driver);
  await nextTurn();
  assert.deepEqual(
    writes.calls.map((call) => call.key),
    ['k1', 'k1'],
  );
});

for (const [kind, spec] of Object.entries(PRESSES)) {
  test(`${kind}: each press asks under a new request key`, async () => {
    // Rule 14 gives each press of "Verify revision N" a new request key. A
    // cancel request takes its own Idempotency-Key too (0030, rule 10).
    const writes = writePort(succeeds(spec.answer), succeeds(spec.answer));
    const driver = await startedDriver(portsForPress(spec, writes));

    spec.press(driver);
    await nextTurn();
    spec.press(driver);
    await nextTurn();

    assert.deepEqual(writes.calls, [
      { target: spec.target, key: 'k1' },
      { target: spec.target, key: 'k2' },
    ]);
  });

  test(`${kind}: a press whose answer is lost is retried under the same key`, async () => {
    // Rule 14: a press whose answer is lost is retried with the same key. The
    // lost answer leaves the press unknown, and the retry's answer applies once.
    const writes = writePort(
      fails(apiError(0, 'network_error')),
      succeeds(spec.answer, true),
    );
    const driver = await startedDriver(portsForPress(spec, writes));

    spec.press(driver);
    await nextTurn();
    assert.equal(spec.slot(driver).status, 'unknown');

    spec.retry(driver);
    await nextTurn();

    assert.equal(writes.calls.length, 2);
    assert.equal(writes.calls[0].key, 'k1');
    assert.equal(writes.calls[1].key, 'k1');
    assert.deepEqual(writes.calls[1].target, spec.target);
    assert.equal(spec.slot(driver).status, 'idle');
    assert.deepEqual(driver.snapshot().rows.jobs, [spec.answer]);
  });

  test(`${kind}: a press while the last one is sending is ignored`, async () => {
    // A press that comes while the press before it in its slot is sending adds
    // no request, so one press makes one request.
    const answer = deferred();
    const writes = writePort(() => answer.promise);
    const driver = await startedDriver(portsForPress(spec, writes));

    spec.press(driver);
    spec.press(driver);
    await nextTurn();
    assert.equal(writes.calls.length, 1);
    assert.equal(spec.slot(driver).status, 'sending');

    answer.resolve({ job: spec.answer, replayed: false });
    await nextTurn();
    assert.equal(writes.calls.length, 1);
    assert.equal(spec.slot(driver).status, 'idle');
  });

  test(`${kind}: a press is sent at once while the first page is still being read`, () => {
    // Writes are sent at once (decision 0007), so a read in flight does not hold
    // a press back: the request is made within the press itself.
    const list = deferred();
    const writes = writePort(succeeds(spec.answer));
    const driver = driverOn(
      portsWith({ listJobs: () => list.promise, [spec.port]: writes.port }),
    );
    driver.start();

    spec.press(driver);

    assert.deepEqual(writes.calls, [{ target: spec.target, key: 'k1' }]);
    assert.equal(driver.snapshot().reading, true);
    assert.equal(spec.slot(driver).status, 'sending');
  });
}

// Rule 14: "`409 too_many_jobs` and `409 job_ended` are shown as such". Each
// code belongs to the press that can get it.
const REFUSALS = {
  verify: 'too_many_jobs',
  cancel: 'job_ended',
};

for (const [kind, code] of Object.entries(REFUSALS)) {
  test(`${kind}: a 409 ${code} is refused as such, and forgetting clears it`, async () => {
    const spec = PRESSES[kind];
    const writes = writePort(fails(apiError(409, code)));
    const driver = await startedDriver(portsForPress(spec, writes));

    spec.press(driver);
    await nextTurn();
    assert.equal(spec.slot(driver).status, 'refused');
    assert.equal(spec.slot(driver).refusal, code);

    spec.forget(driver);
    assert.equal(spec.slot(driver).status, 'idle');
  });
}

test('verify: a replayed job that the list already shows is replaced, not added', async () => {
  // A read of the list between the lost press and its retry can already show
  // the job, since the server committed the first request. The replayed answer
  // then replaces that row, so the job still appears once.
  let listed = [];
  const writes = writePort(
    fails(apiError(0, 'network_error')),
    succeeds(job({ state: 'running' }), true),
  );
  const driver = await startedDriver(
    portsWith({
      listJobs: async () => ({ jobs: listed }),
      enqueueJob: writes.port,
    }),
  );

  driver.verify(3);
  await nextTurn();
  assert.equal(driver.snapshot().verify.status, 'unknown');

  listed = [job()];
  driver.resync();
  await nextTurn();
  assert.deepEqual(driver.snapshot().rows.jobs, [job()]);

  driver.retryVerify();
  await nextTurn();

  assert.equal(writes.calls[1].key, 'k1');
  assert.deepEqual(driver.snapshot().rows.jobs, [job({ state: 'running' })]);
});

/** A change of the feed of ARTIFACT, with the fields the client gives it. */
function feedChange(fields) {
  return {
    cursor: `${'f'.repeat(32)}.1`,
    artifact_id: ARTIFACT,
    revision: 1,
    metadata_revision: 1,
    committed_at_ms: 1,
    ...fields,
  };
}

test('cancel: an answer that a read has overtaken is not shown, and the job is read again', async () => {
  // The feed's read of the job, made after the cancel was sent, shows the job as
  // it was before the cancel committed. The answer then reports the commit, so
  // it must not be shown: the job is read again, and its later state is shown
  // (rule 8).
  const answer = deferred();
  const writes = writePort(() => answer.promise);
  const reads = [
    job({ state: 'running' }),
    job({ state: 'cancelled', finished_at_ms: 2_000 }),
  ];
  const driver = await startedDriver(
    portsWith({
      listJobs: async () => ({ jobs: [job({ state: 'running' })] }),
      getJob: async () => reads.shift(),
      cancelJob: writes.port,
    }),
  );

  PRESSES.cancel.press(driver);
  driver.follow(feedChange({ change: 'job_changed', job_id: JOB }));
  await nextTurn();
  assert.deepEqual(driver.snapshot().rows.jobs, [job({ state: 'running' })]);

  answer.resolve({
    job: job({ state: 'running', cancel_requested: true }),
    replayed: false,
  });
  await nextTurn();
  assert.deepEqual(driver.snapshot().rows.jobs, [
    job({ state: 'cancelled', finished_at_ms: 2_000 }),
  ]);
  assert.equal(PRESSES.cancel.slot(driver).status, 'idle');
});

test('verify: an answer that a first page has overtaken is not shown over that page', async () => {
  // The job of a verification is new when its press is sent. A first page read
  // after the job's change lists it as succeeded. The answer reports it queued,
  // as the commit left it, and arrives after that page: it is not shown over the
  // page, and the job is read again (rule 8).
  const answer = deferred();
  const writes = writePort(() => answer.promise);
  const done = job({ state: 'succeeded', attempt: 1, finished_at_ms: 2_000 });
  let listed = [];
  const driver = await startedDriver(
    portsWith({
      listJobs: async () => ({ jobs: listed }),
      getJob: async () => done,
      enqueueJob: writes.port,
    }),
  );

  PRESSES.verify.press(driver);
  listed = [done];
  driver.follow(feedChange({ change: 'job_changed', job_id: JOB }));
  await nextTurn();
  assert.deepEqual(driver.snapshot().rows.jobs, [done]);

  answer.resolve({ job: job(), replayed: false });
  await nextTurn();
  assert.deepEqual(driver.snapshot().rows.jobs, [done]);
  assert.equal(PRESSES.verify.slot(driver).status, 'idle');
});
