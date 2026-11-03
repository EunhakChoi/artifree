import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  ArtifreeApiError,
  ArtifreeClient,
  applyDataEvent,
  followData,
  listDataMembers,
  openData,
} from '../../packages/client/dist/index.js';

// The data query and subscription client (decision 0038) against a scripted
// network: a page is accepted only if it keeps the promises of the contract,
// events are checked field by field, following reconnects from the last
// cursor, and the reducer rebuilds a value from events and refuses an event
// that cannot follow the view.

const ARTIFACT = `art_${'1'.repeat(32)}`;
const FEED = 'ab'.repeat(16);
const cursor = (version) => `d.${FEED}.${version}`;

/** A client whose network is `respond`; `requests` lists what it received. */
function scripted(respond) {
  const requests = [];
  const client = new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:7410',
    accessToken: 'token',
    fetch: async (url, init) => {
      requests.push({ url, init });
      return respond(url, init, requests.length);
    },
  });
  return { client, requests };
}

async function failure(promise) {
  try {
    await promise;
  } catch (error) {
    assert.ok(error instanceof ArtifreeApiError, String(error));
    return error;
  }
  throw new assert.AssertionError({ message: 'Expected a rejection.' });
}

function page(canonical = {}, query = {}) {
  return {
    artifact_id: ARTIFACT,
    canonical: {
      data_version: 4,
      exists: true,
      members: [{ name: 'a' }, { name: 'b' }],
      ...canonical,
    },
    query: { path: '', select: 'names', limit: 50, ...query },
  };
}

test('A members page is requested with its query encoded and returned as parsed', async () => {
  const body = page(
    { members: [{ name: 'a1' }, { name: 'a2' }], next_after: 'a2' },
    { path: 'todos', prefix: 'a', select: 'names', limit: 2, after: 'a0' },
  );
  const { client, requests } = scripted(() => Response.json(body));
  const result = await listDataMembers(client, ARTIFACT, {
    path: 'todos',
    prefix: 'a',
    limit: 2,
    after: 'a0',
  });
  assert.deepEqual(result, body);
  const [{ url, init }] = requests;
  assert.equal(
    url,
    `http://127.0.0.1:7410/v1/artifacts/${ARTIFACT}/data/members?path=todos&prefix=a&limit=2&after=a0`,
  );
  assert.equal(init.method, 'GET');
  assert.equal(init.headers.authorization, 'Bearer token');
});

test('A listing without a query asks for the defaults and nulls read as absent', async () => {
  const { client, requests } = scripted(() =>
    Response.json(
      page(
        { members: [], exists: false, next_after: null },
        { prefix: null, after: null },
      ),
    ),
  );
  const result = await listDataMembers(client, ARTIFACT);
  assert.equal(
    requests[0].url,
    `http://127.0.0.1:7410/v1/artifacts/${ARTIFACT}/data/members`,
  );
  assert.deepEqual(result, {
    artifact_id: ARTIFACT,
    canonical: { data_version: 4, exists: false, members: [] },
    query: { path: '', select: 'names', limit: 50 },
  });
});

test('A stored null is a value of a member; names are checked by UTF-8 bytes', async () => {
  const values = page(
    {
      members: [
        { name: 'a', value: null },
        // U+E000 is three bytes (EE ..), U+10000 four (F0 ..): by bytes the
        // second sorts after, though a UTF-16 comparison says the opposite.
        { name: '', value: { x: [1] } },
        { name: '\u{10000}', value: 'text' },
      ],
    },
    { select: 'values' },
  );
  const { client } = scripted(() => Response.json(values));
  const result = await listDataMembers(client, ARTIFACT, { select: 'values' });
  assert.deepEqual(result.canonical.members, values.canonical.members);
  assert.ok(Object.hasOwn(result.canonical.members[0], 'value'));
});

test('A member too large for a page is accepted without its value, and only then', async () => {
  const body = page(
    {
      members: [
        { name: 'big', too_large: true },
        { name: 'small', value: [1] },
      ],
    },
    { select: 'values' },
  );
  const { client } = scripted(() => Response.json(body));
  const result = await listDataMembers(client, ARTIFACT, { select: 'values' });
  assert.deepEqual(result.canonical.members, body.canonical.members);
  assert.ok(!Object.hasOwn(result.canonical.members[0], 'value'));
});

const BREAKS = [
  ['names out of order', page({ members: [{ name: 'b' }, { name: 'a' }] }), {}],
  ['a repeated name', page({ members: [{ name: 'a' }, { name: 'a' }] }), {}],
  [
    'UTF-16 order instead of byte order',
    page({ members: [{ name: '\u{10000}' }, { name: '' }] }),
    {},
  ],
  [
    'a name not after the cursor',
    page({ members: [{ name: 'a' }] }, { after: 'a' }),
    { after: 'a' },
  ],
  [
    'a name without the prefix',
    page({ members: [{ name: 'b' }] }, { prefix: 'a' }),
    { prefix: 'a' },
  ],
  [
    'more members than the limit',
    page({ members: [{ name: 'a' }, { name: 'b' }] }, { limit: 1 }),
    { limit: 1 },
  ],
  [
    'a value that was not selected',
    page({ members: [{ name: 'a', value: 1 }] }),
    {},
  ],
  [
    'a selected value that is missing',
    page({ members: [{ name: 'a' }] }, { select: 'values' }),
    { select: 'values' },
  ],
  ['a next cursor that is not the last name', page({ next_after: 'a' }), {}],
  [
    'a next cursor on an empty page',
    page({ members: [], next_after: 'a' }),
    {},
  ],
  ['members of an object that is not there', page({ exists: false }), {}],
  [
    'too_large with a value',
    page(
      { members: [{ name: 'a', too_large: true, value: 1 }] },
      { select: 'values' },
    ),
    { select: 'values' },
  ],
  [
    'too_large that is false',
    page({ members: [{ name: 'a', too_large: false }] }, { select: 'values' }),
    { select: 'values' },
  ],
  [
    'too_large on a page of names',
    page({ members: [{ name: 'a', too_large: true }] }),
    {},
  ],
  ['a negative version', page({ data_version: -1 }), {}],
  ['a path that breaks the grammar', page({}, { path: 'a//b' }), {}],
  ['a limit of zero', page({}, { limit: 0 }), {}],
  ['a limit above the maximum', page({}, { limit: 101 }), {}],
  [
    'another select than asked',
    page({ members: [] }, { select: 'values' }),
    {},
  ],
  ['another prefix than asked', page({ members: [] }, { prefix: 'x' }), {}],
  ['another path than asked', page({}, { path: 'other' }), {}],
  ['another artifact', { ...page(), artifact_id: `art_${'2'.repeat(32)}` }, {}],
  ['no canonical state', { artifact_id: ARTIFACT, query: page().query }, {}],
  ['a body that is not an object', [], {}],
];

test('A page that breaks the contract is rejected as invalid_response', async () => {
  for (const [label, body, query] of BREAKS) {
    const { client } = scripted(() => Response.json(body));
    const error = await failure(listDataMembers(client, ARTIFACT, query));
    assert.deepEqual(
      [label, error.status, error.code],
      [label, 200, 'invalid_response'],
    );
  }
});

test('A refused listing keeps the code of the server', async () => {
  const { client } = scripted(() =>
    Response.json(
      { error: { code: 'not_found', message: 'artifact not found' } },
      { status: 404 },
    ),
  );
  const error = await failure(listDataMembers(client, ARTIFACT));
  assert.deepEqual([error.status, error.code], [404, 'not_found']);
});

const snapshot = (version, extra = {}) => ({
  cursor: cursor(version),
  data_version: version,
  path: '',
  exists: true,
  value: {},
  ...extra,
});

/** One server-sent event of the data stream. */
function sse(type, data) {
  return `event: ${type}\nid: ${data.cursor}\ndata: ${JSON.stringify(data)}\n\n`;
}

function stream(parts, { status = 200, onCancel } = {}) {
  const encoder = new TextEncoder();
  const body = new ReadableStream({
    start(controller) {
      for (const part of parts) controller.enqueue(encoder.encode(part));
      if (onCancel === undefined) controller.close();
    },
    cancel: onCancel,
  });
  return new Response(body, {
    status,
    headers: { 'content-type': 'text/event-stream; charset=utf-8' },
  });
}

async function eventsOf(response, options) {
  const { client } = scripted(() => response);
  const events = [];
  for await (const event of await openData(client, ARTIFACT, options))
    events.push(event);
  return events;
}

test('Data events are parsed in order, across chunks, and unknown types are skipped', async () => {
  const text =
    sse('snapshot', snapshot(1)) +
    sse('change', {
      cursor: cursor(2),
      data_version: 2,
      path: 'a',
      exists: true,
      value: null,
    }) +
    'event: heartbeat\ndata: {}\n\n' +
    sse('change', {
      cursor: cursor(3),
      data_version: 3,
      path: 'a',
      exists: false,
    }) +
    sse('position', { cursor: cursor(5), data_version: 5 }) +
    sse('reset', snapshot(6, { value: { z: 1 } }));
  // Cut in the middle of a line, and of an event.
  const parts = [text.slice(0, 60), text.slice(60, 200), text.slice(200)];
  const events = await eventsOf(stream(parts));
  assert.deepEqual(
    events.map(({ type, data }) => [type, data.data_version]),
    [
      ['snapshot', 1],
      ['change', 2],
      ['change', 3],
      ['position', 5],
      ['reset', 6],
    ],
  );
  assert.equal(events[1].data.value, null);
  assert.ok(!Object.hasOwn(events[2].data, 'value'));
});

const BAD_EVENTS = [
  ['snapshot', { ...snapshot(1), cursor: `${FEED}.1` }, 'a change feed cursor'],
  ['snapshot', { ...snapshot(1), cursor: `d.${FEED}.01` }, 'a padded version'],
  ['snapshot', { ...snapshot(1), cursor: `d.${FEED.toUpperCase()}.1` }, 'case'],
  ['snapshot', { ...snapshot(1), data_version: '1' }, 'a text version'],
  ['snapshot', { ...snapshot(1), path: 'a b' }, 'a path with a space'],
  ['snapshot', { ...snapshot(1), exists: 1 }, 'a number for exists'],
  ['snapshot', { ...snapshot(1), exists: false }, 'a value where none exists'],
  [
    'snapshot',
    { cursor: cursor(1), data_version: 1, path: '', exists: true },
    'no value where one exists',
  ],
  ['change', { ...snapshot(2), path: '/x' }, 'a path with a leading slash'],
  ['position', { cursor: cursor(2) }, 'a position without a version'],
  ['position', { cursor: 7, data_version: 2 }, 'a number for a cursor'],
  ['reset', null, 'no data'],
];

test('A known event that breaks the contract rejects the stream', async () => {
  for (const [type, data, label] of BAD_EVENTS) {
    const text = `event: ${type}\ndata: ${JSON.stringify(data)}\n\n`;
    const error = await failure(eventsOf(stream([text])));
    assert.deepEqual(
      [label, error.status, error.code],
      [label, 200, 'invalid_response'],
    );
  }
  const notJson = await failure(
    eventsOf(stream(['event: snapshot\ndata: {oops\n\n'])),
  );
  assert.equal(notJson.code, 'invalid_response');
});

test('A stream is opened for the path and resumes with the cursor in Last-Event-ID', async () => {
  const { client, requests } = scripted(() => stream([]));
  await openData(client, ARTIFACT, { path: 'todos/t1', after: cursor(7) });
  await openData(client, ARTIFACT);
  assert.equal(
    requests[0].url,
    `http://127.0.0.1:7410/v1/artifacts/${ARTIFACT}/data/events?path=todos%2Ft1`,
  );
  assert.equal(requests[0].init.headers['last-event-id'], cursor(7));
  assert.equal(requests[0].init.headers.accept, 'text/event-stream');
  assert.equal(
    requests[1].url,
    `http://127.0.0.1:7410/v1/artifacts/${ARTIFACT}/data/events`,
  );
  assert.ok(!('last-event-id' in requests[1].init.headers));
});

test('A successful answer that is not an event stream is invalid_response', async () => {
  const { client } = scripted(
    () =>
      new Response('<html>Sign in</html>', {
        status: 200,
        headers: { 'content-type': 'text/html' },
      }),
  );
  const error = await failure(openData(client, ARTIFACT));
  assert.deepEqual([error.status, error.code], [200, 'invalid_response']);
});

/** Follows until `wanted` events arrived; every answer is used in turn. */
async function followed(
  answers,
  { wanted, path, after, retryDelayMs = { min: 1, max: 2 } } = {},
) {
  const controller = new AbortController();
  const events = [];
  const disconnects = [];
  const { client, requests } = scripted(() => {
    const answer = answers.length > 1 ? answers.shift() : answers[0];
    return answer();
  });
  await followData(client, ARTIFACT, {
    signal: controller.signal,
    retryDelayMs,
    ...(path === undefined ? {} : { path }),
    ...(after === undefined ? {} : { after }),
    onEvent(event) {
      events.push(event);
      if (events.length === wanted) controller.abort();
    },
    onDisconnect(error, retryInMs) {
      disconnects.push([error?.status, error?.code, retryInMs]);
    },
  });
  return {
    events,
    disconnects,
    resumed: requests.map(({ init }) => init.headers['last-event-id']),
    urls: requests.map(({ url }) => url),
  };
}

const changeAt = (version) =>
  sse('change', {
    cursor: cursor(version),
    data_version: version,
    path: 'a',
    exists: true,
    value: version,
  });

test('Following reconnects from the last cursor after a loss, a 429, or a 5xx, without a gap', async () => {
  const result = await followed(
    [
      () => stream([sse('snapshot', snapshot(1)), changeAt(2)]),
      () => {
        throw new TypeError('connection refused');
      },
      () => new Response('', { status: 502 }),
      () =>
        Response.json(
          { error: { code: 'rate_limited', message: 'too many streams' } },
          { status: 429, headers: { 'retry-after': '0' } },
        ),
      () => stream([changeAt(3)]),
    ],
    { wanted: 3, path: 'a' },
  );
  assert.deepEqual(
    result.events.map(({ type, data }) => [type, data.data_version]),
    [
      ['snapshot', 1],
      ['change', 2],
      ['change', 3],
    ],
  );
  assert.deepEqual(result.resumed, [
    undefined,
    cursor(2),
    cursor(2),
    cursor(2),
    cursor(2),
  ]);
  assert.ok(
    result.urls.every((url) => url.endsWith('/data/events?path=a')),
    result.urls.join(),
  );
  assert.deepEqual(result.disconnects, [
    [undefined, undefined, 1],
    [0, 'network_error', 2],
    [502, 'invalid_response', 2],
    [429, 'rate_limited', 1],
  ]);
});

test('Following stops on a failure a retry cannot fix', async () => {
  const fatal = [
    [
      () =>
        Response.json(
          { error: { code: 'not_found', message: 'artifact not found' } },
          { status: 404 },
        ),
      [404, 'not_found'],
    ],
    [
      () =>
        Response.json(
          { error: { code: 'unauthenticated', message: 'no' } },
          { status: 401 },
        ),
      [401, 'unauthenticated'],
    ],
    [
      () => stream(['event: change\ndata: {"cursor":"x"}\n\n']),
      [200, 'invalid_response'],
    ],
  ];
  for (const [answer, expected] of fatal) {
    const error = await failure(followed([answer]));
    assert.deepEqual([error.status, error.code], expected);
  }
});

test('A Retry-After is kept between the configured delays, never a hot loop', async () => {
  const limited = (seconds) =>
    Response.json(
      { error: { code: 'rate_limited', message: 'too many streams' } },
      { status: 429, headers: { 'retry-after': String(seconds) } },
    );
  const seconds = [0, 1, 3600];
  const { client } = scripted((url, init, count) =>
    limited(seconds[count - 1]),
  );
  const controller = new AbortController();
  const delays = [];
  // Aborting at the third wait ends the test even when a delay was not capped.
  await followData(client, ARTIFACT, {
    signal: controller.signal,
    retryDelayMs: { min: 20, max: 60 },
    onEvent() {},
    onDisconnect(error, retryInMs) {
      delays.push(retryInMs);
      if (delays.length === seconds.length) controller.abort();
    },
  });
  assert.deepEqual(
    delays,
    [20, 60, 60],
    'zero waits the minimum, an hour the maximum',
  );
});

test('A stream the server accepts and closes at once keeps backing off', async () => {
  const result = await followed(
    [
      () => stream([]),
      () => stream([]),
      () => stream([]),
      () => stream([sse('snapshot', snapshot(1))]),
    ],
    { wanted: 1, retryDelayMs: { min: 10, max: 30 } },
  );
  assert.deepEqual(
    result.disconnects.map(([, , delay]) => delay),
    [10, 20, 30],
  );
});

test('What onEvent throws ends following and is not retried', async () => {
  const { client, requests } = scripted(() =>
    stream([sse('snapshot', snapshot(1))]),
  );
  const boom = new Error('the application failed');
  await assert.rejects(
    followData(client, ARTIFACT, {
      signal: new AbortController().signal,
      onEvent() {
        throw boom;
      },
    }),
    (error) => error === boom,
  );
  assert.equal(requests.length, 1);
});

test('Aborting stops following at once, during a wait or on an open stream', async () => {
  const controller = new AbortController();
  const waiting = scripted(() =>
    Response.json(
      { error: { code: 'rate_limited', message: 'too many streams' } },
      { status: 429, headers: { 'retry-after': '600' } },
    ),
  );
  const started = performance.now();
  await followData(waiting.client, ARTIFACT, {
    signal: controller.signal,
    onEvent() {},
    onDisconnect() {
      controller.abort();
    },
  });
  assert.ok(performance.now() - started < 2000, 'waited after the abort');

  let cancelled = false;
  const open = new AbortController();
  const held = scripted(() =>
    stream([sse('snapshot', snapshot(1))], {
      onCancel() {
        cancelled = true;
      },
    }),
  );
  await followData(held.client, ARTIFACT, {
    signal: open.signal,
    onEvent() {
      open.abort();
    },
  });
  assert.ok(cancelled, 'the open stream was left running');
  assert.equal(held.requests.length, 1);
});

// The reducer. Each case is a sequence of events and the view it must give.
const change = (version, path, value) => ({
  type: 'change',
  data: {
    cursor: cursor(version),
    data_version: version,
    path,
    ...(value === undefined ? { exists: false } : { exists: true, value }),
  },
});
const snap = (version, path, value, type = 'snapshot') => ({
  type,
  data: {
    cursor: cursor(version),
    data_version: version,
    path,
    ...(value === undefined ? { exists: false } : { exists: true, value }),
  },
});
const position = (version) => ({
  type: 'position',
  data: { cursor: cursor(version), data_version: version },
});

function replay(events) {
  let view;
  for (const event of events) view = applyDataEvent(view, event);
  return view;
}

const REDUCTIONS = [
  [
    'a change at the watched path replaces its value',
    [snap(1, 'a', { x: 1 }), change(2, 'a', [1, 2])],
    { exists: true, value: [1, 2], dataVersion: 2 },
  ],
  [
    'a change below sets one member and keeps the rest',
    [snap(1, 'a', { x: 1, y: 2 }), change(2, 'a/y', { z: null })],
    { exists: true, value: { x: 1, y: { z: null } }, dataVersion: 2 },
  ],
  [
    'a change below makes the objects above when nothing was there',
    [snap(1, 'a', undefined), change(2, 'a/b/c', 7)],
    { exists: true, value: { b: { c: 7 } }, dataVersion: 2 },
  ],
  [
    'a removal below leaves the empty parent, as the server does',
    [snap(1, 'a', { b: { c: 1 } }), change(2, 'a/b/c', undefined)],
    { exists: true, value: { b: {} }, dataVersion: 2 },
  ],
  [
    'a removal of the watched value leaves nothing',
    [snap(1, 'a', { b: 1 }), change(2, 'a', undefined)],
    { exists: false, dataVersion: 2 },
  ],
  [
    'a removal below nothing changes nothing',
    [snap(1, 'a', undefined), change(2, 'a/b', undefined)],
    { exists: false, dataVersion: 2 },
  ],
  [
    'a stored null at the watched path is a value',
    [snap(1, 'a', { b: 1 }), change(2, 'a', null)],
    { exists: true, value: null, dataVersion: 2 },
  ],
  [
    'the root changes by member',
    [
      snap(1, '', {}),
      change(2, 'todos/t1', { done: false }),
      change(3, 'n', 1),
    ],
    {
      exists: true,
      value: { todos: { t1: { done: false } }, n: 1 },
      dataVersion: 3,
    },
  ],
  [
    'a position moves the version and the cursor only',
    [snap(1, 'a', { b: 1 }), position(9)],
    { exists: true, value: { b: 1 }, dataVersion: 9, cursor: cursor(9) },
  ],
  [
    'a reset discards what was held',
    [
      snap(1, 'a', { b: 1 }),
      change(2, 'a/c', 2),
      snap(8, 'a', { d: 1 }, 'reset'),
    ],
    { exists: true, value: { d: 1 }, dataVersion: 8 },
  ],
];

test('The reducer rebuilds the value from snapshot, reset, change, and position', () => {
  for (const [label, events, expected] of REDUCTIONS) {
    const view = replay(events);
    assert.equal(view.exists, expected.exists, label);
    assert.equal(view.dataVersion, expected.dataVersion, label);
    assert.equal(
      view.cursor,
      expected.cursor ?? cursor(expected.dataVersion),
      label,
    );
    if ('value' in expected)
      assert.deepEqual(view.value, expected.value, label);
    else if (!expected.exists)
      assert.ok(!Object.hasOwn(view, 'value'), `${label}: a value remained`);
  }
});

test('An event for __proto__ is stored as a member and does not touch prototypes', () => {
  const view = replay([snap(1, '', {}), change(2, '__proto__/polluted', true)]);
  assert.deepEqual(Object.keys(view.value), ['__proto__']);
  assert.equal(Object.getPrototypeOf(view.value), Object.prototype);
  assert.equal({}.polluted, undefined);
  assert.deepEqual(JSON.parse(JSON.stringify(view.value)).__proto__, {
    polluted: true,
  });
});

test('The reducer does not mutate the view it was given', () => {
  const first = applyDataEvent(undefined, snap(1, 'a', { x: { y: 1 } }));
  const before = structuredClone(first);
  const second = applyDataEvent(first, change(2, 'a/x/y', 2));
  assert.deepEqual(first, before);
  assert.deepEqual(second.value, { x: { y: 2 } });
});

const REFUSED = [
  ['a change before any snapshot', [change(1, 'a', 1)]],
  ['a position before any snapshot', [position(1)]],
  ['a change that is not newer', [snap(5, 'a', {}), change(5, 'a/b', 1)]],
  ['a change that is older', [snap(5, 'a', {}), change(4, 'a/b', 1)]],
  ['a position that goes back', [snap(5, 'a', {}), position(4)]],
  ['a change beside the watched path', [snap(1, 'a', {}), change(2, 'b', 1)]],
  [
    'a change above the watched path',
    [snap(1, 'a/b', {}), change(2, 'a', { b: 1 })],
  ],
  [
    'a change through a value that is not an object',
    [snap(1, 'a', 5), change(2, 'a/b', 1)],
  ],
];

test('An event that cannot follow the view is refused, not applied', () => {
  for (const [label, events] of REFUSED) {
    assert.throws(
      () => replay(events),
      (error) =>
        error instanceof ArtifreeApiError && error.code === 'invalid_response',
      label,
    );
  }
});
