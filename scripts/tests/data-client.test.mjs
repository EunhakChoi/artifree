import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  ArtifreeApiError,
  ArtifreeClient,
  HEALTHY_AFTER_MS,
  IDLE_TIMEOUT_MS,
  MAX_BACKOFF_MS,
  MAX_EVENT_BYTES,
  MIN_WAIT_MS,
  RETRY_AFTER_CEILING_MS,
  compareUtf8,
  followData,
  listDataMembers,
  parseDataMembers,
} from '../../packages/client/dist/index.js';

// The data query and stream client (decision 0040) against a scripted
// network and injected timers: nothing here sleeps or opens a socket.

const ART = `art_${'0'.repeat(32)}`;
const encoder = new TextEncoder();

// Queries --------------------------------------------------------------------

function page(overrides = {}) {
  return {
    artifact_id: ART,
    query: { path: 'c', prefix: '', select: 'names', limit: 25 },
    data_version: 4,
    collection: true,
    members: [{ name: 'a' }, { name: 'b' }],
    ...overrides,
  };
}

const expectation = (overrides = {}) => ({
  artifactId: ART,
  path: 'c',
  prefix: '',
  select: 'names',
  limit: 25,
  after: undefined,
  ...overrides,
});

function jsonResponse(body, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'content-type': 'application/json' },
  });
}

function recordingClient(answer) {
  const calls = [];
  const client = new ArtifreeClient({
    baseUrl: 'http://api.test',
    accessToken: 'token',
    fetch: async (url, init) => {
      calls.push({ url: String(url), init });
      return answer();
    },
  });
  return { client, calls };
}

test('A listing sends the normalized query and returns the validated page', async () => {
  const { client, calls } = recordingClient(() =>
    jsonResponse(
      page({
        query: {
          path: 'a/b',
          prefix: 'é',
          select: 'values',
          limit: 7,
          after: 'a b',
        },
        members: [
          { name: 'éa', value: null },
          { name: 'éb', too_large: true },
        ],
        next_after: 'éb',
      }),
    ),
  );
  const result = await listDataMembers(client, ART, {
    path: 'a/b',
    prefix: 'é',
    select: 'values',
    limit: 7,
    after: 'a b',
  });
  assert.equal(calls.length, 1);
  const url = new URL(calls[0].url);
  assert.equal(url.pathname, `/v1/artifacts/${ART}/data/members`);
  assert.deepEqual(Object.fromEntries(url.searchParams), {
    path: 'a/b',
    prefix: 'é',
    select: 'values',
    limit: '7',
    after: 'a b',
  });
  assert.equal(calls[0].init.headers.authorization, 'Bearer token');
  assert.deepEqual(result.members, [
    { name: 'éa', value: null },
    { name: 'éb', too_large: true },
  ]);
  assert.equal(result.next_after, 'éb');
  assert.equal(result.data_version, 4);
});

test('A listing with no request asks for the root, names, and 25', async () => {
  const { client, calls } = recordingClient(() =>
    jsonResponse(
      page({ query: { path: '', prefix: '', select: 'names', limit: 25 } }),
    ),
  );
  await listDataMembers(client, ART);
  const url = new URL(calls[0].url);
  assert.deepEqual(Object.fromEntries(url.searchParams), {
    select: 'names',
    limit: '25',
  });
});

test('A listing that breaks a limit is refused without sending', async () => {
  const { client, calls } = recordingClient(() => jsonResponse(page()));
  const refused = [
    { limit: 0 },
    { limit: 101 },
    { limit: 1.5 },
    { limit: Number.NaN },
    { prefix: 'p'.repeat(257) },
    { prefix: 'é'.repeat(129) },
    { after: '' },
    { after: 'a'.repeat(257) },
    { select: 'keys' },
  ];
  for (const request of refused) {
    await assert.rejects(
      listDataMembers(client, ART, request),
      (error) =>
        error instanceof ArtifreeApiError &&
        error.code === 'invalid_request' &&
        error.status === 0,
      JSON.stringify(request),
    );
  }
  assert.equal(calls.length, 0);
  // The bounds themselves are sent.
  const { client: bounds, calls: sent } = recordingClient(() =>
    jsonResponse(
      page({
        query: {
          path: '',
          prefix: 'p'.repeat(256),
          select: 'names',
          limit: 100,
        },
        members: [],
      }),
    ),
  );
  await listDataMembers(bounds, ART, { limit: 100, prefix: 'p'.repeat(256) });
  assert.equal(sent.length, 1);
});

test('Members are ordered by UTF-8 bytes, which differs from JavaScript string order', () => {
  // U+FF5E is before U+10000 by UTF-8 bytes, after it by UTF-16 code units.
  const [bmp, astral] = ['～', '\u{10000}'];
  assert.ok(compareUtf8(bmp, astral) < 0);
  assert.ok(bmp > astral);
  const ordered = page({
    members: [{ name: '～' }, { name: '\u{10000}' }],
  });
  assert.ok(parseDataMembers(ordered, expectation()));
  const backwards = page({
    members: [{ name: '\u{10000}' }, { name: '～' }],
  });
  assert.equal(parseDataMembers(backwards, expectation()), undefined);
});

test('A listing that disagrees with its request is not accepted', () => {
  const refuse = (label, body, wanted = expectation()) =>
    assert.equal(parseDataMembers(body, wanted), undefined, label);
  assert.ok(parseDataMembers(page(), expectation()));
  refuse('another artifact', page({ artifact_id: `art_${'1'.repeat(32)}` }));
  refuse(
    'another limit',
    page({ query: { path: 'c', prefix: '', select: 'names', limit: 10 } }),
  );
  refuse(
    'another path',
    page({ query: { path: 'd', prefix: '', select: 'names', limit: 25 } }),
  );
  refuse(
    'another select',
    page({ query: { path: 'c', prefix: '', select: 'values', limit: 25 } }),
  );
  refuse(
    'an extra query field',
    page({
      query: {
        path: 'c',
        prefix: '',
        select: 'names',
        limit: 25,
        order: 'desc',
      },
    }),
  );
  refuse(
    'a cursor nobody sent',
    page({
      query: { path: 'c', prefix: '', select: 'names', limit: 25, after: 'a' },
    }),
  );
  refuse('a missing cursor', page(), expectation({ after: 'a' }));
  refuse('a repeated name', page({ members: [{ name: 'a' }, { name: 'a' }] }));
  refuse(
    'a name out of order',
    page({ members: [{ name: 'b' }, { name: 'a' }] }),
  );
  refuse(
    'a name at the cursor',
    page({ members: [{ name: 'a' }] }),
    expectation({ after: 'a' }),
  );
  refuse(
    'a name before the cursor',
    page({
      query: { path: 'c', prefix: '', select: 'names', limit: 25, after: 'b' },
      members: [{ name: 'a' }],
    }),
    expectation({ after: 'b' }),
  );
  refuse(
    'a name without the prefix',
    page({
      query: { path: 'c', prefix: 'x', select: 'names', limit: 25 },
      members: [{ name: 'xa' }, { name: 'y' }],
    }),
    expectation({ prefix: 'x' }),
  );
  refuse(
    'more members than the limit',
    page({
      query: { path: 'c', prefix: '', select: 'names', limit: 1 },
      members: [{ name: 'a' }, { name: 'b' }],
    }),
    expectation({ limit: 1 }),
  );
  refuse('members of a non-collection', page({ collection: false }));
  refuse('a cursor that is not the last name', page({ next_after: 'a' }));
  refuse('a cursor with no members', page({ members: [], next_after: 'a' }));
  refuse('a null cursor', page({ next_after: null }));
  refuse('an empty name', page({ members: [{ name: '' }] }));
  refuse('a name of 257 bytes', page({ members: [{ name: 'n'.repeat(257) }] }));
  refuse('a version that is not a count', page({ data_version: -1 }));
  refuse('a value among names', page({ members: [{ name: 'a', value: 1 }] }));
  refuse(
    'a flag among names',
    page({ members: [{ name: 'a', too_large: true }] }),
  );
  const values = (members) =>
    page({
      query: { path: 'c', prefix: '', select: 'values', limit: 25 },
      members,
    });
  const wanted = expectation({ select: 'values' });
  assert.ok(
    parseDataMembers(
      values([
        { name: 'a', value: null },
        { name: 'b', too_large: true },
      ]),
      wanted,
    ),
  );
  refuse('a bare name among values', values([{ name: 'a' }]), wanted);
  refuse(
    'a value and a flag',
    values([{ name: 'a', value: 1, too_large: true }]),
    wanted,
  );
  refuse('a false flag', values([{ name: 'a', too_large: false }]), wanted);
  refuse('a null flag', values([{ name: 'a', too_large: null }]), wanted);
  refuse(
    'an unknown field',
    values([{ name: 'a', value: 1, extra: 1 }]),
    wanted,
  );
});

test('A response that does not match is invalid_response', async () => {
  const { client } = recordingClient(() =>
    jsonResponse(page({ collection: 'yes' })),
  );
  await assert.rejects(
    listDataMembers(client, ART, { path: 'c' }),
    (error) =>
      error instanceof ArtifreeApiError && error.code === 'invalid_response',
  );
});

test('A missing artifact is not_found', async () => {
  const { client } = recordingClient(() =>
    jsonResponse(
      { error: { code: 'not_found', message: 'artifact not found' } },
      404,
    ),
  );
  await assert.rejects(
    listDataMembers(client, ART),
    (error) =>
      error instanceof ArtifreeApiError &&
      error.code === 'not_found' &&
      error.status === 404,
  );
});

// Streams --------------------------------------------------------------------

const state = (version, value, path = 'n') => ({
  artifact_id: ART,
  path,
  data_version: version,
  exists: value !== undefined,
  ...(value === undefined ? {} : { value }),
});
const frame = (type, data) =>
  `event: ${type}\ndata: ${JSON.stringify(data)}\n\n`;

/** A response whose body is `chunks`; `end: false` keeps the stream open. */
function eventStream(chunks, { end = true, headers = {}, onCancel } = {}) {
  return () =>
    new Response(
      new ReadableStream({
        start(controller) {
          for (const chunk of chunks) controller.enqueue(encoder.encode(chunk));
          if (end) controller.close();
        },
        cancel() {
          onCancel?.();
        },
      }),
      {
        status: 200,
        headers: { 'content-type': 'text/event-stream', ...headers },
      },
    );
}

const failure =
  (status, code, headers = {}) =>
  () =>
    new Response(JSON.stringify({ error: { code, message: code } }), {
      status,
      headers: { 'content-type': 'application/json', ...headers },
    });

/** Steps run in order; when they run out, following is aborted. */
function scripted(steps, controller) {
  const calls = [];
  const fetchStep = async (url, init) => {
    calls.push({ url: String(url), init });
    const step = steps[calls.length - 1];
    if (step === undefined) {
      controller.abort();
      throw new Error('the script is over');
    }
    if (step === 'network') throw new TypeError('fetch failed');
    return step(init);
  };
  return { fetch: fetchStep, calls };
}

/**
 * Timers that record each wait and finish it at once. The idle watch of a
 * connection (IDLE_TIMEOUT_MS) ends only when the test says so, or with the
 * read it watches.
 */
function timers({ idleFiresAt } = {}) {
  const waits = [];
  let now = 0;
  let idleWatches = 0;
  return {
    waits,
    advance: (milliseconds) => {
      now += milliseconds;
    },
    impl: {
      now: () => now,
      sleep: (milliseconds, signal) => {
        if (milliseconds === IDLE_TIMEOUT_MS) {
          idleWatches += 1;
          if (idleWatches === idleFiresAt) return Promise.resolve();
          return new Promise((resolve) => {
            signal.addEventListener('abort', resolve, { once: true });
          });
        }
        waits.push(milliseconds);
        return Promise.resolve();
      },
    },
  };
}

async function run(
  steps,
  { timer = timers(), path = 'n', onEvent, onDisconnect } = {},
) {
  const controller = new AbortController();
  const { fetch, calls } = scripted(steps, controller);
  const events = [];
  const disconnects = [];
  const outcome = await followData(
    { baseUrl: 'http://api.test/', accessToken: 'secret', fetch },
    ART,
    {
      ...(path === undefined ? {} : { path }),
      signal: controller.signal,
      timers: timer.impl,
      onEvent: (event) => {
        events.push(event);
        onEvent?.(event, controller);
      },
      onDisconnect: (error, retryInMs) => {
        disconnects.push({ code: error?.code ?? 'ended', retryInMs });
        onDisconnect?.(error, controller);
      },
    },
  ).then(
    () => ({ resolved: true }),
    (error) => ({ error }),
  );
  return { events, disconnects, calls, outcome, timer };
}

test('A stream delivers its snapshot and then each change, as sent', async () => {
  const { events, calls, outcome } = await run(
    [
      eventStream([
        frame('snapshot', state(3, { a: 1 }, 'to-do/x')),
        ': keepalive\n\n',
        frame('change', state(5, { a: 2 }, 'to-do/x')),
        frame('change', { ...state(6, undefined, 'to-do/x') }),
      ]),
    ],
    { path: 'to-do/x' },
  );
  assert.deepEqual(outcome, { resolved: true });
  assert.deepEqual(
    events.map(({ type, data }) => [type, data.data_version, data.value]),
    [
      ['snapshot', 3, { a: 1 }],
      ['change', 5, { a: 2 }],
      ['change', 6, undefined],
    ],
  );
  const url = new URL(calls[0].url);
  assert.equal(
    url.origin + url.pathname,
    `http://api.test/v1/artifacts/${ART}/data/events`,
  );
  assert.equal(url.searchParams.get('path'), 'to-do/x');
  assert.equal(calls[0].init.headers.authorization, 'Bearer secret');
  assert.equal(calls[0].init.headers.accept, 'text/event-stream');
});

test('The root is watched with no path parameter', async () => {
  const { calls } = await run([eventStream([])], { path: '' });
  assert.equal(new URL(calls[0].url).search, '');
});

test('A stream that breaks the contract rejects and is not retried', async () => {
  const cases = {
    'a change first': [frame('change', state(1, 1))],
    'a second snapshot': [
      frame('snapshot', state(1, 1)),
      frame('snapshot', state(2, 1)),
    ],
    'a repeated version': [
      frame('snapshot', state(2, 1)),
      frame('change', state(2, 2)),
    ],
    'a lower version': [
      frame('snapshot', state(2, 1)),
      frame('change', state(1, 2)),
    ],
    'another path': [frame('snapshot', state(1, 1, 'other'))],
    'another artifact': [
      frame('snapshot', {
        ...state(1, 1),
        artifact_id: `art_${'1'.repeat(32)}`,
      }),
    ],
    'a missing value': [
      frame('snapshot', { ...state(1, 1), value: undefined }),
    ],
    'a value that does not exist': [
      frame('snapshot', { ...state(1, 1), exists: false }),
    ],
    'data that is not JSON': ['event: snapshot\ndata: {nope\n\n'],
  };
  for (const [label, chunks] of Object.entries(cases)) {
    const { outcome, calls, events } = await run([
      eventStream(chunks),
      eventStream([]),
    ]);
    assert.ok(
      outcome.error instanceof ArtifreeApiError &&
        outcome.error.code === 'invalid_response',
      `${label}: ${String(outcome.error)}`,
    );
    assert.equal(calls.length, 1, `${label} was retried`);
    assert.ok(events.length <= 1, label);
  }
});

test('A value too large to stream rejects with data_too_large and is not retried', async () => {
  const large = frame('too_large', {
    artifact_id: ART,
    path: 'n',
    data_version: 2,
    max_bytes: 131072,
  });
  const { outcome, calls, events } = await run([
    eventStream([frame('snapshot', state(1, 1)), large]),
    eventStream([]),
  ]);
  assert.ok(
    outcome.error instanceof ArtifreeApiError &&
      outcome.error.code === 'data_too_large',
  );
  assert.equal(calls.length, 1);
  assert.equal(events.length, 1);
  const malformed = await run([
    eventStream(['event: too_large\ndata: {}\n\n']),
  ]);
  assert.equal(malformed.outcome.error.code, 'invalid_response');
});

const OTHER_ART = `art_${'1'.repeat(32)}`;

/** An event of `type` about `n` at `version`, with parts overridden. */
function eventAbout(type, version, { artifact = ART, path = 'n' } = {}) {
  return type === 'too_large'
    ? frame('too_large', {
        artifact_id: artifact,
        path,
        data_version: version,
        max_bytes: 131072,
      })
    : frame(type, { ...state(version, 1, path), artifact_id: artifact });
}

test('Every event type is held to the watched artifact, path, and order', async () => {
  const types = ['snapshot', 'change', 'too_large'];
  // After a snapshot at version 5 was delivered.
  const after = {
    'another artifact': { version: 6, where: { artifact: OTHER_ART } },
    'another path': { version: 6, where: { path: 'other' } },
    'a stale version': { version: 4, where: {} },
    'an equal version': { version: 5, where: {} },
  };
  for (const type of types)
    for (const [label, { version, where }] of Object.entries(after)) {
      const { outcome, calls, events } = await run([
        eventStream(
          [eventAbout('snapshot', 5), eventAbout(type, version, where)],
          {
            end: false,
          },
        ),
      ]);
      assert.ok(
        outcome.error instanceof ArtifreeApiError &&
          outcome.error.code === 'invalid_response',
        `${type} with ${label}: ${String(outcome.error)}`,
      );
      assert.equal(calls.length, 1, `${type} with ${label} was retried`);
      assert.equal(events.length, 1, `${type} with ${label} was delivered`);
    }
  // As the first event of a connection.
  const first = {
    'another artifact': { version: 1, where: { artifact: OTHER_ART } },
    'another path': { version: 1, where: { path: 'other' } },
  };
  for (const type of types)
    for (const [label, { version, where }] of Object.entries(first)) {
      const { outcome, events } = await run([
        eventStream([eventAbout(type, version, where)], { end: false }),
      ]);
      assert.equal(
        outcome.error?.code,
        'invalid_response',
        `first ${type} with ${label}: ${String(outcome.error)}`,
      );
      assert.equal(events.length, 0);
    }
  const changeFirst = await run([
    eventStream([eventAbout('change', 1)], { end: false }),
  ]);
  assert.equal(changeFirst.outcome.error?.code, 'invalid_response');
  // What is accepted: a newer change, and a too_large that is newer than what
  // was delivered or first.
  const newer = await run([
    eventStream([eventAbout('snapshot', 5), eventAbout('change', 6)]),
  ]);
  assert.deepEqual(newer.outcome, { resolved: true });
  assert.deepEqual(
    newer.events.map(({ data }) => data.data_version),
    [5, 6],
  );
  for (const events of [
    [eventAbout('snapshot', 5), eventAbout('too_large', 6)],
    [eventAbout('too_large', 1)],
  ]) {
    const { outcome, calls } = await run([eventStream(events)]);
    assert.equal(outcome.error?.code, 'data_too_large');
    assert.equal(calls.length, 1);
  }
});

test('A path the server would not accept is refused before anything is sent', async () => {
  const paths = [
    'a//b',
    '/a',
    'a/',
    '/',
    'a b',
    'é',
    '../x',
    'a/./b',
    'a'.repeat(65),
    Array.from({ length: 17 }, () => 's').join('/'),
  ];
  for (const path of paths) {
    const { outcome, calls } = await run([eventStream([])], { path });
    assert.ok(
      outcome.error instanceof ArtifreeApiError &&
        outcome.error.code === 'invalid_request',
      `${JSON.stringify(path)}: ${String(outcome.error)}`,
    );
    assert.equal(calls.length, 0, JSON.stringify(path));
    const asked = recordingClient(() => jsonResponse({}));
    const listing = await listDataMembers(asked.client, ART, { path }).then(
      () => undefined,
      (error) => error,
    );
    assert.equal(listing?.code, 'invalid_request', JSON.stringify(path));
    assert.equal(asked.calls.length, 0, JSON.stringify(path));
  }
  // The longest accepted forms.
  const longest = [
    'a'.repeat(64),
    Array.from({ length: 16 }, () => 's').join('/'),
  ];
  for (const path of longest) {
    const { calls } = await run([eventStream([])], { path });
    assert.ok(calls.length > 0, `${path} was not sent`);
  }
});

test('Event types this client does not know are skipped', async () => {
  const { events, outcome } = await run([
    eventStream([
      frame('mystery', { a: 1 }),
      frame('snapshot', state(1, 1)),
      'id: 7\nretry: 5\nevent: other\ndata: {}\n\n',
      frame('change', state(2, 2)),
    ]),
  ]);
  assert.deepEqual(outcome, { resolved: true });
  assert.deepEqual(
    events.map((event) => event.data.data_version),
    [1, 2],
  );
});

test('The line ends of the standard are read, also across chunks', async () => {
  const text =
    frame('snapshot', state(1, 'x')) + frame('change', state(2, 'y'));
  const crlf = text.replaceAll('\n', '\r\n');
  const bytes = [...crlf].map((char) => char);
  const { events } = await run([eventStream(bytes)]);
  assert.deepEqual(
    events.map((event) => event.data.value),
    ['x', 'y'],
  );
  // Data split over lines is joined with a line feed.
  const split =
    'event: snapshot\ndata: {"artifact_id":"' +
    ART +
    '",\ndata: "path":"n","data_version":1,"exists":false}\n\n';
  const joined = await run([eventStream([split])]);
  assert.equal(joined.events.length, 1);
  // An event that the body ends before its blank line is dropped.
  const cut = await run([
    eventStream([frame('snapshot', state(1, 1)) + 'event: change\ndata: {"x"']),
  ]);
  assert.equal(cut.events.length, 1);
});

test('A line or an event over the limit rejects instead of growing', async () => {
  const long = 'x'.repeat(MAX_EVENT_BYTES + 1);
  const cases = {
    'a long comment': [`: ${long}\n`],
    'a long line without an end': [long],
    'a long line in pieces': [
      long.slice(0, 100_000),
      long.slice(100_000, 200_000),
      long.slice(200_000),
    ],
    'a long data line': [`event: snapshot\ndata: ${long}\n\n`],
  };
  for (const [label, chunks] of Object.entries(cases)) {
    const { outcome, calls } = await run([eventStream(chunks, { end: false })]);
    assert.ok(
      outcome.error instanceof ArtifreeApiError &&
        outcome.error.code === 'invalid_response',
      `${label}: ${String(outcome.error)}`,
    );
    assert.equal(calls.length, 1, label);
  }
  // Many short data lines add up to an event over the limit, although each
  // line is short and the joined data is valid JSON (spaces between tokens).
  const text = JSON.stringify(state(1, 1));
  const padding = Array.from(
    { length: 300 },
    () => `data: ${' '.repeat(1000)}`,
  );
  const lines = [
    `data: ${text.slice(0, 1)}`,
    ...padding,
    `data: ${text.slice(1)}`,
  ];
  const many = [`event: snapshot\n${lines.join('\n')}\n\n`];
  const added = await run([eventStream(many, { end: false })]);
  assert.ok(
    added.outcome.error instanceof ArtifreeApiError &&
      added.outcome.error.code === 'invalid_response',
    String(added.outcome.error),
  );
  assert.equal(added.calls.length, 1);
});

test('A stream that ends cleanly is opened again after a wait that grows', async () => {
  const quick = () => eventStream([frame('snapshot', state(1, 1))]);
  const { events, disconnects, timer, calls } = await run([
    quick(),
    quick(),
    quick(),
    quick(),
    quick(),
  ]);
  assert.equal(calls.length, 6, 'five streams, then the aborting call');
  assert.equal(events.length, 5, 'each connection begins with a snapshot');
  assert.deepEqual(timer.waits, [500, 1000, 2000, 4000, 8000]);
  assert.deepEqual(
    disconnects.map((d) => d.code),
    Array(5).fill('ended'),
  );
  assert.ok(timer.waits.every((wait) => wait >= MIN_WAIT_MS));
});

test('The wait stops growing at its ceiling and starts over after a healthy connection', async () => {
  const timer = timers();
  const quick = eventStream([frame('snapshot', state(1, 1))]);
  const steps = [...Array(10).fill(quick)];
  let seen = 0;
  const { timer: used } = await run(steps, {
    timer,
    onEvent: () => {
      seen += 1;
      // The ninth connection stays open for the healthy time.
      if (seen === 9) timer.advance(HEALTHY_AFTER_MS);
    },
  });
  assert.deepEqual(
    used.waits.slice(0, 8),
    [500, 1000, 2000, 4000, 8000, 16000, 30000, 30000],
  );
  assert.equal(
    used.waits[8],
    500,
    'a healthy connection starts the doubling over',
  );
  assert.ok(used.waits.every((wait) => wait <= MAX_BACKOFF_MS));
});

test('Retry-After is honored up to an hour and at least the floor', async () => {
  const cases = [
    ['7', 7000],
    ['0', MIN_WAIT_MS],
    ['3600', 3_600_000],
    ['99999', RETRY_AFTER_CEILING_MS],
  ];
  for (const [header, wanted] of cases) {
    const { disconnects, calls } = await run([
      failure(429, 'rate_limited', { 'retry-after': header }),
    ]);
    assert.equal(calls.length, 2, header);
    assert.deepEqual(
      disconnects,
      [{ code: 'rate_limited', retryInMs: wanted }],
      header,
    );
  }
  const unavailable = await run([
    failure(503, 'unavailable', { 'retry-after': '10' }),
  ]);
  assert.deepEqual(unavailable.disconnects, [
    { code: 'unavailable', retryInMs: 10_000 },
  ]);
  // A proxy's error page without the API's body still retries.
  const proxy = await run([
    () =>
      new Response('<html>', { status: 502, headers: { 'retry-after': '3' } }),
  ]);
  assert.deepEqual(proxy.disconnects, [
    { code: 'invalid_response', retryInMs: 3000 },
  ]);
});

test('A refusal without Retry-After backs off, and a lost connection retries', async () => {
  const { disconnects, timer } = await run([
    failure(503, 'unavailable'),
    'network',
    failure(429, 'rate_limited'),
  ]);
  assert.deepEqual(
    disconnects.map((d) => d.code),
    ['unavailable', 'network_error', 'rate_limited'],
  );
  assert.deepEqual(timer.waits, [500, 1000, 2000]);
});

test('A failure that a new connection cannot fix rejects at once', async () => {
  for (const [status, code] of [
    [401, 'unauthenticated'],
    [404, 'not_found'],
    [422, 'invalid_request'],
    [400, 'invalid_request'],
  ]) {
    const { outcome, calls, disconnects } = await run([
      failure(status, code),
      eventStream([]),
    ]);
    assert.ok(
      outcome.error instanceof ArtifreeApiError &&
        outcome.error.status === status &&
        outcome.error.code === code,
      `${status}: ${String(outcome.error)}`,
    );
    assert.equal(calls.length, 1, `${status} was retried`);
    assert.deepEqual(disconnects, []);
  }
  const html = await run([
    () =>
      new Response('hello', {
        status: 200,
        headers: { 'content-type': 'text/html' },
      }),
  ]);
  assert.equal(html.outcome.error.code, 'invalid_response');
  assert.equal(html.calls.length, 1);
});

test('A connection that sends nothing is dropped and opened again', async () => {
  let cancelled = 0;
  const silent = eventStream([frame('snapshot', state(1, 1))], {
    end: false,
    onCancel: () => (cancelled += 1),
  });
  const { events, disconnects, calls } = await run([silent, eventStream([])], {
    timer: timers({ idleFiresAt: 2 }),
  });
  // The first read finds the snapshot; the next read has nothing, and the
  // idle watch (already fired) wins it.
  assert.equal(events.length, 1);
  assert.equal(disconnects[0].code, 'network_error');
  assert.equal(calls.length >= 2, true);
  assert.equal(cancelled, 1, 'the silent connection was not closed');
});

test('Aborting resolves, closes the body, and delivers nothing more', async () => {
  let cancelled = 0;
  const controller = new AbortController();
  const events = [];
  const body = eventStream(
    [
      frame('snapshot', state(1, 1)) +
        frame('change', state(2, 2)) +
        frame('change', state(3, 3)),
    ],
    { end: false, onCancel: () => (cancelled += 1) },
  );
  const done = await followData(
    { baseUrl: 'http://api.test', accessToken: 't', fetch: async () => body() },
    ART,
    {
      path: 'n',
      signal: controller.signal,
      timers: timers().impl,
      onEvent: (event) => {
        events.push(event.data.data_version);
        // One chunk holds three events; the first one aborts.
        controller.abort();
      },
    },
  ).then(() => 'resolved');
  assert.equal(done, 'resolved');
  assert.deepEqual(events, [1], 'an event followed the abort');
  assert.equal(cancelled, 1);
});

test('Aborting during a wait resolves without opening another connection', async () => {
  const controller = new AbortController();
  let calls = 0;
  let waiting;
  const inWait = new Promise((resolve) => {
    waiting = resolve;
  });
  const done = followData(
    {
      baseUrl: 'http://api.test',
      accessToken: 't',
      fetch: async () => {
        calls += 1;
        return failure(503, 'unavailable')();
      },
    },
    ART,
    {
      signal: controller.signal,
      timers: {
        now: () => 0,
        sleep: (_milliseconds, signal) =>
          new Promise((resolve) => {
            signal.addEventListener('abort', resolve, { once: true });
            waiting();
          }),
      },
      onEvent: () => undefined,
    },
  );
  await inWait;
  controller.abort();
  await done;
  assert.equal(calls, 1);
});

test('Aborting before the first request sends nothing', async () => {
  const controller = new AbortController();
  controller.abort();
  let calls = 0;
  await followData(
    {
      baseUrl: 'http://api.test',
      accessToken: 't',
      fetch: async () => {
        calls += 1;
        throw new Error('sent');
      },
    },
    ART,
    { signal: controller.signal, onEvent: () => undefined },
  );
  assert.equal(calls, 0);
});

test('What the event handler throws ends following and is not retried', async () => {
  const boom = new Error('boom');
  const { outcome, calls } = await run(
    [eventStream([frame('snapshot', state(1, 1))]), eventStream([])],
    {
      onEvent: () => {
        throw boom;
      },
    },
  );
  assert.equal(outcome.error, boom);
  assert.equal(calls.length, 1);
});
