import assert from 'node:assert/strict';
import { mock, test } from 'node:test';
import {
  ArtifreeApiError,
  ArtifreeClient,
} from '../../packages/client/dist/index.js';
import { wait } from '../../packages/client/dist/retry.js';
import {
  readBoundedErrorJson,
  readBoundedJson,
} from '../../packages/client/dist/transport.js';

// The transport rules of the TypeScript client (decision 0040 rules 17 to 19):
// bounded reads, the stall limit after headers, and the retry policy. Time and
// randomness are injected; no test sleeps.

const ARTIFACT_ID = `art_${'0'.repeat(32)}`;
const FEED = 'f'.repeat(32);
const cursor = (sequence) => `${FEED}.${sequence}`;
const changeData = (sequence) =>
  JSON.stringify({
    cursor: cursor(sequence),
    artifact_id: ARTIFACT_ID,
    change: 'revised',
    revision: sequence,
    metadata_revision: 1,
    committed_at_ms: 1,
  });
const changeEvent = (sequence) =>
  `event: change\ndata: ${changeData(sequence)}\n\n`;
const encoder = new TextEncoder();

/** Lets promise chains and stream reads settle; no clock time passes. */
async function settle() {
  for (let turn = 0; turn < 8; turn += 1)
    await new Promise((resolve) => setImmediate(resolve));
}

/** A clock that only moves when a test advances it. */
class FakeClock {
  now = 0;
  /** Timers ever set, also those that ran or were cancelled. */
  scheduled = 0;
  #timers = [];
  #sequence = 0;
  after = (milliseconds, run) => {
    this.scheduled += 1;
    const timer = {
      at: this.now + milliseconds,
      order: (this.#sequence += 1),
      run,
    };
    this.#timers.push(timer);
    return () => {
      this.#timers = this.#timers.filter((other) => other !== timer);
    };
  };
  /** Timers still waiting. */
  get active() {
    return this.#timers.length;
  }
  async advance(milliseconds) {
    const end = this.now + milliseconds;
    for (;;) {
      await settle();
      const due = this.#timers
        .filter((timer) => timer.at <= end)
        .sort((a, b) => a.at - b.at || a.order - b.order)[0];
      if (due === undefined) break;
      this.#timers = this.#timers.filter((timer) => timer !== due);
      this.now = due.at;
      due.run();
    }
    this.now = end;
    await settle();
  }
}

/** A response body fed by the test; `state` records what the client did. */
function pushed({ status = 200, headers = {} } = {}) {
  const state = { cancelled: false };
  let controller;
  const body = new ReadableStream({
    start(c) {
      controller = c;
    },
    cancel() {
      state.cancelled = true;
    },
  });
  return {
    state,
    response: new Response(body, {
      status,
      headers: { 'content-type': 'text/event-stream', ...headers },
    }),
    push: (text) => {
      controller.enqueue(
        typeof text === 'string' ? encoder.encode(text) : text,
      );
    },
    end: () => {
      controller.close();
    },
  };
}

/**
 * A body that makes a chunk for each read and ends after `limit` reads, so a
 * client that does not stop at its bound reads all of them.
 */
function pulled(chunk, { limit = 100, status = 200, headers = {} } = {}) {
  const state = { cancelled: false, pulls: 0 };
  const body = new ReadableStream(
    {
      pull(controller) {
        state.pulls += 1;
        controller.enqueue(encoder.encode(chunk));
        if (state.pulls >= limit) controller.close();
      },
      cancel() {
        state.cancelled = true;
      },
    },
    { highWaterMark: 0 },
  );
  return {
    state,
    response: new Response(body, {
      status,
      headers: { 'content-type': 'text/event-stream', ...headers },
    }),
  };
}

function clientWith(respond, options = {}) {
  return new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:7410',
    accessToken: 'token',
    fetch: async (url, init) => respond(url, init),
    ...options,
  });
}

async function rejection(promise) {
  try {
    await promise;
  } catch (error) {
    assert.ok(error instanceof ArtifreeApiError, String(error));
    return error;
  }
  return assert.fail('expected a rejection');
}

async function drain(events) {
  const seen = [];
  for await (const event of events) seen.push(event);
  return seen;
}

// ---- The event-stream parser's bounds (rule 19) ----

/**
 * Reads a stream that stays open after its data, so that only the parser's
 * bound, not the end of the body, can stop it.
 */
async function readOpen(body) {
  const events = await clientWith(() => body.response, {
    clock: new FakeClock(),
  }).openChanges();
  const outcome = reading(events);
  await settle();
  return outcome;
}

test('An endless line stops at 65,536 characters and cancels the body', async () => {
  const body = pulled('x'.repeat(10_000));
  const error = await rejection(
    clientWith(() => body.response)
      .openChanges()
      .then(drain),
  );
  assert.deepEqual([error.status, error.code], [200, 'invalid_response']);
  assert.equal(body.state.cancelled, true);
  assert.ok(body.state.pulls <= 9, `read ${body.state.pulls} chunks`);
});

test('A line of exactly 65,536 characters passes, one more does not', async () => {
  const comment = (units) => `:${'x'.repeat(units - 1)}`;
  const exact = pushed();
  exact.push(`${comment(65_536)}`);
  exact.push(`\n${changeEvent(1)}`);
  exact.end();
  const events = await clientWith(() => exact.response).openChanges();
  assert.equal((await drain(events)).length, 1);

  // Over the bound before its line end arrives.
  const unfinished = pushed();
  unfinished.push(comment(65_537));
  const error = (await readOpen(unfinished)).ended;
  assert.equal(error?.code, 'invalid_response');
  assert.equal(unfinished.state.cancelled, true);

  // Over the bound in a chunk that also holds its line end.
  const finished = pushed();
  finished.push(`${comment(65_537)}\n${changeEvent(1)}`);
  const second = (await readOpen(finished)).ended;
  assert.equal(second?.code, 'invalid_response');
  assert.equal(finished.state.cancelled, true);
});

test('An event of 65,536 characters of type and data passes, one more does not', async () => {
  // Type `ping` (4) and three data lines joined by two LF.
  const event = (last) =>
    `event: ping\n${[30_000, 30_000, last]
      .map((units) => `data: ${'a'.repeat(units)}\n`)
      .join('')}\n`;
  const exact = pushed();
  exact.push(event(5_530) + changeEvent(1));
  exact.end();
  const events = await clientWith(() => exact.response).openChanges();
  assert.equal((await drain(events)).length, 1);

  const over = pushed();
  over.push(event(5_531));
  const error = (await readOpen(over)).ended;
  assert.deepEqual([error?.status, error?.code], [200, 'invalid_response']);
  assert.equal(over.state.cancelled, true);
});

test('The event bound counts one event, not the stream', async () => {
  const big = `event: ping\ndata: ${'a'.repeat(40_000)}\n\n`;
  const body = pushed();
  body.push(big + big + big + changeEvent(1));
  body.end();
  const events = await clientWith(() => body.response).openChanges();
  assert.equal((await drain(events)).length, 1);
});

// ---- Bounded body reads (rule 19) ----

const jsonOf = (bytes) => `"${'a'.repeat(bytes - 2)}"`;

test('A success body of 1,064,960 bytes is read, one more stops at once', async () => {
  const context = {
    clock: new FakeClock(),
    subject: 'The test connection',
    keepCause: true,
  };
  // Each read delivers 100,000 bytes; 30 reads would deliver 3 MB.
  const body = pulled('a'.repeat(100_000), { limit: 30 });
  const error = await rejection(readBoundedJson(body.response, context));
  assert.deepEqual([error.status, error.code], [200, 'invalid_response']);
  assert.equal(body.state.cancelled, true);
  assert.ok(body.state.pulls <= 12, `read ${body.state.pulls} chunks`);

  // The bound itself, in 11 chunks: 10 of 100,000 and one of 64,960.
  const sizes = [...Array(10).fill(100_000), 64_960];
  let index = 0;
  const exact = new Response(
    new ReadableStream(
      {
        pull(controller) {
          const size = sizes[index];
          index += 1;
          controller.enqueue(
            encoder.encode(
              index === 1
                ? `"${'a'.repeat(size - 1)}`
                : index === sizes.length
                  ? `${'a'.repeat(size - 1)}"`
                  : 'a'.repeat(size),
            ),
          );
          if (index === sizes.length) controller.close();
        },
      },
      { highWaterMark: 0 },
    ),
  );
  assert.equal((await readBoundedJson(exact, context)).length, 1_064_958);
});

test('A success body of one chunk is bounded too', async () => {
  const context = {
    clock: new FakeClock(),
    subject: 'The test connection',
    keepCause: true,
  };
  const exact = await readBoundedJson(new Response(jsonOf(1_064_960)), context);
  assert.equal(exact.length, 1_064_958);
  const error = await rejection(
    readBoundedJson(new Response(jsonOf(1_064_961)), context),
  );
  assert.equal(error.code, 'invalid_response');
});

const apiError = (code, bytes) => {
  const empty = JSON.stringify({ error: { code, message: '' } });
  return JSON.stringify({
    error: { code, message: 'a'.repeat(bytes - empty.length) },
  });
};

test('An error body of 65,536 bytes is an API body, one more byte is not', async () => {
  const exact = await rejection(
    clientWith(
      () =>
        new Response(apiError('unavailable', 65_536), {
          status: 503,
          headers: { 'retry-after': '3' },
        }),
    ).openChanges(),
  );
  assert.deepEqual(
    [exact.status, exact.code, exact.retryAfterSeconds],
    [503, 'unavailable', 3],
  );

  // The body is a valid API error, so only its size can make it none.
  const over = await rejection(
    clientWith(
      () =>
        new Response(apiError('unavailable', 65_537), {
          status: 503,
          headers: { 'retry-after': '3' },
        }),
    ).openChanges(),
  );
  assert.deepEqual(
    [over.status, over.code, over.retryAfterSeconds],
    [503, 'invalid_response', 3],
  );
  const limited = await rejection(
    clientWith(
      () =>
        new Response(apiError('unavailable', 65_537), {
          status: 429,
        }),
    ).openChanges(),
  );
  assert.deepEqual([limited.status, limited.code], [429, 'rate_limited']);
});

test('An endless error body stops at the bound with the body cancelled', async () => {
  const body = pulled(' '.repeat(10_000), { status: 502 });
  const error = await rejection(clientWith(() => body.response).openChanges());
  assert.deepEqual([error.status, error.code], [502, 'invalid_response']);
  assert.equal(body.state.cancelled, true);
  assert.ok(body.state.pulls <= 9, `read ${body.state.pulls} chunks`);
});

test('Calls that did not opt in keep reading an error body whole', async () => {
  const error = await rejection(
    clientWith(
      () => new Response(apiError('unavailable', 200_000), { status: 503 }),
    ).getArtifact(ARTIFACT_ID),
  );
  assert.deepEqual([error.status, error.code], [503, 'unavailable']);
});

test('A bounded error read that is not JSON counts as no API body', async () => {
  const context = {
    clock: new FakeClock(),
    subject: 'The test connection',
    keepCause: true,
  };
  assert.equal(
    await readBoundedErrorJson(new Response('<h1>Bad gateway</h1>'), context),
    undefined,
  );
  assert.equal(
    await readBoundedErrorJson(new Response(null), context),
    undefined,
  );
});

// ---- Waits and stalls (rule 18) ----

/** Reads `events` in the background, recording how it ended. */
function reading(events) {
  const outcome = { seen: [], ended: undefined };
  void (async () => {
    try {
      for await (const event of events) outcome.seen.push(event);
      outcome.ended = 'ended';
    } catch (error) {
      outcome.ended = error;
    }
  })();
  return outcome;
}

test('A stream silent for 45 seconds after its headers fails as a network failure', async () => {
  const clock = new FakeClock();
  const body = pushed();
  const events = await clientWith(() => body.response, {
    clock,
  }).openChanges();
  const outcome = reading(events);
  await clock.advance(44_999);
  assert.equal(outcome.ended, undefined);
  assert.equal(body.state.cancelled, false);
  await clock.advance(1);
  assert.ok(outcome.ended instanceof ArtifreeApiError);
  assert.deepEqual(
    [outcome.ended.status, outcome.ended.code],
    [0, 'network_error'],
  );
  assert.equal(body.state.cancelled, true);
  assert.equal(clock.active, 0);
});

test('The stall limit counts from the headers, before the first read', async () => {
  const clock = new FakeClock();
  const body = pushed();
  const events = await clientWith(() => body.response, {
    clock,
  }).openChanges();
  await clock.advance(45_000);
  assert.equal(body.state.cancelled, true);
  const error = await rejection(drain(events));
  assert.equal(error.code, 'network_error');
});

test('A comment every 15 seconds keeps a stream open; silence after it does not', async () => {
  const clock = new FakeClock();
  const body = pushed();
  const events = await clientWith(() => body.response, {
    clock,
  }).openChanges();
  const outcome = reading(events);
  for (let tick = 0; tick < 40; tick += 1) {
    await clock.advance(15_000);
    body.push(': keep-alive\n\n');
  }
  assert.equal(outcome.ended, undefined, 'a comment did not count as a byte');
  body.push(changeEvent(1));
  await clock.advance(44_999);
  assert.equal(outcome.seen.length, 1);
  assert.equal(outcome.ended, undefined);
  await clock.advance(1);
  assert.equal(outcome.ended.code, 'network_error');
});

test('Empty chunks are no bytes: a stream that sends only them still stalls', async () => {
  const clock = new FakeClock();
  const body = pushed();
  const events = await clientWith(() => body.response, {
    clock,
  }).openChanges();
  const outcome = reading(events);
  // An empty chunk every 15 seconds, like a comment but with no byte.
  for (let tick = 0; tick < 2; tick += 1) {
    await clock.advance(15_000);
    body.push(new Uint8Array(0));
  }
  assert.equal(outcome.ended, undefined);
  await clock.advance(15_000);
  assert.equal(outcome.ended?.code, 'network_error');
  assert.equal(body.state.cancelled, true);
});

test('Empty chunks are no bytes: an error body that sends only them still stalls', async () => {
  const clock = new FakeClock();
  const body = pushed({ status: 502 });
  const pending = rejection(
    clientWith(() => body.response, { clock }).openChanges(),
  );
  for (let tick = 0; tick < 2; tick += 1) {
    await clock.advance(15_000);
    body.push(new Uint8Array(0));
  }
  await clock.advance(15_000);
  const error = await pending;
  assert.deepEqual([error.status, error.code], [0, 'network_error']);
  assert.equal(body.state.cancelled, true);
});

test('A stream that ends cleanly leaves no timer running', async () => {
  const clock = new FakeClock();
  const body = pushed();
  const events = await clientWith(() => body.response, {
    clock,
  }).openChanges();
  body.push(changeEvent(1));
  body.end();
  assert.equal((await drain(events)).length, 1);
  assert.equal(clock.active, 0);
});

test('An error body silent for 45 seconds after its headers fails as a network failure', async () => {
  const clock = new FakeClock();
  const body = pushed({ status: 502 });
  const pending = rejection(
    clientWith(() => body.response, { clock }).openChanges(),
  );
  await clock.advance(45_000);
  const error = await pending;
  assert.deepEqual([error.status, error.code], [0, 'network_error']);
  assert.equal(body.state.cancelled, true);
  assert.equal(clock.active, 0);
});

test('A success body stalls 45 seconds after its last byte, not its start', async () => {
  const clock = new FakeClock();
  const context = { clock, subject: 'The test connection', keepCause: true };
  const body = pushed();
  const outcome = { value: undefined, error: undefined };
  const read = readBoundedJson(body.response, context).then(
    (value) => {
      outcome.value = value;
    },
    (error) => {
      outcome.error = error;
    },
  );
  // Longer in total than 45 seconds, but a byte every 40.
  body.push('["a",');
  for (let tick = 0; tick < 5; tick += 1) {
    await clock.advance(40_000);
    body.push('"b",');
  }
  assert.equal(outcome.error, undefined);
  body.push('"c"]');
  body.end();
  await read;
  assert.deepEqual(outcome.value, ['a', 'b', 'b', 'b', 'b', 'b', 'c']);

  const silent = pushed();
  const stalled = rejection(readBoundedJson(silent.response, context));
  silent.push('[1,');
  await clock.advance(45_000);
  assert.equal((await stalled).code, 'network_error');
  assert.equal(silent.state.cancelled, true);
});

test('An abort ends a body read at once, also when the body ignores its cancel', async () => {
  const clock = new FakeClock();
  const controller = new AbortController();
  let cancelled = false;
  // A reader whose pending read never settles by itself.
  const body = {
    getReader: () => ({
      read: () => new Promise(() => {}),
      cancel: async () => {
        cancelled = true;
      },
    }),
  };
  const pending = rejection(
    readBoundedErrorJson(
      { body, status: 502 },
      {
        clock,
        signal: controller.signal,
        subject: 'The test connection',
        keepCause: true,
      },
    ),
  );
  await settle();
  controller.abort();
  const error = await pending;
  assert.equal(error.code, 'network_error');
  assert.equal(cancelled, true);
  assert.equal(clock.active, 0);
});

test('A stall ends a body read although the body ignores its cancel', async () => {
  const clock = new FakeClock();
  const body = {
    getReader: () => ({
      read: () => new Promise(() => {}),
      cancel: async () => {},
    }),
  };
  const pending = rejection(
    readBoundedJson(
      { body, status: 200 },
      { clock, subject: 'The test connection', keepCause: true },
    ),
  );
  await clock.advance(45_000);
  assert.equal((await pending).code, 'network_error');
});

test('A request without headers is held for an hour without any timer', async () => {
  const clock = new FakeClock();
  const controller = new AbortController();
  let aborted = false;
  const client = clientWith(
    (url, init) =>
      new Promise((resolve, reject) => {
        init.signal.addEventListener('abort', () => {
          aborted = true;
          reject(new DOMException('aborted', 'AbortError'));
        });
      }),
    { clock },
  );
  const requests = [
    rejection(client.openChanges({ signal: controller.signal })),
    rejection(client.getArtifact(ARTIFACT_ID, { signal: controller.signal })),
  ];
  await clock.advance(3_600_000);
  assert.equal(clock.scheduled, 0, 'a timer was set before the headers');
  assert.equal(aborted, false);
  controller.abort();
  const errors = await Promise.all(requests);
  assert.equal(aborted, true);
  assert.deepEqual(
    errors.map((error) => error.code),
    ['network_error', 'network_error'],
  );
});

test('Headers that arrive after an hour start the stall limit then', async () => {
  const clock = new FakeClock();
  let answer;
  const client = clientWith(
    () =>
      new Promise((resolve) => {
        answer = resolve;
      }),
    { clock },
  );
  const opening = client.openChanges();
  await clock.advance(3_600_000);
  const body = pushed();
  answer(body.response);
  const outcome = reading(await opening);
  await clock.advance(44_999);
  assert.equal(outcome.ended, undefined);
  await clock.advance(1);
  assert.equal(outcome.ended.code, 'network_error');
});

// ---- followChanges: retry policy (rule 17) ----

/** A stream the server closes `openMs` after accepting it. */
function closing(clock, openMs) {
  const body = pushed();
  clock.after(openMs, () => {
    body.end();
  });
  return body.response;
}

/**
 * Follows with fake time. `respond(index)` answers the index-th connection;
 * `disconnects` lists the delays announced before each wait.
 */
function following(clock, respond, { random = () => 0, ...options } = {}) {
  const controller = new AbortController();
  const requests = [];
  const disconnects = [];
  const client = clientWith(
    async () => {
      requests.push(clock.now);
      return respond(requests.length - 1);
    },
    random === null ? { clock } : { clock, random },
  );
  const state = { settled: undefined };
  const done = client
    .followChanges({
      signal: controller.signal,
      onEvent() {},
      onDisconnect(error, retryInMs) {
        disconnects.push({ code: error?.code, retryInMs });
      },
      ...options,
    })
    .then(
      () => {
        state.settled = 'resolved';
      },
      (error) => {
        state.settled = error;
      },
    );
  return {
    requests,
    disconnects,
    state,
    done,
    delays: () => disconnects.map((disconnect) => disconnect.retryInMs),
    stop: () => {
      controller.abort();
      return done;
    },
  };
}

const cleanEnd = (clock) => () => closing(clock, 0);

test('Delays double from 0.5 s to the 30 s cap, at half with the lowest jitter', async () => {
  const clock = new FakeClock();
  const follow = following(clock, cleanEnd(clock), { random: () => 0 });
  await clock.advance(200_000);
  // Bases of 0.5, 1, 2, 4, 8, 16, and then 30 seconds.
  assert.deepEqual(
    follow.delays().slice(0, 9),
    [250, 500, 1000, 2000, 4000, 8000, 15_000, 15_000, 15_000],
  );
  await follow.stop();
});

test('A jitter near 1 waits just under the base, never over it', async () => {
  const clock = new FakeClock();
  const follow = following(clock, cleanEnd(clock), { random: () => 0.999 });
  await clock.advance(600_000);
  const delays = follow.delays();
  const bases = [500, 1000, 2000, 4000, 8000, 16_000, 30_000, 30_000];
  bases.forEach((base, index) => {
    assert.ok(
      delays[index] < base && delays[index] >= base * 0.9995 - 1,
      `delay ${delays[index]} for base ${base}`,
    );
  });
  await follow.stop();
});

test('The jitter is drawn for every delay, between half and all of the base', async () => {
  const clock = new FakeClock();
  const draws = [0.25, 0.5, 0.75];
  const follow = following(clock, cleanEnd(clock), {
    random: () => draws.shift() ?? 0,
  });
  await clock.advance(10_000);
  assert.deepEqual(follow.delays().slice(0, 4), [
    500 * 0.625,
    1000 * 0.75,
    2000 * 0.875,
    2000,
  ]);
  await follow.stop();
});

test('The configured delays set the base and the cap', async () => {
  const clock = new FakeClock();
  const follow = following(clock, cleanEnd(clock), {
    random: () => 0,
    retryDelayMs: { min: 100, max: 400 },
  });
  await clock.advance(10_000);
  assert.deepEqual(follow.delays().slice(0, 5), [50, 100, 200, 200, 200]);
  await follow.stop();
});

test('Without an injected random source the jitter comes from Math.random', async () => {
  const draw = mock.method(Math, 'random', () => 0.5);
  try {
    const clock = new FakeClock();
    const follow = following(clock, cleanEnd(clock), { random: null });
    await clock.advance(1);
    // Base 500 ms at u = 0.5.
    assert.deepEqual(follow.delays().slice(0, 1), [375]);
    await follow.stop();
  } finally {
    draw.mock.restore();
  }
});

const unavailable = (retryAfter) => () =>
  new Response(apiError('unavailable', 100), {
    status: 503,
    headers: { 'retry-after': `${retryAfter}` },
  });

test('A Retry-After above the delay is waited, below it is not', async () => {
  const clock = new FakeClock();
  // Attempts 0 to 6 end cleanly (delays up to 15 s at the lowest jitter),
  // then 503s: with Retry-After 120 and then 2.
  const follow = following(
    clock,
    (index) =>
      index < 7
        ? closing(clock, 0)
        : index === 7
          ? unavailable(120)()
          : unavailable(2)(),
    { random: () => 0 },
  );
  await clock.advance(200_000);
  const delays = follow.delays();
  assert.deepEqual(delays.slice(5, 7), [8000, 15_000]);
  // Attempt 7: base 30 s, half 15 s, Retry-After 120 s is the floor.
  assert.equal(delays[7], 120_000);
  // Attempt 8: Retry-After 2 s is under the 15 s delay, which stays.
  assert.equal(delays[8], 15_000);
  await follow.stop();
});

test('A Retry-After of 1 second raises only a delay below it', async () => {
  const clock = new FakeClock();
  const follow = following(clock, unavailable(1), { random: () => 0 });
  await clock.advance(10_000);
  assert.deepEqual(follow.delays().slice(0, 3), [1000, 1000, 1000]);
  await follow.stop();
});

test('Retry-After: 999999 waits 3,600 seconds', async () => {
  const clock = new FakeClock();
  const follow = following(clock, unavailable(999_999));
  await clock.advance(1);
  assert.deepEqual(follow.delays(), [3_600_000]);
  await clock.advance(3_599_998);
  assert.equal(follow.requests.length, 1, 'reconnected before the hour');
  await clock.advance(1);
  assert.equal(follow.requests.length, 2);
  assert.equal(follow.requests[1], 3_600_000);
  await follow.stop();
});

test('A stream accepted and dropped every second backs off to the 30 s cap', async () => {
  const clock = new FakeClock();
  const follow = following(clock, () => closing(clock, 1000), {
    random: () => 0,
  });
  await clock.advance(120_000);
  assert.deepEqual(
    follow.delays().slice(0, 12),
    [
      250, 500, 1000, 2000, 4000, 8000, 15_000, 15_000, 15_000, 15_000, 15_000,
      15_000,
    ],
  );
  await follow.stop();
  assert.equal(clock.active, 0);
});

test('A stream open for 30.001 seconds restarts the backoff, 29.999 does not', async () => {
  const run = async (openMs) => {
    const clock = new FakeClock();
    const follow = following(
      clock,
      (index) => closing(clock, index === 4 ? openMs : 1000),
      { random: () => 0 },
    );
    await clock.advance(80_000);
    const delays = follow.delays().slice(0, 5);
    await follow.stop();
    return delays;
  };
  // After four quick drops the fifth stream stays open.
  assert.deepEqual(await run(30_001), [250, 500, 1000, 2000, 250]);
  assert.deepEqual(await run(29_999), [250, 500, 1000, 2000, 4000]);
});

test('The 30 seconds to restart do not follow a smaller configured maximum', async () => {
  const clock = new FakeClock();
  const follow = following(
    clock,
    (index) => closing(clock, index === 4 ? 1500 : 100),
    { random: () => 0, retryDelayMs: { min: 100, max: 1000 } },
  );
  await clock.advance(20_000);
  // The fifth stream outlived max (1 s) but not 30 s: no restart.
  assert.deepEqual(follow.delays().slice(0, 6), [50, 100, 200, 400, 500, 500]);
  await follow.stop();
});

test('A restart counts the time a stream stays open, not its events', async () => {
  const clock = new FakeClock();
  const follow = following(
    clock,
    (index) => {
      const body = pushed();
      // Events arrive, but the stream is dropped after one second.
      body.push(changeEvent(index + 1));
      clock.after(1000, () => {
        body.end();
      });
      return body.response;
    },
    { random: () => 0 },
  );
  await clock.advance(40_000);
  assert.deepEqual(follow.delays().slice(0, 4), [250, 500, 1000, 2000]);
  await follow.stop();
});

test('A stalled stream is dropped and reconnected after the delay', async () => {
  const clock = new FakeClock();
  const follow = following(
    clock,
    (index) => {
      const body = pushed();
      if (index === 0) body.push(': hello\n\n');
      else body.push(changeEvent(1));
      return body.response;
    },
    { random: () => 0 },
  );
  await clock.advance(44_999);
  assert.equal(follow.requests.length, 1);
  await clock.advance(1);
  assert.deepEqual(follow.disconnects, [
    { code: 'network_error', retryInMs: 250 },
  ]);
  await clock.advance(249);
  assert.equal(follow.requests.length, 1);
  await clock.advance(1);
  assert.deepEqual(follow.requests, [0, 45_250]);
  await follow.stop();
  assert.equal(clock.active, 0);
});

test('A stream with a comment every 15 seconds is not reconnected', async () => {
  const clock = new FakeClock();
  const body = pushed();
  const follow = following(clock, () => body.response);
  for (let tick = 0; tick < 20; tick += 1) {
    await clock.advance(15_000);
    body.push(': keep-alive\n\n');
  }
  assert.equal(follow.requests.length, 1);
  assert.deepEqual(follow.disconnects, []);
  await follow.stop();
  assert.equal(body.state.cancelled, true);
});

test('Aborting during a wait resolves at once and stops its timer', async () => {
  const clock = new FakeClock();
  const follow = following(clock, unavailable(3600));
  await clock.advance(1);
  assert.equal(follow.state.settled, undefined);
  assert.equal(clock.active, 1);
  follow.stop();
  await settle();
  assert.equal(follow.state.settled, 'resolved');
  assert.equal(clock.active, 0);
  assert.equal(follow.requests.length, 1);
});

test('A revoked token ends following even after reconnects', async () => {
  const clock = new FakeClock();
  const follow = following(clock, (index) =>
    index < 2
      ? closing(clock, 0)
      : new Response(apiError('unauthenticated', 100), { status: 401 }),
  );
  await clock.advance(10_000);
  assert.ok(follow.state.settled instanceof ArtifreeApiError);
  assert.equal(follow.state.settled.code, 'unauthenticated');
  assert.equal(follow.requests.length, 3);
  assert.equal(clock.active, 0);
});

// ---- The retry wait, whatever the clock does ----

test('A wait ends when its clock runs the callback before returning', async () => {
  const immediate = {
    after: (milliseconds, run) => {
      run();
      return () => {};
    },
  };
  const controller = new AbortController();
  await wait(immediate, 1000, controller.signal);
});

test('A wait cancels its timer once, also when the clock fires after an abort', async () => {
  let fire;
  let cancels = 0;
  const late = {
    after: (milliseconds, run) => {
      fire = run;
      return () => {
        cancels += 1;
      };
    },
  };
  const controller = new AbortController();
  const waiting = wait(late, 1000, controller.signal);
  controller.abort();
  await waiting;
  // A clock that ignores its cancel still runs the callback.
  fire();
  assert.equal(cancels, 1);
});
