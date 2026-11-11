import assert from 'node:assert/strict';
import { test } from 'node:test';
import { ArtifreeClient } from '../../packages/client/dist/index.js';

// Decision 0055, rule 11: followJob opens the change feed before its first
// read, and reads the job again on the changes that can end or change the
// wait. Each test runs the public client against a fake server and clock.

// A wait that never settles fails its test instead of hanging the run.
const HANG_LIMIT_MS = 10_000;
const artifact = `art_${'0'.repeat(32)}`;
const otherArtifact = `art_${'1'.repeat(32)}`;
const jobId = `job_${'a'.repeat(32)}`;
const otherJobId = `job_${'b'.repeat(32)}`;
const cursor = (sequence) => `${'f'.repeat(32)}.${sequence}`;
const position = (sequence) => ({ cursor: cursor(sequence) });
const flush = () => new Promise((resolve) => setImmediate(resolve));
const notFound = {
  status: 404,
  body: { error: { code: 'not_found', message: 'Job not found.' } },
};

function job(state, fields = {}) {
  return {
    id: jobId,
    artifact_id: artifact,
    revision: 1,
    kind: 'verify_revision',
    state,
    attempt: 1,
    max_attempts: 3,
    cancel_requested: false,
    created_at_ms: 1,
    ...fields,
  };
}

function changeEvent(change, sequence, fields = {}) {
  return {
    cursor: cursor(sequence),
    artifact_id: artifact,
    change,
    revision: 1,
    metadata_revision: 1,
    committed_at_ms: 1,
    ...fields,
  };
}

function count(entries, entry) {
  return entries.filter((item) => item === entry).length;
}

function deferred() {
  let resolve;
  const promise = new Promise((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

// One change-feed response body that the test writes into. The client cancels
// it when the wait settles, so a write after that is reported as such.
function feed() {
  let writer;
  let cancelled = false;
  const body = new ReadableStream({
    start(controller) {
      writer = controller;
    },
    cancel() {
      cancelled = true;
    },
  });
  return {
    body,
    send(type, data) {
      assert.equal(
        cancelled,
        false,
        `the wait ended before the test sent ${type}`,
      );
      writer.enqueue(
        new TextEncoder().encode(
          `event: ${type}\ndata: ${JSON.stringify(data)}\n\n`,
        ),
      );
    },
    fail(error) {
      writer.error(error);
    },
  };
}

class FakeClock {
  now = 0;
  timers = new Map();
  next = 0;

  after(callback, milliseconds) {
    const id = this.next++;
    this.timers.set(id, { callback, at: this.now + milliseconds });
    return () => this.timers.delete(id);
  }

  async advance(milliseconds) {
    const target = this.now + milliseconds;
    for (;;) {
      let due;
      for (const [id, timer] of this.timers)
        if (timer.at <= target && (due === undefined || timer.at < due.at))
          due = { id, ...timer };
      if (due === undefined) break;
      this.timers.delete(due.id);
      this.now = due.at;
      due.callback();
      await flush();
    }
    this.now = target;
    await flush();
  }
}

// The k-th feed request gets streams[k] and the k-th job read gets
// answers[k]. `log` names the requests in the order they were made.
function fakeServer(streams, answers) {
  const log = [];
  const firstOpened = deferred();
  return {
    log,
    opened: firstOpened.promise,
    fetch: async (url) => {
      if (url.endsWith('/v1/changes')) {
        const stream = streams[count(log, 'stream')];
        log.push('stream');
        firstOpened.resolve();
        return new Response(stream.body, {
          headers: { 'content-type': 'text/event-stream' },
        });
      }
      if (url.endsWith(`/v1/jobs/${jobId}`)) {
        log.push('read');
        const answer = answers.shift();
        if (answer === undefined) throw new Error('an unexpected job read');
        return Response.json(answer.body, { status: answer.status ?? 200 });
      }
      throw new Error(`an unexpected request for ${url}`);
    },
  };
}

// A test that fails early leaves its wait pending, and its abort then rejects
// the wait with nobody reading it. Marking it handled keeps that from being a
// second report; a test that awaits the wait still sees its outcome.
function startFollowing(fake, clock = new FakeClock()) {
  const controller = new AbortController();
  const api = new ArtifreeClient({
    baseUrl: 'http://localhost',
    accessToken: 'token',
    clock,
    fetch: fake.fetch,
  });
  const finished = api.followJob(jobId, {
    signal: controller.signal,
    random: () => 0,
  });
  finished.catch(() => {});
  return { controller, finished };
}

test(
  'the feed is opened before the job is read, so no read precedes ready',
  { timeout: HANG_LIMIT_MS },
  async () => {
    const source = feed();
    const fake = fakeServer([source], [{ body: job('running') }]);
    const { controller } = startFollowing(fake);
    try {
      await fake.opened;
      await flush();
      assert.deepEqual(fake.log, ['stream'], 'no job read before ready');
      source.send('ready', position(0));
      await flush();
      assert.deepEqual(
        fake.log,
        ['stream', 'read'],
        'ready starts the first read',
      );
    } finally {
      controller.abort();
    }
  },
);

test(
  'a job that ended before its first read resolves with it after one read',
  { timeout: HANG_LIMIT_MS },
  async () => {
    const source = feed();
    const ended = job('succeeded', { started_at_ms: 2, finished_at_ms: 3 });
    const fake = fakeServer([source], [{ body: ended }]);
    const { finished } = startFollowing(fake);
    await fake.opened;
    await flush();
    source.send('ready', position(0));
    assert.deepEqual(await finished, ended);
    assert.equal(count(fake.log, 'read'), 1, 'one read answers the wait');
  },
);

test(
  'a change to another job is not read, and a change to this job is read until it ends',
  { timeout: HANG_LIMIT_MS },
  async () => {
    const source = feed();
    const ended = job('succeeded', { started_at_ms: 2, finished_at_ms: 4 });
    const fake = fakeServer(
      [source],
      [{ body: job('running', { started_at_ms: 2 }) }, { body: ended }],
    );
    const { controller, finished } = startFollowing(fake);
    try {
      await fake.opened;
      await flush();
      source.send('ready', position(0));
      await flush();
      assert.equal(count(fake.log, 'read'), 1, 'ready starts one read');
      source.send(
        'change',
        changeEvent('job_changed', 1, { job_id: otherJobId }),
      );
      await flush();
      assert.equal(
        count(fake.log, 'read'),
        1,
        'a change to another job is not read',
      );
      source.send('change', changeEvent('job_changed', 2, { job_id: jobId }));
      await flush();
      assert.equal(count(fake.log, 'read'), 2, 'a change to this job is read');
      assert.deepEqual(await finished, ended);
    } finally {
      controller.abort();
    }
  },
);

test('a reset reads the job again', { timeout: HANG_LIMIT_MS }, async () => {
  const source = feed();
  const fake = fakeServer(
    [source],
    [{ body: job('running') }, { body: job('succeeded') }],
  );
  const { controller, finished } = startFollowing(fake);
  try {
    await fake.opened;
    await flush();
    source.send('ready', position(0));
    await flush();
    source.send('reset', position(1));
    await flush();
    assert.equal(count(fake.log, 'read'), 2, 'a reset reads the job again');
    assert.deepEqual(await finished, job('succeeded'));
  } finally {
    controller.abort();
  }
});

test(
  'a ready on a reconnected feed reads the job again',
  { timeout: HANG_LIMIT_MS },
  async () => {
    const first = feed();
    const second = feed();
    const fake = fakeServer(
      [first, second],
      [{ body: job('running') }, { body: job('succeeded') }],
    );
    const clock = new FakeClock();
    const { controller, finished } = startFollowing(fake, clock);
    try {
      await fake.opened;
      await flush();
      first.send('ready', position(0));
      await flush();
      assert.equal(count(fake.log, 'read'), 1, 'ready starts one read');
      first.fail(new Error('connection lost'));
      await flush();
      // With random() at 0 the first retry waits half of the 500 ms minimum.
      await clock.advance(250);
      assert.equal(count(fake.log, 'stream'), 2, 'the feed reconnects');
      assert.equal(
        count(fake.log, 'read'),
        1,
        'the reconnection reads nothing before its ready',
      );
      second.send('ready', position(1));
      await flush();
      assert.equal(
        count(fake.log, 'read'),
        2,
        'a ready on the new connection reads the job again',
      );
      assert.deepEqual(await finished, job('succeeded'));
    } finally {
      controller.abort();
    }
  },
);

test(
  "a recovery of the job's artifact reads the job again",
  { timeout: HANG_LIMIT_MS },
  async () => {
    const source = feed();
    const fake = fakeServer(
      [source],
      [{ body: job('running') }, { body: job('succeeded') }],
    );
    const { controller, finished } = startFollowing(fake);
    try {
      await fake.opened;
      await flush();
      source.send('ready', position(0));
      await flush();
      source.send('change', changeEvent('recovered', 1));
      await flush();
      assert.equal(
        count(fake.log, 'read'),
        2,
        'a recovery of the artifact reads the job again',
      );
      assert.deepEqual(await finished, job('succeeded'));
    } finally {
      controller.abort();
    }
  },
);

test(
  'a recovery of another artifact does not read the job',
  { timeout: HANG_LIMIT_MS },
  async () => {
    const source = feed();
    const fake = fakeServer([source], [{ body: job('running') }]);
    const { controller } = startFollowing(fake);
    try {
      await fake.opened;
      await flush();
      source.send('ready', position(0));
      await flush();
      source.send(
        'change',
        changeEvent('recovered', 1, { artifact_id: otherArtifact }),
      );
      await flush();
      assert.equal(
        count(fake.log, 'read'),
        1,
        'a recovery of another artifact is not read',
      );
    } finally {
      controller.abort();
    }
  },
);

for (const kind of ['deleted', 'purged']) {
  test(
    `a ${kind} change of the job's artifact reads the job, and its not-found answer rejects the wait`,
    { timeout: HANG_LIMIT_MS },
    async () => {
      const source = feed();
      const fake = fakeServer([source], [{ body: job('running') }, notFound]);
      const { controller, finished } = startFollowing(fake);
      try {
        await fake.opened;
        await flush();
        source.send('ready', position(0));
        await flush();
        source.send('change', changeEvent(kind, 1));
        await flush();
        assert.equal(
          count(fake.log, 'read'),
          2,
          `a ${kind} change reads the job again`,
        );
        await assert.rejects(finished, { status: 404, code: 'not_found' });
      } finally {
        controller.abort();
      }
    },
  );
}
