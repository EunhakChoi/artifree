import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  ArtifreeApiError,
  ArtifreeClient,
  followRoom,
  joinRoom,
  leaveRoom,
  parseRoomSnapshot,
  pollRoom,
  sendRoomEvent,
} from '../../packages/client/dist/index.js';

// The room client (decision 0034) against a scripted network: every number
// and flag a snapshot states is checked against the others, calls refuse
// bad input without sending, and the follower recovers from outages by
// heartbeating again, and from a lost participant by joining again.

const SESSION = 'e'.repeat(32);
const ARTIFACT = `art_${'0'.repeat(32)}`;
const ROOM = `/v1/runtime-sessions/${SESSION}/artifacts/${ARTIFACT}/room/participants`;

function member(participant, tab, self, state = '') {
  return {
    participant,
    tab,
    actor: 'local',
    is_me: participant === self.participant,
    same_tab: tab === self.tab,
    state,
  };
}

/** A valid snapshot for viewer 4 in tab 2 with a frame (5) and tab 1 (3). */
function snapshot(overrides = {}) {
  const self = { participant: 4, tab: 2 };
  return {
    participant: 4,
    epoch: 7,
    tab: 2,
    lease_ms: 15_000,
    members: [member(3, 1, self), member(4, 2, self), member(5, 2, self)],
    events: [],
    head: 10,
    missed: false,
    ...overrides,
  };
}

function event(seq, participant, tab, self = { participant: 4, tab: 2 }) {
  return {
    seq,
    participant,
    tab,
    is_me: participant === self.participant,
    same_tab: tab === self.tab,
    kind: 'cursor',
    data: 'x',
  };
}

const heartbeat = { participant: 4, epoch: 7, after: 8 };
const join = { after: 0 };

test('A consistent snapshot is accepted, with its flags as the server computed them', () => {
  const view = parseRoomSnapshot(
    snapshot({ events: [event(9, 3, 1), event(10, 5, 2)] }),
    heartbeat,
  );
  assert.ok(view);
  assert.deepEqual(
    view.members.map((m) => [m.participant, m.is_me, m.same_tab]),
    [
      [3, false, false],
      [4, true, true],
      [5, false, true],
    ],
  );
  assert.deepEqual(
    view.events.map((e) => [e.seq, e.is_me, e.same_tab]),
    [
      [9, false, false],
      [10, false, true],
    ],
  );
});

test('A resuming join may carry the events its participant missed, and say that some were dropped', () => {
  const view = parseRoomSnapshot(
    snapshot({ events: [event(9, 3, 1), event(10, 5, 2)], missed: true }),
    join,
  );
  assert.ok(view);
  assert.deepEqual(
    view.events.map((e) => e.seq),
    [9, 10],
  );
  assert.equal(view.missed, true);
});

const refused = [
  ['another participant than the call named', { participant: 5 }, heartbeat],
  [
    'a consistent snapshot of another participant than the call named',
    {
      participant: 5,
      members: [
        member(3, 1, { participant: 5, tab: 2 }),
        member(4, 2, { participant: 5, tab: 2 }),
        member(5, 2, { participant: 5, tab: 2 }),
      ],
    },
    heartbeat,
  ],
  [
    'the viewer flag on a sibling instead of the viewer',
    {
      members: [
        member(3, 1, { participant: 4, tab: 2 }),
        { ...member(4, 2, { participant: 4, tab: 2 }), is_me: false },
        { ...member(5, 2, { participant: 4, tab: 2 }), is_me: true },
      ],
    },
    heartbeat,
  ],
  [
    'a viewer listed under another tab than its own',
    {
      members: [
        member(3, 1, { participant: 4, tab: 2 }),
        {
          participant: 4,
          tab: 1,
          actor: 'local',
          is_me: true,
          same_tab: false,
          state: '',
        },
        member(5, 2, { participant: 4, tab: 2 }),
      ],
    },
    heartbeat,
  ],
  [
    'a room that does not list the viewer, flags otherwise right',
    {
      members: [
        member(3, 1, { participant: 4, tab: 2 }),
        member(5, 2, { participant: 4, tab: 2 }),
      ],
    },
    heartbeat,
  ],
  ['another epoch than the call named', { epoch: 8 }, heartbeat],
  ['a head behind the position given', { head: 7 }, heartbeat],
  [
    'no member that is the viewer',
    {
      members: [
        member(3, 1, { participant: 9, tab: 9 }),
        member(5, 2, { participant: 9, tab: 9 }),
      ],
    },
    heartbeat,
  ],
  [
    'two members that are the viewer',
    {
      members: [
        { ...member(4, 2, { participant: 4, tab: 2 }) },
        { ...member(5, 2, { participant: 4, tab: 2 }), is_me: true },
      ],
    },
    heartbeat,
  ],
  [
    'a member that is the viewer under another number',
    {
      members: [
        { ...member(3, 1, { participant: 4, tab: 2 }), is_me: true },
        member(4, 2, { participant: 4, tab: 2 }),
      ],
    },
    heartbeat,
  ],
  [
    'same_tab that disagrees with the tab labels',
    {
      members: [
        { ...member(3, 1, { participant: 4, tab: 2 }), same_tab: true },
        member(4, 2, { participant: 4, tab: 2 }),
      ],
    },
    heartbeat,
  ],
  [
    'members not in participant order',
    {
      members: [
        member(5, 2, { participant: 4, tab: 2 }),
        member(4, 2, { participant: 4, tab: 2 }),
      ],
    },
    heartbeat,
  ],
  [
    'a participant listed twice',
    {
      members: [
        member(4, 2, { participant: 4, tab: 2 }),
        member(4, 2, { participant: 4, tab: 2 }),
      ],
    },
    heartbeat,
  ],
  [
    'seventeen members',
    {
      members: Array.from({ length: 17 }, (_, i) =>
        member(i + 1, 2, { participant: 4, tab: 2 }),
      ),
    },
    heartbeat,
  ],
  [
    'a member state over 256 bytes',
    {
      members: [member(4, 2, { participant: 4, tab: 2 }, 'é'.repeat(129))],
    },
    heartbeat,
  ],
  [
    'an event not newer than the position given',
    { events: [event(8, 3, 1)] },
    heartbeat,
  ],
  [
    'events out of order',
    { events: [event(10, 3, 1), event(9, 3, 1)] },
    heartbeat,
  ],
  ['an event beyond the head', { events: [event(11, 3, 1)] }, heartbeat],
  [
    'an event whose is_me disagrees with its sender',
    { events: [{ ...event(9, 3, 1), is_me: true }] },
    heartbeat,
  ],
  [
    'an event whose same_tab disagrees with its tab',
    { events: [{ ...event(9, 3, 1), same_tab: true }] },
    heartbeat,
  ],
  [
    'an event kind with a space',
    { events: [{ ...event(9, 3, 1), kind: 'a b' }] },
    heartbeat,
  ],
  [
    'an event over 512 bytes',
    { events: [{ ...event(9, 3, 1), data: 'é'.repeat(257) }] },
    heartbeat,
  ],
  [
    '129 events',
    {
      head: 200,
      events: Array.from({ length: 129 }, (_, i) => event(i + 9, 3, 1)),
    },
    heartbeat,
  ],
  ['events in a join not newer than 0', { events: [event(0, 3, 1)] }, join],
  ['events in a join beyond the head', { events: [event(11, 3, 1)] }, join],
  ['a zero lease', { lease_ms: 0 }, heartbeat],
  ['a lease over an hour', { lease_ms: 3_600_001 }, heartbeat],
  ['a fractional participant', { participant: 4.5 }, join],
  ['an unsafe epoch', { epoch: 2 ** 53 }, join],
  ['a negative head', { head: -1 }, join],
  ['a string where a number is', { tab: '2' }, join],
  ['a zero participant', { participant: 0 }, join],
  ['missed that is not a boolean', { missed: 'no' }, heartbeat],
];

for (const [name, change, expectation] of refused) {
  test(`A snapshot is refused with ${name}`, () => {
    assert.equal(
      parseRoomSnapshot({ ...snapshot(), ...change }, expectation),
      undefined,
    );
  });
}

test('Anything but an object is refused, and an unknown field is not copied', () => {
  for (const value of [undefined, null, [], 'x', 4, true])
    assert.equal(parseRoomSnapshot(value, join), undefined);
  const view = parseRoomSnapshot({ ...snapshot(), extra: 1 }, heartbeat);
  assert.ok(view);
  assert.equal('extra' in view, false);
});

/** A client whose room calls are answered by `respond(request)`. */
async function clientWith(respond) {
  const requests = [];
  const client = new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:7410',
    accessToken: 'token',
    fetch: async (url, init) => {
      if (url.endsWith('/v1/runtime-sessions'))
        return Response.json({ session_id: SESSION });
      const path = new URL(url).pathname;
      const request = {
        path,
        method: init?.method,
        body: init?.body === undefined ? undefined : JSON.parse(init.body),
        authorization: init?.headers?.authorization,
        signal: init?.signal,
      };
      requests.push(request);
      return respond(request);
    },
  });
  await client.openRuntimeSession();
  return { client, requests };
}

const gone = () =>
  Response.json(
    { error: { code: 'not_found', message: 'no such participant' } },
    { status: 404 },
  );

test('A join, a poll and an event are sent to the tab and artifact they name', async () => {
  const { client, requests } = await clientWith((request) =>
    request.path.endsWith('/events')
      ? Response.json({ seq: 11 })
      : Response.json(snapshot()),
  );
  const joined = await joinRoom(client, ARTIFACT, {
    slot: 'page',
    state: 'reading',
  });
  assert.equal(joined.participant, 4);
  const who = { artifactId: ARTIFACT, participant: 4, epoch: 7 };
  await pollRoom(client, who, { after: 10, state: 'editing' });
  assert.equal(
    await sendRoomEvent(client, who, { kind: 'cursor', data: 'x' }),
    11,
  );
  assert.deepEqual(
    requests.map((r) => [r.method, r.path, r.body]),
    [
      ['POST', ROOM, { slot: 'page', state: 'reading' }],
      ['POST', `${ROOM}/4/7/heartbeat`, { after: 10, state: 'editing' }],
      ['POST', `${ROOM}/4/7/events`, { kind: 'cursor', data: 'x' }],
    ],
  );
  assert.ok(requests.every((r) => r.authorization === 'Bearer token'));
});

test('A poll must be answered for the participant and epoch it named', async () => {
  const { client } = await clientWith(() =>
    Response.json(snapshot({ epoch: 8 })),
  );
  await assert.rejects(
    pollRoom(
      client,
      { artifactId: ARTIFACT, participant: 4, epoch: 7 },
      { after: 0 },
    ),
    (error) =>
      error instanceof ArtifreeApiError && error.code === 'invalid_response',
  );
});

test('Bad input is rejected as a promise, before anything is sent', async () => {
  const { client, requests } = await clientWith(() =>
    Response.json(snapshot()),
  );
  const who = { artifactId: ARTIFACT, participant: 4, epoch: 7 };
  const calls = [
    () => joinRoom(client, ARTIFACT, { slot: 'has space' }),
    () => joinRoom(client, ARTIFACT, { slot: '' }),
    () => joinRoom(client, ARTIFACT, { slot: 'a', state: 'é'.repeat(129) }),
    () => joinRoom(client, ARTIFACT, null),
    () => pollRoom(client, { ...who, participant: 0 }, { after: 0 }),
    () => pollRoom(client, { ...who, epoch: 1.5 }, { after: 0 }),
    () => pollRoom(client, who, { after: -1 }),
    () => pollRoom(client, who, { after: 2 ** 53 }),
    () => pollRoom(client, null, { after: 0 }),
    () => sendRoomEvent(client, who, { kind: 'bad kind', data: '' }),
    () => sendRoomEvent(client, who, { kind: 'k', data: 'é'.repeat(257) }),
    () => leaveRoom(client, { ...who, epoch: 0 }),
  ];
  for (const call of calls) {
    let promise;
    assert.doesNotThrow(() => {
      promise = call();
    });
    await assert.rejects(promise, (error) => error instanceof Error);
  }
  assert.equal(requests.length, 0);
});

test('The 512th byte of an event and the 256th byte of a state are sent', async () => {
  const { client, requests } = await clientWith((request) =>
    request.path.endsWith('/events')
      ? Response.json({ seq: 1 })
      : Response.json(snapshot()),
  );
  const who = { artifactId: ARTIFACT, participant: 4, epoch: 7 };
  await sendRoomEvent(client, who, {
    kind: 'k'.repeat(32),
    data: 'é'.repeat(256),
  });
  await joinRoom(client, ARTIFACT, {
    slot: 'x'.repeat(32),
    state: 'é'.repeat(128),
  });
  assert.equal(requests.length, 2);
});

test('An event receipt must be a positive safe integer', async () => {
  for (const seq of [0, -1, 1.5, 2 ** 53, '1', null]) {
    const { client } = await clientWith(() => Response.json({ seq }));
    await assert.rejects(
      sendRoomEvent(
        client,
        { artifactId: ARTIFACT, participant: 4, epoch: 7 },
        { kind: 'k', data: '' },
      ),
      (error) => error.code === 'invalid_response',
      `seq ${seq}`,
    );
  }
});

test('Leaving is confirmed by no content, and a participant already gone is not an error', async () => {
  const who = { artifactId: ARTIFACT, participant: 4, epoch: 7 };
  const { client, requests } = await clientWith(
    () => new Response(null, { status: 204 }),
  );
  await leaveRoom(client, who);
  assert.equal(requests[0].path, `${ROOM}/4/7/leave`);
  const missing = await clientWith(gone);
  await leaveRoom(missing.client, who);
  const odd = await clientWith(() => Response.json({}));
  await assert.rejects(
    leaveRoom(odd.client, who),
    (error) => error.code === 'invalid_response',
  );
  const down = await clientWith(() =>
    Response.json(
      { error: { code: 'unavailable', message: 'x' } },
      { status: 503 },
    ),
  );
  await assert.rejects(
    leaveRoom(down.client, who),
    (error) => error.status === 503,
  );
});

/**
 * Follows a room through scripted answers. `script` is called for each room
 * call and returns a snapshot body or a Response, or throws; the follower is
 * stopped after `sleeps` waits. Waiting is recorded, never done.
 */
async function follow(script, { sleeps = 4, options = {} } = {}) {
  const calls = [];
  const waits = [];
  const failures = [];
  const received = [];
  const stops = new AbortController();
  const { client } = await clientWith((request) => {
    calls.push(request.path.replace(ROOM, ''));
    if (request.path.endsWith('/leave'))
      return new Response(null, { status: 204 });
    const answer = script(request, calls.length);
    return answer instanceof Response ? answer : Response.json(answer);
  });
  await followRoom(client, {
    artifactId: ARTIFACT,
    slot: 'page',
    signal: stops.signal,
    onSnapshot: (followed) => received.push(followed),
    onDisconnect: (error, retryInMs) => failures.push([error.code, retryInMs]),
    sleep: async (milliseconds) => {
      waits.push(milliseconds);
      if (waits.length >= sleeps) stops.abort();
    },
    ...options,
  });
  return { calls, waits, failures, received };
}

const unavailable = () =>
  Response.json(
    { error: { code: 'unavailable', message: 'x' } },
    { status: 503 },
  );

test('The follower joins, then renews the lease at a fifth of it from the last head', async () => {
  let head = 10;
  const { calls, waits, received } = await follow((request) => {
    if (!request.path.endsWith('/heartbeat')) return snapshot({ head });
    head += 1;
    return snapshot({ head, events: [event(head, 3, 1)] });
  });
  assert.deepEqual(calls, [
    '',
    '/4/7/heartbeat',
    '/4/7/heartbeat',
    '/4/7/heartbeat',
    '/4/7/leave',
  ]);
  assert.deepEqual(waits, [3_000, 3_000, 3_000, 3_000]);
  assert.deepEqual(
    received.map((r) => [r.joined, r.snapshot.head, r.snapshot.events.length]),
    [
      [true, 10, 0],
      [false, 11, 1],
      [false, 12, 1],
      [false, 13, 1],
    ],
  );
});

test('A heartbeat reads from the head of the previous snapshot', async () => {
  const afters = [];
  await follow((request) => {
    if (!request.path.endsWith('/heartbeat')) return snapshot({ head: 10 });
    afters.push(request.body.after);
    const head = 10 + afters.length;
    return snapshot({ head, events: [event(head, 3, 1)] });
  });
  assert.deepEqual(afters, [10, 11, 12]);
});

test('A short outage costs nothing: the same participant is renewed, not joined again', async () => {
  const { calls, waits, failures, received } = await follow(
    (request, count) =>
      count === 2 || count === 3 ? unavailable() : snapshot(),
    { sleeps: 4 },
  );
  assert.deepEqual(calls.slice(0, 4), [
    '',
    '/4/7/heartbeat',
    '/4/7/heartbeat',
    '/4/7/heartbeat',
  ]);
  assert.deepEqual(waits, [3_000, 500, 1_000, 3_000]);
  assert.deepEqual(failures, [
    ['unavailable', 500],
    ['unavailable', 1_000],
  ]);
  assert.equal(received.filter((r) => r.joined).length, 1);
});

test('A network failure backs off from 500 ms and doubles, and Retry-After wins', async () => {
  const { waits } = await follow(
    (request, count) => {
      if (count === 1 || count > 5) return snapshot();
      if (count <= 4) throw new TypeError('fetch failed');
      return new Response(
        JSON.stringify({ error: { code: 'rate_limited', message: 'x' } }),
        { status: 429, headers: { 'retry-after': '7' } },
      );
    },
    { sleeps: 6 },
  );
  assert.deepEqual(waits, [3_000, 500, 1_000, 2_000, 7_000, 3_000]);
});

test('The backoff stops growing at its limit and starts again after a success', async () => {
  const { waits } = await follow(
    (request, count) => {
      if (count === 1 || count === 8) return snapshot();
      if (count < 8) throw new TypeError('fetch failed');
      if (count === 9) throw new TypeError('fetch failed');
      return snapshot();
    },
    { sleeps: 9, options: { retryDelayMs: { min: 500, max: 2_000 } } },
  );
  assert.deepEqual(
    waits,
    [3_000, 500, 1_000, 2_000, 2_000, 2_000, 2_000, 3_000, 500],
  );
});

test('A participant that is gone is replaced by a join at once, and the new head is used', async () => {
  const replacement = snapshot({
    participant: 9,
    epoch: 2,
    head: 40,
    members: [member(9, 2, { participant: 9, tab: 2 })],
  });
  const afters = [];
  const { calls, waits, received } = await follow(
    (request, count) => {
      if (request.path.endsWith('/heartbeat')) {
        afters.push(request.body.after);
        return count === 2 ? gone() : replacement;
      }
      return count === 1 ? snapshot({ head: 10 }) : replacement;
    },
    { sleeps: 3 },
  );
  assert.deepEqual(calls.slice(0, 4), [
    '',
    '/4/7/heartbeat',
    '',
    '/9/2/heartbeat',
  ]);
  // No wait between the lost heartbeat and the join.
  assert.deepEqual(waits, [3_000, 3_000, 3_000]);
  assert.deepEqual(
    received.map((r) => [r.joined, r.snapshot.participant]),
    [
      [true, 4],
      [true, 9],
      [false, 9],
    ],
  );
  assert.deepEqual(afters, [10, 40]);
});

test('A failure retrying cannot fix ends following with that failure', async () => {
  for (const [status, code] of [
    [404, 'runtime_session_expired'],
    [401, 'unauthenticated'],
    [400, 'invalid_request'],
  ]) {
    const stops = new AbortController();
    const { client } = await clientWith(() =>
      Response.json({ error: { code, message: 'x' } }, { status }),
    );
    await assert.rejects(
      followRoom(client, {
        artifactId: ARTIFACT,
        slot: 'page',
        signal: stops.signal,
        onSnapshot: () => assert.fail('nothing was joined'),
        sleep: async () => assert.fail('no retry'),
      }),
      (error) => error.code === code,
      code,
    );
  }
});

test('A body that breaks the contract ends following', async () => {
  const stops = new AbortController();
  const { client } = await clientWith(() =>
    Response.json(snapshot({ lease_ms: 0 })),
  );
  await assert.rejects(
    followRoom(client, {
      artifactId: ARTIFACT,
      slot: 'page',
      signal: stops.signal,
      onSnapshot: () => assert.fail('a zero lease is not a snapshot'),
      sleep: async () => assert.fail('no retry'),
    }),
    (error) => error.code === 'invalid_response',
  );
});

test('What the handler throws ends following and is never retried, whatever it is', async () => {
  for (const thrown of [
    new Error('handler'),
    new ArtifreeApiError('handler api error', {
      status: 503,
      code: 'unavailable',
    }),
  ]) {
    const stops = new AbortController();
    const { client, requests } = await clientWith(() =>
      Response.json(snapshot()),
    );
    await assert.rejects(
      followRoom(client, {
        artifactId: ARTIFACT,
        slot: 'page',
        signal: stops.signal,
        onSnapshot: () => {
          throw thrown;
        },
        sleep: async () => assert.fail('no retry'),
      }),
      (error) => error === thrown,
    );
    assert.equal(requests.length, 1);
  }
});

test('State is read for the join and for every heartbeat', async () => {
  const bodies = [];
  let count = 0;
  await follow(
    (request) => {
      bodies.push(request.body);
      return snapshot();
    },
    {
      sleeps: 3,
      options: { state: () => `state-${++count}` },
    },
  );
  assert.deepEqual(
    bodies.map((b) => b.state),
    ['state-1', 'state-2', 'state-3'],
  );
});

test('Aborting leaves the room once, and a failed leave is ignored', async () => {
  const { calls } = await follow(() => snapshot(), { sleeps: 1 });
  assert.deepEqual(
    calls.filter((c) => c.endsWith('/leave')),
    ['/4/7/leave'],
  );
  const stops = new AbortController();
  const { client } = await clientWith((request) =>
    request.path.endsWith('/leave')
      ? Response.json(
          { error: { code: 'internal', message: 'x' } },
          { status: 500 },
        )
      : Response.json(snapshot()),
  );
  await followRoom(client, {
    artifactId: ARTIFACT,
    slot: 'page',
    signal: stops.signal,
    onSnapshot: () => {},
    sleep: async () => stops.abort(),
  });
});

test('An abort during the first join leaves nothing to leave and resolves', async () => {
  const stops = new AbortController();
  const { client, requests } = await clientWith(() => {
    stops.abort();
    throw new TypeError('aborted');
  });
  await followRoom(client, {
    artifactId: ARTIFACT,
    slot: 'page',
    signal: stops.signal,
    onSnapshot: () => assert.fail('no snapshot'),
    sleep: async () => assert.fail('no wait'),
  });
  assert.equal(requests.length, 1);
});

/** A cleanup timer that never expires by itself, only when cancelled. */
const patientTimer = (_milliseconds, signal) =>
  new Promise((resolve) => signal.addEventListener('abort', () => resolve()));

test(
  'A leave that never answers cannot keep the follower from settling after an abort',
  { timeout: 5_000 },
  async () => {
    const stops = new AbortController();
    const timers = [];
    let asked = () => {};
    const leaveAsked = new Promise((resolve) => {
      asked = resolve;
    });
    const { client, requests } = await clientWith((request) => {
      if (!request.path.endsWith('/leave')) return Response.json(snapshot());
      asked();
      return new Promise(() => {});
    });
    await followRoom(client, {
      artifactId: ARTIFACT,
      slot: 'page',
      signal: stops.signal,
      onSnapshot: () => {},
      sleep: async () => stops.abort(),
      // The bound is reached once the leave was sent and not answered.
      cleanupTimer: async (milliseconds) => {
        timers.push(milliseconds);
        await leaveAsked;
      },
    });
    assert.deepEqual(timers, [3_000], 'the leave is bounded by three seconds');
    assert.deepEqual(
      requests.map((r) => r.path.replace(ROOM, '')),
      ['', '/4/7/leave'],
    );
  },
);

test(
  'An abort while the first join is in flight still leaves the participant that join made',
  { timeout: 5_000 },
  async () => {
    const stops = new AbortController();
    let answer = () => assert.fail('no join was sent');
    let started = () => {};
    const inFlight = new Promise((resolve) => {
      started = resolve;
    });
    const { client, requests } = await clientWith((request) => {
      if (request.path.endsWith('/leave'))
        return new Response(null, { status: 204 });
      return new Promise((resolve, reject) => {
        answer = () => resolve(Response.json(snapshot()));
        // A transport that honors the signal the follower passed on.
        request.signal.addEventListener('abort', () =>
          reject(new DOMException('aborted', 'AbortError')),
        );
        started();
      });
    });
    const following = followRoom(client, {
      artifactId: ARTIFACT,
      slot: 'page',
      signal: stops.signal,
      onSnapshot: () => assert.fail('nothing is handed over after an abort'),
      sleep: async () => assert.fail('no wait'),
      cleanupTimer: patientTimer,
    });
    await inFlight;
    stops.abort();
    answer();
    await following;
    assert.deepEqual(
      requests.map((r) => r.path.replace(ROOM, '')),
      ['', '/4/7/leave'],
      'the participant the join made is left',
    );
  },
);

test(
  'A first join that does not answer within the bound after an abort is given up',
  { timeout: 5_000 },
  async () => {
    const stops = new AbortController();
    let started = () => {};
    const inFlight = new Promise((resolve) => {
      started = resolve;
    });
    const { client, requests } = await clientWith(
      (request) =>
        new Promise((_resolve, reject) => {
          request.signal.addEventListener('abort', () =>
            reject(new DOMException('aborted', 'AbortError')),
          );
          started();
        }),
    );
    const following = followRoom(client, {
      artifactId: ARTIFACT,
      slot: 'page',
      signal: stops.signal,
      onSnapshot: () => assert.fail('no snapshot'),
      sleep: async () => assert.fail('no wait'),
      cleanupTimer: async () => {},
    });
    await inFlight;
    stops.abort();
    await following;
    assert.equal(requests.length, 1, 'no leave: the participant is unknown');
  },
);

const heartbeatGone = (request) =>
  request.path.endsWith('/heartbeat') ? gone() : snapshot();

test('A participant lost again and again is joined again only after a growing wait, reported each time', async () => {
  const { waits, failures, received } = await follow(heartbeatGone, {
    sleeps: 5,
  });
  // join, wait; lost: join at once, wait; lost again: 500 ms, join, wait;
  // lost again: 1 s.
  assert.deepEqual(waits, [3_000, 3_000, 500, 3_000, 1_000]);
  assert.deepEqual(failures, [
    ['not_found', 500],
    ['not_found', 1_000],
  ]);
  assert.deepEqual(
    received.map((r) => r.joined),
    [true, true, true],
  );
});

test('The wait after a participant is lost again and again stops growing at the maximum', async () => {
  const { waits, failures } = await follow(heartbeatGone, {
    sleeps: 7,
    options: { retryDelayMs: { min: 500, max: 1_000 } },
  });
  assert.deepEqual(waits, [3_000, 3_000, 500, 3_000, 1_000, 3_000, 1_000]);
  assert.deepEqual(failures, [
    ['not_found', 500],
    ['not_found', 1_000],
    ['not_found', 1_000],
  ]);
});

test('A loss after a heartbeat that worked is joined again at once, whenever it happens', async () => {
  let heartbeats = 0;
  const { waits, failures } = await follow(
    (request) => {
      if (!request.path.endsWith('/heartbeat')) return snapshot();
      heartbeats += 1;
      return heartbeats % 2 === 1 ? snapshot() : gone();
    },
    { sleeps: 7 },
  );
  assert.deepEqual(waits, [3_000, 3_000, 3_000, 3_000, 3_000, 3_000, 3_000]);
  assert.deepEqual(failures, []);
});
