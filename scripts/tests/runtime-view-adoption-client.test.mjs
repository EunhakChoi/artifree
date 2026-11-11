import assert from 'node:assert/strict';
import { afterEach, beforeEach, describe, mock, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';
import {
  ArtifreeApiError,
  ArtifreeClient,
  isAdoptionRequest,
} from '../../packages/client/dist/index.js';

// Decision 0054 at the client and the request gate: an adoption is marked
// (rule 8), keeps its connection (rule 11) and is sent as a write. The gate is
// web code without DOM or React imports; bundle it as it ships.
const bundle = await build({
  entryPoints: [
    fileURLToPath(
      new URL('../../apps/web/src/artifacts/request-gate.ts', import.meta.url),
    ),
  ],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const { RequestGate, requestKind } = await import(
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
);

const BASE = 'http://127.0.0.1:7410';
const ARTIFACT = `art_${'0'.repeat(32)}`;
const VIEW_ID = 'd'.repeat(32);
const SESSION = 'e'.repeat(32);
const FRESH = 'f'.repeat(32);
const RUNTIME_VIEWS = `/v1/artifacts/${ARTIFACT}/revisions/1/runtime-views`;
const FRAME = `${RUNTIME_VIEWS}/${VIEW_ID}/2/source/read`;
const PREVIEWS = `/v1/artifacts/${ARTIFACT}/revisions/1/previews`;
const BASES = [
  'http://h',
  'http://h/',
  'https://api.example/prefix',
  // A prefix that itself looks like a version segment.
  'https://api.example/v1/api',
];

/** `base` joined with `path` the way the client does. */
const joined = (base, path) => `${base.replace(/\/+$/, '')}${path}`;

/** The runtime view the server answers, as the client parses it. */
function viewResponse(session, generation, lifecycle) {
  return Response.json(
    {
      url: `http://127.0.0.1:7411/r/${'b'.repeat(64)}/`,
      enabled: true,
      session_id: session,
      artifact_id: ARTIFACT,
      revision: 1,
      head_revision: 1,
      sdk: 'artifree.app.v1',
      lifecycle,
      declared: ['metadata.read'],
      supported: ['metadata.read'],
      granted: [],
      grant_version: 0,
      view_id: VIEW_ID,
      generation,
      initialization: 'c'.repeat(64),
      expires_at_ms: 1_000_000,
      lease_ms: 900_000,
    },
    { status: 201 },
  );
}

/** The answer of a server that no longer knows the connection. */
function expiredSession() {
  return Response.json(
    {
      error: {
        code: 'runtime_session_expired',
        message: 'Connection expired.',
      },
    },
    { status: 404 },
  );
}

/**
 * A client with an open runtime connection. `answer(init, session)` answers
 * each request except a connection open, where `session` is the session the
 * request names. `calls` lists every request; `opens()` counts connections.
 */
async function connected(answer) {
  const calls = [];
  let opened = 0;
  const client = new ArtifreeClient({
    baseUrl: BASE,
    accessToken: 'token',
    fetch: async (url, init) => {
      calls.push({ url, init });
      if (url.endsWith('/v1/runtime-sessions'))
        return Response.json({ session_id: ++opened === 1 ? SESSION : FRESH });
      return answer(init, JSON.parse(init.body).session_id);
    },
  });
  await client.openRuntimeSession();
  return { client, calls, opens: () => opened };
}

/** The view requests among `calls`. */
function viewRequests(calls) {
  return calls.filter(({ url }) => url.endsWith('/runtime-views'));
}

/** The rejection of `promise`, or undefined when it resolves. */
function rejectionOf(promise) {
  return promise.then(
    () => undefined,
    (error) => error,
  );
}

// A plain view and an adoption are one POST to the same URL. Only the
// adoption's body names the generation it replaces (decision 0054, rules 3
// and 14), and the mark it carries is never serialized.
const VIEW_REQUESTS = [
  {
    name: 'a plain view',
    options: {},
    generation: 1,
    lifecycle: 'restart',
    adoption: false,
    body: { session_id: SESSION, view_id: VIEW_ID, generation: 1 },
  },
  {
    name: 'an adoption',
    options: { adopt: { generation: 1 } },
    generation: 2,
    lifecycle: 'adopt',
    adoption: true,
    body: {
      session_id: SESSION,
      view_id: VIEW_ID,
      generation: 2,
      adopt: { generation: 1 },
    },
  },
];

for (const kind of VIEW_REQUESTS)
  test(`${kind.name} is one POST to the same URL, with only its own body`, async () => {
    const { client, calls } = await connected(() =>
      viewResponse(SESSION, kind.generation, kind.lifecycle),
    );
    const view = await client.createRuntimeView(
      ARTIFACT,
      1,
      VIEW_ID,
      kind.generation,
      kind.options,
    );
    assert.equal(view.lifecycle, kind.lifecycle);
    const requests = viewRequests(calls);
    assert.equal(requests.length, 1, 'one request, with no retry');
    const [{ url, init }] = requests;
    assert.equal(url, `${BASE}${RUNTIME_VIEWS}`);
    assert.equal(init.method, 'POST');
    assert.deepEqual(JSON.parse(init.body), kind.body);
    assert.deepEqual(Object.keys(init.headers).sort(), [
      'authorization',
      'content-type',
    ]);
    assert.equal(init.headers.authorization, 'Bearer token');
    assert.equal(init.headers['content-type'], 'application/json');
    assert.equal(isAdoptionRequest(init), kind.adoption);
  });

test('An adoption whose connection expired fails, and no connection is opened', async () => {
  const { client, calls, opens } = await connected((init, session) =>
    session === SESSION ? expiredSession() : viewResponse(FRESH, 2, 'adopt'),
  );
  const error = await rejectionOf(
    client.createRuntimeView(ARTIFACT, 1, VIEW_ID, 2, {
      adopt: { generation: 1 },
    }),
  );
  assert.equal(opens(), 1, 'only the initial connection was opened');
  assert.ok(error instanceof ArtifreeApiError, String(error));
  assert.equal(error.code, 'runtime_session_expired');
  assert.equal(error.status, 404);
  assert.equal(viewRequests(calls).length, 1, 'the adoption was not retried');
});

test('A plain view whose connection expired replaces it once and succeeds', async () => {
  const { client, calls, opens } = await connected((init, session) =>
    session === SESSION ? expiredSession() : viewResponse(FRESH, 1, 'restart'),
  );
  const view = await client.createRuntimeView(ARTIFACT, 1, VIEW_ID, 1, {});
  assert.equal(view.session_id, FRESH);
  assert.equal(opens(), 2);
  assert.deepEqual(
    viewRequests(calls).map(({ init }) => JSON.parse(init.body).session_id),
    [SESSION, FRESH],
  );
});

test('An adoption is answered only with the adopt lifecycle', async () => {
  const restart = await connected(() => viewResponse(SESSION, 2, 'restart'));
  await assert.rejects(
    restart.client.createRuntimeView(ARTIFACT, 1, VIEW_ID, 2, {
      adopt: { generation: 1 },
    }),
    { code: 'invalid_response' },
  );
  const adopted = await connected(() => viewResponse(SESSION, 2, 'adopt'));
  const view = await adopted.client.createRuntimeView(ARTIFACT, 1, VIEW_ID, 2, {
    adopt: { generation: 1 },
  });
  assert.equal(view.lifecycle, 'adopt');
});

for (const generation of [0, 1.5, 2, 3])
  test(`An adopted generation of ${generation} is refused, when the new generation is 2, before anything is sent`, async () => {
    const { client, calls } = await connected(() =>
      viewResponse(SESSION, 2, 'adopt'),
    );
    const before = calls.length;
    await assert.rejects(
      client.createRuntimeView(ARTIFACT, 1, VIEW_ID, 2, {
        adopt: { generation },
      }),
      RangeError,
    );
    assert.equal(calls.length, before, 'no request was made');
  });

test('Only a bare runtime-views POST is upgraded by the adoption mark', () => {
  for (const [method, path, adoption, kind] of [
    ['POST', RUNTIME_VIEWS, true, 'write'],
    ['POST', RUNTIME_VIEWS, false, 'read'],
    // Omitted means no adoption.
    ['POST', RUNTIME_VIEWS, undefined, 'read'],
    ['POST', PREVIEWS, true, 'read'],
    ['POST', FRAME, true, 'frame'],
  ])
    assert.equal(
      requestKind(method, path, adoption),
      kind,
      `${method} ${path} ${adoption}`,
    );
  for (const base of BASES)
    assert.equal(
      requestKind('POST', joined(base, RUNTIME_VIEWS), true),
      'write',
      base,
    );
  // The mark changes no other request: each keeps the kind it has without it.
  for (const [method, path] of [
    ['GET', RUNTIME_VIEWS],
    ['PUT', RUNTIME_VIEWS],
    ['POST', `/v1/artifacts/${ARTIFACT}/runtime-views`],
    ['POST', `${RUNTIME_VIEWS}/${VIEW_ID}`],
    ['GET', '/v1/changes'],
  ])
    assert.equal(
      requestKind(method, path, true),
      requestKind(method, path),
      `${method} ${path}`,
    );
});

/** Lets settled promises run their continuations. */
async function settle() {
  for (let round = 0; round < 10; round += 1) await Promise.resolve();
}

/** Moves the mocked clock forward, then lets the gate react. */
async function advance(milliseconds) {
  mock.timers.tick(milliseconds);
  await settle();
}

/** A page whose visibility the test sets. */
function page() {
  const listeners = new Set();
  let visible = true;
  return {
    visibility: {
      visible: () => visible,
      subscribe(onChange) {
        listeners.add(onChange);
        return () => listeners.delete(onChange);
      },
    },
    show(next) {
      visible = next;
      for (const listener of listeners) listener();
    },
  };
}

/** A server the test answers by hand; `sent` lists requests as they arrive. */
function server() {
  const sent = [];
  return {
    sent,
    paths: () => sent.map(({ path }) => path),
    send: (input, init) =>
      new Promise((resolve) => {
        sent.push({
          path: `${init?.method ?? 'GET'} ${new URL(input, 'http://x').pathname}`,
          resolve,
        });
      }),
    answer(index, response = new Response('{}')) {
      sent[index].resolve(response);
    },
  };
}

/** A refusal for too many requests, with `Retry-After` of `seconds`. */
function limited(seconds) {
  return new Response('{"error":{"code":"rate_limited"}}', {
    status: 429,
    headers: { 'retry-after': String(seconds) },
  });
}

/**
 * A page, and a gate over a server the test answers. Each send the gate makes
 * is recorded in `trace` as the URL it receives.
 */
function setup() {
  const view = page();
  const api = server();
  const trace = [];
  const gate = new RequestGate(view.visibility, (input, init) => {
    trace.push(`send ${init?.method ?? 'GET'} ${input}`);
    return api.send(input, init);
  });
  return { view, api, gate, trace };
}

/**
 * A client whose runtime connection is open through `gate`. Its opening
 * request is answered and then forgotten, so `api` indexes start after it.
 */
async function connectedThrough(gate, api) {
  const client = new ArtifreeClient({
    baseUrl: '',
    accessToken: 'token',
    fetch: gate.fetch,
  });
  const opening = client.openRuntimeSession();
  await settle();
  api.answer(0, Response.json({ session_id: SESSION }));
  await opening;
  api.sent.shift();
  return client;
}

describe('the request gate and a view adoption', () => {
  beforeEach(() => mock.timers.enable({ apis: ['setTimeout', 'Date'] }));
  afterEach(() => mock.timers.reset());

  test('A hidden page holds a plain view but sends an adoption at once, after its dispatch notice', async () => {
    const { view, api, gate, trace } = setup();
    const client = await connectedThrough(gate, api);
    trace.length = 0;
    view.show(false);
    const plain = client.createRuntimeView(ARTIFACT, 1, VIEW_ID, 1, {
      onDispatch: () => trace.push('plain dispatch'),
    });
    await settle();
    assert.deepEqual(trace, [], 'a plain view is a read: held while hidden');
    const adoption = client.createRuntimeView(ARTIFACT, 1, VIEW_ID, 2, {
      adopt: { generation: 1 },
      onDispatch: () => trace.push('dispatch'),
    });
    await settle();
    assert.deepEqual(
      trace,
      ['dispatch', `send POST ${RUNTIME_VIEWS}`],
      'an adoption is a write: notified, then sent, while hidden',
    );
    view.show(true);
    await settle();
    assert.deepEqual(trace, [
      'dispatch',
      `send POST ${RUNTIME_VIEWS}`,
      'plain dispatch',
      `send POST ${RUNTIME_VIEWS}`,
    ]);
    api.answer(0, viewResponse(SESSION, 2, 'adopt'));
    api.answer(1, viewResponse(SESSION, 1, 'restart'));
    assert.equal((await adoption).lifecycle, 'adopt');
    assert.equal((await plain).generation, 1);
  });

  test('An adoption is sent while a read runs, and its refusal holds later reads', async () => {
    const { api, gate } = setup();
    const client = await connectedThrough(gate, api);
    const running = gate.fetch('/v1/artifacts');
    await settle();
    const adoption = client.createRuntimeView(ARTIFACT, 1, VIEW_ID, 2, {
      adopt: { generation: 1 },
    });
    await settle();
    assert.deepEqual(
      api.paths(),
      ['GET /v1/artifacts', `POST ${RUNTIME_VIEWS}`],
      'the adoption is not queued behind the running read',
    );
    const later = gate.fetch('/v1/artifacts/a');
    api.answer(1, limited(30));
    await assert.rejects(adoption, {
      code: 'rate_limited',
      retryAfterSeconds: 30,
    });
    api.answer(0);
    await (await running).text();
    await settle();
    assert.deepEqual(
      api.paths(),
      ['GET /v1/artifacts', `POST ${RUNTIME_VIEWS}`],
      'the refusal holds the next read after the running one ends',
    );
    await advance(29_999);
    assert.equal(api.sent.length, 2);
    await advance(1);
    assert.equal(api.paths().at(-1), 'GET /v1/artifacts/a');
    api.answer(2, Response.json({ artifacts: [] }));
    assert.equal((await later).status, 200);
  });
});
