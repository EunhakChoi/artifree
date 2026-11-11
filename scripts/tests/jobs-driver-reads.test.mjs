import assert from 'node:assert/strict';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// Decision 0055 of the Jobs tab, rules 5, 13, 14 and 15: which reads the tab
// makes, when, and in what order. The driver is bundled as the page ships it
// and runs on a fake clock and fake ports, so a minute of polling costs no real
// time. Expected times, queries and rows are written out from the rules.

// The driver is web code that imports the client; bundle it as it ships.
const bundle = await build({
  entryPoints: [
    fileURLToPath(
      new URL('../../apps/web/src/artifacts/jobs-driver.ts', import.meta.url),
    ),
  ],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const { JobsDriver } = await import(
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
);

const ARTIFACT = `art_${'0'.repeat(32)}`;
const OTHER_ARTIFACT = `art_${'1'.repeat(32)}`;
const JOB_A = `job_${'a'.repeat(32)}`;
const JOB_B = `job_${'b'.repeat(32)}`;
const JOB_C = `job_${'c'.repeat(32)}`;
const JOB_D = `job_${'d'.repeat(32)}`;
const JOB_NEW = `job_${'e'.repeat(32)}`;
const JOB_UNLISTED = `job_${'f'.repeat(32)}`;
const WORKER = {
  worker: {
    mode: 'supervised',
    state: 'running',
    restarts: 0,
    dropped_lines: 0,
  },
};
const flush = () => new Promise((resolve) => setImmediate(resolve));

/** A clock that moves only when a test advances it; ties run in arming order. */
class FakeClock {
  #time = 0;
  #timers = new Map();
  #next = 0;

  now() {
    return this.#time;
  }

  after(callback, milliseconds) {
    const id = this.#next++;
    this.#timers.set(id, { at: this.#time + milliseconds, callback });
    return () => {
      this.#timers.delete(id);
    };
  }

  get pending() {
    return this.#timers.size;
  }

  async advance(milliseconds) {
    // Work that is due now (a read answered at once, say) runs before the clock
    // moves, so that it arms its timers at the time it really ran.
    await flush();
    const target = this.#time + milliseconds;
    for (;;) {
      let due;
      for (const [id, timer] of this.#timers)
        if (timer.at <= target && (due === undefined || timer.at < due.at))
          due = { id, ...timer };
      if (due === undefined) break;
      this.#timers.delete(due.id);
      this.#time = due.at;
      due.callback();
      await flush();
    }
    this.#time = target;
    await flush();
  }
}

/** A job of ARTIFACT, created at `created` (ms), in `state`. */
function job(id, state, created = 1) {
  return {
    id,
    artifact_id: ARTIFACT,
    revision: 1,
    kind: 'verify_revision',
    state,
    attempt: 1,
    max_attempts: 3,
    cancel_requested: false,
    created_at_ms: created,
  };
}

/** A page of the list: its jobs, and the token of the page after it. */
function page(jobs, next = null) {
  return { jobs, next_page_token: next };
}

/** A change of the feed of ARTIFACT, with the fields the client gives it. */
function change(fields) {
  return {
    cursor: `${'f'.repeat(32)}.1`,
    artifact_id: ARTIFACT,
    revision: 1,
    metadata_revision: 1,
    committed_at_ms: 1,
    ...fields,
  };
}

/** A promise that the test settles by resolving it. */
function deferred() {
  let resolve;
  const promise = new Promise((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

/**
 * The ports of one artifact, answered from `answers`. Each name lists its
 * answers in call order, and the last one repeats. An answer is a value, an
 * `Error` that the call rejects with, or a promise that the test settles.
 * Every call is recorded with its fake time, its input (the query, the id or
 * the request) and its signal or request key. `peak()` is the most reads that
 * were in flight at once.
 */
function fakePorts(clock, answers) {
  const calls = [];
  const served = Object.fromEntries(
    Object.keys(answers).map((name) => [name, 0]),
  );
  let inFlight = 0;
  let peak = 0;

  const respond = (name, input, extra) => {
    calls.push({ name, input, at: clock.now(), ...extra });
    const list = answers[name] ?? [];
    if (list.length === 0) {
      return Promise.reject(new Error(`No answer is scripted for ${name}.`));
    }
    const answer = list[Math.min(served[name], list.length - 1)];
    served[name] += 1;
    if (answer instanceof Error) return Promise.reject(answer);
    if (answer instanceof Promise) return answer;
    return Promise.resolve(answer);
  };

  const read = (name, input, extra) => {
    inFlight += 1;
    peak = Math.max(peak, inFlight);
    return respond(name, input, extra).finally(() => {
      inFlight -= 1;
    });
  };

  return {
    calls,
    peak: () => peak,
    ports: {
      listJobs: (query, signal) => read('listJobs', query, { signal }),
      getJob: (id, signal) => read('getJob', id, { signal }),
      getBackground: (signal) => read('getBackground', undefined, { signal }),
      enqueueJob: (request, key) => respond('enqueueJob', request, { key }),
      cancelJob: (id, key) => respond('cancelJob', id, { key }),
    },
  };
}

/**
 * A driver of ARTIFACT on a fake clock and the fake ports of `answers`. The
 * feed is connected unless `connected` is false. Keys are `key-1`, `key-2`, and
 * so on.
 */
function driverWith(answers, { connected = true } = {}) {
  const clock = new FakeClock();
  const fake = fakePorts(clock, answers);
  let keys = 0;
  const driver = new JobsDriver(fake.ports, ARTIFACT, clock, () => {
    keys += 1;
    return `key-${keys}`;
  });
  driver.setConnected(connected);
  return { clock, driver, fake };
}

/** A driver started on `answers`, with the reads it starts at once settled. */
async function started(answers, options) {
  const made = driverWith(answers, options);
  made.driver.start();
  await flush();
  return made;
}

/** The calls named `name`, in call order. */
function named(fake, name) {
  return fake.calls.filter((call) => call.name === name);
}

/** The names of all the calls, in call order. */
function namesOf(fake) {
  return fake.calls.map((call) => call.name);
}

/** The inputs of the calls named `name`, in call order. */
function inputsOf(fake, name) {
  return named(fake, name).map((call) => call.input);
}

/** The fake times of the calls named `name`, in call order. */
function timesOf(fake, name) {
  return named(fake, name).map((call) => call.at);
}

// Rule 15: what a change of the feed makes the tab read.
const FEED_CASES = [
  {
    name: 'a job_changed for a listed job reads that job only',
    feed: change({ change: 'job_changed', job_id: JOB_A }),
    reads: [['getJob', JOB_A]],
  },
  {
    name: 'a job_changed for an unlisted job reads the first page only',
    feed: change({ change: 'job_changed', job_id: JOB_UNLISTED }),
    reads: [['listJobs', { limit: 50 }]],
  },
  {
    name: 'recovered reads the first page only',
    feed: change({ change: 'recovered' }),
    reads: [['listJobs', { limit: 50 }]],
  },
  {
    name: 'a job_changed of another artifact reads nothing',
    feed: change({
      artifact_id: OTHER_ARTIFACT,
      change: 'job_changed',
      job_id: JOB_A,
    }),
    reads: [],
  },
  {
    name: 'recovered of another artifact reads nothing',
    feed: change({ artifact_id: OTHER_ARTIFACT, change: 'recovered' }),
    reads: [],
  },
];

for (const kase of FEED_CASES) {
  test(`following the feed: ${kase.name}`, async () => {
    const { driver, fake } = await started({
      listJobs: [page([job(JOB_A, 'running', 300)])],
      getBackground: [WORKER],
      getJob: [job(JOB_A, 'succeeded', 300)],
    });
    const before = fake.calls.length;
    driver.follow(kase.feed);
    await flush();
    const reads = fake.calls
      .slice(before)
      .map((call) => [call.name, call.input]);
    assert.deepEqual(reads, kase.reads);
  });
}

test('start() reads the first page, and the worker once the page has settled', async () => {
  const firstPage = deferred();
  const { driver, fake } = driverWith({
    listJobs: [firstPage.promise],
    getBackground: [WORKER],
  });
  driver.start();
  assert.deepEqual(namesOf(fake), ['listJobs']);
  await flush();
  assert.deepEqual(namesOf(fake), ['listJobs']);
  firstPage.resolve(page([job(JOB_A, 'queued', 300)]));
  await flush();
  assert.deepEqual(namesOf(fake), ['listJobs', 'getBackground']);
  assert.deepEqual(inputsOf(fake, 'listJobs'), [{ limit: 50 }]);
  assert.equal(fake.peak(), 1);
});

test('job changes that arrive during a first page read are read after it, one job at a time', async () => {
  const laterRead = deferred();
  const { driver, fake } = await started({
    listJobs: [
      page([job(JOB_A, 'running', 300), job(JOB_B, 'queued', 200)]),
      laterRead.promise,
      page([job(JOB_A, 'succeeded', 300), job(JOB_B, 'succeeded', 200)]),
    ],
    getBackground: [WORKER],
    getJob: [job(JOB_A, 'succeeded', 300), job(JOB_B, 'succeeded', 200)],
  });
  driver.follow(change({ change: 'recovered' }));
  driver.follow(change({ change: 'job_changed', job_id: JOB_A }));
  driver.follow(change({ change: 'job_changed', job_id: JOB_B }));
  await flush();
  assert.equal(named(fake, 'listJobs').length, 2);
  assert.deepEqual(inputsOf(fake, 'getJob'), []);
  laterRead.resolve(
    page([job(JOB_A, 'running', 300), job(JOB_B, 'queued', 200)]),
  );
  await flush();
  assert.equal(named(fake, 'listJobs').length, 3);
  assert.deepEqual(inputsOf(fake, 'getJob'), [JOB_A, JOB_B]);
  assert.equal(fake.peak(), 1);
});

test('a job read replaces the row of that job and no other row', async () => {
  const { driver } = await started({
    listJobs: [page([job(JOB_A, 'running', 300), job(JOB_B, 'queued', 200)])],
    getBackground: [WORKER],
    getJob: [job(JOB_A, 'succeeded', 300)],
  });
  driver.follow(change({ change: 'job_changed', job_id: JOB_A }));
  await flush();
  assert.deepEqual(driver.snapshot().rows.jobs, [
    job(JOB_A, 'succeeded', 300),
    job(JOB_B, 'queued', 200),
  ]);
});

test('a first page read that answers after a job read shows its own rows', async () => {
  const { driver } = await started({
    listJobs: [
      page([job(JOB_A, 'running', 300), job(JOB_B, 'queued', 200)]),
      page([job(JOB_A, 'failed', 300), job(JOB_B, 'cancelled', 200)]),
    ],
    getBackground: [WORKER],
    getJob: [job(JOB_A, 'succeeded', 300)],
  });
  driver.follow(change({ change: 'job_changed', job_id: JOB_A }));
  await flush();
  driver.follow(change({ change: 'recovered' }));
  await flush();
  assert.deepEqual(driver.snapshot().rows.jobs, [
    job(JOB_A, 'failed', 300),
    job(JOB_B, 'cancelled', 200),
  ]);
});

test('a failed first page read shows its message; retry() clears it and reads page one again', async () => {
  const { driver, fake } = driverWith({
    listJobs: [
      new Error('The server did not answer.'),
      page([job(JOB_A, 'queued', 300)]),
    ],
    getBackground: [WORKER],
  });
  driver.start();
  await flush();
  assert.equal(driver.snapshot().readError, 'The server did not answer.');
  assert.equal(driver.snapshot().rows, undefined);
  driver.retry();
  assert.equal(driver.snapshot().readError, undefined);
  assert.deepEqual(inputsOf(fake, 'listJobs'), [{ limit: 50 }, { limit: 50 }]);
  await flush();
  assert.equal(driver.snapshot().readError, undefined);
  assert.deepEqual(driver.snapshot().rows.jobs, [job(JOB_A, 'queued', 300)]);
});

test('Load more reads the page after the loaded jobs and appends it; with no page left it reads nothing', async () => {
  const { driver, fake } = await started({
    listJobs: [
      page([job(JOB_A, 'running', 300), job(JOB_B, 'queued', 200)], 'tok'),
      page([job(JOB_C, 'succeeded', 100)]),
    ],
    getBackground: [WORKER],
  });
  driver.loadMore();
  await flush();
  assert.deepEqual(inputsOf(fake, 'listJobs'), [
    { limit: 50 },
    { limit: 50, page_token: 'tok' },
  ]);
  assert.deepEqual(driver.snapshot().rows.jobs, [
    job(JOB_A, 'running', 300),
    job(JOB_B, 'queued', 200),
    job(JOB_C, 'succeeded', 100),
  ]);
  assert.equal(driver.snapshot().rows.next, undefined);
  const calls = fake.calls.length;
  driver.loadMore();
  await flush();
  assert.equal(fake.calls.length, calls);
});

test('while the feed is not connected, the first page is read every 15 seconds and the worker is not', async () => {
  const { clock, driver, fake } = driverWith(
    { listJobs: [page([])], getBackground: [WORKER] },
    { connected: false },
  );
  driver.start();
  await clock.advance(30_000);
  assert.deepEqual(timesOf(fake, 'listJobs'), [0, 15_000, 30_000]);
  assert.deepEqual(timesOf(fake, 'getBackground'), [0]);
});

test('the poll stops when the feed connects, and no read follows at 45 seconds', async () => {
  const { clock, driver, fake } = driverWith(
    { listJobs: [page([])], getBackground: [WORKER] },
    { connected: false },
  );
  driver.start();
  await clock.advance(31_000);
  driver.setConnected(true);
  assert.equal(clock.pending, 0);
  await clock.advance(29_000);
  assert.deepEqual(timesOf(fake, 'listJobs'), [0, 15_000, 30_000]);
});

test('while the feed is connected, the first page is not polled', async () => {
  const { clock, fake } = await started({
    listJobs: [page([])],
    getBackground: [WORKER],
  });
  await clock.advance(60_000);
  assert.deepEqual(timesOf(fake, 'listJobs'), [0]);
});

test('stop() cancels the poll and leaves no timer', async () => {
  const { clock, driver, fake } = driverWith(
    { listJobs: [page([])], getBackground: [WORKER] },
    { connected: false },
  );
  driver.start();
  await clock.advance(15_000);
  driver.stop();
  assert.equal(clock.pending, 0);
  await clock.advance(60_000);
  assert.deepEqual(timesOf(fake, 'listJobs'), [0, 15_000]);
});

test('a job queued for 5 seconds is overdue; the worker is read then and every 15 seconds while it stays queued', async () => {
  const { clock, driver, fake } = await started({
    listJobs: [page([job(JOB_A, 'queued', 1)])],
    getBackground: [WORKER],
  });
  await clock.advance(4_999);
  assert.deepEqual([...driver.snapshot().overdue], []);
  await clock.advance(1);
  assert.deepEqual([...driver.snapshot().overdue], [JOB_A]);
  await clock.advance(15_000);
  assert.deepEqual(timesOf(fake, 'getBackground'), [0, 5_000, 20_000]);
});

test('a job that is no longer queued leaves the overdue flag and ends the worker reads', async () => {
  const { clock, driver, fake } = await started({
    listJobs: [page([job(JOB_A, 'queued', 1)])],
    getBackground: [WORKER],
    getJob: [job(JOB_A, 'running', 1)],
  });
  await clock.advance(5_000);
  assert.deepEqual([...driver.snapshot().overdue], [JOB_A]);
  driver.follow(change({ change: 'job_changed', job_id: JOB_A }));
  await flush();
  assert.deepEqual([...driver.snapshot().overdue], []);
  assert.deepEqual(timesOf(fake, 'getBackground'), [0, 5_000]);
  await clock.advance(60_000);
  assert.deepEqual(timesOf(fake, 'getBackground'), [0, 5_000]);
  assert.equal(clock.pending, 0);
});

/**
 * A driver stopped while its poll read is in flight, whose answer is still
 * held. Returns the clock, the fake ports, the poll's call and the snapshot
 * taken at the stop.
 */
async function stoppedDuringPoll() {
  const late = deferred();
  const { clock, driver, fake } = driverWith(
    {
      listJobs: [page([job(JOB_A, 'queued', 1)]), late.promise],
      getBackground: [WORKER],
    },
    { connected: false },
  );
  driver.start();
  await clock.advance(15_000);
  const poll = named(fake, 'listJobs')[1];
  driver.stop();
  return { clock, driver, fake, poll, late, stopped: driver.snapshot() };
}

test('stop() abandons the read in flight: its late answer changes nothing and no timer stays', async () => {
  const { clock, driver, fake, poll, late, stopped } =
    await stoppedDuringPoll();
  assert.equal(poll.signal.aborted, true);
  assert.equal(clock.pending, 0);
  late.resolve(page([job(JOB_A, 'succeeded', 1)]));
  await flush();
  assert.equal(driver.snapshot(), stopped);
  const calls = fake.calls.length;
  await clock.advance(60_000);
  assert.equal(fake.calls.length, calls);
});

test('start() after stop() reads the first page again', async () => {
  const { driver, fake } = await stoppedDuringPoll();
  driver.start();
  await flush();
  assert.equal(named(fake, 'listJobs').length, 3);
  assert.deepEqual(inputsOf(fake, 'listJobs').at(-1), { limit: 50 });
});

test('resync() does nothing before start() and after stop()', async () => {
  const { clock, driver, fake } = driverWith({
    listJobs: [page([])],
    getBackground: [WORKER],
  });
  driver.resync();
  await clock.advance(60_000);
  assert.deepEqual(namesOf(fake), []);
  driver.start();
  await flush();
  driver.stop();
  const calls = fake.calls.length;
  driver.resync();
  await flush();
  assert.equal(fake.calls.length, calls);
});

test("resync() during a read discards that read's answer and reads page one again", async () => {
  const stale = deferred();
  const fresh = deferred();
  const { driver, fake } = driverWith({
    listJobs: [stale.promise, fresh.promise],
    getBackground: [WORKER],
  });
  driver.start();
  driver.resync();
  stale.resolve(page([job(JOB_B, 'queued', 200)]));
  await flush();
  assert.equal(driver.snapshot().rows, undefined);
  assert.deepEqual(inputsOf(fake, 'listJobs'), [{ limit: 50 }, { limit: 50 }]);
  fresh.resolve(page([job(JOB_A, 'running', 300)]));
  await flush();
  assert.deepEqual(driver.snapshot().rows.jobs, [job(JOB_A, 'running', 300)]);
});

test('resync() keeps the loaded pages after the first one, and the next page is the one after them', async () => {
  const { driver, fake } = await started({
    listJobs: [
      page([job(JOB_A, 'running', 300), job(JOB_B, 'queued', 200)], 'tok-2'),
      page(
        [job(JOB_C, 'succeeded', 100), job(JOB_D, 'succeeded', 90)],
        'tok-3',
      ),
      page([job(JOB_A, 'failed', 300), job(JOB_B, 'queued', 200)], 'tok-2'),
    ],
    getBackground: [WORKER],
  });
  driver.loadMore();
  await flush();
  driver.resync();
  await flush();
  assert.deepEqual(driver.snapshot().rows.jobs, [
    job(JOB_A, 'failed', 300),
    job(JOB_B, 'queued', 200),
    job(JOB_C, 'succeeded', 100),
    job(JOB_D, 'succeeded', 90),
  ]);
  assert.equal(driver.snapshot().rows.next, 'tok-3');
  driver.loadMore();
  await flush();
  assert.deepEqual(inputsOf(fake, 'listJobs').at(-1), {
    limit: 50,
    page_token: 'tok-3',
  });
});

test('a reread past loaded jobs offers Load more, and Load more reads the jobs between', async () => {
  // Jobs a and b were created after the loaded job c; job d between b and c.
  const { driver, fake } = await started({
    listJobs: [
      page([job(JOB_C, 'succeeded', 100)]),
      page([job(JOB_A, 'queued', 300), job(JOB_B, 'queued', 200)], 'tok-new'),
      page([job(JOB_D, 'queued', 150), job(JOB_C, 'succeeded', 100)]),
    ],
    getBackground: [WORKER],
  });
  driver.resync();
  await flush();
  assert.equal(driver.snapshot().rows.next, 'tok-new');
  driver.loadMore();
  await flush();
  assert.deepEqual(inputsOf(fake, 'listJobs').at(-1), {
    limit: 50,
    page_token: 'tok-new',
  });
  assert.deepEqual(driver.snapshot().rows.jobs, [
    job(JOB_A, 'queued', 300),
    job(JOB_B, 'queued', 200),
    job(JOB_D, 'queued', 150),
    job(JOB_C, 'succeeded', 100),
  ]);
  assert.equal(driver.snapshot().rows.next, undefined);
});

test('resync() keeps a verify press that is still sending, and its job joins the rows', async () => {
  const sent = deferred();
  const { driver, fake } = await started({
    listJobs: [
      page([job(JOB_A, 'running', 300), job(JOB_B, 'queued', 200)]),
      page([job(JOB_A, 'running', 300), job(JOB_B, 'queued', 200)]),
    ],
    getBackground: [WORKER],
    enqueueJob: [sent.promise],
  });
  driver.verify(3);
  driver.resync();
  await flush();
  assert.equal(driver.snapshot().verify.status, 'sending');
  sent.resolve({ job: job(JOB_NEW, 'queued', 400), replayed: false });
  await flush();
  assert.equal(driver.snapshot().verify.status, 'idle');
  assert.deepEqual(
    driver.snapshot().rows.jobs.map((listed) => listed.id),
    [JOB_NEW, JOB_A, JOB_B],
  );
  assert.deepEqual(
    named(fake, 'enqueueJob').map((call) => call.input),
    [{ kind: 'verify_revision', revision: 3 }],
  );
  assert.equal(named(fake, 'enqueueJob')[0].key, 'key-1');
});

test('a reset drops the loaded rows and their next page, so Load more reads on from the new first page', async () => {
  // Rule 15: after a reset the database may be another one. Its first page shares
  // job a with the loaded rows, which a merge would keep with the older jobs and
  // with the old next page.
  const { driver, fake } = driverWith({
    listJobs: [
      page([job(JOB_A, 'running', 300), job(JOB_B, 'queued', 200)], 'tok-2'),
      page([job(JOB_C, 'succeeded', 100)], 'tok-3'),
      page([job(JOB_A, 'failed', 300)], 'tok-new'),
    ],
    getBackground: [WORKER],
  });
  driver.start(0);
  await flush();
  driver.loadMore();
  await flush();
  assert.deepEqual(driver.snapshot().rows, {
    jobs: [
      job(JOB_A, 'running', 300),
      job(JOB_B, 'queued', 200),
      job(JOB_C, 'succeeded', 100),
    ],
    next: 'tok-3',
  });
  driver.resync(1);
  await flush();
  assert.deepEqual(driver.snapshot().rows, {
    jobs: [job(JOB_A, 'failed', 300)],
    next: 'tok-new',
  });
  driver.loadMore();
  await flush();
  assert.deepEqual(inputsOf(fake, 'listJobs'), [
    { limit: 50 },
    { limit: 50, page_token: 'tok-2' },
    { limit: 50 },
    { limit: 50, page_token: 'tok-new' },
  ]);
});

test('a resync with the reset count the rows were read under keeps the loaded pages, as a ready does', async () => {
  const { driver, fake } = driverWith({
    listJobs: [
      page([job(JOB_A, 'running', 300), job(JOB_B, 'queued', 200)], 'tok-2'),
      page([job(JOB_C, 'succeeded', 100)], 'tok-3'),
      page([job(JOB_A, 'failed', 300)], 'tok-new'),
    ],
    getBackground: [WORKER],
  });
  driver.start(0);
  await flush();
  driver.loadMore();
  await flush();
  driver.resync(0);
  await flush();
  assert.deepEqual(driver.snapshot().rows, {
    jobs: [
      job(JOB_A, 'failed', 300),
      job(JOB_B, 'queued', 200),
      job(JOB_C, 'succeeded', 100),
    ],
    next: 'tok-3',
  });
  assert.equal(named(fake, 'listJobs').length, 3);
});

test('start() after a reset that came while the tab was away drops the loaded rows', async () => {
  const { driver, fake } = driverWith({
    listJobs: [
      page([job(JOB_A, 'running', 300), job(JOB_B, 'queued', 200)], 'tok-2'),
      page([job(JOB_A, 'failed', 300)], 'tok-new'),
    ],
    getBackground: [WORKER],
  });
  driver.start(0);
  await flush();
  driver.stop();
  driver.start(1);
  await flush();
  assert.deepEqual(driver.snapshot().rows, {
    jobs: [job(JOB_A, 'failed', 300)],
    next: 'tok-new',
  });
  assert.deepEqual(inputsOf(fake, 'listJobs'), [{ limit: 50 }, { limit: 50 }]);
});

test('start() with the reset count the rows were read under keeps the loaded pages, as a reopened tab does', async () => {
  const { driver } = driverWith({
    listJobs: [
      page([job(JOB_A, 'running', 300), job(JOB_B, 'queued', 200)], 'tok-2'),
      page([job(JOB_C, 'succeeded', 100)], 'tok-3'),
      page([job(JOB_A, 'failed', 300)], 'tok-new'),
    ],
    getBackground: [WORKER],
  });
  driver.start(0);
  await flush();
  driver.loadMore();
  await flush();
  driver.stop();
  driver.start(0);
  await flush();
  assert.deepEqual(driver.snapshot().rows, {
    jobs: [
      job(JOB_A, 'failed', 300),
      job(JOB_B, 'queued', 200),
      job(JOB_C, 'succeeded', 100),
    ],
    next: 'tok-3',
  });
});

test('a reset abandons a Load more in flight: its late answer and its old token change nothing', async () => {
  const late = deferred();
  const { driver, fake } = driverWith({
    listJobs: [
      page([job(JOB_A, 'running', 300)], 'tok-2'),
      late.promise,
      page([job(JOB_A, 'failed', 300)], 'tok-new'),
    ],
    getBackground: [WORKER],
  });
  driver.start(0);
  await flush();
  driver.loadMore();
  await flush();
  driver.resync(1);
  await flush();
  late.resolve(page([job(JOB_B, 'queued', 200)], 'tok-old'));
  await flush();
  assert.deepEqual(driver.snapshot().rows, {
    jobs: [job(JOB_A, 'failed', 300)],
    next: 'tok-new',
  });
  // A later read of the first page must not carry the abandoned Load more along.
  driver.follow(change({ change: 'job_changed', job_id: JOB_UNLISTED }));
  await flush();
  assert.deepEqual(inputsOf(fake, 'listJobs'), [
    { limit: 50 },
    { limit: 50, page_token: 'tok-2' },
    { limit: 50 },
    { limit: 50 },
  ]);
});

test('a reset drops a Load more that was owed behind a read in flight', async () => {
  const worker = deferred();
  const { driver, fake } = driverWith({
    listJobs: [
      page([job(JOB_A, 'running', 300)], 'tok-2'),
      page([job(JOB_A, 'failed', 300)], 'tok-new'),
    ],
    getBackground: [worker.promise],
  });
  driver.start(0);
  await flush();
  // The worker read is in flight, so the Load more is only owed.
  driver.loadMore();
  driver.resync(1);
  await flush();
  worker.resolve(WORKER);
  await flush();
  assert.deepEqual(driver.snapshot().rows, {
    jobs: [job(JOB_A, 'failed', 300)],
    next: 'tok-new',
  });
  assert.deepEqual(inputsOf(fake, 'listJobs'), [{ limit: 50 }, { limit: 50 }]);
});

test('a reset keeps a verify press whose answer was lost, and its retry sends the same request key', async () => {
  const { driver, fake } = driverWith({
    listJobs: [
      page([job(JOB_A, 'running', 300)]),
      page([job(JOB_A, 'running', 300)]),
    ],
    getBackground: [WORKER],
    enqueueJob: [
      new Error('The answer was lost.'),
      { job: job(JOB_NEW, 'queued', 400), replayed: true },
    ],
  });
  driver.start(0);
  await flush();
  driver.verify(3);
  await flush();
  assert.equal(driver.snapshot().verify.status, 'unknown');
  driver.resync(1);
  await flush();
  assert.equal(driver.snapshot().verify.status, 'unknown');
  driver.retryVerify();
  await flush();
  assert.equal(driver.snapshot().verify.status, 'idle');
  assert.deepEqual(
    named(fake, 'enqueueJob').map((call) => call.key),
    ['key-1', 'key-1'],
  );
});

test('a reset leaves a worker read in flight to finish, and reads the first page after it', async () => {
  const worker = deferred();
  const { driver, fake } = driverWith({
    listJobs: [
      page([job(JOB_A, 'running', 300)], 'tok-2'),
      page([job(JOB_A, 'failed', 300)], 'tok-new'),
    ],
    getBackground: [worker.promise],
  });
  driver.start(0);
  await flush();
  driver.resync(1);
  await flush();
  assert.equal(driver.snapshot().rows, undefined);
  worker.resolve(WORKER);
  await flush();
  assert.deepEqual(driver.snapshot().background, WORKER);
  assert.deepEqual(driver.snapshot().rows, {
    jobs: [job(JOB_A, 'failed', 300)],
    next: 'tok-new',
  });
  assert.deepEqual(inputsOf(fake, 'listJobs'), [{ limit: 50 }, { limit: 50 }]);
});
