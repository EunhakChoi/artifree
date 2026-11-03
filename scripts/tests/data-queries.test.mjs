import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, before, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import {
  ArtifreeApiError,
  ArtifreeClient,
  MAX_EVENT_VALUE_BYTES,
  MAX_MEMBER_NAME_BYTES,
  MAX_PATH_SEGMENTS,
  MAX_RETRY_MS,
  MAX_SEGMENT_BYTES,
  MEMBERS_DEFAULT_LIMIT,
  MEMBERS_MAX_LIMIT,
  compareUtf8,
  dataPathProblem,
  followData,
  listDataMembers,
} from '../../packages/client/dist/index.js';
import { serverSentEvents } from '../../packages/client/dist/sse.js';
import { buildServer, startServer } from '../lib/server-process.mjs';

// The data queries and subscriptions of decision 0040 through the TypeScript
// client: the byte-level event parser, every event of the stream against
// every kind of fault, the retry classification and its waits with injected
// timers, abort at every point, the grammar the server and client share, and
// the real server binary.
const root = fileURLToPath(new URL('../../', import.meta.url));
const ARTIFACT = `art_${'0'.repeat(32)}`;
const encoder = new TextEncoder();
const unhandled = [];
process.on('unhandledRejection', (reason) => unhandled.push(reason));
after(() => {
  assert.deepEqual(unhandled, [], 'a promise rejected without a handler');
});

const bytes = (text) => encoder.encode(text);

async function* chunksOf(...pieces) {
  for (const piece of pieces)
    yield typeof piece === 'string' ? bytes(piece) : piece;
}

async function drain(source, limits = {}) {
  const events = [];
  for await (const event of serverSentEvents(source, {
    maxLineBytes: 1_000_000,
    maxEventBytes: 1_000_000,
    ...limits,
  }))
    events.push(event);
  return events;
}

function without(object, key) {
  const copy = { ...object };
  delete copy[key];
  return copy;
}

// ---- the event parser: bytes, not characters ------------------------------

test('the parser reads the three line ends, also split across chunks', async () => {
  const text = 'event: a\r\ndata: 1\r\n\revent: b\rdata: 2\n\n';
  const whole = await drain(chunksOf(text));
  assert.deepEqual(whole, [
    { type: 'a', data: '1' },
    { type: 'b', data: '2' },
  ]);
  const bytewise = await drain(
    chunksOf(...[...bytes(text)].map((b) => Uint8Array.of(b))),
  );
  assert.deepEqual(bytewise, whole);
  // A CR at the end of a chunk and its LF in the next, with an empty chunk
  // between: one line end, not two.
  const split = await drain(chunksOf('data: x\r', new Uint8Array(), '\n\r\n'));
  assert.deepEqual(split, [{ type: 'message', data: 'x' }]);
});

test('the parser follows the standard for fields, comments, and incomplete events', async () => {
  const text = [
    ': comment',
    'data:no space',
    'data:  two spaces',
    'unknown: field',
    'id: 7',
    'retry: 1',
    'data',
    '',
    'event: ignored-without-data',
    '',
    'event: kept',
    'data: tail',
    '', // dispatched
    'data: never ended',
  ].join('\n');
  assert.deepEqual(await drain(chunksOf(text)), [
    { type: 'message', data: 'no space\n two spaces\n' },
    { type: 'kept', data: 'tail' },
  ]);
  assert.deepEqual(await drain(chunksOf('﻿data: bom\n\n')), [
    { type: 'message', data: 'bom' },
  ]);
  // Only the start of the stream has a byte order mark to drop.
  assert.deepEqual(await drain(chunksOf('data: a\n\n﻿data: b\n\n')), [
    { type: 'message', data: 'a' },
  ]);
});

test('a multi-byte character split across chunks is decoded whole', async () => {
  const all = bytes('data: é€😀\n\n');
  const events = await drain(
    chunksOf(all.subarray(0, 7), all.subarray(7, 10), all.subarray(10)),
  );
  assert.deepEqual(events, [{ type: 'message', data: 'é€😀' }]);
});

test('bytes that are not UTF-8 are an invalid response', async () => {
  await assert.rejects(
    drain(chunksOf(Uint8Array.of(0x64, 0x61, 0x74, 0x61, 0x3a, 0xff, 0x0a))),
    (error) => error.code === 'invalid_response',
  );
});

test('a line limit counts bytes: 200,000 two-byte characters pass a character limit and not a byte limit', async () => {
  const line = `data: ${'é'.repeat(200_000)}\n\n`;
  assert.ok(line.length < 300_000 && bytes(line).length > 400_000);
  await assert.rejects(
    drain(chunksOf(line), { maxLineBytes: 300_000, maxEventBytes: 10_000_000 }),
    (error) => error.code === 'invalid_response' && /line/.test(error.message),
  );
  // The same text fits a limit in bytes that is large enough.
  const fits = await drain(chunksOf(line), {
    maxLineBytes: 400_100,
    maxEventBytes: 400_100,
  });
  assert.equal(fits[0].data.length, 200_000);
});

test('an event limit counts the bytes of all its data lines', async () => {
  const lines = `${`data: ${'é'.repeat(10_000)}\n`.repeat(6)}\n`;
  assert.ok(lines.length < 100_000, 'in characters it would fit');
  await assert.rejects(
    drain(chunksOf(lines), { maxLineBytes: 100_000, maxEventBytes: 100_000 }),
    (error) => error.code === 'invalid_response' && /event/.test(error.message),
  );
  // Five such lines (100,005 bytes with their LFs) are over, four are under.
  const four = `${`data: ${'é'.repeat(10_000)}\n`.repeat(4)}\n`;
  assert.equal(
    (
      await drain(chunksOf(four), {
        maxLineBytes: 100_000,
        maxEventBytes: 100_000,
      })
    ).length,
    1,
  );
});

test('an unfinished line is bounded in bytes however small the chunks are', async () => {
  const tiny = [...bytes('é'.repeat(5_000))].map((b) => Uint8Array.of(b));
  await assert.rejects(
    drain(chunksOf(...tiny), { maxLineBytes: 5_000 }),
    (error) => error.code === 'invalid_response',
  );
  // One chunk with no line end at all is refused without waiting for more.
  await assert.rejects(
    drain(chunksOf(new Uint8Array(6_000).fill(0x61)), { maxLineBytes: 5_000 }),
    (error) => error.code === 'invalid_response',
  );
});

test(
  'a long line in one-byte chunks costs linear time',
  { timeout: 30_000 },
  async () => {
    // Rescanning everything received at each chunk would cost about 4.5e10
    // steps here and the test would not end.
    const count = 300_000;
    const prefix = bytes('data: ');
    async function* many() {
      yield prefix;
      for (let index = 0; index < count; index += 1) yield Uint8Array.of(0x61);
      yield bytes('\n\n');
    }
    const events = await drain(many(), {
      maxLineBytes: 400_000,
      maxEventBytes: 400_000,
    });
    assert.equal(events[0].data.length, count);
  },
);

// ---- the grammar the server and client share ---------------------------------

function rustConstant(source, name) {
  const match = new RegExp(`const ${name}: [a-z0-9]+ = ([0-9_ *]+);`).exec(
    source,
  );
  assert.ok(match, `${name} not found`);
  return match[1]
    .split('*')
    .map((factor) => Number(factor.replaceAll('_', '').trim()))
    .reduce((product, factor) => product * factor, 1);
}

test('the client constants equal the Rust constants and the generated schema', async () => {
  const [data, members, events, schemaText] = await Promise.all([
    readFile(join(root, 'crates/artifree-domain/src/data.rs'), 'utf8'),
    readFile(join(root, 'crates/artifree-domain/src/members.rs'), 'utf8'),
    readFile(join(root, 'crates/artifree-server/src/data_events.rs'), 'utf8'),
    readFile(join(root, 'contracts/generated/api-v1.schema.json'), 'utf8'),
  ]);
  assert.equal(MAX_PATH_SEGMENTS, rustConstant(data, 'MAX_PATH_DEPTH'));
  assert.equal(MAX_SEGMENT_BYTES, rustConstant(data, 'MAX_SEGMENT_BYTES'));
  assert.equal(
    MAX_MEMBER_NAME_BYTES,
    rustConstant(data, 'MAX_MEMBER_NAME_BYTES'),
  );
  assert.equal(MEMBERS_DEFAULT_LIMIT, rustConstant(members, 'DEFAULT_LIMIT'));
  assert.equal(MEMBERS_MAX_LIMIT, rustConstant(members, 'MAX_LIMIT'));
  assert.equal(
    MAX_EVENT_VALUE_BYTES,
    rustConstant(events, 'EVENT_VALUE_BYTES'),
  );
  const schema = JSON.parse(schemaText);
  const limit = schema.$defs.ListDataMembersQuery.properties.limit;
  assert.equal(limit.minimum, 1);
  assert.equal(limit.maximum, MEMBERS_MAX_LIMIT);
});

const BAD_PATHS = [
  ['a leading slash', '/a'],
  ['a trailing slash', 'a/'],
  ['a doubled slash', 'a//b'],
  ['only a slash', '/'],
  ['a dot', 'a.b'],
  ['a space', 'a b'],
  ['percent encoding', 'a%2Fb'],
  ['a non-ASCII letter', 'é'],
  ['a 65-character segment', 'a'.repeat(65)],
  ['17 segments', Array(17).fill('s').join('/')],
];

test('the path grammar accepts what the server accepts and nothing else', () => {
  for (const good of [
    '',
    'a',
    'a-b_C9',
    'a'.repeat(64),
    Array(16).fill('s').join('/'),
  ])
    assert.equal(dataPathProblem(good), undefined, JSON.stringify(good));
  for (const [name, bad] of BAD_PATHS)
    assert.notEqual(dataPathProblem(bad), undefined, name);
});

test('UTF-8 byte order is not UTF-16 order', () => {
  assert.ok(compareUtf8('～', '\u{10000}') < 0);
  const [bmp, astral] = ['～', '\u{10000}'];
  assert.ok(bmp > astral, 'JavaScript orders them the other way');
  assert.ok(
    compareUtf8('a', 'b') < 0 &&
      compareUtf8('b', 'a') > 0 &&
      compareUtf8('a', 'a') === 0,
  );
  assert.ok(compareUtf8('a', 'ab') < 0);
});

// ---- a scripted network ------------------------------------------------------

function scripted(handler) {
  const calls = [];
  const client = new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:1',
    accessToken: 'secret',
    // Like a real fetch, a request in flight ends when its signal aborts.
    fetch: (url, init) => {
      calls.push({ url: String(url), init });
      const signal = init?.signal;
      return new Promise((resolve, reject) => {
        const aborted = () => reject(new DOMException('aborted', 'AbortError'));
        if (signal?.aborted) return aborted();
        signal?.addEventListener('abort', aborted, { once: true });
        Promise.resolve()
          .then(() => handler(calls.length, String(url), init))
          .then(resolve, reject);
      });
    },
  });
  return { client, calls };
}

function sse(...events) {
  return events
    .map(
      ({ name, data }) =>
        `event: ${name}\ndata: ${typeof data === 'string' ? data : JSON.stringify(data)}\n\n`,
    )
    .join('');
}

function streamResponse(text, { hold = false, onCancel } = {}) {
  let sent = false;
  const body = new ReadableStream({
    pull(controller) {
      if (!sent) {
        sent = true;
        controller.enqueue(typeof text === 'string' ? bytes(text) : text);
        if (!hold) controller.close();
      }
      return hold ? new Promise(() => {}) : undefined;
    },
    cancel() {
      onCancel?.();
    },
  });
  return new Response(body, {
    status: 200,
    headers: { 'content-type': 'text/event-stream' },
  });
}

function errorResponse(status, headers = {}, code = 'internal') {
  return new Response(
    JSON.stringify({ error: { code, message: `HTTP ${status}` } }),
    {
      status,
      headers: { 'content-type': 'application/json', ...headers },
    },
  );
}

const snap = (version, extra = {}) => ({
  name: 'snapshot',
  data: {
    artifact_id: ARTIFACT,
    path: 'todos',
    data_version: version,
    exists: true,
    value: { n: version },
    ...extra,
  },
});
const change = (version, extra = {}) => ({
  ...snap(version, extra),
  name: 'change',
});
const big = (version, extra = {}) => ({
  name: 'too_large',
  data: {
    artifact_id: ARTIFACT,
    path: 'todos',
    data_version: version,
    max_bytes: 131072,
    ...extra,
  },
});

/** Follows until the script is exhausted: the last response aborts. */
async function follow(responses, options = {}) {
  const controller = new AbortController();
  const delivered = [];
  const waits = [];
  const disconnects = [];
  const { client, calls } = scripted((n) => {
    const next = responses[n - 1];
    if (next === undefined) {
      controller.abort();
      return new Promise(() => {});
    }
    return typeof next === 'function' ? next() : next;
  });
  let outcome;
  try {
    await followData(client, ARTIFACT, {
      path: 'todos',
      signal: controller.signal,
      onEvent: (event) => delivered.push(event),
      onDisconnect: (error, ms) =>
        disconnects.push([error?.code ?? 'ended', error?.status, ms]),
      sleep: async (ms) => {
        waits.push(ms);
      },
      now: () => 0,
      ...options,
    });
    outcome = { resolved: true };
  } catch (error) {
    outcome = { error };
  }
  return { outcome, delivered, waits, disconnects, calls };
}

test('a stream delivers the snapshot and each change in order with the request it should make', async () => {
  const run = await follow([
    streamResponse(
      `: heartbeat\n\n${sse(snap(1), change(2), change(5, { value: null }))}: again\n\n`,
    ),
  ]);
  assert.deepEqual(run.outcome, { resolved: true });
  assert.deepEqual(
    run.delivered.map((e) => [e.type, e.dataVersion, e.exists, e.value]),
    [
      ['snapshot', 1, true, { n: 1 }],
      ['change', 2, true, { n: 2 }],
      ['change', 5, true, null],
    ],
  );
  const [first] = run.calls;
  assert.equal(
    first.url,
    `http://127.0.0.1:1/v1/artifacts/${ARTIFACT}/data/events?path=todos`,
  );
  assert.equal(first.init.headers.accept, 'text/event-stream');
  assert.equal(first.init.headers.authorization, 'Bearer secret');
  assert.equal(first.init.method, 'GET');
});

test('a nothing-there snapshot has no value, and the root is always there', async () => {
  const empty = await follow([
    streamResponse(
      sse({
        name: 'snapshot',
        data: {
          artifact_id: ARTIFACT,
          path: 'todos',
          data_version: 0,
          exists: false,
        },
      }),
    ),
  ]);
  assert.equal(empty.delivered[0].exists, false);
  assert.equal('value' in empty.delivered[0], false);
  const root = await follow(
    [
      streamResponse(
        sse({
          name: 'snapshot',
          data: {
            artifact_id: ARTIFACT,
            path: '',
            data_version: 0,
            exists: true,
            value: {},
          },
        }),
      ),
    ],
    { path: undefined },
  );
  assert.deepEqual(root.delivered[0].value, {});
});

// C2: every event type against every fault. Each is an invalid_response that
// ends following (no second request).
const FAULTS = [
  [
    'another artifact',
    'snapshot',
    (e) => ({ ...e, artifact_id: `art_${'1'.repeat(32)}` }),
  ],
  [
    'another artifact',
    'change',
    (e) => ({ ...e, artifact_id: `art_${'1'.repeat(32)}` }),
  ],
  [
    'another artifact',
    'too_large',
    (e) => ({ ...e, artifact_id: `art_${'1'.repeat(32)}` }),
  ],
  ['another path', 'snapshot', (e) => ({ ...e, path: 'other' })],
  ['another path', 'change', (e) => ({ ...e, path: 'other' })],
  ['another path', 'too_large', (e) => ({ ...e, path: 'other' })],
  ['an extra field', 'snapshot', (e) => ({ ...e, revision: 3 })],
  ['an extra field', 'change', (e) => ({ ...e, revision: 3 })],
  ['an extra field', 'too_large', (e) => ({ ...e, revision: 3 })],
  ['a missing artifact_id', 'snapshot', (e) => without(e, 'artifact_id')],
  ['a missing artifact_id', 'change', (e) => without(e, 'artifact_id')],
  ['a missing artifact_id', 'too_large', (e) => without(e, 'artifact_id')],
  ['a missing data_version', 'snapshot', (e) => without(e, 'data_version')],
  ['a missing data_version', 'change', (e) => without(e, 'data_version')],
  ['a missing data_version', 'too_large', (e) => without(e, 'data_version')],
  ['a string version', 'snapshot', (e) => ({ ...e, data_version: '3' })],
  ['a string version', 'change', (e) => ({ ...e, data_version: '3' })],
  ['a string version', 'too_large', (e) => ({ ...e, data_version: '3' })],
  ['a negative version', 'snapshot', (e) => ({ ...e, data_version: -1 })],
  ['a fractional version', 'change', (e) => ({ ...e, data_version: 5.5 })],
  ['a missing exists', 'snapshot', (e) => without(e, 'exists')],
  ['a missing exists', 'change', (e) => without(e, 'exists')],
  ['a non-boolean exists', 'snapshot', (e) => ({ ...e, exists: 1 })],
  [
    'a value where nothing exists',
    'snapshot',
    (e) => ({ ...e, exists: false }),
  ],
  ['a value where nothing exists', 'change', (e) => ({ ...e, exists: false })],
  ['no value where something exists', 'snapshot', (e) => without(e, 'value')],
  ['no value where something exists', 'change', (e) => without(e, 'value')],
  [
    'a number beyond 2^53 - 1 in the value',
    'snapshot',
    (e) => ({ ...e, value: { n: 1e300 } }),
  ],
  [
    'a number beyond 2^53 - 1 in the value',
    'change',
    (e) => ({ ...e, value: [1, [2e20]] }),
  ],
  [
    'a value nested too deeply',
    'snapshot',
    (e) => ({
      ...e,
      value: Array(40)
        .fill(0)
        .reduce((inner) => [inner], 1),
    }),
  ],
  ['a max_bytes of zero', 'too_large', (e) => ({ ...e, max_bytes: 0 })],
  ['a fractional max_bytes', 'too_large', (e) => ({ ...e, max_bytes: 1.5 })],
  ['a value on a too_large event', 'too_large', (e) => ({ ...e, value: {} })],
  ['a max_bytes on a value event', 'snapshot', (e) => ({ ...e, max_bytes: 1 })],
];

for (const [fault, kind, apply] of FAULTS) {
  test(`${kind} with ${fault} is an invalid response`, async () => {
    const make = { snapshot: snap, change, too_large: big }[kind];
    const faulty = {
      name: kind,
      data: apply(make(kind === 'snapshot' ? 1 : 2).data),
    };
    // A change and a too_large follow a good snapshot; a snapshot is first.
    const events = kind === 'snapshot' ? [faulty] : [snap(1), faulty];
    // An absent-exists snapshot is checked as it is.
    const run = await follow([
      streamResponse(sse(...events)),
      streamResponse(sse(snap(1))),
    ]);
    assert.ok(
      run.outcome.error instanceof ArtifreeApiError,
      `${fault}: ${run.outcome.error}`,
    );
    assert.equal(
      run.outcome.error.code,
      'invalid_response',
      run.outcome.error.message,
    );
    assert.equal(run.outcome.error.status, 200);
    assert.equal(run.calls.length, 1, 'a breach is not retried');
    assert.deepEqual(run.waits, []);
  });
}

test('the root with exists false or a value that is not an object is an invalid response', async () => {
  for (const data of [
    { artifact_id: ARTIFACT, path: '', data_version: 1, exists: false },
    {
      artifact_id: ARTIFACT,
      path: '',
      data_version: 1,
      exists: true,
      value: [1],
    },
    {
      artifact_id: ARTIFACT,
      path: '',
      data_version: 1,
      exists: true,
      value: null,
    },
  ]) {
    const run = await follow(
      [streamResponse(sse({ name: 'snapshot', data }))],
      { path: undefined },
    );
    assert.equal(
      run.outcome.error?.code,
      'invalid_response',
      JSON.stringify(data),
    );
  }
});

const SEQUENCE_FAULTS = [
  ['a snapshot twice', [snap(1), snap(2)]],
  ['a change before the snapshot', [change(2)]],
  ['a repeated version', [snap(3), change(3)]],
  ['a version going back', [snap(3), change(2)]],
  ['a too_large that is not newer', [snap(3), big(3)]],
  ['a snapshot after a change', [snap(1), change(2), snap(3)]],
  ['an unknown event', [{ name: 'heartbeat', data: {} }]],
  ['an event without a name', [{ name: 'message', data: snap(1).data }]],
  ['data that is not JSON', [{ name: 'snapshot', data: '{nope' }]],
  ['data that is not an object', [{ name: 'snapshot', data: '[1]' }]],
];

for (const [name, events] of SEQUENCE_FAULTS) {
  test(`${name} is an invalid response`, async () => {
    const run = await follow([
      streamResponse(sse(...events)),
      streamResponse(sse(snap(1))),
    ]);
    assert.equal(
      run.outcome.error?.code,
      'invalid_response',
      `${run.outcome.error}`,
    );
    assert.equal(run.calls.length, 1);
  });
}

test('a version above 2^53 - 1 is refused by name, never compared', async () => {
  const exact =
    '{"artifact_id":"' +
    ARTIFACT +
    '","path":"todos","data_version":9007199254740993,"exists":false}';
  const run = await follow([
    streamResponse(sse({ name: 'snapshot', data: exact })),
  ]);
  assert.equal(run.outcome.error?.code, 'invalid_response');
  assert.match(run.outcome.error.message, /2\^53/);
  // The largest safe version is accepted.
  const safe =
    '{"artifact_id":"' +
    ARTIFACT +
    '","path":"todos","data_version":9007199254740991,"exists":false}';
  const ok = await follow([
    streamResponse(sse({ name: 'snapshot', data: safe })),
  ]);
  assert.equal(ok.delivered[0].dataVersion, Number.MAX_SAFE_INTEGER);
});

test('a too_large event is permanent whether first or newer, and the stream is not reopened', async () => {
  for (const events of [[big(4)], [snap(1), big(2)]]) {
    const run = await follow([
      streamResponse(sse(...events)),
      streamResponse(sse(snap(9))),
    ]);
    assert.equal(run.outcome.error?.code, 'data_too_large');
    assert.equal(run.calls.length, 1);
  }
});

for (const [name, first, second] of [
  ['the same version', snap(3), change(3)],
  ['a lower version', snap(3), change(2)],
]) {
  test(`${name} within one connection is an invalid response`, async () => {
    const run = await follow([streamResponse(sse(first, second))]);
    assert.equal(run.outcome.error?.code, 'invalid_response');
    assert.match(run.outcome.error.message, /does not exceed/);
    assert.equal(run.calls.length, 1);
  });
}

test('a reconnect may repeat the last version and may not go below it', async () => {
  const same = await follow([
    streamResponse(sse(snap(1), change(4))),
    streamResponse(sse(snap(4))),
  ]);
  assert.deepEqual(
    same.delivered.map((e) => e.dataVersion),
    [1, 4, 4],
  );
  const back = await follow([
    streamResponse(sse(snap(1), change(4))),
    streamResponse(sse(snap(3))),
  ]);
  assert.equal(back.outcome.error?.code, 'invalid_response');
  assert.match(back.outcome.error.message, /below 4/);
});

test('events in one chunk after an abort are not delivered', async () => {
  const controller = new AbortController();
  const delivered = [];
  const { client } = scripted(() =>
    streamResponse(sse(snap(1), change(2), change(3))),
  );
  await followData(client, ARTIFACT, {
    path: 'todos',
    signal: controller.signal,
    onEvent: (event) => {
      delivered.push(event.dataVersion);
      controller.abort();
    },
  });
  assert.deepEqual(delivered, [1]);
});

// ---- retry classification and the waits ---------------------------------------

test('what trying again can fix is retried and the rest rejects', async () => {
  const retried = [
    [
      'a network error',
      () => Promise.reject(new TypeError('fetch failed')),
      'network_error',
    ],
    ['429', () => errorResponse(429, {}, 'rate_limited'), 'rate_limited'],
    ['500', () => errorResponse(500), 'internal'],
    ['503', () => errorResponse(503, {}, 'unavailable'), 'unavailable'],
    [
      'a proxy 502 page',
      () => new Response('<html>', { status: 502 }),
      'invalid_response',
    ],
  ];
  for (const [name, respond, code] of retried) {
    const run = await follow([respond, streamResponse(sse(snap(1)))]);
    assert.deepEqual(run.outcome, { resolved: true }, name);
    assert.equal(run.calls.length >= 2, true, `${name} was not retried`);
    assert.equal(run.disconnects[0][0], code, name);
    assert.equal(run.delivered.length, 1, name);
  }
  const permanent = [
    ['400', () => errorResponse(400, {}, 'invalid_request')],
    ['401', () => errorResponse(401, {}, 'unauthenticated')],
    ['403', () => errorResponse(403, {}, 'forbidden')],
    ['404', () => errorResponse(404, {}, 'not_found')],
    ['422', () => errorResponse(422, {}, 'invalid_request')],
    [
      'a 200 that is not an event stream',
      () =>
        new Response('{}', {
          status: 200,
          headers: { 'content-type': 'application/json' },
        }),
    ],
  ];
  for (const [name, respond] of permanent) {
    const run = await follow([respond, streamResponse(sse(snap(1)))]);
    assert.ok(run.outcome.error instanceof ArtifreeApiError, name);
    assert.equal(run.calls.length, 1, `${name} was retried`);
    assert.deepEqual(run.waits, [], name);
  }
});

test('an error thrown by the handler rejects and is not retried', async () => {
  const boom = new Error('handler');
  const { client, calls } = scripted(() =>
    streamResponse(sse(snap(1), change(2))),
  );
  await assert.rejects(
    followData(client, ARTIFACT, {
      path: 'todos',
      signal: new AbortController().signal,
      onEvent: () => {
        throw boom;
      },
      sleep: async () => {},
    }),
    (error) => error === boom,
  );
  assert.equal(calls.length, 1);
});

test('a stream that ends without an error waits and reconnects, and the waits double', async () => {
  const closed = () => streamResponse('');
  const run = await follow(Array(5).fill(closed), {
    retryDelayMs: { min: 100, max: 1000 },
  });
  assert.deepEqual(run.waits, [100, 200, 400, 800, 1000]);
  assert.deepEqual(
    run.disconnects.map((d) => d[0]),
    Array(5).fill('ended'),
  );
});

test('a connection that stayed up starts the waits over, one that did not does not', async () => {
  let clock = 0;
  const responses = [
    // Short-lived: a snapshot, then the clock moves 1 s and it ends.
    () => ((clock += 1_000), streamResponse(sse(snap(1)))),
    () => ((clock += 1_000), streamResponse(sse(snap(2)))),
    // Long-lived: 20 s.
    () => ((clock += 20_000), streamResponse(sse(snap(3)))),
    () => ((clock += 1_000), streamResponse(sse(snap(4)))),
  ];
  const run = await follow(responses, {
    retryDelayMs: { min: 100, max: 10_000 },
    stableMs: 10_000,
    now: () => clock,
  });
  assert.deepEqual(run.waits, [100, 200, 100, 200]);
});

test('Retry-After is honored, with a floor and an hour as the ceiling', async () => {
  const limited = (seconds) => () =>
    errorResponse(429, { 'retry-after': seconds }, 'rate_limited');
  const run = await follow(
    [limited('0'), limited('3'), limited('999999'), limited('1')],
    { retryDelayMs: { min: 500, max: 30_000 } },
  );
  // 0 s: the floor; 3 s over the doubled backoff of 1 s; a huge delay is
  // cut to an hour; 1 s under the backoff of 4 s keeps the backoff.
  assert.deepEqual(run.waits, [500, 3_000, MAX_RETRY_MS, 4_000]);
  assert.equal(MAX_RETRY_MS, 3_600_000);
});

test('no wait is ever shorter than the floor or longer than an hour', async () => {
  const run = await follow(
    Array(40).fill(() => errorResponse(503, {}, 'unavailable')),
    {
      retryDelayMs: { min: 50, max: 7_200_000 },
    },
  );
  assert.ok(
    run.waits.every((ms) => ms >= 50 && ms <= MAX_RETRY_MS),
    `${run.waits}`,
  );
  assert.equal(Math.max(...run.waits), MAX_RETRY_MS);
});

// ---- abort at every point -------------------------------------------------------

test('an already aborted signal resolves before any request', async () => {
  const controller = new AbortController();
  controller.abort();
  const { client, calls } = scripted(() => {
    throw new Error('requested');
  });
  await followData(client, ARTIFACT, {
    path: 'todos',
    signal: controller.signal,
    onEvent: () => {},
  });
  assert.equal(calls.length, 0);
});

test('an abort during a wait resolves without another request', async () => {
  const controller = new AbortController();
  const { client, calls } = scripted(() =>
    errorResponse(503, {}, 'unavailable'),
  );
  await followData(client, ARTIFACT, {
    path: 'todos',
    signal: controller.signal,
    onEvent: () => {},
    sleep: async () => controller.abort(),
  });
  assert.equal(calls.length, 1);
});

test('an abort during a read resolves and cancels the body', async () => {
  const controller = new AbortController();
  let cancelled = false;
  const { client } = scripted(() =>
    streamResponse(sse(snap(1)), {
      hold: true,
      onCancel: () => (cancelled = true),
    }),
  );
  const delivered = [];
  const done = followData(client, ARTIFACT, {
    path: 'todos',
    signal: controller.signal,
    onEvent: (event) => {
      delivered.push(event);
      queueMicrotask(() => controller.abort());
    },
  });
  await done;
  assert.equal(delivered.length, 1);
  assert.ok(cancelled, 'the body was not cancelled');
});

test('an abort during the request resolves', async () => {
  const controller = new AbortController();
  const { client } = scripted(
    (_n, _url, init) =>
      new Promise((_resolve, reject) => {
        init.signal.addEventListener('abort', () =>
          reject(new DOMException('aborted', 'AbortError')),
        );
        queueMicrotask(() => controller.abort());
      }),
  );
  await followData(client, ARTIFACT, {
    path: 'todos',
    signal: controller.signal,
    onEvent: () => {},
  });
});

test('a permanent error that arrives after the abort resolves', async () => {
  const controller = new AbortController();
  const client = new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:1',
    accessToken: 'secret',
    // A fetch that ignores its signal and answers late.
    fetch: async () => {
      controller.abort();
      return errorResponse(404, {}, 'not_found');
    },
  });
  await followData(client, ARTIFACT, {
    path: 'todos',
    signal: controller.signal,
    onEvent: () => {},
  });
});

test('a connection lost while reading is retried', async () => {
  const lost = () =>
    new Response(
      new ReadableStream({
        start(controller) {
          controller.enqueue(bytes(sse(snap(1))));
        },
        pull() {
          throw new Error('reset');
        },
      }),
      { status: 200, headers: { 'content-type': 'text/event-stream' } },
    );
  const run = await follow([lost, streamResponse(sse(snap(2)))]);
  assert.deepEqual(run.outcome, { resolved: true });
  assert.equal(run.disconnects[0][0], 'network_error');
  assert.deepEqual(
    run.delivered.map((e) => e.dataVersion),
    [1, 2],
  );
});

// ---- local validation: nothing is sent for a bad request ---------------------------

test('followData and listDataMembers refuse bad parameters before sending', async () => {
  const { client, calls } = scripted(() => {
    throw new Error('sent');
  });
  const never = new AbortController().signal;
  for (const [name, bad] of BAD_PATHS) {
    await assert.rejects(
      followData(client, ARTIFACT, {
        path: bad,
        signal: never,
        onEvent: () => {},
      }),
      (error) => error.code === 'invalid_request',
      `follow ${name}`,
    );
    await assert.rejects(
      listDataMembers(client, ARTIFACT, { path: bad }),
      (error) => error.code === 'invalid_request',
      `list ${name}`,
    );
  }
  for (const query of [
    { limit: 0 },
    { limit: 101 },
    { limit: 1.5 },
    { limit: Number.NaN },
    { select: 'keys' },
    { after: '' },
    { after: '\ud800' },
    { after: 'a'.repeat(257) },
    { after: 'é'.repeat(129) },
  ])
    await assert.rejects(
      listDataMembers(client, ARTIFACT, query),
      (error) => error.code === 'invalid_request',
      JSON.stringify(query),
    );
  for (const retryDelayMs of [
    { min: 0, max: 5 },
    { min: 10, max: 5 },
    { min: Number.NaN, max: 5 },
  ])
    await assert.rejects(
      followData(client, ARTIFACT, {
        signal: never,
        onEvent: () => {},
        retryDelayMs,
      }),
      (error) => error.code === 'invalid_request',
    );
  assert.equal(calls.length, 0);
});

// ---- member pages ----------------------------------------------------------------

function page(overrides = {}) {
  return {
    artifact_id: ARTIFACT,
    path: '',
    select: 'names',
    limit: 50,
    data_version: 3,
    is_collection: true,
    members: [{ name: 'a' }, { name: 'b' }],
    ...overrides,
  };
}

const json = (body, init = {}) =>
  new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'content-type': 'application/json' },
    ...init,
  });

test('a page is requested with every default explicit and percent-encoded', async () => {
  const { client, calls } = scripted(() => json(page()));
  const result = await listDataMembers(client, ARTIFACT);
  assert.deepEqual(result.members, [{ name: 'a' }, { name: 'b' }]);
  assert.equal(
    calls[0].url,
    `http://127.0.0.1:1/v1/artifacts/${ARTIFACT}/data/members?select=names&limit=50`,
  );
  const second = scripted(() =>
    json(
      page({
        path: 'todos/a',
        select: 'values',
        limit: 7,
        after: 'é +&',
        members: [{ name: 'ü', value: null }],
      }),
    ),
  );
  const page2 = await listDataMembers(second.client, ARTIFACT, {
    path: 'todos/a',
    select: 'values',
    limit: 7,
    after: 'é +&',
  });
  assert.deepEqual(page2.members, [{ name: 'ü', value: null }]);
  const url = new URL(second.calls[0].url);
  assert.equal(url.searchParams.get('after'), 'é +&');
  assert.equal(url.searchParams.get('path'), 'todos/a');
  assert.equal(url.searchParams.get('limit'), '7');
});

const PAGE_FAULTS = [
  ['another artifact', { artifact_id: `art_${'1'.repeat(32)}` }, {}],
  ['another path', { path: 'x' }, {}],
  ['another select', { select: 'values' }, {}],
  ['another limit', { limit: 49 }, {}],
  ['a cursor that was not sent', { after: 'a' }, {}],
  ['a cursor that was sent but not echoed', {}, { query: { after: 'a' } }],
  ['another cursor', { after: 'z' }, { query: { after: 'a' } }],
  ['an unknown field', { revision: 1 }, {}],
  ['a missing field', { is_collection: undefined }, {}],
  ['a null optional field', { next_after: null }, {}],
  [
    'a version above 2^53 - 1',
    { data_version: Number.MAX_SAFE_INTEGER + 2 },
    {},
  ],
  ['a non-numeric version', { data_version: '3' }, {}],
  ['members out of order', { members: [{ name: 'b' }, { name: 'a' }] }, {}],
  ['a repeated member', { members: [{ name: 'a' }, { name: 'a' }] }, {}],
  [
    'members in UTF-16 order',
    { members: [{ name: '\u{10000}' }, { name: '～' }] },
    {},
  ],
  [
    'a member at or before the cursor',
    { after: 'b', members: [{ name: 'b' }] },
    { query: { after: 'b' } },
  ],
  ['more members than the limit', { limit: 1 }, { query: { limit: 1 } }],
  ['a value on a names page', { members: [{ name: 'a', value: 1 }] }, {}],
  [
    'too_large on a names page',
    { members: [{ name: 'a', too_large: true }] },
    {},
  ],
  ['an unknown member field', { members: [{ name: 'a', x: 1 }] }, {}],
  ['members of a non-collection', { is_collection: false }, {}],
  ['a next_after that is not the last name', { next_after: 'a' }, {}],
  ['a next_after without members', { members: [], next_after: 'a' }, {}],
  ['a name that is empty', { members: [{ name: '' }] }, {}],
  ['a name over 256 bytes', { members: [{ name: 'é'.repeat(129) }] }, {}],
  [
    'both a value and too_large',
    { select: 'values', members: [{ name: 'a', value: 1, too_large: true }] },
    { query: { select: 'values' } },
  ],
  [
    'neither a value nor too_large',
    { select: 'values', members: [{ name: 'a' }] },
    { query: { select: 'values' } },
  ],
  [
    'too_large false',
    { select: 'values', members: [{ name: 'a', too_large: false }] },
    { query: { select: 'values' } },
  ],
  [
    'a number beyond 2^53 - 1 in a value',
    { select: 'values', members: [{ name: 'a', value: { n: 1e30 } }] },
    { query: { select: 'values' } },
  ],
];

for (const [fault, overrides, { query } = {}] of PAGE_FAULTS) {
  test(`a page with ${fault} is an invalid response`, async () => {
    const body = page(overrides);
    for (const key of Object.keys(body))
      if (body[key] === undefined) delete body[key];
    const { client } = scripted(() => json(body));
    await assert.rejects(
      listDataMembers(client, ARTIFACT, query ?? {}),
      (error) => error.code === 'invalid_response' && error.status === 200,
    );
  });
}

test('a page in UTF-8 order, with a value or too_large, and a final cursor is accepted', async () => {
  const body = page({
    select: 'values',
    limit: 3,
    after: 'A',
    members: [
      { name: 'b', value: { x: [1, 'y'] } },
      { name: '～', too_large: true },
      { name: '\u{10000}', value: null },
    ],
    next_after: '\u{10000}',
  });
  const { client } = scripted(() => json(body));
  const result = await listDataMembers(client, ARTIFACT, {
    select: 'values',
    limit: 3,
    after: 'A',
  });
  assert.equal(result.next_after, '\u{10000}');
  assert.deepEqual(result.members[1], { name: '～', too_large: true });
});

test('a page larger than a megabyte is refused without being read to its end', async () => {
  let pulled = 0;
  let cancelled = false;
  const { client } = scripted(
    () =>
      new Response(
        new ReadableStream({
          pull(controller) {
            pulled += 1;
            controller.enqueue(new Uint8Array(256 * 1024).fill(0x20));
          },
          cancel() {
            cancelled = true;
          },
        }),
        { status: 200, headers: { 'content-type': 'application/json' } },
      ),
  );
  await assert.rejects(
    listDataMembers(client, ARTIFACT),
    (error) => error.code === 'invalid_response',
  );
  assert.ok(
    pulled <= 6 && cancelled,
    `read ${pulled} chunks, cancelled ${cancelled}`,
  );
});

test('a page whose body is not JSON, or whose request fails, is the client error for it', async () => {
  await assert.rejects(
    listDataMembers(
      scripted(() => new Response('nope', { status: 200 })).client,
      ARTIFACT,
    ),
    (error) => error.code === 'invalid_response',
  );
  await assert.rejects(
    listDataMembers(
      scripted(() => errorResponse(404, {}, 'not_found')).client,
      ARTIFACT,
    ),
    (error) => error.code === 'not_found' && error.status === 404,
  );
});

// ---- the real server -----------------------------------------------------------------

let binary;
let scratch;
before(async () => {
  binary = buildServer(root);
  scratch = await mkdtemp(join(tmpdir(), 'artifree-data-queries-test-'));
});
after(async () => {
  await rm(scratch, { recursive: true, force: true });
});

test('the real server: a snapshot, a change, a page, and a permanent 404', async () => {
  const server = await startServer(binary, join(scratch, 'real'));
  try {
    const client = new ArtifreeClient({
      baseUrl: server.api_url,
      accessToken: server.token,
    });
    const { artifact } = await client.createArtifact(
      {
        title: 'Data',
        files: [{ path: 'index.html', encoding: 'utf8', content: '<p>1</p>' }],
      },
      { idempotencyKey: 'data-queries-create' },
    );
    const controller = new AbortController();
    const seen = [];
    let wake;
    const arrived = () => new Promise((resolve) => (wake = resolve));
    let next = arrived();
    const following = followData(client, artifact.id, {
      path: 'todos',
      signal: controller.signal,
      onEvent: (event) => {
        seen.push(event);
        wake();
        next = arrived();
      },
    });
    await Promise.race([next, following]);
    assert.deepEqual(
      seen.map((e) => [e.type, e.dataVersion, e.exists]),
      [['snapshot', 0, false]],
    );
    const pending = next;
    await client.setData(
      artifact.id,
      {
        path: 'todos',
        base_data_version: 0,
        value: { b: { done: true }, a: 1 },
      },
      { idempotencyKey: 'data-queries-set-1' },
    );
    await pending;
    assert.deepEqual(seen[1].value, { a: 1, b: { done: true } });
    assert.deepEqual([seen[1].type, seen[1].dataVersion], ['change', 1]);
    const members = await listDataMembers(client, artifact.id, {
      path: 'todos',
      select: 'values',
    });
    assert.deepEqual(members.members, [
      { name: 'a', value: 1 },
      { name: 'b', value: { done: true } },
    ]);
    assert.equal(members.data_version, 1);
    controller.abort();
    await following;

    await assert.rejects(
      followData(client, `art_${'f'.repeat(32)}`, {
        signal: new AbortController().signal,
        onEvent: () => {},
      }),
      (error) => error.code === 'not_found' && error.status === 404,
    );
  } finally {
    await server.stop();
  }
});
