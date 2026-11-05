import assert from 'node:assert/strict';
import { getEventListeners } from 'node:events';
import { test } from 'node:test';
import { ArtifreeClient } from '../../packages/client/dist/index.js';
import { serverSentEvents } from '../../packages/client/dist/change-feed.js';
import {
  ERROR_BODY_LIMIT,
  SUCCESS_BODY_LIMIT,
  readErrorJson,
  readSuccessJson,
  reconnectDelayMs,
} from '../../packages/client/dist/transport.js';

// The transport of decision 0040 rules 17 to 19, against a fake server, a
// fake clock, and a fake random source. Every test names the wrong
// implementation it fails; a test that waits for real time is a bug.

const encoder = new TextEncoder();
const flush = () => new Promise((resolve) => setImmediate(resolve));
const hang = () => new Promise(() => {});

/** A clock that only moves when a test says so. */
class FakeClock {
  now = 0;
  #timers = new Map();
  #next = 0;

  /** `immediate` calls back before `after` returns; `late` ignores cancels. */
  constructor({ immediate = false, late = false } = {}) {
    this.immediate = immediate;
    this.late = late;
  }

  after(callback, milliseconds) {
    if (this.immediate) {
      callback();
      return () => {};
    }
    const id = this.#next++;
    this.#timers.set(id, { at: this.now + milliseconds, callback });
    return () => {
      if (!this.late) this.#timers.delete(id);
    };
  }

  get pending() {
    return this.#timers.size;
  }

  /** Milliseconds until the earliest timer. */
  get nextIn() {
    const times = [...this.#timers.values()].map((timer) => timer.at);
    return times.length === 0 ? undefined : Math.min(...times) - this.now;
  }

  async advance(milliseconds) {
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

/** A body the test feeds by hand; `cancelled.count` counts its cancels. */
function pipe({ cancel } = {}) {
  let controller;
  const cancelled = { count: 0 };
  const body = new ReadableStream({
    start(c) {
      controller = c;
    },
    cancel(reason) {
      cancelled.count += 1;
      return cancel?.(reason);
    },
  });
  return {
    body,
    cancelled,
    push(part) {
      controller.enqueue(
        typeof part === 'string' ? encoder.encode(part) : part,
      );
    },
    end() {
      controller.close();
    },
    fail(error) {
      controller.error(error);
    },
  };
}

/** A body of `count` chunks of `size` bytes; `state` shows how far it was read. */
function bulk(count, size, { cancel } = {}) {
  const state = { pulls: 0, cancelled: 0 };
  const body = new ReadableStream({
    pull(controller) {
      if (state.pulls === count) controller.close();
      else {
        state.pulls += 1;
        controller.enqueue(new Uint8Array(size).fill(97));
      }
    },
    cancel() {
      state.cancelled += 1;
      return cancel?.();
    },
  });
  return { body, state };
}

/** A body that sends `chunk` and then nothing, and whose cancel never settles. */
function oneChunkThenSilence(chunk) {
  let sent = false;
  return new ReadableStream({
    pull(controller) {
      if (sent) return hang();
      sent = true;
      controller.enqueue(chunk);
    },
    cancel: hang,
  });
}

/** A body whose read and cancel never settle. */
function hostileBody() {
  return new ReadableStream({ pull: hang, cancel: hang });
}

const eventsResponse = (source, init = {}) =>
  new Response(source.body ?? source, {
    status: 200,
    headers: { 'content-type': 'text/event-stream' },
    ...init,
  });

/** A fetch that is held until the request is aborted, as a real one is. */
const held = (signal) =>
  new Promise((_, reject) => {
    signal.addEventListener(
      'abort',
      () => reject(new DOMException('aborted', 'AbortError')),
      { once: true },
    );
  });

const FEED = 'f'.repeat(32);
const cursor = (sequence) => `${FEED}.${sequence}`;
const changeEvent = (sequence) =>
  `event: change\ndata: ${JSON.stringify({
    cursor: cursor(sequence),
    artifact_id: `art_${'0'.repeat(32)}`,
    change: 'revised',
    revision: sequence,
    metadata_revision: 1,
    committed_at_ms: 1,
  })}\n\n`;

function clientOn(respond, clock) {
  return new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:7410',
    accessToken: 'token',
    clock,
    fetch: async (url, init) => respond(init),
  });
}

/**
 * Follows the feed on `clock`. `respond(n, init)` answers the n-th request.
 * The delays default to 500 ms to 30 s with u = 0, so a delay is half its base.
 */
function drive({
  respond,
  clock,
  random = () => 0,
  retryDelay = { min: 500, max: 30_000 },
  controller = new AbortController(),
  onEvent = () => {},
}) {
  const requests = [];
  const events = [];
  const disconnects = [];
  const client = clientOn((init) => {
    requests.push(init);
    return respond(requests.length, init);
  }, clock);
  const run = {
    controller,
    requests,
    events,
    disconnects,
    connects: 0,
    stop: () => controller.abort(),
    done: client.followChanges({
      signal: controller.signal,
      random,
      retryDelayMs: retryDelay,
      onEvent(event) {
        events.push(event);
        onEvent(event, controller);
      },
      onConnect() {
        run.connects += 1;
      },
      onDisconnect(error, retryInMs) {
        disconnects.push({
          status: error?.status,
          code: error?.code,
          retryAfter: error?.retryAfterSeconds,
          retryInMs,
        });
      },
    }),
  };
  run.done.catch(() => {});
  return run;
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

// --- the event stream parser (rule 19) ------------------------------------

/** Chunks as a source that shows how far it was read and whether it was closed. */
function source(chunks) {
  const state = { consumed: 0, closed: false };
  async function* generate() {
    try {
      for (const chunk of chunks) {
        state.consumed += 1;
        yield chunk;
      }
    } finally {
      state.closed = true;
    }
  }
  return { chunks: generate(), state };
}

async function parsed(chunks) {
  const events = [];
  for await (const event of serverSentEvents(source(chunks).chunks))
    events.push(event);
  return events;
}

const dataLines = (total) => {
  // Lines of 1,000 units, joined by LF, `total` units in all.
  const full = Math.floor(total / 1001);
  const rest = total - full * 1001;
  return [...Array(full).fill('a'.repeat(1000)), 'a'.repeat(rest)];
};

test('The parser stops an endless line at the bound and closes its source', async () => {
  // 2,000 chunks of 1,000 units and no line end: an unbounded parser reads
  // them all.
  const endless = source(Array(2000).fill('x'.repeat(1000)));
  const error = await rejection(
    (async () => {
      for await (const event of serverSentEvents(endless.chunks))
        assert.fail(`event ${event.type}`);
    })(),
  );
  assert.deepEqual([error.status, error.code], [200, 'invalid_response']);
  assert.equal(endless.state.consumed, 66, 'read on past the bound');
  assert.ok(endless.state.closed, 'the source was not closed');
});

test('A data line counts its field name: 65,536 units pass and 65,537 fail', async () => {
  const line = (units) => `data:${'x'.repeat(units - 5)}`;
  assert.deepEqual(await parsed([`${line(65_536)}\n\n`]), [
    { type: 'message', data: 'x'.repeat(65_531) },
  ]);
  // The line end is not counted.
  assert.deepEqual(
    (await parsed([`${line(65_536)}\r\n\r\n`])).map(
      (event) => event.data.length,
    ),
    [65_531],
  );
  // Its value is under the bound, so only a measure of the whole line fails.
  const error = await rejection(parsed([`${line(65_537)}\n\n`]));
  assert.equal(error.code, 'invalid_response');
});

test('A line is measured whole, also when it arrives in pieces', async () => {
  const pieces = (total) => [
    `: ${'x'.repeat(40_000)}`,
    'y'.repeat(total - 40_002),
    '\n',
    'data: ok\n\n',
  ];
  assert.deepEqual(await parsed(pieces(65_536)), [
    { type: 'message', data: 'ok' },
  ]);
  // Past the bound the chunk that crosses it is refused: its line end never
  // arrives, and a source that stops here must not be waited for.
  const last = source(
    [...pieces(65_537).slice(0, 2), 'never read'].map(String),
  );
  const error = await rejection(
    (async () => {
      for await (const event of serverSentEvents(last.chunks))
        assert.fail(`event ${event.type}`);
    })(),
  );
  assert.equal(error.code, 'invalid_response');
  assert.equal(last.state.consumed, 2);
  assert.ok(last.state.closed);
});

test('A chunk that leaves an unfinished line past the bound is refused before it is split', async () => {
  // The complete event in front of the oversized tail is not delivered:
  // nothing of the chunk was split.
  const delivered = [];
  const error = await rejection(
    (async () => {
      for await (const event of serverSentEvents(
        source(['data: first\n\n' + 'x'.repeat(65_537)]).chunks,
      ))
        delivered.push(event);
    })(),
  );
  assert.equal(error.code, 'invalid_response');
  assert.deepEqual(delivered, []);
});

test('An event is its last type plus its data lines and the LFs between them: 65,536 pass and 65,537 fail', async () => {
  const event = (type, lines) =>
    `${type === undefined ? '' : `event: ${type}\n`}${lines.map((line) => `data: ${line}\n`).join('')}\n`;
  // type 4 + data 65,532 = 65,536.
  assert.deepEqual(
    (await parsed([event('ping', dataLines(65_532))])).map(
      (found) => found.data.length,
    ),
    [65_532],
  );
  // One more unit in the data, or the LFs left out of the measure.
  assert.equal(
    (await rejection(parsed([event('ping', dataLines(65_533))]))).code,
    'invalid_response',
  );
  // The type counts: each alone fits, together 65,537.
  const big = (typeUnits, dataUnits) =>
    `event: ${'t'.repeat(typeUnits)}\ndata: ${'d'.repeat(dataUnits)}\n\n`;
  assert.equal((await parsed([big(40_000, 25_536)])).length, 1);
  assert.equal(
    (await rejection(parsed([big(40_000, 25_537)]))).code,
    'invalid_response',
  );
  // Only the last `event` value counts.
  assert.equal(
    (
      await parsed([
        `event: ${'t'.repeat(40_000)}\nevent: ${'u'.repeat(40_000)}\ndata: ${'d'.repeat(25_536)}\n\n`,
      ])
    ).length,
    1,
  );
});

test('Many small data lines of one event stop at the bound with the source closed', async () => {
  const lines = Array(5000).fill('data: ' + 'x'.repeat(100) + '\n');
  const lines100 = source(lines);
  const error = await rejection(
    (async () => {
      for await (const found of serverSentEvents(lines100.chunks))
        assert.fail(`event ${found.type}`);
    })(),
  );
  assert.equal(error.code, 'invalid_response');
  assert.ok(lines100.state.consumed < 700, 'read on past the bound');
  assert.ok(lines100.state.closed);
});

test('The parser reads each chunk once: a long line in one-unit chunks is fast', async () => {
  // Searching the pending text again for each chunk takes quadratic time
  // here (about two billion steps); reading only the new text takes a blink.
  const chunks = Array.from(`: ${'x'.repeat(65_534)}`);
  chunks.push('\n', '\n');
  const started = performance.now();
  assert.deepEqual(await parsed(chunks), []);
  assert.ok(
    performance.now() - started < 1500,
    `took ${performance.now() - started} ms`,
  );
});

test('A line end split between chunks, with empty chunks between, ends one line', async () => {
  assert.deepEqual(
    await parsed([
      'event: a\r',
      '',
      '\ndata: 1\r',
      '\r',
      '\ndata: 2\n\n',
      'data: 3\r\r',
    ]),
    [
      { type: 'a', data: '1' },
      { type: 'message', data: '2' },
      { type: 'message', data: '3' },
    ],
  );
});

// --- bounded body reads (rule 19) -----------------------------------------

const readOptions = (clock, signal) => ({
  clock,
  signal,
  status: 200,
  what: 'test body',
  keepCause: true,
});

/** A response whose body is a JSON string of exactly `bytes` bytes. */
function jsonBodyOf(bytes, chunkSize = 65_536) {
  const text = `"${'a'.repeat(bytes - 2)}"`;
  const encoded = encoder.encode(text);
  let offset = 0;
  const state = { pulls: 0, cancelled: 0 };
  const body = new ReadableStream({
    pull(controller) {
      if (offset >= encoded.length) controller.close();
      else {
        state.pulls += 1;
        controller.enqueue(encoded.slice(offset, offset + chunkSize));
        offset += chunkSize;
      }
    },
    cancel() {
      state.cancelled += 1;
    },
  });
  return { response: new Response(body), state };
}

test('A success body of 1,064,960 bytes is read; 1,064,961 is invalid, and a larger one is cancelled as it arrives', async () => {
  const clock = new FakeClock();
  const within = jsonBodyOf(SUCCESS_BODY_LIMIT);
  const value = await readSuccessJson(within.response, readOptions(clock));
  assert.equal(value.length, SUCCESS_BODY_LIMIT - 2);

  const over = jsonBodyOf(SUCCESS_BODY_LIMIT + 1);
  const error = await rejection(
    readSuccessJson(over.response, readOptions(clock)),
  );
  assert.deepEqual([error.status, error.code], [200, 'invalid_response']);
  // 17 chunks of 64 KiB cross the bound; the body of 8 MiB is not read on.
  const huge = bulk(128, 65_536);
  await rejection(readSuccessJson(new Response(huge.body), readOptions(clock)));
  assert.ok(huge.state.pulls <= 20, `read ${huge.state.pulls} chunks`);
  assert.equal(huge.state.cancelled, 1);
  assert.equal(clock.pending, 0);
});

test('An error body of 65,536 bytes is read; 65,537 counts as no API body, and a larger one is cancelled as it arrives', async () => {
  const clock = new FakeClock();
  const within = jsonBodyOf(ERROR_BODY_LIMIT, 1000);
  assert.equal(
    (await readErrorJson(within.response, readOptions(clock))).length,
    ERROR_BODY_LIMIT - 2,
  );
  const over = jsonBodyOf(ERROR_BODY_LIMIT + 1, 1000);
  assert.equal(
    await readErrorJson(over.response, readOptions(clock)),
    undefined,
  );
  const huge = bulk(128, 1000);
  assert.equal(
    await readErrorJson(new Response(huge.body), readOptions(clock)),
    undefined,
  );
  assert.ok(huge.state.pulls <= 70, `read ${huge.state.pulls} chunks`);
  assert.equal(huge.state.cancelled, 1);
  assert.equal(clock.pending, 0);
});

test('A body that is not JSON, or is empty, is no API body for either read', async () => {
  const clock = new FakeClock();
  for (const read of [readErrorJson, readSuccessJson]) {
    assert.equal(
      await read(new Response('<h1>Bad gateway</h1>'), readOptions(clock)),
      undefined,
    );
    assert.equal(await read(new Response(null), readOptions(clock)), undefined);
  }
});

test('A success body that stalls or is lost is a network failure; an error body is no API body', async () => {
  const clock = new FakeClock();
  const stalled = pipe();
  const reading = rejection(
    readSuccessJson(new Response(stalled.body), readOptions(clock)),
  );
  await clock.advance(45_000);
  const failure = await reading;
  assert.deepEqual([failure.status, failure.code], [200, 'network_error']);
  assert.equal(stalled.cancelled.count, 1);

  const lost = pipe();
  lost.push('"par');
  const losing = rejection(
    readSuccessJson(new Response(lost.body), readOptions(clock)),
  );
  await flush();
  lost.fail(new Error('reset'));
  assert.equal((await losing).code, 'network_error');

  const silent = pipe();
  const answer = readErrorJson(new Response(silent.body), readOptions(clock));
  await clock.advance(45_000);
  assert.equal(await answer, undefined);
  assert.equal(silent.cancelled.count, 1);

  const dropped = pipe();
  const dropping = readErrorJson(
    new Response(dropped.body),
    readOptions(clock),
  );
  await flush();
  dropped.fail(new Error('reset'));
  assert.equal(await dropping, undefined);
  assert.equal(clock.pending, 0);
});

test('Aborting a bounded read rejects with the signal reason and leaves nothing behind', async () => {
  for (const read of [readErrorJson, readSuccessJson]) {
    const clock = new FakeClock();
    const controller = new AbortController();
    const slow = pipe();
    slow.push('"abc');
    const reading = rejection(
      read(new Response(slow.body), readOptions(clock, controller.signal)),
    );
    await flush();
    const reason = new Error('stop');
    controller.abort(reason);
    assert.equal(await reading, reason);
    assert.equal(slow.cancelled.count, 1);
    noLeftovers(clock, controller);
  }
  // Aborted before the read starts.
  const clock = new FakeClock();
  const controller = new AbortController();
  controller.abort();
  const unread = pipe();
  const error = await rejection(
    readErrorJson(
      new Response(unread.body),
      readOptions(clock, controller.signal),
    ),
  );
  assert.equal(error.name, 'AbortError');
  assert.equal(unread.cancelled.count, 1);
  noLeftovers(clock, controller);
});

test('A bounded read is not held back by a body whose read and cancel never settle', async () => {
  const clock = new FakeClock();
  // Past the bound: the cancel never settles, the result still comes.
  const stuck = oneChunkThenSilence(new Uint8Array(ERROR_BODY_LIMIT + 1));
  assert.equal(
    await readErrorJson(new Response(stuck), readOptions(clock)),
    undefined,
  );
  const stuckSuccess = oneChunkThenSilence(
    new Uint8Array(SUCCESS_BODY_LIMIT + 1),
  );
  assert.equal(
    (
      await rejection(
        readSuccessJson(new Response(stuckSuccess), readOptions(clock)),
      )
    ).code,
    'invalid_response',
  );
  // A stall with a read that never settles.
  const reading = readErrorJson(
    new Response(hostileBody()),
    readOptions(clock),
  );
  await clock.advance(45_000);
  assert.equal(await reading, undefined);
  // An abort with a read that never settles.
  const controller = new AbortController();
  const aborted = rejection(
    readSuccessJson(
      new Response(hostileBody()),
      readOptions(clock, controller.signal),
    ),
  );
  await flush();
  controller.abort();
  assert.equal((await aborted).name, 'AbortError');
});

test('An empty chunk is not a byte: it does not restart the stall limit of a bounded read', async () => {
  const clock = new FakeClock();
  const trickle = pipe();
  const reading = readErrorJson(new Response(trickle.body), readOptions(clock));
  await clock.advance(30_000);
  trickle.push(new Uint8Array(0));
  await clock.advance(10_000);
  trickle.push(new Uint8Array(0));
  await clock.advance(4_999);
  trickle.push(new Uint8Array(0));
  await clock.advance(1);
  assert.equal(await reading, undefined);
  assert.equal(trickle.cancelled.count, 1);
});

// --- the stream: waits and stalls (rule 18) -------------------------------

test('45 seconds of silence after the headers drops the stream and reconnects', async () => {
  const clock = new FakeClock();
  const streams = [];
  const run = drive({
    clock,
    respond: () => {
      streams.push(pipe());
      return eventsResponse(streams.at(-1));
    },
  });
  await flush();
  assert.equal(run.requests.length, 1);
  await clock.advance(44_999);
  assert.deepEqual(run.disconnects, []);
  await clock.advance(1);
  assert.deepEqual(run.disconnects, [
    { status: 0, code: 'network_error', retryAfter: undefined, retryInMs: 250 },
  ]);
  assert.equal(streams[0].cancelled.count, 1);
  await clock.advance(249);
  assert.equal(run.requests.length, 1);
  await clock.advance(1);
  assert.equal(run.requests.length, 2);
  run.stop();
  await run.done;
  noLeftovers(clock, run.controller);
});

test('A comment every 15 seconds keeps the stream; silence after the last byte drops it', async () => {
  const clock = new FakeClock();
  const stream = pipe();
  const run = drive({ clock, respond: () => eventsResponse(stream) });
  await flush();
  for (let round = 0; round < 20; round += 1) {
    await clock.advance(15_000);
    stream.push(': keep-alive\n\n');
    await flush();
  }
  assert.deepEqual(run.disconnects, []);
  await clock.advance(44_999);
  assert.deepEqual(run.disconnects, []);
  await clock.advance(1);
  assert.equal(run.disconnects.length, 1);
  assert.equal(run.disconnects[0].code, 'network_error');
  run.stop();
  await run.done;
});

test('An empty chunk is not a byte: the stream stalls 45 seconds after its last real byte', async () => {
  const clock = new FakeClock();
  const stream = pipe();
  const run = drive({ clock, respond: () => eventsResponse(stream) });
  await flush();
  await clock.advance(30_000);
  stream.push(new Uint8Array(0));
  await clock.advance(10_000);
  stream.push(new Uint8Array(0));
  await clock.advance(4_999);
  assert.deepEqual(run.disconnects, []);
  await clock.advance(1);
  assert.equal(run.disconnects.length, 1);
  run.stop();
  await run.done;
});

test('A request with no headers for an hour is held without a timer and ends only by abort', async () => {
  const clock = new FakeClock();
  const run = drive({ clock, respond: (n, init) => held(init.signal) });
  await clock.advance(3_600_000);
  assert.equal(clock.pending, 0, 'a timer runs before the headers');
  assert.equal(run.requests.length, 1);
  assert.deepEqual(run.disconnects, []);
  run.stop();
  await run.done;
  noLeftovers(clock, run.controller);

  const direct = new AbortController();
  const opening = rejection(
    clientOn((init) => held(init.signal), clock).openChanges({
      signal: direct.signal,
    }),
  );
  await clock.advance(3_600_000);
  assert.equal(clock.pending, 0);
  direct.abort();
  assert.equal((await opening).code, 'network_error');
});

test('The stall limit runs from the headers until the first read, not from the first read', async () => {
  const clock = new FakeClock();
  const stream = pipe();
  const controller = new AbortController();
  const events = await clientOn(
    () => eventsResponse(stream),
    clock,
  ).openChanges({ signal: controller.signal });
  await clock.advance(30_000);
  const first = rejection(events.next());
  await clock.advance(14_999);
  assert.equal(clock.pending, 1);
  await clock.advance(1);
  assert.equal((await first).code, 'network_error');
  assert.equal(stream.cancelled.count, 1);
  noLeftovers(clock, controller);
});

test('A consumer that holds an event for longer than 45 seconds is not a stall', async () => {
  const clock = new FakeClock();
  const stream = pipe();
  const controller = new AbortController();
  const events = await clientOn(
    () => eventsResponse(stream),
    clock,
  ).openChanges({ signal: controller.signal });
  stream.push(changeEvent(1));
  const first = await events.next();
  assert.equal(first.value.type, 'change');
  assert.equal(clock.pending, 0, 'the stall timer runs while nobody reads');
  await clock.advance(100_000);
  const second = events.next();
  await flush();
  assert.equal(clock.pending, 1);
  stream.push(changeEvent(2));
  assert.equal((await second).value.change.revision, 2);
  controller.abort();
  assert.equal((await events.next()).done, true);
  noLeftovers(clock, controller);
});

test('A stream nobody iterates releases its timer and listener when the stall limit passes', async () => {
  const clock = new FakeClock();
  const stream = pipe();
  const controller = new AbortController();
  await clientOn(() => eventsResponse(stream), clock).openChanges({
    signal: controller.signal,
  });
  assert.equal(clock.pending, 1);
  assert.equal(getEventListeners(controller.signal, 'abort').length, 1);
  await clock.advance(45_000);
  noLeftovers(clock, controller);
  assert.equal(stream.cancelled.count, 1);
});

// --- error bodies of a stream open (rules 18, 19) -------------------------

test('A 429 whose body stalls is a rate limit without an API body: its Retry-After is the floor', async () => {
  const clock = new FakeClock();
  const body = pipe();
  const run = drive({
    clock,
    respond: (n) =>
      n === 1
        ? new Response(body.body, {
            status: 429,
            headers: { 'retry-after': '120' },
          })
        : eventsResponse(pipe()),
  });
  await flush();
  assert.deepEqual(run.disconnects, [], 'a timer ran before the headers');
  await clock.advance(45_000);
  assert.deepEqual(run.disconnects, [
    { status: 429, code: 'rate_limited', retryAfter: 120, retryInMs: 120_000 },
  ]);
  assert.equal(body.cancelled.count, 1);
  await clock.advance(119_999);
  assert.equal(run.requests.length, 1);
  await clock.advance(1);
  assert.equal(run.requests.length, 2);
  run.stop();
  await run.done;
});

test('A 503 whose body is lost is retried; a 401 and a 404 whose bodies stall reject with their status', async () => {
  const clock = new FakeClock();
  const lost = pipe();
  const retried = drive({
    clock,
    respond: (n) =>
      n === 1
        ? new Response(lost.body, { status: 503 })
        : eventsResponse(pipe()),
  });
  await flush();
  lost.fail(new Error('reset'));
  await flush();
  assert.deepEqual(retried.disconnects, [
    {
      status: 503,
      code: 'invalid_response',
      retryAfter: undefined,
      retryInMs: 250,
    },
  ]);
  await clock.advance(250);
  assert.equal(retried.requests.length, 2);
  retried.stop();
  await retried.done;

  for (const status of [401, 404]) {
    const stalled = pipe();
    const run = drive({
      clock,
      respond: () => new Response(stalled.body, { status }),
    });
    await flush();
    await clock.advance(45_000);
    const error = await rejection(run.done);
    assert.deepEqual([error.status, error.code], [status, 'invalid_response']);
    assert.equal(run.requests.length, 1, `HTTP ${status} was retried`);
    assert.deepEqual(run.disconnects, []);
    assert.equal(stalled.cancelled.count, 1);
  }
});

test('A stalled or lost error body keeps its API body when it arrived whole', async () => {
  const clock = new FakeClock();
  const run = drive({
    clock,
    respond: (n) =>
      n === 1
        ? Response.json(
            { error: { code: 'unavailable', message: 'busy' } },
            { status: 503, headers: { 'retry-after': '7' } },
          )
        : eventsResponse(pipe()),
  });
  await flush();
  assert.deepEqual(run.disconnects, [
    { status: 503, code: 'unavailable', retryAfter: 7, retryInMs: 7000 },
  ]);
  run.stop();
  await run.done;
});

test('An error body past 65,536 bytes is no API body: a 429 is a rate limit with its Retry-After, a 401 stops', async () => {
  const clock = new FakeClock();
  const oversize = (status, headers) => {
    const big = bulk(128, 1000);
    return { response: new Response(big.body, { status, headers }), big };
  };
  const limited = oversize(429, { 'retry-after': '5' });
  const run = drive({
    clock,
    respond: (n) => (n === 1 ? limited.response : eventsResponse(pipe())),
  });
  await flush();
  assert.deepEqual(run.disconnects, [
    { status: 429, code: 'rate_limited', retryAfter: 5, retryInMs: 5000 },
  ]);
  assert.equal(limited.big.state.cancelled, 1);
  assert.ok(limited.big.state.pulls <= 70);
  run.stop();
  await run.done;

  const refused = oversize(401, {});
  const stopped = drive({ clock, respond: () => refused.response });
  const error = await rejection(stopped.done);
  assert.deepEqual([error.status, error.code], [401, 'invalid_response']);
  assert.equal(refused.big.state.cancelled, 1);
});

test('An empty chunk is not a byte: an error body stalls 45 seconds after its last real byte', async () => {
  const clock = new FakeClock();
  const body = pipe();
  const run = drive({
    clock,
    respond: (n) =>
      n === 1
        ? new Response(body.body, {
            status: 429,
            headers: { 'retry-after': '120' },
          })
        : eventsResponse(pipe()),
  });
  await flush();
  await clock.advance(30_000);
  body.push(new Uint8Array(0));
  await clock.advance(10_000);
  body.push(new Uint8Array(0));
  await clock.advance(4_999);
  assert.deepEqual(run.disconnects, []);
  await clock.advance(1);
  assert.equal(run.disconnects.length, 1);
  run.stop();
  await run.done;
});

test('Aborting during an error-body read ends following at once and rejects a direct open with the reason', async () => {
  const clock = new FakeClock();
  const body = pipe();
  const run = drive({
    clock,
    respond: () =>
      new Response(body.body, {
        status: 429,
        headers: { 'retry-after': '120' },
      }),
  });
  await flush();
  assert.equal(clock.pending, 1);
  run.stop();
  await run.done;
  assert.deepEqual(run.disconnects, []);
  assert.equal(body.cancelled.count, 1);
  noLeftovers(clock, run.controller);

  const direct = new AbortController();
  const slow = pipe();
  const opening = rejection(
    clientOn(() => new Response(slow.body, { status: 502 }), clock).openChanges(
      { signal: direct.signal },
    ),
  );
  await flush();
  const reason = new Error('give up');
  direct.abort(reason);
  assert.equal(await opening, reason);
  noLeftovers(clock, direct);
});

test('A client call that does not ask for bounds keeps reading a big error body whole', async () => {
  const message = 'x'.repeat(70_000);
  const client = clientOn(
    () =>
      Response.json({ error: { code: 'not_found', message } }, { status: 404 }),
    new FakeClock(),
  );
  const error = await rejection(client.getArtifact(`art_${'0'.repeat(32)}`));
  assert.deepEqual([error.status, error.code], [404, 'not_found']);
  assert.equal(error.message, message);
});

// --- cancelling is started, never awaited ---------------------------------

test('An event over the bound is reported while the body cancel never settles', async () => {
  const clock = new FakeClock();
  const body = oneChunkThenSilence(encoder.encode(`: ${'x'.repeat(70_000)}`));
  const error = await rejection(
    drive({ clock, respond: () => eventsResponse(body) }).done,
  );
  assert.deepEqual([error.status, error.code], [200, 'invalid_response']);
});

test('An abort, a stall, and a refused content type are reported while read and cancel never settle', async () => {
  const clock = new FakeClock();
  const aborting = drive({
    clock,
    respond: () => eventsResponse(hostileBody()),
  });
  await flush();
  aborting.stop();
  await aborting.done;
  noLeftovers(clock, aborting.controller);

  const stalling = drive({
    clock,
    respond: (n) =>
      n === 1 ? eventsResponse(hostileBody()) : eventsResponse(pipe()),
  });
  await flush();
  await clock.advance(45_000);
  assert.equal(stalling.disconnects[0].code, 'network_error');
  stalling.stop();
  await stalling.done;

  const refused = await rejection(
    clientOn(
      () =>
        new Response(hostileBody(), {
          headers: { 'content-type': 'text/html' },
        }),
      clock,
    ).openChanges(),
  );
  assert.deepEqual([refused.status, refused.code], [200, 'invalid_response']);

  const thrown = new Error('handler failed');
  const stopped = drive({
    clock,
    respond: () => {
      const stream = pipe({ cancel: hang });
      stream.push(changeEvent(1));
      return eventsResponse(stream);
    },
    onEvent() {
      throw thrown;
    },
  });
  assert.equal(await rejection(stopped.done), thrown);
});

test('A normal end is reported at once and a stream that ends is retried', async () => {
  const clock = new FakeClock();
  const first = pipe();
  const run = drive({
    clock,
    respond: (n) => (n === 1 ? eventsResponse(first) : eventsResponse(pipe())),
  });
  await flush();
  first.push(changeEvent(1));
  first.end();
  await flush();
  assert.deepEqual(run.disconnects, [
    {
      status: undefined,
      code: undefined,
      retryAfter: undefined,
      retryInMs: 250,
    },
  ]);
  assert.equal(run.events.length, 1);
  run.stop();
  await run.done;
});

// --- retry policy (rule 17) ------------------------------------------------

const delay = (parameters) =>
  reconnectDelayMs({
    min: 500,
    max: 30_000,
    attempt: 0,
    random: () => 0,
    retryAfterSeconds: undefined,
    ...parameters,
  });

test('The delay is half its base to just under all of it, doubling from min up to max', () => {
  assert.deepEqual(
    [0, 1, 2, 3, 4, 5, 6, 7, 8].map((attempt) => delay({ attempt })),
    [250, 500, 1000, 2000, 4000, 8000, 15_000, 15_000, 15_000],
  );
  assert.equal(delay({ random: () => 0.999 }), 499.75);
  assert.equal(delay({ attempt: 6, random: () => 0.9999999 }) < 30_000, true);
  assert.equal(delay({ attempt: 6, random: () => 0.9999999 }) > 29_999, true);
  assert.equal(delay({ attempt: 6, random: () => 0.5 }), 22_500);
  // A very long outage and a zero minimum stay numbers.
  assert.equal(delay({ attempt: 5000 }), 15_000);
  assert.equal(delay({ min: 0, attempt: 5000, max: 10 }), 0);
  // A broken random source cannot take the delay outside [base/2, base].
  assert.equal(delay({ random: () => Number.NaN }), 250);
  assert.equal(delay({ random: () => 7 }), 500);
});

test('Retry-After is a floor, clamped to one hour', () => {
  assert.equal(delay({ attempt: 6, retryAfterSeconds: 3 }), 15_000);
  assert.equal(delay({ attempt: 6, retryAfterSeconds: 60 }), 60_000);
  assert.equal(delay({ retryAfterSeconds: 0 }), 250);
  assert.equal(delay({ retryAfterSeconds: 999_999 }), 3_600_000);
  assert.equal(delay({ retryAfterSeconds: 3600 }), 3_600_000);
});

test('Retry-After: 999999 waits 3,600 seconds on the clock, no longer', async () => {
  const clock = new FakeClock();
  const run = drive({
    clock,
    respond: (n) =>
      n === 1
        ? new Response('', {
            status: 429,
            headers: { 'retry-after': '999999' },
          })
        : eventsResponse(pipe()),
  });
  await flush();
  assert.equal(run.disconnects[0].retryInMs, 3_600_000);
  assert.equal(clock.nextIn, 3_600_000);
  await clock.advance(3_599_999);
  assert.equal(run.requests.length, 1);
  await clock.advance(1);
  assert.equal(run.requests.length, 2);
  run.stop();
  await run.done;
});

test('A stream accepted and dropped every second backs off to the cap', async () => {
  const clock = new FakeClock();
  const streams = [];
  const run = drive({
    clock,
    respond: () => {
      streams.push(pipe());
      return eventsResponse(streams.at(-1));
    },
  });
  for (let round = 0; round < 9; round += 1) {
    await flush();
    await clock.advance(1000);
    streams.at(-1).end();
    await flush();
    await clock.advance(run.disconnects.at(-1).retryInMs);
  }
  assert.deepEqual(
    run.disconnects.map((disconnect) => disconnect.retryInMs),
    [250, 500, 1000, 2000, 4000, 8000, 15_000, 15_000, 15_000],
  );
  assert.equal(run.connects, 10);
  run.stop();
  await run.done;
  noLeftovers(clock, run.controller);
});

test('A connection open for 30 seconds resets the count; one open 29.999 seconds does not', async () => {
  for (const [openFor, expected] of [
    [30_000, 250],
    [29_999, 1000],
  ]) {
    const clock = new FakeClock();
    const streams = [];
    const run = drive({
      clock,
      respond: () => {
        streams.push(pipe());
        return eventsResponse(streams.at(-1));
      },
    });
    // Two quick drops: the count is 2.
    for (const alive of [1000, 1000, openFor]) {
      await flush();
      await clock.advance(alive);
      // Keeps the connection from stalling meanwhile.
      streams.at(-1).end();
      await flush();
      await clock.advance(run.disconnects.at(-1).retryInMs);
    }
    assert.deepEqual(
      run.disconnects.map((disconnect) => disconnect.retryInMs),
      [250, 500, expected],
      `open ${openFor} ms`,
    );
    run.stop();
    await run.done;
    noLeftovers(clock, run.controller);
  }
});

test('The 30 seconds of a reset do not depend on the configured maximum delay', async () => {
  const clock = new FakeClock();
  const streams = [];
  const run = drive({
    clock,
    retryDelay: { min: 500, max: 2000 },
    respond: () => {
      streams.push(pipe());
      return eventsResponse(streams.at(-1));
    },
  });
  // Open for 5 s each, far past the maximum of 2 s and short of 30 s.
  for (let round = 0; round < 3; round += 1) {
    await flush();
    await clock.advance(5000);
    streams.at(-1).end();
    await flush();
    await clock.advance(run.disconnects.at(-1).retryInMs);
  }
  assert.deepEqual(
    run.disconnects.map((disconnect) => disconnect.retryInMs),
    [250, 500, 1000],
  );
  run.stop();
  await run.done;
});

test('The reset timer starts when the stream is accepted and stops when it fails', async () => {
  const clock = new FakeClock();
  const stream = pipe();
  const run = drive({
    clock,
    respond: (n, init) =>
      n === 1 ? eventsResponse(stream) : held(init.signal),
  });
  await flush();
  // The reset timer and the stall timer.
  assert.equal(clock.pending, 2);
  stream.fail(new Error('reset'));
  await flush();
  // Only the wait before reconnecting.
  assert.equal(clock.pending, 1);
  run.stop();
  await run.done;
  noLeftovers(clock, run.controller);
});

// --- abort and clocks ------------------------------------------------------

test('Abort ends following at once at every point and leaves no timer or listener', async () => {
  const points = {
    'before the call': async () => {
      const clock = new FakeClock();
      const controller = new AbortController();
      controller.abort();
      const run = drive({
        clock,
        controller,
        respond: () => assert.fail('sent'),
      });
      await run.done;
      assert.equal(run.requests.length, 0);
      return { clock, controller };
    },
    'before headers': async () => {
      const clock = new FakeClock();
      const run = drive({ clock, respond: (n, init) => held(init.signal) });
      await flush();
      run.stop();
      await run.done;
      return { clock, controller: run.controller };
    },
    'mid-body': async () => {
      const clock = new FakeClock();
      const body = pipe();
      const run = drive({ clock, respond: () => eventsResponse(body) });
      await flush();
      body.push(changeEvent(1));
      await flush();
      run.stop();
      await run.done;
      assert.equal(body.cancelled.count, 1);
      assert.equal(run.events.length, 1);
      return { clock, controller: run.controller };
    },
    'while waiting to reconnect': async () => {
      const clock = new FakeClock();
      const body = pipe();
      const run = drive({ clock, respond: () => eventsResponse(body) });
      await flush();
      body.end();
      await flush();
      assert.equal(run.disconnects.length, 1);
      run.stop();
      await run.done;
      return { clock, controller: run.controller };
    },
    'during an error-body read': async () => {
      const clock = new FakeClock();
      const body = pipe();
      const run = drive({
        clock,
        respond: () => new Response(body.body, { status: 503 }),
      });
      await flush();
      run.stop();
      await run.done;
      assert.equal(body.cancelled.count, 1);
      return { clock, controller: run.controller };
    },
  };
  for (const [name, scenario] of Object.entries(points)) {
    const { clock, controller } = await scenario();
    assert.equal(clock.pending, 0, `${name}: a timer was left running`);
    assert.equal(
      getEventListeners(controller.signal, 'abort').length,
      0,
      `${name}: an abort listener was left behind`,
    );
  }
});

test('A clock that calls back at once settles the wait exactly once', async () => {
  const clock = new FakeClock({ immediate: true });
  let drops = 0;
  const controller = new AbortController();
  const client = clientOn(() => new Response('', { status: 503 }), clock);
  await client.followChanges({
    signal: controller.signal,
    onEvent() {},
    onDisconnect() {
      drops += 1;
      if (drops === 3) controller.abort();
    },
  });
  assert.equal(drops, 3);
});

test('A clock that calls back late, after the abort, changes nothing', async () => {
  const clock = new FakeClock({ late: true });
  const body = pipe();
  const run = drive({
    clock,
    respond: (n) => (n === 1 ? eventsResponse(body) : assert.fail('resent')),
  });
  await flush();
  body.end();
  await flush();
  assert.equal(run.disconnects.length, 1);
  run.stop();
  await run.done;
  // Every timer, including the cancelled ones, now runs.
  await clock.advance(3_600_000);
  assert.equal(run.requests.length, 1);
  assert.equal(run.disconnects.length, 1);
});

test('A clock that calls back at once stalls a body without reading an unset timer', async () => {
  const clock = new FakeClock({ immediate: true });
  const controller = new AbortController();
  const events = await clientOn(
    () => eventsResponse(pipe()),
    clock,
  ).openChanges({ signal: controller.signal });
  assert.equal((await rejection(events.next())).code, 'network_error');
});

test('A body that was already used is a network failure for a success read and no API body for an error read', async () => {
  const clock = new FakeClock();
  const used = () => {
    const response = new Response('{"a":1}');
    return response.text().then(() => response);
  };
  const error = await rejection(
    readSuccessJson(await used(), readOptions(clock)),
  );
  assert.equal(error.code, 'network_error');
  assert.equal(
    await readErrorJson(await used(), readOptions(clock)),
    undefined,
  );
});
