import assert from 'node:assert/strict';
import { getEventListeners } from 'node:events';
import { test } from 'node:test';
import { ArtifreeApiError } from '../../packages/client/dist/errors.js';
import { followJob } from '../../packages/client/dist/follow-job.js';

// Rule 11 of decision 0055: while the change feed is not connected, the job is
// read at least every 15 seconds; while it is connected, the reads follow the
// feed's events and not the clock. Time is a fake clock, so a minute of silence
// costs no real time. Each test names the wrong implementation it fails.

const JOB_ID = `job_${'a'.repeat(32)}`;
const ARTIFACT_ID = `art_${'b'.repeat(32)}`;
const flush = () => new Promise((resolve) => setImmediate(resolve));

/** A clock that moves only when a test advances it; ties run in arming order. */
class FakeClock {
  now = 0;
  #timers = new Map();
  #next = 0;

  after(callback, milliseconds) {
    const id = this.#next++;
    this.#timers.set(id, { at: this.now + milliseconds, callback });
    return () => {
      this.#timers.delete(id);
    };
  }

  get pending() {
    return this.#timers.size;
  }

  async advance(milliseconds) {
    // Work that is due now (a failed open re-arming a timer, say) runs before
    // the clock moves, so that it arms its timers at the time it really ran.
    await flush();
    const target = this.now + milliseconds;
    for (;;) {
      let due;
      for (const [id, timer] of this.#timers)
        if (timer.at <= target && (due === undefined || timer.at < due.at))
          due = { id, ...timer };
      if (due === undefined) break;
      this.#timers.delete(due.id);
      this.now = due.at;
      due.callback();
      await flush();
    }
    this.now = target;
    await flush();
  }
}

/** The job as a read answers it, in `state`. */
function jobIn(state) {
  return { id: JOB_ID, artifact_id: ARTIFACT_ID, state };
}

/**
 * A read that gives its answers in turn, the last one repeating. `at` lists the
 * fake time of each call; an `Error` answer is thrown.
 */
function scriptedReads(clock, answers) {
  const at = [];
  let calls = 0;
  return {
    at,
    read: async () => {
      at.push(clock.now);
      const answer = answers[Math.min(calls, answers.length - 1)];
      calls += 1;
      if (answer instanceof Error) throw answer;
      return answer;
    },
  };
}

/** Every open is refused with `error`; `counts.opens` counts the attempts. */
function refusingOpen(error, counts) {
  return async () => {
    counts.opens += 1;
    throw error;
  };
}

/**
 * Every open is accepted: the stream says `ready`, then stays silent until its
 * signal aborts, as a connected server does.
 */
function silentOpen(counts) {
  return async (_after, signal) => {
    counts.opens += 1;
    return (async function* events() {
      yield { type: 'ready', cursor: 'c1' };
      await new Promise((resolve) => {
        signal.addEventListener('abort', resolve, { once: true });
      });
    })();
  };
}

/**
 * Starts following the job. The promise gets a handler, so a rejection that a
 * test expects is not reported as unhandled.
 */
function startFollowing(transport, signal, options = {}) {
  const done = followJob(transport, JOB_ID, { signal, ...options });
  done.catch(() => {});
  return done;
}

async function rejection(promise) {
  return promise.then(
    () => assert.fail('The promise must reject.'),
    (error) => error,
  );
}

function noLeftovers(clock, controller) {
  assert.equal(clock.pending, 0, 'a timer was left running');
  assert.equal(
    getEventListeners(controller.signal, 'abort').length,
    0,
    'an abort listener was left behind',
  );
}

/** A 503 from the server, which rule 11 retries. */
const unavailable = new ArtifreeApiError('The server is unavailable.', {
  status: 503,
  code: 'unavailable',
});

/** Read failures that rule 11 retries after 15 seconds. */
const RETRYABLE_FAILURES = [
  ['503', unavailable],
  [
    '429',
    new ArtifreeApiError('Too many requests.', {
      status: 429,
      code: 'rate_limited',
    }),
  ],
  [
    'a network failure',
    new ArtifreeApiError('The connection failed.', {
      status: 0,
      code: 'network_error',
    }),
  ],
];

/** Read failures that end the wait with the error itself (rule 11). */
const FINAL_FAILURES = [
  [
    '401',
    new ArtifreeApiError('Sign in again.', {
      status: 401,
      code: 'unauthenticated',
    }),
  ],
  [
    '404',
    new ArtifreeApiError('No such job.', { status: 404, code: 'not_found' }),
  ],
];

// Fails when nothing arms the poll while the feed is down, so that no read
// comes until a reconnect succeeds. The reconnect waits are 250 seconds here,
// so the poll is the only thing that can read the job.
test('while the feed is refused and reconnects are far apart, the job is read at 15 and at 30 seconds', async () => {
  const clock = new FakeClock();
  const controller = new AbortController();
  const counts = { opens: 0 };
  const { at, read } = scriptedReads(clock, [jobIn('running')]);
  const done = startFollowing(
    { clock, open: refusingOpen(unavailable, counts), read },
    controller.signal,
    { retryDelayMs: { min: 500_000, max: 500_000 }, random: () => 0 },
  );
  await clock.advance(14_999);
  assert.deepEqual(at, [], 'no read before 15 seconds');
  await clock.advance(1);
  assert.deepEqual(at, [15_000], 'a read at 15 seconds while not connected');
  await clock.advance(15_000);
  assert.deepEqual(at, [15_000, 30_000], 'a read again at 30 seconds');
  assert.equal(
    counts.opens,
    1,
    'the reads came from the poll, not a reconnect',
  );
  controller.abort();
  await rejection(done);
});

// Fails when every failed reconnect restarts the 15-second wait, so reconnects
// that come sooner than 15 seconds keep the poll from firing.
test('a feed refused with its default reconnect backoff still gets the job read at 15 and at 30 seconds', async () => {
  const clock = new FakeClock();
  const controller = new AbortController();
  const counts = { opens: 0 };
  const { at, read } = scriptedReads(clock, [jobIn('running')]);
  const done = startFollowing(
    { clock, open: refusingOpen(unavailable, counts), read },
    controller.signal,
    { random: () => 0 },
  );
  await clock.advance(15_000);
  assert.deepEqual(
    at,
    [15_000],
    'a read at 15 seconds, while reconnects are sooner',
  );
  await clock.advance(15_000);
  assert.deepEqual(at, [15_000, 30_000], 'a read again at 30 seconds');
  controller.abort();
  await rejection(done);
});

// Fails when the poll keeps running while the feed is connected.
test('while the feed is connected and silent for 60 seconds, the job is read once', async () => {
  const clock = new FakeClock();
  const controller = new AbortController();
  const counts = { opens: 0 };
  const { at, read } = scriptedReads(clock, [jobIn('running')]);
  const done = startFollowing(
    { clock, open: silentOpen(counts), read },
    controller.signal,
  );
  // A zero advance lets the feed open and its `ready` run.
  await clock.advance(0);
  assert.deepEqual(at, [0], 'the first read follows ready, at once');
  await clock.advance(60_000);
  assert.deepEqual(at, [0], 'no read while connected and silent');
  assert.equal(counts.opens, 1, 'the feed stayed connected');
  controller.abort();
  await rejection(done);
});

for (const [label, failure] of RETRYABLE_FAILURES) {
  // Fails when a retryable failure is not retried, or is retried at once.
  test(`a read that fails with ${label} is read again 15 seconds later`, async () => {
    const clock = new FakeClock();
    const controller = new AbortController();
    const counts = { opens: 0 };
    const succeeded = jobIn('succeeded');
    const { at, read } = scriptedReads(clock, [failure, succeeded]);
    const done = startFollowing(
      { clock, open: silentOpen(counts), read },
      controller.signal,
    );
    await clock.advance(0);
    assert.deepEqual(at, [0], 'the first read fails at once');
    await clock.advance(14_999);
    assert.deepEqual(at, [0], 'the retry waits 15 seconds');
    await clock.advance(1);
    assert.deepEqual(at, [0, 15_000], 'the job is read again at 15 seconds');
    assert.equal(await done, succeeded);
    await flush();
    noLeftovers(clock, controller);
  });
}

for (const [label, failure] of FINAL_FAILURES) {
  // Fails when the error is replaced or wrapped, or when it is read again.
  test(`a read that fails with ${label} rejects with that error and is not read again`, async () => {
    const clock = new FakeClock();
    const controller = new AbortController();
    const counts = { opens: 0 };
    const { at, read } = scriptedReads(clock, [failure, jobIn('running')]);
    const done = startFollowing(
      { clock, open: silentOpen(counts), read },
      controller.signal,
    );
    assert.equal(await rejection(done), failure);
    await clock.advance(60_000);
    assert.deepEqual(at, [0], 'the failed read is not repeated');
    await flush();
    noLeftovers(clock, controller);
  });
}

// Fails when the feed is opened before the signal is checked.
test('a signal that has already aborted rejects with its reason before the feed is opened', async () => {
  const clock = new FakeClock();
  const controller = new AbortController();
  const reason = new Error('The caller stopped waiting.');
  controller.abort(reason);
  const counts = { opens: 0 };
  const { at, read } = scriptedReads(clock, [jobIn('running')]);
  const done = startFollowing(
    { clock, open: silentOpen(counts), read },
    controller.signal,
  );
  assert.equal(await rejection(done), reason);
  await clock.advance(60_000);
  assert.equal(counts.opens, 0, 'the feed is not opened');
  assert.deepEqual(at, [], 'the job is not read');
  noLeftovers(clock, controller);
});

// Fails when an abort leaves the poll, the reconnect wait or the abort listener
// behind, or lets the feed reconnect afterwards.
test('aborting while waiting rejects with the signal reason and leaves no timer, listener or reconnect', async () => {
  const clock = new FakeClock();
  const controller = new AbortController();
  const reason = new Error('The caller stopped waiting.');
  const counts = { opens: 0 };
  const { at, read } = scriptedReads(clock, [jobIn('running')]);
  const done = startFollowing(
    { clock, open: refusingOpen(unavailable, counts), read },
    controller.signal,
    { random: () => 0 },
  );
  await clock.advance(5_000);
  assert.deepEqual(at, [], 'no read before the abort');
  const opensAtAbort = counts.opens;
  controller.abort(reason);
  assert.equal(await rejection(done), reason);
  noLeftovers(clock, controller);
  await clock.advance(60_000);
  assert.equal(counts.opens, opensAtAbort, 'no reconnect after the abort');
  assert.deepEqual(at, [], 'no read after the abort');
});
