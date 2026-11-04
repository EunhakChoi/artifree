import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import {
  ArtifreeApiError,
  DATA_LIMITS,
  followData,
  isRetryable,
  listDataMembers,
  serverSentEvents,
} from '../../packages/client/dist/index.js';

// The data client (decision 0040): a byte-level event parser, a validating
// follower with explicit retry rules and abort semantics, and the members
// query, all against a scripted network.

const A = `art_${'0'.repeat(32)}`;
const encoder = new TextEncoder();
const LIMITS = { lineBytes: 8192, dataBytes: 8192, pendingBytes: 16384 };

const bytes = (part) =>
  typeof part === 'string' ? encoder.encode(part) : part;

async function* chunked(parts) {
  for (const part of parts) yield bytes(part);
}

async function collect(parts, limits = LIMITS) {
  const events = [];
  for await (const event of serverSentEvents(chunked(parts), limits))
    events.push(event);
  return events;
}

async function invalid(parts, limits = LIMITS) {
  await assert.rejects(
    collect(parts, limits),
    (error) =>
      error instanceof ArtifreeApiError && error.code === 'invalid_response',
  );
}

/** Every way of cutting `text` into two or three chunks. */
function splits(text) {
  const data = encoder.encode(text);
  const cuts = [];
  for (let i = 0; i <= data.length; i += 1) {
    cuts.push([data.subarray(0, i), data.subarray(i)]);
    for (let j = i; j <= data.length; j += 7)
      cuts.push([data.subarray(0, i), data.subarray(i, j), data.subarray(j)]);
  }
  return cuts;
}

// Parser --------------------------------------------------------------------

const SPEC_TABLE = [
  ['LF', 'event: a\ndata: x\n\n', [{ type: 'a', data: 'x' }]],
  ['CRLF', 'event: a\r\ndata: x\r\n\r\n', [{ type: 'a', data: 'x' }]],
  ['CR', 'event: a\rdata: x\r\r', [{ type: 'a', data: 'x' }]],
  [
    'mixed terminators',
    'data: x\r\ndata: y\n\r\n',
    [{ type: 'message', data: 'x\ny' }],
  ],
  [
    'comment lines',
    ': hi\ndata: x\n:more\n\n',
    [{ type: 'message', data: 'x' }],
  ],
  [
    'unknown fields, id and retry',
    'foo: 1\nid: 7\nretry: 9\ndata: x\n\n',
    [{ type: 'message', data: 'x' }],
  ],
  ['data without a space', 'data:x\n\n', [{ type: 'message', data: 'x' }]],
  ['data with one space', 'data: x\n\n', [{ type: 'message', data: 'x' }]],
  [
    'data with two spaces keeps one',
    'data:  x\n\n',
    [{ type: 'message', data: ' x' }],
  ],
  ['data with no colon is empty', 'data\n\n', [{ type: 'message', data: '' }]],
  [
    'empty data lines join with LF',
    'data:\ndata:\n\n',
    [{ type: 'message', data: '\n' }],
  ],
  [
    'an event without data is not dispatched',
    'event: a\n\ndata: x\n\n',
    [{ type: 'message', data: 'x' }],
  ],
  ['blank lines alone are nothing', '\n\n\r\n', []],
  [
    'two events',
    'data: 1\n\ndata: 2\n\n',
    [
      { type: 'message', data: '1' },
      { type: 'message', data: '2' },
    ],
  ],
  [
    'the type resets after an event',
    'event: a\ndata: 1\n\ndata: 2\n\n',
    [
      { type: 'a', data: '1' },
      { type: 'message', data: '2' },
    ],
  ],
  [
    'the last event field wins',
    'event: a\nevent: b\ndata: 1\n\n',
    [{ type: 'b', data: '1' }],
  ],
  [
    'an unfinished final event is never dispatched',
    'data: 1\n\ndata: 2\n',
    [{ type: 'message', data: '1' }],
  ],
  [
    'a final line without terminator is not dispatched',
    'data: 1\n\ndata: 2',
    [{ type: 'message', data: '1' }],
  ],
  [
    'a byte order mark is removed',
    '﻿data: x\n\n',
    [{ type: 'message', data: 'x' }],
  ],
  ['a mark later is data', 'data: ﻿x\n\n', [{ type: 'message', data: '﻿x' }]],
];

for (const [name, text, expected] of SPEC_TABLE) {
  test(`parser: ${name}, whole and cut anywhere`, async () => {
    assert.deepEqual(await collect([text]), expected);
    for (const parts of splits(text))
      assert.deepEqual(await collect(parts), expected);
  });
}

test('parser: a lone CR at the end of a chunk is not a second line ending', async () => {
  assert.deepEqual(await collect(['data: x\r', '\n\r', '\ndata: y\r\r']), [
    { type: 'message', data: 'x' },
    { type: 'message', data: 'y' },
  ]);
});

test('parser: the byte order mark is removed however the bytes are split', async () => {
  const mark = [0xef, 0xbb, 0xbf];
  const rest = encoder.encode('data: x\n\n');
  const expected = [{ type: 'message', data: 'x' }];
  assert.deepEqual(await collect([Uint8Array.from(mark), rest]), expected);
  assert.deepEqual(
    await collect(mark.map((byte) => Uint8Array.of(byte)).concat([rest])),
    expected,
  );
  assert.deepEqual(
    await collect([
      Uint8Array.of(mark[0]),
      Uint8Array.from([mark[1], mark[2], ...rest]),
    ]),
    expected,
  );
  // The mark is not counted: a line exactly at its limit still fits.
  const tight = { lineBytes: 8, dataBytes: 8, pendingBytes: 16 };
  assert.deepEqual(
    await collect(
      [
        Uint8Array.of(0xef),
        Uint8Array.of(0xbb),
        Uint8Array.of(0xbf),
        'data:abc\n\n',
      ],
      tight,
    ),
    [{ type: 'message', data: 'abc' }],
  );
});

test('parser: a started mark that is not one is invalid UTF-8, not silently dropped', async () => {
  await invalid([Uint8Array.of(0xef, 0xbb), 'data: x\n\n']);
  await invalid([Uint8Array.of(0xef, 0xbb)]);
});

test('parser: invalid UTF-8 anywhere is invalid_response', async () => {
  await invalid([
    Uint8Array.of(0x64, 0x61, 0x74, 0x61, 0x3a, 0xff, 0x0a, 0x0a),
  ]);
  await invalid([': ', Uint8Array.of(0xc3, 0x28), '\n']);
  await invalid(['data: ok\n\n', Uint8Array.of(0xff)]);
  // A multibyte sequence cut off by the end of the body.
  await invalid(['data: ok\n\ndata: ', Uint8Array.of(0xc3)]);
  await invalid([Uint8Array.of(0xe2, 0x82)]);
  // A valid sequence cut off by a chunk boundary is fine.
  const euro = encoder.encode('data: €\n\n');
  assert.deepEqual(await collect([euro.subarray(0, 7), euro.subarray(7)]), [
    { type: 'message', data: '€' },
  ]);
});

test('parser: limits count UTF-8 bytes, not characters', async () => {
  const limits = {
    lineBytes: 300_000,
    dataBytes: 300_000,
    pendingBytes: 600_000,
  };
  // 200,000 characters: under the limit as ASCII, over it as two-byte letters.
  const ascii = await collect([`data: ${'e'.repeat(200_000)}\n\n`], limits);
  assert.equal(ascii[0].data.length, 200_000);
  await invalid([`data: ${'é'.repeat(200_000)}\n\n`], limits);
  await invalid([`: ${'é'.repeat(200_000)}\n`], limits);
});

test('parser: one-byte chunks of a large event are linear and bounded', async () => {
  const limits = {
    lineBytes: 600_000,
    dataBytes: 600_000,
    pendingBytes: 1_200_000,
  };
  const body = encoder.encode(`data: ${'é'.repeat(200_000)}\n\n`);
  const started = performance.now();
  const events = await collect(
    Array.from(body, (byte) => Uint8Array.of(byte)),
    limits,
  );
  assert.equal(events[0].data.length, 200_000);
  assert.ok(
    performance.now() - started < 10_000,
    'one-byte chunks must not rescan',
  );
  // And the same body is refused at a smaller byte limit however it is cut.
  const small = {
    lineBytes: 399_999,
    dataBytes: 399_999,
    pendingBytes: 800_000,
  };
  await invalid(
    Array.from(body, (byte) => Uint8Array.of(byte)),
    small,
  );
  await invalid([body], small);
});

test('parser: event data and pending bytes are bounded across lines', async () => {
  const limits = { lineBytes: 120, dataBytes: 100, pendingBytes: 150 };
  const line = `data: ${'x'.repeat(60)}\n`;
  // Two lines are 121 bytes of data: over dataBytes.
  await invalid([line, line, '\n'], limits);
  // An unfinished line on top of held data is over pendingBytes.
  await invalid([line, `data: ${'y'.repeat(95)}`], {
    ...limits,
    dataBytes: 200,
  });
  assert.deepEqual(await collect([`data: ${'x'.repeat(100)}\n\n`], limits), [
    { type: 'message', data: 'x'.repeat(100) },
  ]);
  await invalid([`data: ${'x'.repeat(101)}\n\n`], limits);
});

// Follower -------------------------------------------------------------------

const config = (fetch) => ({
  baseUrl: 'http://api.test',
  accessToken: 'tok',
  fetch,
});

function body(chunks, { error, hold } = {}) {
  return new ReadableStream({
    start(controller) {
      for (const chunk of chunks) controller.enqueue(bytes(chunk));
      // Chunks queued before an error would be dropped, so the failure comes
      // after the reader had a turn.
      if (error !== undefined) setTimeout(() => controller.error(error), 5);
      else if (hold !== true) controller.close();
    },
  });
}

function eventResponse(chunks, options) {
  return new Response(body(chunks, options), {
    status: 200,
    headers: { 'content-type': 'text/event-stream' },
  });
}

function failure(status, code, headers = {}) {
  return new Response(JSON.stringify({ error: { code, message: code } }), {
    status,
    headers,
  });
}

const frame = (type, data) =>
  `event: ${type}\ndata: ${typeof data === 'string' ? data : JSON.stringify(data)}\n\n`;
const app = (version, value, path = '', over = {}) => ({
  artifact_id: A,
  path,
  data_version: version,
  exists: value !== undefined,
  ...(value === undefined ? {} : { value }),
  ...over,
});
const snapshot = (version, value, path) =>
  frame('snapshot', app(version, value, path));
const change = (version, value, path) =>
  frame('change', app(version, value, path));
const tooLarge = (version, over = {}) =>
  frame('too_large', {
    artifact_id: A,
    path: '',
    data_version: version,
    limit_bytes: 131_072,
    ...over,
  });

/**
 * Runs the follower against scripted steps. A step is a Response, a thrown
 * error, or a function of the call. After the last step the run aborts.
 */
async function run(
  steps,
  { path, retry, onEvent, onConnect, onDisconnect, clock, presetAbort } = {},
) {
  const controller = new AbortController();
  const log = {
    events: [],
    calls: [],
    delays: [],
    disconnects: [],
    connects: 0,
  };
  let now = 0;
  const fetch = async (url, init) => {
    log.calls.push(String(url));
    const step = steps[log.calls.length - 1];
    if (step === undefined) {
      controller.abort();
      throw new Error('script exhausted');
    }
    if (log.calls.length === steps.length) queueMicrotask(() => undefined);
    const value =
      typeof step === 'function'
        ? await step({
            init,
            controller,
            advance: (ms) => {
              now += ms;
            },
          })
        : step;
    if (value instanceof Error) throw value;
    return value;
  };
  if (presetAbort === true) controller.abort();
  let result;
  let error;
  try {
    result = await followData(
      config(fetch),
      { artifactId: A, ...(path === undefined ? {} : { path }) },
      {
        signal: controller.signal,
        retry,
        clock: clock ?? (() => now),
        sleep: async (ms, signal) => {
          log.delays.push(ms);
          if (log.delays.length > 20) controller.abort();
          if (signal.aborted) return;
        },
        onEvent: (event) => {
          log.events.push(event);
          onEvent?.(event, controller);
        },
        onConnect: () => {
          log.connects += 1;
          onConnect?.(controller);
        },
        onDisconnect: (failureSeen, retryInMs) => {
          log.disconnects.push([failureSeen?.code, retryInMs]);
          onDisconnect?.(controller);
        },
      },
    );
  } catch (caught) {
    error = caught;
  }
  return { result, error, log, controller };
}

/** A script that ends the run once its steps are used. */
const ending = (...steps) => [
  ...steps,
  async ({ controller }) => {
    controller.abort();
    return new Error('stop');
  },
];

test('follow: snapshot then changes, a clean close backs off, and a reconnect starts with a fresh snapshot', async () => {
  const { result, log } = await run(
    ending(
      eventResponse([snapshot(5, { a: 1 }), change(6, { a: 2 })]),
      eventResponse([snapshot(2, { a: 9 })]),
    ),
  );
  assert.equal(result, 'aborted');
  assert.deepEqual(
    log.events.map((event) => [
      event.type,
      event.data.data_version,
      event.data.value,
    ]),
    [
      ['snapshot', 5, { a: 1 }],
      ['change', 6, { a: 2 }],
      ['snapshot', 2, { a: 9 }],
    ],
  );
  assert.deepEqual(log.delays, [500, 1000]);
  assert.equal(log.connects, 2);
  assert.equal(log.calls[0], `http://api.test/v1/artifacts/${A}/data/events`);
});

test('follow: the path is validated before anything is sent and is sent encoded', async () => {
  for (const path of [
    'a//b',
    '/a',
    'a/',
    'a b',
    'é',
    'a/..',
    'x'.repeat(65),
    Array(17).fill('a').join('/'),
  ]) {
    const { error, log } = await run([], { path });
    assert.equal(error?.code, 'invalid_request', path);
    assert.equal(log.calls.length, 0, path);
  }
  const { log } = await run(
    ending(eventResponse([snapshot(1, 5, 'a/b-c_9')])),
    { path: 'a/b-c_9' },
  );
  assert.equal(
    log.calls[0],
    `http://api.test/v1/artifacts/${A}/data/events?path=a%2Fb-c_9`,
  );
  const bad = await followData(
    config(() => assert.fail('sent')),
    { artifactId: 'nope' },
    { signal: new AbortController().signal, onEvent() {} },
  ).catch((e) => e);
  assert.equal(bad.code, 'invalid_request');
});

/** Hostile numbers the text of an event can hold but JSON.stringify cannot make. */
const deep = (levels, inner = '1') =>
  '['.repeat(levels) + inner + ']'.repeat(levels);

const FAULTS = [
  [
    'an artifact that is not the one asked for',
    frame('snapshot', app(1, {}, '', { artifact_id: `art_${'1'.repeat(32)}` })),
  ],
  [
    'a path that is not the one asked for',
    frame('snapshot', app(1, {}, 'other')),
  ],
  ['a version that is not an integer', frame('snapshot', app(1.5, {}))],
  ['a negative version', frame('snapshot', app(-1, {}))],
  [
    'a version above 2^53 - 1',
    frame(
      'snapshot',
      '{"artifact_id":"' +
        A +
        '","path":"","data_version":9007199254740993,"exists":true,"value":{}}',
    ),
  ],
  ['a version that is a string', frame('snapshot', app('1', {}))],
  ['an unknown field', frame('snapshot', app(1, {}, '', { extra: true }))],
  [
    'exists without a value',
    frame('snapshot', app(1, undefined, '', { exists: true })),
  ],
  [
    'a value where nothing exists',
    frame('snapshot', app(1, undefined, '', { exists: false, value: 1 })),
  ],
  [
    'an exists that is not a boolean',
    frame('snapshot', app(1, {}, '', { exists: 1 })),
  ],
  [
    'a nested number that is not finite',
    frame(
      'snapshot',
      `{"artifact_id":"${A}","path":"","data_version":1,"exists":true,"value":{"a":[1,{"b":1e400}]}}`,
    ),
  ],
  [
    'a nested number beyond 2^53 - 1',
    frame(
      'snapshot',
      `{"artifact_id":"${A}","path":"","data_version":1,"exists":true,"value":{"a":[[9007199254740993]]}}`,
    ),
  ],
  [
    'a value nested too deep',
    frame(
      'snapshot',
      `{"artifact_id":"${A}","path":"","data_version":1,"exists":true,"value":{"a":${deep(32)}}}`,
    ),
  ],
  ['a scalar at the root', frame('snapshot', app(1, 5))],
  ['a change first', change(1, {})],
  ['an unknown event type as the only data is skipped, not a fault', undefined],
  ['data that is not JSON', frame('snapshot', '{nope')],
  ['data that is an array', frame('snapshot', '[1]')],
  ['a second snapshot', snapshot(1, {}) + snapshot(2, {})],
  ['a change with the same version', snapshot(1, {}) + change(1, {})],
  ['a change with an older version', snapshot(4, {}) + change(3, {})],
  ['a too_large with another limit', tooLarge(1, { limit_bytes: 5 })],
  [
    'a too_large with a missing field',
    frame('too_large', { artifact_id: A, path: '', data_version: 1 }),
  ],
  ['a too_large with an unknown field', tooLarge(1, { extra: 1 })],
  ['a too_large that is not newer', snapshot(3, {}) + tooLarge(3)],
  ['an error event that is malformed', frame('error', { oops: 1 })],
];

for (const [name, text] of FAULTS) {
  if (text === undefined) continue;
  test(`follow: ${name} is refused`, async () => {
    const { error, log } = await run([eventResponse([text])]);
    assert.ok(error instanceof ArtifreeApiError, String(error));
    assert.equal(error.code, 'invalid_response');
    assert.equal(log.calls.length, 1, 'a contract fault is not retried');
    for (const event of log.events)
      assert.ok(event.type === 'snapshot' || event.type === 'change');
  });
}

test('follow: an unknown event type is skipped', async () => {
  const { log } = await run(
    ending(eventResponse([frame('hello', '{}'), snapshot(1, {})])),
  );
  assert.deepEqual(
    log.events.map((event) => event.type),
    ['snapshot'],
  );
});

test('follow: values are checked recursively, with the nesting the path leaves room for', async () => {
  const nested = (levels) => JSON.parse(deep(levels));
  // At the root the space is level 1 and the value is the space: 32 levels hold.
  const at32 = JSON.parse(`{"a":${deep(31)}}`);
  const ok = await run(ending(eventResponse([snapshot(1, at32)])));
  assert.equal(ok.log.events.length, 1);
  const over = await run([
    eventResponse([
      frame(
        'snapshot',
        `{"artifact_id":"${A}","path":"","data_version":1,"exists":true,"value":{"a":${deep(32)}}}`,
      ),
    ]),
  ]);
  assert.equal(over.error?.code, 'invalid_response');
  // At a path of two segments the value may be 30 levels deep, not 31.
  const two = await run(
    ending(eventResponse([snapshot(1, nested(30), 'a/b')])),
    { path: 'a/b' },
  );
  assert.equal(two.log.events.length, 1);
  const tooDeep = await run([eventResponse([snapshot(1, nested(31), 'a/b')])], {
    path: 'a/b',
  });
  assert.equal(tooDeep.error?.code, 'invalid_response');
  // A depth far beyond the cap is refused without exhausting the stack.
  const hostile = await run(
    [
      eventResponse([
        frame(
          'snapshot',
          `{"artifact_id":"${A}","path":"n","data_version":1,"exists":true,"value":${deep(5000)}}`,
        ),
      ]),
    ],
    { path: 'n' },
  );
  assert.equal(hostile.error?.code, 'invalid_response');
});

test('follow: a __proto__ member is inert', async () => {
  const text = `{"artifact_id":"${A}","path":"","data_version":1,"exists":true,"value":{"__proto__":{"polluted":true}}}`;
  const { log } = await run(ending(eventResponse([frame('snapshot', text)])));
  const value = log.events[0].data.value;
  assert.equal(Object.hasOwn(value, '__proto__'), true);
  assert.equal(Object.getPrototypeOf(value), Object.prototype);
  assert.equal({}.polluted, undefined);
});

test('follow: a stored null is present, a missing value is not', async () => {
  const { log } = await run(
    ending(eventResponse([snapshot(1, null, 'n'), change(2, undefined, 'n')])),
    { path: 'n' },
  );
  assert.deepEqual(
    log.events.map((event) => event.data),
    [app(1, null, 'n'), app(2, undefined, 'n')],
  );
  assert.equal(Object.hasOwn(log.events[0].data, 'value'), true);
  assert.equal(Object.hasOwn(log.events[1].data, 'value'), false);
});

const RETRY_TABLE = [
  ['401', () => failure(401, 'unauthenticated'), false],
  ['403', () => failure(403, 'forbidden'), false],
  ['404', () => failure(404, 'not_found'), false],
  ['400', () => failure(400, 'invalid_request'), false],
  ['422', () => failure(422, 'invalid_request'), false],
  ['500 internal', () => failure(500, 'internal'), false],
  [
    '200 that is not an event stream',
    () =>
      new Response('{}', {
        status: 200,
        headers: { 'content-type': 'application/json' },
      }),
    false,
  ],
  [
    'an error page without a body',
    () => new Response('nope', { status: 502 }),
    true,
  ],
  ['429', () => failure(429, 'rate_limited'), true],
  ['502', () => failure(502, 'unavailable'), true],
  ['503', () => failure(503, 'unavailable'), true],
  ['504', () => failure(504, 'unavailable'), true],
  ['a network error', () => new TypeError('fetch failed'), true],
];

for (const [name, make, retried] of RETRY_TABLE) {
  test(`follow: an open that fails with ${name} is ${retried ? 'retried' : 'fatal'}`, async () => {
    const { error, log } = await run(
      retried
        ? ending(make(), eventResponse([snapshot(1, {})]))
        : [make(), make()],
    );
    if (retried) {
      assert.equal(log.calls.length, 3, 'retried then connected');
      assert.deepEqual(log.delays[0], 500);
    } else {
      assert.ok(error instanceof ArtifreeApiError, String(error));
      assert.equal(log.calls.length, 1);
    }
  });
}

test('follow: isRetryable is the documented rule', () => {
  const make = (status, code) => new ArtifreeApiError('x', { status, code });
  assert.equal(isRetryable(make(0, 'network_error')), true);
  assert.equal(isRetryable(make(429, 'rate_limited')), true);
  assert.equal(isRetryable(make(503, 'unavailable')), true);
  assert.equal(isRetryable(make(500, 'internal')), false);
  assert.equal(isRetryable(make(200, 'internal')), false);
  assert.equal(isRetryable(make(200, 'unavailable')), true);
  assert.equal(isRetryable(make(200, 'invalid_response')), false);
  assert.equal(isRetryable(make(404, 'not_found')), false);
});

test('follow: Retry-After is used within 500 ms and 1 h', async () => {
  const cases = [
    ['0', 500],
    ['1', 1000],
    ['5', 5000],
    ['3600', 3_600_000],
    ['99999', 3_600_000],
  ];
  for (const [header, expected] of cases) {
    const { log } = await run(
      ending(failure(503, 'unavailable', { 'retry-after': header })),
    );
    assert.deepEqual(log.delays, [expected], `Retry-After ${header}`);
  }
});

test('follow: delays double up to the cap and open failures never reset them', async () => {
  const { log } = await run(
    ending(...Array.from({ length: 6 }, () => failure(503, 'unavailable'))),
    { retry: { minMs: 500, maxMs: 4000, stableMs: 0 } },
  );
  assert.deepEqual(log.delays, [500, 1000, 2000, 4000, 4000, 4000]);
});

test('follow: the delay starts over only after a connection that delivered an event and stayed up', async () => {
  // The clock moves while the connection is open, so the uptime is measured
  // from the moment it opened.
  const session =
    (events, uptime) =>
    async ({ advance }) =>
      new Response(
        new ReadableStream({
          async start(controller) {
            for (const event of events) controller.enqueue(bytes(event));
            await new Promise((resolve) => setTimeout(resolve, 5));
            advance(uptime);
            controller.close();
          },
        }),
        { status: 200, headers: { 'content-type': 'text/event-stream' } },
      );
  const options = { retry: { minMs: 500, maxMs: 30_000, stableMs: 10_000 } };
  // Stable: opened, one event, up for the whole time.
  const stable = await run(
    ending(
      failure(503, 'unavailable'),
      failure(503, 'unavailable'),
      session([snapshot(1, {})], 10_000),
      session([snapshot(1, {})], 1),
    ),
    options,
  );
  // The first two doubled; the stable close starts over; the short one doubles again.
  assert.deepEqual(stable.log.delays, [500, 1000, 500, 1000]);
  // Up for long but without an event: not stable.
  const silent = await run(
    ending(session([], 60_000), session([], 60_000)),
    options,
  );
  assert.deepEqual(silent.log.delays, [500, 1000]);
  // An event but not for long enough: not stable.
  const brief = await run(
    ending(
      session([snapshot(1, {})], 9_999),
      session([snapshot(1, {})], 9_999),
    ),
    options,
  );
  assert.deepEqual(brief.log.delays, [500, 1000]);
});

test('follow: too_large is terminal whatever follows it', async () => {
  const trailing = [
    [
      'trailing events',
      eventResponse([
        snapshot(1, {}),
        tooLarge(2),
        snapshot(3, {}),
        change(4, {}),
      ]),
    ],
    [
      'a lost connection',
      eventResponse([tooLarge(1)], { error: new Error('reset') }),
    ],
    [
      'garbage after it',
      eventResponse([tooLarge(1), 'event: snapshot\ndata: {nope\n\n']),
    ],
    ['a clean close', eventResponse([snapshot(1, {}), tooLarge(2)])],
  ];
  for (const [name, response] of trailing) {
    const { result, error, log } = await run([
      response,
      eventResponse([snapshot(9, {})]),
    ]);
    assert.equal(error, undefined, name);
    assert.equal(result, 'too_large', name);
    assert.equal(log.calls.length, 1, `${name}: no reconnect`);
    assert.deepEqual(log.delays, [], `${name}: no backoff`);
    assert.equal(log.events.at(-1).type, 'too_large', name);
    assert.ok(log.events.length <= 2, `${name}: nothing after it`);
  }
});

// Abort --------------------------------------------------------------------------

/** Fails the test if a rejection goes unhandled while `work` runs. */
async function withoutUnhandledRejections(work) {
  const seen = [];
  const listener = (reason) => seen.push(reason);
  process.on('unhandledRejection', listener);
  try {
    await work();
    await new Promise((resolve) => setTimeout(resolve, 20));
  } finally {
    process.off('unhandledRejection', listener);
  }
  assert.deepEqual(seen, []);
}

const CALLBACKS = [
  [
    'onConnect',
    {
      script: () => [
        eventResponse([snapshot(1, {})]),
        eventResponse([snapshot(1, {})]),
      ],
      options: (abort) => ({ onConnect: abort }),
    },
  ],
  [
    'onEvent for a snapshot',
    {
      script: () => [
        eventResponse([snapshot(1, {}), change(2, {})]),
        eventResponse([snapshot(1, {})]),
      ],
      options: (abort) => ({
        onEvent: (event, c) => event.type === 'snapshot' && abort(c),
      }),
    },
  ],
  [
    'onEvent for a change',
    {
      script: () => [
        eventResponse([snapshot(1, {}), change(2, {})]),
        eventResponse([snapshot(1, {})]),
      ],
      options: (abort) => ({
        onEvent: (event, c) => event.type === 'change' && abort(c),
      }),
    },
  ],
  [
    'onEvent for too_large',
    {
      script: () => [
        eventResponse([tooLarge(1)]),
        eventResponse([snapshot(1, {})]),
      ],
      options: (abort) => ({
        onEvent: (event, c) => event.type === 'too_large' && abort(c),
      }),
    },
  ],
  [
    'onDisconnect',
    {
      script: () => [
        eventResponse([snapshot(1, {})]),
        eventResponse([snapshot(1, {})]),
      ],
      options: (abort) => ({ onDisconnect: abort }),
    },
  ],
];

for (const [name, { script, options }] of CALLBACKS) {
  for (const throws of [false, true]) {
    test(`abort: ${name} may abort${throws ? ' and then throw' : ''}, and following resolves with no further requests`, async () => {
      await withoutUnhandledRejections(async () => {
        const abort = (controller) => {
          controller.abort();
          if (throws) throw new Error('after the abort');
        };
        const { result, error, log } = await run(script(), options(abort));
        assert.equal(error, undefined);
        assert.equal(result, 'aborted');
        assert.equal(log.calls.length, 1, 'no request after the abort');
        assert.deepEqual(log.delays, [], 'no wait after the abort');
        if (name === 'onEvent for a snapshot')
          assert.equal(log.events.length, 1, 'nothing after the abort');
      });
    });
  }
}

test('abort: a callback that throws without aborting ends following with that error', async () => {
  const boom = new Error('boom');
  for (const [name, options] of [
    [
      'onEvent',
      {
        onEvent: () => {
          throw boom;
        },
      },
    ],
    [
      'onConnect',
      {
        onConnect: () => {
          throw boom;
        },
      },
    ],
    [
      'onDisconnect',
      {
        onDisconnect: () => {
          throw boom;
        },
      },
    ],
  ]) {
    const { error, log } = await run(
      [eventResponse([snapshot(1, {})]), eventResponse([snapshot(1, {})])],
      options,
    );
    assert.equal(error, boom, name);
    assert.ok(log.calls.length <= 1, `${name}: no reconnect`);
  }
});

test('abort: before the start nothing is requested', async () => {
  await withoutUnhandledRejections(async () => {
    const { result, log } = await run([eventResponse([snapshot(1, {})])], {
      presetAbort: true,
    });
    assert.equal(result, 'aborted');
    assert.deepEqual(log.calls, []);
  });
});

test('abort: during the wait between attempts', async () => {
  await withoutUnhandledRejections(async () => {
    const controller = new AbortController();
    let calls = 0;
    const result = await followData(
      config(async () => {
        calls += 1;
        return failure(503, 'unavailable');
      }),
      { artifactId: A },
      {
        signal: controller.signal,
        onEvent() {},
        sleep: async (_ms, signal) => {
          controller.abort();
          assert.equal(signal.aborted, true);
        },
      },
    );
    assert.equal(result, 'aborted');
    assert.equal(calls, 1);
  });
});

test('abort: while a stream is quiet the body is cancelled and following resolves', async () => {
  await withoutUnhandledRejections(async () => {
    let cancelled = false;
    const quiet = new ReadableStream({
      start(controller) {
        controller.enqueue(bytes(snapshot(1, {})));
      },
      cancel() {
        cancelled = true;
      },
    });
    const { result, log } = await run(
      [
        () =>
          new Response(quiet, {
            status: 200,
            headers: { 'content-type': 'text/event-stream' },
          }),
      ],
      {
        onEvent: (_event, controller) =>
          setTimeout(() => controller.abort(), 5),
      },
    );
    assert.equal(result, 'aborted');
    assert.equal(cancelled, true);
    assert.equal(log.calls.length, 1);
  });
});

test('abort: while the open is pending, even for a fetch that ignores the signal', async () => {
  await withoutUnhandledRejections(async () => {
    const controller = new AbortController();
    let late;
    const pending = new Promise((resolve) => {
      late = resolve;
    });
    const running = followData(
      config(() => pending),
      { artifactId: A },
      { signal: controller.signal, onEvent() {} },
    );
    controller.abort();
    assert.equal(await running, 'aborted');
    let cancelled = false;
    late(
      new Response(
        new ReadableStream({
          cancel() {
            cancelled = true;
          },
        }),
        { status: 200, headers: { 'content-type': 'text/event-stream' } },
      ),
    );
    await new Promise((resolve) => setTimeout(resolve, 10));
    assert.equal(cancelled, true, 'a response that arrives late is cancelled');
  });
});

// Members ---------------------------------------------------------------------------

const page = (over = {}) => ({
  artifact_id: A,
  path: '',
  select: 'names',
  limit: 50,
  data_version: 3,
  exists: true,
  members: [{ name: 'a' }, { name: 'b' }],
  ...over,
});

function pager(answer, calls = []) {
  return config(async (url) => {
    calls.push(String(url));
    return new Response(
      JSON.stringify(typeof answer === 'function' ? answer(url) : answer),
      {
        status: 200,
        headers: { 'content-type': 'application/json' },
      },
    );
  });
}

test('members: a page is sent with its normalized query and parsed', async () => {
  const calls = [];
  const result = await listDataMembers(
    pager(
      page({
        path: 'a/b',
        select: 'values',
        limit: 2,
        after: 'x',
        members: [
          { name: 'y', value: [1, null] },
          { name: 'z', too_large: true },
        ],
        next_after: 'z',
      }),
      calls,
    ),
    A,
    { path: 'a/b', select: 'values', limit: 2, after: 'x' },
  );
  assert.equal(result.next_after, 'z');
  assert.equal(result.members[0].value[0], 1);
  assert.equal(
    calls[0],
    `http://api.test/v1/artifacts/${A}/data/members?path=a%2Fb&select=values&limit=2&after=x`,
  );
  const plain = [];
  await listDataMembers(pager(page(), plain), A);
  assert.equal(
    plain[0],
    `http://api.test/v1/artifacts/${A}/data/members?select=names&limit=50`,
  );
});

test('members: bad parameters are refused before anything is sent', async () => {
  const bad = [
    { path: 'a//b' },
    { path: 'é' },
    { select: 'keys' },
    { limit: 0 },
    { limit: 101 },
    { limit: 1.5 },
    { limit: Number.NaN },
    { after: '' },
    { after: 'é'.repeat(129) },
  ];
  for (const request of bad) {
    const calls = [];
    await assert.rejects(
      listDataMembers(pager(page(), calls), A, request),
      (e) => e.code === 'invalid_request',
      JSON.stringify(request),
    );
    assert.equal(calls.length, 0, JSON.stringify(request));
  }
  await assert.rejects(
    listDataMembers(pager(page()), 'art_x'),
    (e) => e.code === 'invalid_request',
  );
  // 256 bytes exactly is fine.
  await listDataMembers(
    pager(page({ after: 'é'.repeat(128), members: [] })),
    A,
    { after: 'é'.repeat(128) },
  );
});

const MISMATCHES = [
  ['another artifact', { artifact_id: `art_${'1'.repeat(32)}` }],
  ['another path', { path: 'x' }],
  ['another select', { select: 'values' }],
  ['another limit', { limit: 10 }],
  ['an after that was not sent', { after: 'q' }],
  ['a version that is not an integer', { data_version: 'x' }],
  ['a version above 2^53 - 1', { data_version: 2 ** 53 }],
  ['an unknown field', { extra: 1 }],
  ['members out of order', { members: [{ name: 'b' }, { name: 'a' }] }],
  ['a repeated member', { members: [{ name: 'a' }, { name: 'a' }] }],
  [
    'UTF-8 order broken by a code unit order',
    { members: [{ name: '\u{1F600}' }, { name: '～' }] },
  ],
  [
    'more members than the limit',
    { limit: 1, members: [{ name: 'a' }, { name: 'b' }] },
  ],
  ['a value under names', { members: [{ name: 'a', value: 1 }] }],
  ['too_large under names', { members: [{ name: 'a', too_large: true }] }],
  [
    'neither value nor too_large under values',
    { select: 'values', members: [{ name: 'a' }] },
  ],
  [
    'both value and too_large under values',
    { select: 'values', members: [{ name: 'a', value: 1, too_large: true }] },
  ],
  [
    'too_large false',
    { select: 'values', members: [{ name: 'a', too_large: false }] },
  ],
  ['an unknown member field', { members: [{ name: 'a', x: 1 }] }],
  ['an empty name', { members: [{ name: '' }] }],
  ['members of a path that does not exist', { exists: false }],
  ['a cursor that is not the last member', { next_after: 'a' }],
  ['a cursor without members', { members: [], next_after: 'a' }],
  [
    'a nested number that is not finite',
    { select: 'values', members: [{ name: 'a', value: { n: Infinity } }] },
  ],
];

for (const [name, over] of MISMATCHES) {
  test(`members: a response with ${name} is refused`, async () => {
    const request = over.select === 'values' ? { select: 'values' } : {};
    const send = over.limit === 1 ? { ...request, limit: 1 } : request;
    const stub = pager(page(over));
    // Infinity cannot travel as JSON: send the text with the number written out.
    const config2 = name.includes('not finite')
      ? {
          ...stub,
          fetch: async () =>
            new Response(
              `{"artifact_id":"${A}","path":"","select":"values","limit":50,"data_version":3,"exists":true,"members":[{"name":"a","value":{"n":1e400}}]}`,
              { status: 200 },
            ),
        }
      : stub;
    await assert.rejects(
      listDataMembers(config2, A, send),
      (e) => e instanceof ArtifreeApiError && e.code === 'invalid_response',
    );
  });
}

test('members: members are ordered by UTF-8 bytes', async () => {
  // U+FF5E (EF BD 9E) sorts before U+1F600 (F0 9F 98 80) in UTF-8, not in UTF-16.
  const ok = await listDataMembers(
    pager(page({ members: [{ name: '～' }, { name: '\u{1F600}' }] })),
    A,
  );
  assert.deepEqual(
    ok.members.map((m) => m.name),
    ['～', '\u{1F600}'],
  );
});

test('members: a server error is typed and a non-JSON body is invalid_response', async () => {
  await assert.rejects(
    listDataMembers(
      config(async () => failure(404, 'not_found')),
      A,
    ),
    (e) => e.code === 'not_found' && e.status === 404,
  );
  await assert.rejects(
    listDataMembers(
      config(async () => new Response('nope', { status: 200 })),
      A,
    ),
    (e) => e.code === 'invalid_response',
  );
  await assert.rejects(
    listDataMembers(
      config(async () => failure(429, 'rate_limited', { 'retry-after': '5' })),
      A,
    ),
    (e) => e.code === 'rate_limited' && e.retryAfterSeconds === 5,
  );
});

// The shared grammar -------------------------------------------------------------------

function rustConstant(file, name) {
  const source = readFileSync(
    new URL(`../../${file}`, import.meta.url),
    'utf8',
  );
  const match = new RegExp(`pub const ${name}: [a-z0-9]+ = ([0-9_ *]+);`).exec(
    source,
  );
  assert.ok(match, `${name} not found in ${file}`);
  return match[1]
    .split('*')
    .reduce(
      (product, factor) => product * Number(factor.replaceAll('_', '').trim()),
      1,
    );
}

test('the client grammar and bounds are the ones the Rust code states', () => {
  const domain = 'crates/artifree-domain/src/data.rs';
  const contract = 'crates/artifree-contracts/src/api.rs';
  assert.equal(
    DATA_LIMITS.maxPathDepth,
    rustConstant(domain, 'MAX_PATH_DEPTH'),
  );
  assert.equal(
    DATA_LIMITS.maxSegmentBytes,
    rustConstant(domain, 'MAX_SEGMENT_BYTES'),
  );
  assert.equal(DATA_LIMITS.maxNesting, rustConstant(domain, 'MAX_NESTING'));
  assert.equal(
    DATA_LIMITS.maxMemberNameBytes,
    rustConstant(contract, 'DATA_MEMBER_NAME_MAX_BYTES'),
  );
  assert.equal(
    DATA_LIMITS.maxMemberNameBytes,
    rustConstant(domain, 'MAX_MEMBER_NAME_BYTES'),
  );
  assert.equal(
    DATA_LIMITS.maxMembersLimit,
    rustConstant(contract, 'DATA_MEMBERS_MAX_LIMIT'),
  );
  assert.equal(
    DATA_LIMITS.defaultMembersLimit,
    rustConstant(contract, 'DATA_MEMBERS_DEFAULT_LIMIT'),
  );
  assert.equal(
    DATA_LIMITS.memberMaxBytes,
    rustConstant(contract, 'DATA_MEMBER_MAX_BYTES'),
  );
  assert.equal(
    DATA_LIMITS.pageMaxBytes,
    rustConstant(contract, 'DATA_PAGE_MAX_BYTES'),
  );
  assert.equal(
    DATA_LIMITS.eventMaxBytes,
    rustConstant(contract, 'DATA_EVENT_MAX_BYTES'),
  );
});
