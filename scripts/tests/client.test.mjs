import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  ArtifreeApiError,
  ArtifreeClient,
  encodeFile,
  newIdempotencyKey,
  newRandomId,
} from '../../packages/client/dist/index.js';

const artifact = {
  id: `art_${'0'.repeat(32)}`,
  kind: 'app',
  title: 'Demo',
  pinned: false,
  current_revision: 1,
  metadata_revision: 1,
  deleted_at_ms: null,
  created_at_ms: 1,
  updated_at_ms: 1,
};

test('Random identifiers work without randomUUID and never use a weak fallback', () => {
  const originalUuid = Object.getOwnPropertyDescriptor(crypto, 'randomUUID');
  const originalValues = Object.getOwnPropertyDescriptor(
    crypto,
    'getRandomValues',
  );
  try {
    Object.defineProperty(crypto, 'randomUUID', {
      configurable: true,
      value: undefined,
    });
    Object.defineProperty(crypto, 'getRandomValues', {
      configurable: true,
      value: (bytes) => {
        assert.equal(bytes.length, 16);
        bytes.set([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]);
        return bytes;
      },
    });
    assert.equal(newRandomId(), '000102030405060708090a0b0c0d0e0f');
    assert.equal(newIdempotencyKey(), 'key-00010203-0405060708090a0b0c0d0e0f');
    Object.defineProperty(crypto, 'getRandomValues', {
      configurable: true,
      value: () => {
        throw new Error('secure randomness unavailable');
      },
    });
    assert.throws(newRandomId, /secure randomness unavailable/);
    assert.throws(newIdempotencyKey, /secure randomness unavailable/);
  } finally {
    if (originalUuid === undefined) delete crypto.randomUUID;
    else Object.defineProperty(crypto, 'randomUUID', originalUuid);
    if (originalValues === undefined) delete crypto.getRandomValues;
    else Object.defineProperty(crypto, 'getRandomValues', originalValues);
  }
});

/** A client whose every request is answered by `respond`. */
function clientWith(respond) {
  return new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:7410',
    accessToken: 'token',
    fetch: async () => respond(),
  });
}

async function runtimeClientWith(respond) {
  const client = new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:7410',
    accessToken: 'token',
    fetch: async (url) =>
      url.endsWith('/v1/runtime-sessions')
        ? Response.json({ session_id: 'e'.repeat(32) })
        : respond(),
  });
  await client.openRuntimeSession();
  return client;
}

/** A response whose body ends with a stream error after `prefix`. */
function cutOff(status, prefix) {
  const body = new ReadableStream({
    start(controller) {
      controller.enqueue(new TextEncoder().encode(prefix));
      controller.error(new Error('connection reset'));
    },
  });
  return new Response(body, {
    status,
    headers: { 'content-type': 'application/json' },
  });
}

async function failure(promise) {
  const error = await promise.then(
    () => assert.fail('The request must fail.'),
    (rejection) => rejection,
  );
  assert.ok(error instanceof ArtifreeApiError, String(error));
  return error;
}

test('Handoff replies must confirm the exact proposal and transition', async () => {
  const session = 'e'.repeat(32);
  const agent = 'a'.repeat(32);
  const proposal = {
    workspace_session_id: session,
    agent_session_id: agent,
    artifact_id: artifact.id,
    revision: 1,
    path: 'index.html',
    file_sha256: 'f'.repeat(64),
    start: 2,
    end: 6,
    intent: 'Edit this',
  };
  const staged = {
    id: 'b'.repeat(32),
    agent_session_id: agent,
    agent_name: 'Editor',
    state: 'staged',
    selection: {
      artifact_id: artifact.id,
      revision: 1,
      path: 'index.html',
      file_sha256: 'f'.repeat(64),
      start: 2,
      end: 6,
      text: 'test',
    },
    intent: 'Edit this',
  };
  let body = staged;
  let status = 201;
  const client = await runtimeClientWith(() => Response.json(body, { status }));
  assert.deepEqual(await client.stageHandoff(proposal), staged);
  body = { ...staged, intent: '\uFEFFEdit this\uFEFF' };
  assert.deepEqual(
    await client.stageHandoff({ ...proposal, intent: body.intent }),
    body,
  );
  body = staged;
  status = 200;
  assert.equal(
    (await failure(client.stageHandoff(proposal))).code,
    'invalid_response',
  );
  status = 201;
  body = {
    ...staged,
    selection: { ...staged.selection, file_sha256: '0'.repeat(64) },
  };
  assert.equal(
    (await failure(client.stageHandoff(proposal))).code,
    'invalid_response',
  );
  status = 200;
  body = staged;
  assert.equal(
    (await failure(client.sendHandoff(staged, session))).code,
    'invalid_response',
  );
  body = { ...staged, state: 'queued', intent: 'a different request' };
  assert.equal(
    (await failure(client.sendHandoff(staged, session))).code,
    'invalid_response',
  );
  body = { ...staged, state: 'queued' };
  assert.equal((await client.sendHandoff(staged, session)).state, 'queued');
  body = { ...staged, state: 'blocked' };
  assert.equal((await client.sendHandoff(staged, session)).state, 'blocked');
  assert.equal(
    (await failure(client.cancelHandoff(staged, session))).code,
    'invalid_response',
  );
  body = { ...staged, state: 'offered' };
  assert.equal(
    (await failure(client.cancelHandoff(staged, session))).code,
    'invalid_response',
  );
  body = { ...staged, state: 'cancelled' };
  assert.equal(
    (await client.cancelHandoff(staged, session)).state,
    'cancelled',
  );
  body = { ...staged, id: 'c'.repeat(32) };
  assert.equal(
    (await failure(client.getHandoff(staged, session))).code,
    'invalid_response',
  );
  assert.equal(
    (await failure(client.sendHandoff(staged, 'd'.repeat(32)))).code,
    'invalid_request',
  );
});

function runtimeView(session, viewId = 'd'.repeat(32), generation = 1) {
  return {
    url: `http://127.0.0.1:7411/r/${'b'.repeat(64)}/`,
    enabled: true,
    session_id: session,
    artifact_id: artifact.id,
    revision: 1,
    head_revision: 1,
    sdk: 'artifree.app.v1',
    declared: ['metadata.read'],
    supported: ['metadata.read'],
    granted: [],
    grant_version: 0,
    view_id: viewId,
    generation,
    initialization: 'c'.repeat(64),
    expires_at_ms: 1_000_000,
    lease_ms: 900_000,
  };
}

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

test('Agent sessions replace an expired connection and retry the read once', async () => {
  const old = 'a'.repeat(32);
  const fresh = 'b'.repeat(32);
  const sessions = [{ id: 'c'.repeat(32), name: 'Editor' }];
  const reads = [];
  let opens = 0;
  const client = new ArtifreeClient({
    baseUrl: '',
    accessToken: 'token',
    fetch: async (url) => {
      if (url.endsWith('/v1/runtime-sessions'))
        return Response.json({ session_id: ++opens === 1 ? old : fresh });
      const session = new URL(url, 'http://localhost').searchParams.get(
        'workspace_session_id',
      );
      reads.push(session);
      return session === old ? expiredSession() : Response.json({ sessions });
    },
  });
  await client.openRuntimeSession();
  assert.deepEqual(await client.agentSessions(), sessions);
  assert.equal(client.runtimeSessionId, fresh);
  assert.equal(opens, 2);
  assert.deepEqual(reads, [old, fresh]);
});

test('An aborted agent-session retry preserves the caller’s cancellation reason', async () => {
  const old = 'a'.repeat(32);
  const fresh = 'b'.repeat(32);
  const reads = [];
  let opens = 0;
  let dispatched;
  const retrySent = new Promise((resolve) => (dispatched = resolve));
  const client = new ArtifreeClient({
    baseUrl: '',
    accessToken: 'token',
    fetch: async (url, init) => {
      if (url.endsWith('/v1/runtime-sessions'))
        return Response.json({ session_id: ++opens === 1 ? old : fresh });
      const session = new URL(url, 'http://localhost').searchParams.get(
        'workspace_session_id',
      );
      reads.push(session);
      if (session === old) return expiredSession();
      return new Promise((resolve, reject) => {
        init.signal.addEventListener(
          'abort',
          () => reject(init.signal.reason),
          {
            once: true,
          },
        );
        dispatched();
      });
    },
  });
  await client.openRuntimeSession();
  const controller = new AbortController();
  const reason = new Error('The caller cancelled the retry.');
  const result = client.agentSessions({ signal: controller.signal }).then(
    () => undefined,
    (error) => error,
  );
  await retrySent;
  controller.abort(reason);
  assert.strictEqual(await result, reason);
  assert.deepEqual(reads, [old, fresh]);
  assert.equal(opens, 2);
});

test('An agent-session retry aborted while reading its response preserves the reason', async () => {
  const old = 'a'.repeat(32);
  const fresh = 'b'.repeat(32);
  const reads = [];
  let opens = 0;
  let started;
  const bodyStarted = new Promise((resolve) => (started = resolve));
  const client = new ArtifreeClient({
    baseUrl: '',
    accessToken: 'token',
    fetch: async (url, init) => {
      if (url.endsWith('/v1/runtime-sessions'))
        return Response.json({ session_id: ++opens === 1 ? old : fresh });
      const session = new URL(url, 'http://localhost').searchParams.get(
        'workspace_session_id',
      );
      reads.push(session);
      if (session === old) return expiredSession();
      return new Response(
        new ReadableStream({
          start(body) {
            init.signal.addEventListener(
              'abort',
              () => body.error(init.signal.reason),
              {
                once: true,
              },
            );
            started();
          },
        }),
        { headers: { 'content-type': 'application/json' } },
      );
    },
  });
  await client.openRuntimeSession();
  const controller = new AbortController();
  const reason = new Error('The caller stopped reading.');
  const result = client.agentSessions({ signal: controller.signal }).then(
    () => undefined,
    (error) => error,
  );
  await bodyStarted;
  controller.abort(reason);
  assert.strictEqual(await result, reason);
  assert.deepEqual(reads, [old, fresh]);
  assert.equal(opens, 2);
});

test('A late successful agent-session retry cannot resolve after cancellation', async () => {
  const old = 'a'.repeat(32);
  const fresh = 'b'.repeat(32);
  const reads = [];
  let opens = 0;
  let dispatched;
  let release;
  const sent = new Promise((resolve) => (dispatched = resolve));
  const held = new Promise((resolve) => (release = resolve));
  const client = new ArtifreeClient({
    baseUrl: '',
    accessToken: 'token',
    fetch: async (url) => {
      if (url.endsWith('/v1/runtime-sessions'))
        return Response.json({ session_id: ++opens === 1 ? old : fresh });
      const session = new URL(url, 'http://localhost').searchParams.get(
        'workspace_session_id',
      );
      reads.push(session);
      if (session === old) return expiredSession();
      dispatched();
      await held;
      return Response.json({
        sessions: [{ id: 'c'.repeat(32), name: 'Editor' }],
      });
    },
  });
  await client.openRuntimeSession();
  const controller = new AbortController();
  const reason = new Error('The caller no longer needs the list.');
  const result = client.agentSessions({ signal: controller.signal }).then(
    () => undefined,
    (error) => error,
  );
  await sent;
  controller.abort(reason);
  release();
  assert.strictEqual(await result, reason);
  assert.deepEqual(reads, [old, fresh]);
  assert.equal(opens, 2);
});

test('An aborted initial agent-session fetch preserves its reason without recovery', async () => {
  let opens = 0;
  let reads = 0;
  let started;
  const dispatched = new Promise((resolve) => (started = resolve));
  const client = new ArtifreeClient({
    baseUrl: '',
    accessToken: 'token',
    fetch: async (url, init) => {
      if (url.endsWith('/v1/runtime-sessions')) {
        opens += 1;
        return Response.json({ session_id: 'a'.repeat(32) });
      }
      reads += 1;
      return new Promise((resolve, reject) => {
        init.signal.addEventListener(
          'abort',
          () => reject(init.signal.reason),
          {
            once: true,
          },
        );
        started();
      });
    },
  });
  await client.openRuntimeSession();
  const controller = new AbortController();
  const reason = new Error('The caller stopped the first read.');
  const result = client.agentSessions({ signal: controller.signal }).then(
    () => undefined,
    (error) => error,
  );
  await dispatched;
  controller.abort(reason);
  assert.strictEqual(await result, reason);
  assert.equal(reads, 1);
  assert.equal(opens, 1);
});

test('A cancelled recovery waiter keeps its reason when the shared replacement fails', async () => {
  const old = 'a'.repeat(32);
  let opens = 0;
  let reads = 0;
  let started;
  let rejectReplacement;
  const opening = new Promise((resolve) => (started = resolve));
  const client = new ArtifreeClient({
    baseUrl: '',
    accessToken: 'token',
    fetch: async (url) => {
      if (url.endsWith('/v1/runtime-sessions')) {
        opens += 1;
        if (opens === 1) return Response.json({ session_id: old });
        return new Promise((resolve, reject) => {
          rejectReplacement = reject;
          started();
        });
      }
      reads += 1;
      return expiredSession();
    },
  });
  await client.openRuntimeSession();
  const controller = new AbortController();
  const reason = new Error('The caller abandoned the replacement.');
  const cancelled = client.agentSessions({ signal: controller.signal }).then(
    () => undefined,
    (error) => error,
  );
  await opening;
  const other = failure(client.agentSessions());
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(reads, 2);
  controller.abort(reason);
  rejectReplacement(new Error('Replacement connection failed.'));
  assert.strictEqual(await cancelled, reason);
  assert.equal((await other).code, 'network_error');
  assert.equal(opens, 2);
  assert.equal(client.runtimeSessionId, old);
});

test('An unrelated failure from an agent-session retry is not retried', async () => {
  const old = 'a'.repeat(32);
  const fresh = 'b'.repeat(32);
  const reads = [];
  let opens = 0;
  const client = new ArtifreeClient({
    baseUrl: '',
    accessToken: 'token',
    fetch: async (url) => {
      if (url.endsWith('/v1/runtime-sessions'))
        return Response.json({ session_id: ++opens === 1 ? old : fresh });
      const session = new URL(url, 'http://localhost').searchParams.get(
        'workspace_session_id',
      );
      reads.push(session);
      if (session === old) return expiredSession();
      return Response.json(
        { error: { code: 'not_found', message: 'Inbox unavailable.' } },
        { status: 404 },
      );
    },
  });
  await client.openRuntimeSession();
  const error = await failure(client.agentSessions());
  assert.deepEqual([error.status, error.code], [404, 'not_found']);
  assert.deepEqual(reads, [old, fresh]);
  assert.equal(opens, 2);
});

test('A network failure from an agent-session retry keeps its cause and is not retried', async () => {
  const old = 'a'.repeat(32);
  const fresh = 'b'.repeat(32);
  const reads = [];
  const cause = new Error('Connection reset.');
  let opens = 0;
  const client = new ArtifreeClient({
    baseUrl: '',
    accessToken: 'token',
    fetch: async (url) => {
      if (url.endsWith('/v1/runtime-sessions'))
        return Response.json({ session_id: ++opens === 1 ? old : fresh });
      const session = new URL(url, 'http://localhost').searchParams.get(
        'workspace_session_id',
      );
      reads.push(session);
      if (session === old) return expiredSession();
      throw cause;
    },
  });
  await client.openRuntimeSession();
  const error = await failure(client.agentSessions());
  assert.deepEqual([error.status, error.code], [0, 'network_error']);
  assert.strictEqual(error.cause, cause);
  assert.deepEqual(reads, [old, fresh]);
  assert.equal(opens, 2);
});

test('Agent sessions do not retry a second expiry or an unrelated failure', async () => {
  const old = 'a'.repeat(32);
  const fresh = 'b'.repeat(32);
  let opens = 0;
  let reads = 0;
  const client = new ArtifreeClient({
    baseUrl: '',
    accessToken: 'token',
    fetch: async (url) => {
      if (url.endsWith('/v1/runtime-sessions'))
        return Response.json({ session_id: ++opens === 1 ? old : fresh });
      reads += 1;
      return expiredSession();
    },
  });
  await client.openRuntimeSession();
  assert.equal(
    (await failure(client.agentSessions())).code,
    'runtime_session_expired',
  );
  assert.equal(client.runtimeSessionId, fresh);
  assert.deepEqual([opens, reads], [2, 2]);

  for (const [status, code] of [
    [404, 'not_found'],
    [503, 'runtime_session_expired'],
    [401, 'unauthenticated'],
    [503, 'unavailable'],
  ]) {
    let attempts = 0;
    const other = new ArtifreeClient({
      baseUrl: '',
      accessToken: 'token',
      fetch: async (url) => {
        if (url.endsWith('/v1/runtime-sessions')) {
          attempts += 1;
          return Response.json({ session_id: old });
        }
        return Response.json(
          { error: { code, message: 'refused' } },
          { status },
        );
      },
    });
    await other.openRuntimeSession();
    const error = await failure(other.agentSessions());
    assert.deepEqual([error.status, error.code], [status, code]);
    assert.equal(attempts, 1);
    assert.equal(other.runtimeSessionId, old);
  }
});

test('Agent sessions share replacement with views while an abandoned read stays cancelled', async () => {
  const old = 'a'.repeat(32);
  const fresh = 'b'.repeat(32);
  const inboxes = [{ id: 'c'.repeat(32), name: 'Editor' }];
  const reads = [];
  let opens = 0;
  let oldRequests = 0;
  let release;
  let started;
  let allStarted;
  const opening = new Promise((resolve) => (started = resolve));
  const allOldRequests = new Promise((resolve) => (allStarted = resolve));
  const held = new Promise((resolve) => (release = resolve));
  const client = new ArtifreeClient({
    baseUrl: '',
    accessToken: 'token',
    fetch: async (url, init) => {
      if (url.endsWith('/v1/runtime-sessions')) {
        opens += 1;
        if (opens === 1) return Response.json({ session_id: old });
        started();
        await held;
        return Response.json({ session_id: fresh });
      }
      const view = url.endsWith('/runtime-views');
      const body = view ? JSON.parse(init.body) : undefined;
      const session = view
        ? body.session_id
        : new URL(url, 'http://localhost').searchParams.get(
            'workspace_session_id',
          );
      if (!view) reads.push(session);
      if (session === old) {
        oldRequests += 1;
        if (oldRequests === 3) allStarted();
        return expiredSession();
      }
      return view
        ? Response.json(runtimeView(fresh, body.view_id, body.generation))
        : Response.json({ sessions: inboxes });
    },
  });
  await client.openRuntimeSession();
  const controller = new AbortController();
  const cancelled = assert.rejects(
    client.agentSessions({ signal: controller.signal }),
    {
      name: 'AbortError',
    },
  );
  await opening;
  const list = client.agentSessions();
  const view = client.createRuntimeView(artifact.id, 1, 'd'.repeat(32), 1);
  await allOldRequests;
  controller.abort();
  release();
  await cancelled;
  assert.deepEqual(await list, inboxes);
  assert.equal((await view).session_id, fresh);
  assert.equal(opens, 2);
  assert.deepEqual(reads, [old, old, fresh]);
});

test('Agent sessions ignore late aborted replies and do not reopen for them', async () => {
  for (const response of [
    () => expiredSession(),
    () => Response.json({ sessions: [{ id: 'c'.repeat(32), name: 'Editor' }] }),
  ]) {
    let opens = 0;
    let release;
    let dispatched;
    const sent = new Promise((resolve) => (dispatched = resolve));
    const held = new Promise((resolve) => (release = resolve));
    const client = new ArtifreeClient({
      baseUrl: '',
      accessToken: 'token',
      fetch: async (url) => {
        if (url.endsWith('/v1/runtime-sessions')) {
          opens += 1;
          return Response.json({ session_id: 'a'.repeat(32) });
        }
        dispatched();
        await held;
        return response();
      },
    });
    await client.openRuntimeSession();
    const controller = new AbortController();
    const pending = assert.rejects(
      client.agentSessions({ signal: controller.signal }),
      {
        name: 'AbortError',
      },
    );
    await sent;
    controller.abort();
    release();
    await pending;
    assert.equal(opens, 1);
  }
});

test('An already cancelled agent-session read does not dispatch', async () => {
  let reads = 0;
  const client = new ArtifreeClient({
    baseUrl: '',
    accessToken: 'token',
    fetch: async (url) => {
      if (url.endsWith('/v1/runtime-sessions'))
        return Response.json({ session_id: 'a'.repeat(32) });
      reads += 1;
      return Response.json({ sessions: [] });
    },
  });
  await client.openRuntimeSession();
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(client.agentSessions({ signal: controller.signal }), {
    name: 'AbortError',
  });
  assert.equal(reads, 0);
});

test('Agent sessions reject an old successful read after another view replaces its connection', async () => {
  const old = 'a'.repeat(32);
  const fresh = 'b'.repeat(32);
  let opens = 0;
  let release;
  let dispatched;
  const sent = new Promise((resolve) => (dispatched = resolve));
  const held = new Promise((resolve) => (release = resolve));
  const client = new ArtifreeClient({
    baseUrl: '',
    accessToken: 'token',
    fetch: async (url, init) => {
      if (url.endsWith('/v1/runtime-sessions'))
        return Response.json({ session_id: ++opens === 1 ? old : fresh });
      if (url.includes('/v1/agent-sessions?')) {
        dispatched();
        await held;
        return Response.json({
          sessions: [{ id: 'c'.repeat(32), name: 'Editor' }],
        });
      }
      const body = JSON.parse(init.body);
      return body.session_id === old
        ? expiredSession()
        : Response.json(runtimeView(fresh, body.view_id, body.generation));
    },
  });
  await client.openRuntimeSession();
  const pending = failure(client.agentSessions());
  await sent;
  await client.createRuntimeView(artifact.id, 1, 'd'.repeat(32), 1);
  release();
  assert.equal((await pending).code, 'invalid_request');
  assert.equal(client.runtimeSessionId, fresh);
});

test('Logout fences a pending agent-session replacement and late expiry', async () => {
  const old = 'a'.repeat(32);
  let opens = 0;
  let release;
  let started;
  const opening = new Promise((resolve) => (started = resolve));
  const held = new Promise((resolve) => (release = resolve));
  const client = new ArtifreeClient({
    baseUrl: '',
    accessToken: 'token',
    fetch: async (url, init) => {
      if (url.endsWith('/v1/runtime-sessions')) {
        opens += 1;
        if (opens === 1) return Response.json({ session_id: old });
        started();
        await held;
        return Response.json({ session_id: 'b'.repeat(32) });
      }
      if (init.method === 'DELETE') return new Response(null, { status: 204 });
      return expiredSession();
    },
  });
  await client.openRuntimeSession();
  const pending = failure(client.agentSessions());
  await opening;
  await client.closeRuntimeSession();
  release();
  assert.equal((await pending).code, 'invalid_request');
  assert.throws(() => client.runtimeSessionId, { code: 'invalid_request' });
  assert.equal(opens, 2);

  let releaseRead;
  let dispatched;
  const sent = new Promise((resolve) => (dispatched = resolve));
  const heldRead = new Promise((resolve) => (releaseRead = resolve));
  let retries = 0;
  const closing = new ArtifreeClient({
    baseUrl: '',
    accessToken: 'token',
    fetch: async (url, init) => {
      if (url.endsWith('/v1/runtime-sessions')) {
        retries += 1;
        return Response.json({ session_id: old });
      }
      if (init.method === 'DELETE') throw new Error('close response lost');
      dispatched();
      await heldRead;
      return expiredSession();
    },
  });
  await closing.openRuntimeSession();
  const late = failure(closing.agentSessions());
  await sent;
  assert.equal(
    (await failure(closing.closeRuntimeSession())).code,
    'network_error',
  );
  releaseRead();
  assert.equal((await late).code, 'invalid_request');
  assert.equal(retries, 1);
});

test('An agent-session read from before logout cannot run on a later connection', async () => {
  const old = 'a'.repeat(32);
  const fresh = 'b'.repeat(32);
  const reads = [];
  let opens = 0;
  let release;
  let dispatched;
  const sent = new Promise((resolve) => (dispatched = resolve));
  const held = new Promise((resolve) => (release = resolve));
  const client = new ArtifreeClient({
    baseUrl: '',
    accessToken: 'token',
    fetch: async (url, init) => {
      if (url.endsWith('/v1/runtime-sessions'))
        return Response.json({ session_id: ++opens === 1 ? old : fresh });
      if (init.method === 'DELETE') return new Response(null, { status: 204 });
      const session = new URL(url, 'http://localhost').searchParams.get(
        'workspace_session_id',
      );
      reads.push(session);
      if (session === old) {
        dispatched();
        await held;
        return expiredSession();
      }
      return Response.json({ sessions: [] });
    },
  });
  await client.openRuntimeSession();
  const pending = failure(client.agentSessions());
  await sent;
  await client.closeRuntimeSession();
  await client.openRuntimeSession();
  release();
  assert.equal((await pending).code, 'invalid_request');
  assert.equal(client.runtimeSessionId, fresh);
  assert.deepEqual(reads, [old]);
  assert.equal(opens, 2);
});

test('An expired stage is not replayed when a later agent-session read reconnects', async () => {
  const old = 'a'.repeat(32);
  const fresh = 'b'.repeat(32);
  const agent = 'c'.repeat(32);
  const proposal = {
    workspace_session_id: old,
    agent_session_id: agent,
    artifact_id: artifact.id,
    revision: 1,
    path: 'index.html',
    file_sha256: 'f'.repeat(64),
    start: 0,
    end: 4,
    intent: 'Edit this',
  };
  const staged = {
    id: 'd'.repeat(32),
    agent_session_id: agent,
    agent_name: 'Editor',
    state: 'staged',
    selection: {
      artifact_id: artifact.id,
      revision: 1,
      path: 'index.html',
      file_sha256: 'f'.repeat(64),
      start: 0,
      end: 4,
      text: 'test',
    },
    intent: 'Edit this',
  };
  const stages = [];
  let opens = 0;
  const client = new ArtifreeClient({
    baseUrl: '',
    accessToken: 'token',
    fetch: async (url, init) => {
      if (url.endsWith('/v1/runtime-sessions'))
        return Response.json({ session_id: ++opens === 1 ? old : fresh });
      if (url === '/v1/handoffs') {
        const body = JSON.parse(init.body);
        stages.push(body.workspace_session_id);
        return body.workspace_session_id === old
          ? expiredSession()
          : Response.json(staged, { status: 201 });
      }
      const session = new URL(url, 'http://localhost').searchParams.get(
        'workspace_session_id',
      );
      return session === old
        ? expiredSession()
        : Response.json({ sessions: [{ id: agent, name: 'Editor' }] });
    },
  });
  await client.openRuntimeSession();
  assert.equal(
    (await failure(client.stageHandoff(proposal))).code,
    'runtime_session_expired',
  );
  assert.deepEqual(stages, [old]);
  assert.equal(opens, 1);
  assert.deepEqual(await client.agentSessions(), [
    { id: agent, name: 'Editor' },
  ]);
  assert.deepEqual(stages, [old]);
  assert.equal(
    (await failure(client.stageHandoff(proposal))).code,
    'invalid_request',
  );
  assert.deepEqual(stages, [old]);
  assert.deepEqual(
    await client.stageHandoff({ ...proposal, workspace_session_id: fresh }),
    staged,
  );
  assert.deepEqual(stages, [old, fresh]);
});

test('A late staged response cannot be confirmed on a replacement connection', async () => {
  const old = 'a'.repeat(32);
  const fresh = 'b'.repeat(32);
  const agent = 'c'.repeat(32);
  const proposal = {
    workspace_session_id: old,
    agent_session_id: agent,
    artifact_id: artifact.id,
    revision: 1,
    path: 'index.html',
    file_sha256: 'f'.repeat(64),
    start: 0,
    end: 4,
    intent: 'Edit this',
  };
  let opens = 0;
  let stages = 0;
  let release;
  let dispatched;
  const sent = new Promise((resolve) => (dispatched = resolve));
  const held = new Promise((resolve) => (release = resolve));
  const client = new ArtifreeClient({
    baseUrl: '',
    accessToken: 'token',
    fetch: async (url) => {
      if (url.endsWith('/v1/runtime-sessions'))
        return Response.json({ session_id: ++opens === 1 ? old : fresh });
      if (url === '/v1/handoffs') {
        stages += 1;
        dispatched();
        await held;
        return Response.json(
          {
            id: 'd'.repeat(32),
            agent_session_id: agent,
            agent_name: 'Editor',
            state: 'staged',
            selection: {
              artifact_id: artifact.id,
              revision: 1,
              path: 'index.html',
              file_sha256: 'f'.repeat(64),
              start: 0,
              end: 4,
              text: 'test',
            },
            intent: 'Edit this',
          },
          { status: 201 },
        );
      }
      const session = new URL(url, 'http://localhost').searchParams.get(
        'workspace_session_id',
      );
      return session === old
        ? expiredSession()
        : Response.json({ sessions: [] });
    },
  });
  await client.openRuntimeSession();
  const stage = failure(client.stageHandoff(proposal));
  await sent;
  assert.deepEqual(await client.agentSessions(), []);
  release();
  assert.equal((await stage).code, 'invalid_response');
  assert.equal(stages, 1);
  assert.equal(client.runtimeSessionId, fresh);
});

test('A body cut off mid-stream is a network error, not a contract error', async () => {
  // The outcome of a mutation is unknown, so callers must be told to retry.
  for (const status of [200, 201, 503]) {
    const client = clientWith(() => cutOff(status, '{"artifact":'));
    const error = await failure(client.getArtifact(artifact.id));
    assert.equal(error.code, 'network_error', `status ${status}`);
  }
  const reader = clientWith(() => cutOff(200, '<h1>'));
  const error = await failure(reader.readFile(artifact.id, 1, 'index.html'));
  assert.equal(error.code, 'network_error');
});

test('A complete body that is not the contract is an invalid response', async () => {
  const client = clientWith(
    () => new Response('<html>proxy error</html>', { status: 200 }),
  );
  const error = await failure(client.getArtifact(artifact.id));
  assert.equal(error.code, 'invalid_response');
});

test('An abandoned runtime open cannot replace a later connection', async () => {
  const pending = [];
  const client = new ArtifreeClient({
    baseUrl: '',
    accessToken: 'token',
    fetch: () => new Promise((resolve) => pending.push(resolve)),
  });
  const old = client.openRuntimeSession();
  const current = client.openRuntimeSession();
  pending[1](Response.json({ session_id: '2'.repeat(32) }));
  await current;
  pending[0](Response.json({ session_id: '1'.repeat(32) }));
  assert.equal((await failure(old)).code, 'invalid_request');
  assert.equal(client.runtimeSessionId, '2'.repeat(32));
});

test('Closing during an open cannot activate its late response', async () => {
  let finish;
  const client = new ArtifreeClient({
    baseUrl: '',
    accessToken: 'token',
    fetch: () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  });
  const opening = client.openRuntimeSession();
  await client.closeRuntimeSession();
  finish(Response.json({ session_id: '1'.repeat(32) }));
  assert.equal((await failure(opening)).code, 'invalid_request');
  assert.throws(() => client.runtimeSessionId, { code: 'invalid_request' });
});

test('An unconfirmed close retains its connection identity for retry', async () => {
  const paths = [];
  let closeAttempts = 0;
  const client = new ArtifreeClient({
    baseUrl: '',
    accessToken: 'token',
    fetch: async (url, init) => {
      paths.push(`${init.method} ${url}`);
      if (init.method === 'POST')
        return Response.json({ session_id: 'a'.repeat(32) });
      closeAttempts += 1;
      if (closeAttempts === 1) throw new Error('response lost');
      if (closeAttempts === 2)
        return new Response('proxy fallback', { status: 200 });
      return new Response(null, { status: 204 });
    },
  });
  await client.openRuntimeSession();
  assert.equal(
    (await failure(client.closeRuntimeSession())).code,
    'network_error',
  );
  assert.equal(client.runtimeSessionId, 'a'.repeat(32));
  assert.equal(
    (await failure(client.closeRuntimeSession())).code,
    'invalid_response',
  );
  assert.equal(client.runtimeSessionId, 'a'.repeat(32));
  await client.closeRuntimeSession();
  assert.equal(paths[1], paths[2]);
  assert.equal(paths[2], paths[3]);
  assert.throws(() => client.runtimeSessionId, { code: 'invalid_request' });
});

test('A runtime view close requires the no-content acknowledgement', async () => {
  const view = { view_id: 'a'.repeat(32), generation: 1 };
  const unconfirmed = clientWith(
    () => new Response('proxy fallback', { status: 200 }),
  );
  assert.equal(
    (await failure(unconfirmed.closeRuntimeView(view))).code,
    'invalid_response',
  );
  const confirmed = clientWith(() => new Response(null, { status: 204 }));
  await confirmed.closeRuntimeView(view);
});

test('An uncertain grant stays with its connection until a clear is confirmed', async () => {
  let calls = 0;
  const notifications = [];
  const client = new ArtifreeClient({
    baseUrl: '',
    accessToken: 'token',
    fetch: async (url, init) => {
      if (url.endsWith('/v1/runtime-sessions'))
        return Response.json({ session_id: 'a'.repeat(32) });
      calls += 1;
      if (calls === 1) throw new Error('answer lost');
      assert.deepEqual(JSON.parse(init.body).capabilities, []);
      return Response.json({ version: calls === 2 ? 0 : 1 });
    },
  });
  await client.openRuntimeSession();
  const view = {
    session_id: 'a'.repeat(32),
    artifact_id: artifact.id,
    revision: 1,
    head_revision: 1,
    grant_version: 0,
  };
  const unsubscribe = client.subscribeRuntimeGrants(() =>
    notifications.push(client.runtimeGrantStatus(view)),
  );
  const allow = client.setRuntimeGrant(view, ['metadata.read']);
  assert.equal(client.runtimeGrantStatus(view), 'pending');
  assert.equal((await failure(allow)).code, 'network_error');
  // A newly mounted preview receives the same connection-owned state.
  assert.equal(client.runtimeGrantStatus({ ...view }), 'unknown');
  assert.equal(
    (await failure(client.setRuntimeGrant(view, ['metadata.read']))).code,
    'invalid_request',
  );
  assert.equal(client.runtimeGrantStatus(view), 'unknown');
  assert.equal(
    (await failure(client.setRuntimeGrant(view, []))).code,
    'invalid_response',
  );
  assert.equal(client.runtimeGrantStatus(view), 'unknown');
  assert.deepEqual(await client.setRuntimeGrant(view, []), { version: 1 });
  assert.equal(client.runtimeGrantStatus(view), 'settled');
  assert.equal(client.runtimeGrantRevision(artifact.id, 1), 3);
  assert.deepEqual(notifications, [
    'pending',
    'unknown',
    'pending',
    'unknown',
    'pending',
    'settled',
  ]);
  unsubscribe();
});

test('Concurrent views replace an expired connection once and require new consent', async () => {
  const old = 'a'.repeat(32);
  const fresh = 'b'.repeat(32);
  let opens = 0;
  let grantRequests = 0;
  let release;
  let started;
  const opening = new Promise((resolve) => (started = resolve));
  const delayed = new Promise((resolve) => (release = resolve));
  const client = new ArtifreeClient({
    baseUrl: '',
    accessToken: 'token',
    fetch: async (url, init) => {
      if (url.endsWith('/v1/runtime-sessions')) {
        opens += 1;
        if (opens === 1) return Response.json({ session_id: old });
        started();
        await delayed;
        return Response.json({ session_id: fresh });
      }
      if (url.endsWith('/runtime-views')) {
        const body = JSON.parse(init.body);
        return body.session_id === old
          ? expiredSession()
          : Response.json(runtimeView(fresh, body.view_id, body.generation));
      }
      grantRequests += 1;
      assert.match(url, new RegExp(`/runtime-sessions/${fresh}/`));
      return Response.json({ version: 1 });
    },
  });
  await client.openRuntimeSession();
  const first = client.createRuntimeView(artifact.id, 1, '1'.repeat(32), 1);
  const second = client.createRuntimeView(artifact.id, 1, '2'.repeat(32), 1);
  const both = Promise.allSettled([first, second]);
  await opening;
  await new Promise((resolve) => setImmediate(resolve));
  release();
  const results = await both;
  assert.equal(opens, 2, 'Concurrent views must share one replacement.');
  assert.deepEqual(
    results.map((result) => result.status),
    ['fulfilled', 'fulfilled'],
  );
  const [one, two] = results.map((result) => result.value);
  assert.equal(client.runtimeSessionId, fresh);
  assert.deepEqual([one.granted, two.granted], [[], []]);
  assert.equal(
    (await failure(client.setRuntimeGrant(runtimeView(old), ['metadata.read'])))
      .code,
    'invalid_request',
  );
  assert.equal(grantRequests, 0);
  await client.setRuntimeGrant(one, ['metadata.read']);
  assert.equal(grantRequests, 1);
});

test('A grant rejected for expiry is never replayed on the new connection', async () => {
  const old = 'a'.repeat(32);
  const fresh = 'b'.repeat(32);
  let opens = 0;
  let grants = 0;
  const client = new ArtifreeClient({
    baseUrl: '',
    accessToken: 'token',
    fetch: async (url) => {
      if (url.endsWith('/v1/runtime-sessions'))
        return Response.json({ session_id: ++opens === 1 ? old : fresh });
      grants += 1;
      return expiredSession();
    },
  });
  await client.openRuntimeSession();
  assert.equal(
    (await failure(client.setRuntimeGrant(runtimeView(old), ['metadata.read'])))
      .code,
    'runtime_session_expired',
  );
  assert.equal(client.runtimeSessionId, fresh);
  assert.equal(grants, 1);
  assert.equal(
    (await failure(client.setRuntimeGrant(runtimeView(old), ['metadata.read'])))
      .code,
    'invalid_request',
  );
  assert.equal(grants, 1);
});

test('A failed replacement stays retryable and never reuses an expired ID', async () => {
  const old = 'a'.repeat(32);
  const fresh = 'b'.repeat(32);
  let opens = 0;
  const client = new ArtifreeClient({
    baseUrl: '',
    accessToken: 'token',
    fetch: async (url, init) => {
      if (url.endsWith('/v1/runtime-sessions')) {
        opens += 1;
        if (opens === 2)
          return Response.json(
            { error: { code: 'unavailable', message: 'Try again.' } },
            { status: 503 },
          );
        if (opens === 3) return Response.json({ session_id: old });
        return Response.json({ session_id: opens === 1 ? old : fresh });
      }
      const body = JSON.parse(init.body);
      return body.session_id === old
        ? expiredSession()
        : Response.json(runtimeView(fresh, body.view_id, body.generation));
    },
  });
  await client.openRuntimeSession();
  const create = () =>
    client.createRuntimeView(artifact.id, 1, 'd'.repeat(32), 1);
  assert.equal((await failure(create())).code, 'unavailable');
  assert.equal(client.runtimeSessionId, old);
  assert.equal((await failure(create())).code, 'invalid_response');
  assert.equal(client.runtimeSessionId, old);
  assert.deepEqual((await create()).granted, []);
  assert.equal(client.runtimeSessionId, fresh);
  assert.equal(opens, 4);
});

test('Logout fences a pending expired-connection replacement', async () => {
  const old = 'a'.repeat(32);
  let opens = 0;
  let release;
  let started;
  const opening = new Promise((resolve) => (started = resolve));
  const delayed = new Promise((resolve) => (release = resolve));
  const client = new ArtifreeClient({
    baseUrl: '',
    accessToken: 'token',
    fetch: async (url, init) => {
      if (url.endsWith('/v1/runtime-sessions')) {
        opens += 1;
        if (opens === 1) return Response.json({ session_id: old });
        started();
        await delayed;
        return Response.json({ session_id: 'b'.repeat(32) });
      }
      if (init.method === 'DELETE') return new Response(null, { status: 204 });
      return expiredSession();
    },
  });
  await client.openRuntimeSession();
  const pending = client.createRuntimeView(artifact.id, 1, 'd'.repeat(32), 1);
  await opening;
  await client.closeRuntimeSession();
  release();
  assert.equal((await failure(pending)).code, 'invalid_request');
  assert.throws(() => client.runtimeSessionId, { code: 'invalid_request' });
});

test('An unconfirmed logout prevents a late expired-view response reopening it', async () => {
  const old = 'a'.repeat(32);
  let opens = 0;
  let closes = 0;
  let release;
  let dispatched;
  const sent = new Promise((resolve) => (dispatched = resolve));
  const held = new Promise((resolve) => (release = resolve));
  const client = new ArtifreeClient({
    baseUrl: '',
    accessToken: 'token',
    fetch: async (url, init) => {
      if (url.endsWith('/v1/runtime-sessions')) {
        opens += 1;
        return Response.json({ session_id: old });
      }
      if (init.method === 'DELETE') {
        closes += 1;
        if (closes === 1) throw new Error('close response lost');
        return new Response(null, { status: 204 });
      }
      dispatched();
      await held;
      return expiredSession();
    },
  });
  await client.openRuntimeSession();
  const pending = failure(
    client.createRuntimeView(artifact.id, 1, 'd'.repeat(32), 1),
  );
  await sent;
  assert.equal(
    (await failure(client.closeRuntimeSession())).code,
    'network_error',
  );
  assert.equal(
    (await failure(client.createRuntimeView(artifact.id, 1, 'e'.repeat(32), 1)))
      .code,
    'invalid_request',
  );
  release();
  assert.equal((await pending).code, 'invalid_request');
  assert.equal(opens, 1);
  assert.equal(client.runtimeSessionId, old);
  await client.closeRuntimeSession();
  assert.throws(() => client.runtimeSessionId, { code: 'invalid_request' });
});

test('Late grant and metadata replies cannot cross to a replacement connection', async () => {
  const old = 'a'.repeat(32);
  const fresh = 'b'.repeat(32);
  let opens = 0;
  let finishGrant;
  let finishMetadata;
  const grantReply = new Promise((resolve) => (finishGrant = resolve));
  const metadataReply = new Promise((resolve) => (finishMetadata = resolve));
  const client = new ArtifreeClient({
    baseUrl: '',
    accessToken: 'token',
    fetch: async (url, init) => {
      if (url.endsWith('/v1/runtime-sessions'))
        return Response.json({ session_id: ++opens === 1 ? old : fresh });
      if (url.endsWith('/grant')) return grantReply;
      if (url.endsWith('/metadata')) return metadataReply;
      const body = JSON.parse(init.body);
      return body.session_id === old
        ? expiredSession()
        : Response.json(runtimeView(fresh, body.view_id, body.generation));
    },
  });
  await client.openRuntimeSession();
  const view = runtimeView(old);
  const grant = assert.rejects(
    client.setRuntimeGrant(view, ['metadata.read']),
    { code: 'invalid_request' },
    'A late grant must not reach the replacement.',
  );
  const metadata = assert.rejects(
    client.runtimeMetadata(view),
    { code: 'invalid_request' },
    'Late metadata must not reach the replacement.',
  );
  await client.createRuntimeView(artifact.id, 1, '1'.repeat(32), 1);
  assert.equal(client.runtimeSessionId, fresh);
  finishGrant(Response.json({ version: 1 }));
  finishMetadata(
    Response.json({
      artifact_id: artifact.id,
      title: 'Demo',
      current_revision: 1,
      metadata_revision: 1,
    }),
  );
  await Promise.all([grant, metadata]);
});

test('A late view response cannot be adopted by a replacement connection', async () => {
  const old = 'a'.repeat(32);
  const fresh = 'b'.repeat(32);
  let opens = 0;
  let release;
  const held = new Promise((resolve) => (release = resolve));
  const client = new ArtifreeClient({
    baseUrl: '',
    accessToken: 'token',
    fetch: async (url, init) => {
      if (url.endsWith('/v1/runtime-sessions'))
        return Response.json({ session_id: ++opens === 1 ? old : fresh });
      const body = JSON.parse(init.body);
      if (body.view_id === '1'.repeat(32)) {
        await held;
        return Response.json(runtimeView(old, body.view_id, body.generation));
      }
      return body.session_id === old
        ? expiredSession()
        : Response.json(runtimeView(fresh, body.view_id, body.generation));
    },
  });
  await client.openRuntimeSession();
  const late = failure(
    client.createRuntimeView(artifact.id, 1, '1'.repeat(32), 1),
  );
  await client.createRuntimeView(artifact.id, 1, '2'.repeat(32), 1);
  assert.equal(client.runtimeSessionId, fresh);
  release();
  assert.equal((await late).code, 'invalid_response');
});

test('Unrelated missing artifacts do not replace a runtime connection', async () => {
  let opens = 0;
  const client = new ArtifreeClient({
    baseUrl: '',
    accessToken: 'token',
    fetch: async (url) => {
      if (url.endsWith('/v1/runtime-sessions')) {
        opens += 1;
        return Response.json({ session_id: 'a'.repeat(32) });
      }
      return Response.json(
        { error: { code: 'not_found', message: 'Artifact not found.' } },
        { status: 404 },
      );
    },
  });
  await client.openRuntimeSession();
  assert.equal(
    (await failure(client.createRuntimeView(artifact.id, 1, 'd'.repeat(32), 1)))
      .code,
    'not_found',
  );
  assert.equal(opens, 1);
});

test('Runtime responses must match the requested view and valid metadata', async () => {
  const viewId = 'a'.repeat(32);
  const view = {
    url: `http://127.0.0.1:7411/r/${'b'.repeat(64)}/`,
    enabled: true,
    session_id: 'e'.repeat(32),
    artifact_id: artifact.id,
    revision: 1,
    head_revision: 1,
    sdk: 'artifree.app.v1',
    declared: ['metadata.read'],
    supported: ['metadata.read'],
    granted: [],
    grant_version: 0,
    view_id: viewId,
    generation: 1,
    initialization: 'c'.repeat(64),
    expires_at_ms: 1_000_000,
    lease_ms: 900_000,
  };
  let sent;
  const client = new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:7410',
    accessToken: 'token',
    fetch: async (url, init) => {
      if (url.endsWith('/v1/runtime-sessions'))
        return Response.json({ session_id: 'e'.repeat(32) });
      sent = { url, init };
      return Response.json(view, { status: 201 });
    },
  });
  await client.openRuntimeSession();
  assert.deepEqual(
    await client.createRuntimeView(artifact.id, 1, viewId, 1),
    view,
  );
  assert.deepEqual(JSON.parse(sent.init.body), {
    session_id: client.runtimeSessionId,
    view_id: viewId,
    generation: 1,
  });
  assert.equal(sent.init.headers.authorization, 'Bearer token');
  assert.deepEqual(
    await (
      await runtimeClientWith(() => Response.json({ ...view, lease_ms: 0 }))
    ).createRuntimeView(artifact.id, 1, viewId, 1),
    { ...view, lease_ms: 0 },
  );
  for (const body of [
    { ...view, url: 'javascript:alert(1)' },
    { ...view, enabled: 'yes' },
    { ...view, session_id: 'd'.repeat(32) },
    { ...view, sdk: 'other' },
    { ...view, supported: [3] },
    { ...view, granted: ['future.write'] },
    { ...view, granted: ['metadata.read'], grant_version: 0 },
    { ...view, grant_version: -1 },
    { ...view, initialization: 'invalid' },
    { ...view, artifact_id: `art_${'1'.repeat(32)}` },
    { ...view, revision: 2 },
    { ...view, view_id: 'd'.repeat(32) },
    { ...view, generation: 2 },
    { ...view, expires_at_ms: Number.MAX_SAFE_INTEGER + 1 },
    { ...view, lease_ms: -1 },
    { ...view, lease_ms: 0.5 },
    { ...view, lease_ms: 2_147_483_648 },
  ]) {
    const error = await failure(
      (await runtimeClientWith(() => Response.json(body))).createRuntimeView(
        artifact.id,
        1,
        viewId,
        1,
      ),
    );
    assert.equal(error.code, 'invalid_response', JSON.stringify(body));
  }
  const valid = {
    artifact_id: artifact.id,
    title: 'New name',
    current_revision: 1,
    metadata_revision: 2,
  };
  assert.deepEqual(
    await (
      await runtimeClientWith(() => Response.json(valid))
    ).runtimeMetadata(view),
    valid,
  );
  for (const body of [
    { ...valid, artifact_id: `art_${'1'.repeat(32)}` },
    { ...valid, current_revision: 0 },
    { ...valid, metadata_revision: Number.MAX_SAFE_INTEGER + 1 },
  ]) {
    const error = await failure(
      (await runtimeClientWith(() => Response.json(body))).runtimeMetadata(
        view,
      ),
    );
    assert.equal(error.code, 'invalid_response', JSON.stringify(body));
  }
});

test('An export is a ZIP archive, whole, or a failure', async () => {
  // The end record of an empty archive.
  const zip = new Uint8Array(22);
  zip.set([0x50, 0x4b, 0x05, 0x06]);
  const answer = (type) => () =>
    new Response(zip, { status: 200, headers: { 'content-type': type } });
  assert.deepEqual(
    await clientWith(answer('application/zip')).exportRevision(artifact.id, 1),
    zip,
  );
  // A proxy's page or a server that does not export yet is not an archive.
  for (const type of ['text/html; charset=utf-8', 'application/json']) {
    const error = await failure(
      clientWith(answer(type)).exportRevision(artifact.id, 1),
    );
    assert.equal(error.code, 'invalid_response', type);
  }
  const cut = clientWith(() => {
    const response = cutOff(200, 'PK');
    response.headers.set('content-type', 'application/zip');
    return response;
  });
  const error = await failure(cut.exportRevision(artifact.id, 1));
  assert.equal(error.code, 'network_error');
});

/** Each whole number from 0 to `last`. */
function upTo(last) {
  return Array.from({ length: last + 1 }, (_, number) => number);
}

test('A part is accepted only when it is exactly the bytes asked for', async () => {
  const bytes = new TextEncoder().encode('abcdef');
  const ranges = upTo(4).flatMap((offset) =>
    [undefined, 1, 2, 3, 4, 5].map((length) => ({ offset, length })),
  );
  // Every Content-Range of positions up to 5, with a body of its length or
  // one byte off.
  const answers = upTo(5).flatMap((first) =>
    upTo(5).flatMap((last) =>
      upTo(5).flatMap((size) =>
        [-1, 0, 1].map((off) => ({
          first,
          last,
          size,
          received: Math.max(0, last - first + 1 + off),
        })),
      ),
    ),
  );
  for (const { offset, length } of ranges) {
    const range = length === undefined ? { offset } : { offset, length };
    for (const { first, last, size, received } of answers) {
      let sent;
      const client = new ArtifreeClient({
        baseUrl: 'http://127.0.0.1:7410',
        accessToken: 'token',
        fetch: async (_url, init) => {
          sent = init.headers.range;
          return new Response(bytes.slice(first, first + received), {
            status: 206,
            headers: { 'content-range': `bytes ${first}-${last}/${size}` },
          });
        },
      });
      // The size the answer states decides where the part ends.
      const end = Math.min(length === undefined ? size : offset + length, size);
      const exact =
        offset < end &&
        first === offset &&
        last === end - 1 &&
        received === end - offset;
      const what = `${JSON.stringify(range)}: bytes ${first}-${last}/${size}, ${received} bytes`;
      const read = client.readFilePart(artifact.id, 1, 'a.txt', range);
      if (exact)
        assert.deepEqual(
          await read,
          { content: bytes.slice(offset, end), offset, size },
          what,
        );
      else assert.equal((await failure(read)).code, 'invalid_response', what);
      const lastAsked = length === undefined ? '' : offset + length - 1;
      assert.equal(sent, `bytes=${offset}-${lastAsked}`, what);
    }
  }
});

test('A part comes only as one well-formed byte range', async () => {
  const answer =
    (status, headers, body = 'ab') =>
    () =>
      new Response(body, { status, headers });
  const read = (respond) =>
    clientWith(respond).readFilePart(artifact.id, 1, 'a.txt', {
      offset: 0,
      length: 2,
    });
  // The unit is matched without regard to case.
  const part = await read(answer(206, { 'content-range': 'BYTES 0-1/4' }));
  assert.deepEqual(
    [part.content, part.size],
    [new TextEncoder().encode('ab'), 4],
  );
  for (const [status, headers] of [
    // A server or proxy that ignored the range sent the whole file.
    [200, {}],
    [200, { 'content-range': 'bytes 0-1/4' }],
    [206, {}],
    [206, { 'content-range': 'bytes 0-1/*' }],
    [206, { 'content-range': 'bytes */4' }],
    [206, { 'content-range': 'bytes=0-1/4' }],
    [206, { 'content-range': 'bytes 0-1/4, bytes 0-1/4' }],
    [206, { 'content-range': 'items 0-1/4' }],
    [206, { 'content-range': 'bytes 0x0-1/4' }],
    [206, { 'content-range': 'bytes 0-1/9007199254740993' }],
    [206, { 'content-type': 'multipart/byteranges; boundary=x' }],
  ]) {
    const error = await failure(read(answer(status, headers)));
    assert.equal(error.code, 'invalid_response', JSON.stringify(headers));
  }
  // The whole file from a server or proxy that ignored the range is not
  // read. The stream is finite, so a client that reads it still ends.
  let cancelled = false;
  let chunks = 0;
  const whole = new ReadableStream({
    pull(controller) {
      chunks += 1;
      if (chunks > 64) controller.close();
      else controller.enqueue(new Uint8Array(1024));
    },
    cancel() {
      cancelled = true;
    },
  });
  const ignored = await failure(read(() => new Response(whole)));
  assert.deepEqual([ignored.code, cancelled], ['invalid_response', true]);
  // A range that starts at or past the end is the server's refusal.
  const refusal = await failure(
    read(() =>
      Response.json(
        { error: { code: 'range_not_satisfiable', message: 'past the end' } },
        { status: 416, headers: { 'content-range': 'bytes */0' } },
      ),
    ),
  );
  assert.deepEqual(
    [refusal.status, refusal.code],
    [416, 'range_not_satisfiable'],
  );
});

test('A range that cannot be asked for is refused before sending', async () => {
  let requests = 0;
  const client = clientWith(() => {
    requests += 1;
    return new Response('a', {
      status: 206,
      headers: {
        'content-range': `bytes ${Number.MAX_SAFE_INTEGER - 1}-${Number.MAX_SAFE_INTEGER - 1}/${Number.MAX_SAFE_INTEGER}`,
      },
    });
  });
  for (const range of [
    { offset: -1 },
    { offset: 1.5 },
    { offset: Number.NaN },
    { offset: 0, length: 0 },
    { offset: 0, length: 1.5 },
    { offset: Number.MAX_SAFE_INTEGER, length: 2 },
    { offset: 2, length: Number.MAX_SAFE_INTEGER },
  ]) {
    const error = await failure(
      client.readFilePart(artifact.id, 1, 'a.txt', range),
    );
    assert.deepEqual(
      [error.status, error.code],
      [0, 'invalid_request'],
      JSON.stringify(range),
    );
  }
  assert.equal(requests, 0);
  // Positions up to the largest safe integer can be asked for and read.
  const part = await client.readFilePart(artifact.id, 1, 'a.txt', {
    offset: Number.MAX_SAFE_INTEGER - 1,
    length: 1,
  });
  assert.equal(part.size, Number.MAX_SAFE_INTEGER);
});

test('Every artifact carries its trash time: null or a time', async () => {
  const read = (body) =>
    clientWith(() => Response.json(body)).getArtifact(artifact.id);
  assert.equal((await read(artifact)).deleted_at_ms, null);
  assert.equal(
    (await read({ ...artifact, deleted_at_ms: 5 })).deleted_at_ms,
    5,
  );
  const missing = { ...artifact };
  delete missing.deleted_at_ms;
  for (const body of [missing, { ...artifact, deleted_at_ms: '5' }]) {
    const error = await failure(read(body));
    assert.equal(error.code, 'invalid_response', JSON.stringify(body));
  }
});

test('An error with a null current revision keeps its code', async () => {
  const client = clientWith(() =>
    Response.json(
      {
        error: {
          code: 'not_found',
          message: 'gone',
          current_revision: null,
        },
      },
      { status: 404 },
    ),
  );
  const error = await failure(client.getArtifact(artifact.id));
  assert.equal(error.code, 'not_found');
  assert.equal(error.currentRevision, undefined);
});

test('Uploads use the smaller wire form and keep exact bytes', () => {
  const encoder = new TextEncoder();
  const text = encodeFile('a.txt', encoder.encode('﻿plain text\r\n'));
  assert.deepEqual(text, {
    path: 'a.txt',
    encoding: 'utf8',
    content: '﻿plain text\r\n',
  });
  // JSON escapes NUL as \u0000 (six bytes), so base64 is smaller.
  const controls = encodeFile('b.txt', new Uint8Array(30));
  assert.equal(controls.encoding, 'base64');
  assert.equal(controls.content, Buffer.alloc(30).toString('base64'));
  const binary = encodeFile('c.png', new Uint8Array([0x89, 0xff, 0x00]));
  assert.equal(binary.encoding, 'base64');
});

test('A rate-limited response keeps its code and retry delay', async () => {
  const client = clientWith(() =>
    Response.json(
      { error: { code: 'rate_limited', message: 'too many requests' } },
      { status: 429, headers: { 'retry-after': '3' } },
    ),
  );
  const error = await failure(client.getArtifact(artifact.id));
  assert.equal(error.code, 'rate_limited');
  assert.equal(error.status, 429);
  assert.equal(error.retryAfterSeconds, 3);
});

test("A proxy's 429 is a rate limit although its body is not an API error", async () => {
  const client = clientWith(
    () =>
      new Response('<h1>Too Many Requests</h1>', {
        status: 429,
        headers: { 'retry-after': '4', 'content-type': 'text/html' },
      }),
  );
  const error = await failure(client.listArtifacts());
  assert.equal(error.code, 'rate_limited');
  assert.equal(error.retryAfterSeconds, 4);
});

test('A read is cancelled through its signal', async () => {
  let received;
  const client = new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:7410',
    accessToken: 'token',
    fetch: async (url, init) => {
      received = init.signal;
      init.signal.throwIfAborted();
      return Response.json(artifact);
    },
  });
  const controller = new AbortController();
  controller.abort();
  const error = await failure(
    client.getArtifact(artifact.id, { signal: controller.signal }),
  );
  assert.equal(received, controller.signal);
  assert.equal(error.code, 'network_error');
});

test('Lists send only the given query and return the next page token', async () => {
  const requests = [];
  const client = new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:7410',
    accessToken: 'token',
    fetch: async (url) => {
      requests.push(new URL(url));
      return Response.json({ artifacts: [artifact], next_page_token: 'next' });
    },
  });
  assert.deepEqual(await client.listArtifacts(), {
    artifacts: [artifact],
    next_page_token: 'next',
  });
  await client.listArtifacts({
    q: 'a plan & more',
    sort: 'created',
    pinned: false,
    limit: 2,
    page_token: 'next',
  });
  assert.equal(requests[0].search, '');
  assert.deepEqual(Object.fromEntries(requests[1].searchParams), {
    q: 'a plan & more',
    sort: 'created',
    pinned: 'false',
    limit: '2',
    page_token: 'next',
  });
  const missingFields = clientWith(() =>
    Response.json({ artifacts: [{ ...artifact, pinned: undefined }] }),
  );
  assert.equal(
    (await failure(missingFields.listArtifacts())).code,
    'invalid_response',
  );
});

test('A metadata conflict reports the current metadata revision', async () => {
  const client = clientWith(() =>
    Response.json(
      {
        error: {
          code: 'metadata_conflict',
          message: 'artifact metadata changed',
          current_metadata_revision: 4,
        },
      },
      { status: 409 },
    ),
  );
  const error = await failure(
    client.updateArtifact(
      artifact.id,
      { base_metadata_revision: 3, title: 'New' },
      { idempotencyKey: 'test-key-1' },
    ),
  );
  assert.equal(error.code, 'metadata_conflict');
  assert.equal(error.currentMetadataRevision, 4);
  assert.equal(error.currentRevision, undefined);
});

test('A restore names the revision, base, and key; manifests need digests', async () => {
  const file = {
    path: 'index.html',
    size: 3,
    sha256: 'ab'.repeat(32),
    media_type: 'text/html; charset=utf-8',
  };
  const commit = (files) => ({
    artifact: { ...artifact, current_revision: 3 },
    revision: { revision: 3, created_by: 'owner', created_at_ms: 1, files },
    replayed: false,
  });
  const sent = [];
  let files = [file];
  const client = new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:7410',
    accessToken: 'token',
    fetch: async (url, init) => {
      sent.push([init.method, String(url), init.headers, init.body]);
      return Response.json(commit(files), { status: 201 });
    },
  });
  const restore = () =>
    client.restoreRevision(
      artifact.id,
      1,
      { base_revision: 2 },
      { idempotencyKey: 'test-key-1' },
    );
  const restored = await restore();
  assert.deepEqual(restored.revision.files, [file]);
  const [method, url, headers, body] = sent[0];
  assert.equal(method, 'POST');
  assert.equal(
    url,
    `http://127.0.0.1:7410/v1/artifacts/${artifact.id}/revisions/1/restore`,
  );
  assert.equal(new Headers(headers).get('idempotency-key'), 'test-key-1');
  assert.deepEqual(JSON.parse(body), { base_revision: 2 });
  for (const sha256 of [undefined, 'AB'.repeat(32), 'ab'.repeat(31)]) {
    files = [{ ...file, sha256 }];
    const error = await failure(restore());
    assert.equal(error.code, 'invalid_response', String(sha256));
  }
});

test('A duplicate names the revision and title; a copy must say what it copied', async () => {
  const sent = [];
  let copied = ['files'];
  const client = new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:7410',
    accessToken: 'token',
    fetch: async (url, init) => {
      sent.push([String(url), init.body]);
      return Response.json(
        {
          artifact: { ...artifact, id: 'art_copy' },
          revision: {
            revision: 1,
            created_by: 'owner',
            created_at_ms: 1,
            files: [],
          },
          copied,
          replayed: false,
        },
        { status: 201 },
      );
    },
  });
  const duplicate = (request) =>
    client.duplicateArtifact(artifact.id, 2, request, {
      idempotencyKey: 'test-key-1',
    });
  assert.deepEqual((await duplicate({})).copied, ['files']);
  await duplicate({ title: 'Mine' });
  assert.deepEqual(sent, [
    [
      `http://127.0.0.1:7410/v1/artifacts/${artifact.id}/revisions/2/duplicate`,
      '{}',
    ],
    [
      `http://127.0.0.1:7410/v1/artifacts/${artifact.id}/revisions/2/duplicate`,
      '{"title":"Mine"}',
    ],
  ]);
  // A part this client does not know could be data it would not report.
  for (const parts of [undefined, 'files', ['files', 'data']]) {
    copied = parts;
    const error = await failure(duplicate({}));
    assert.equal(error.code, 'invalid_response', String(parts));
  }
});

// The change feed. Each rule below is one way an earlier version failed:
// a split line end, an unknown kind or event, an aborted wait, a proxy
// error, and a successful response that is not the feed.

const FEED = 'f'.repeat(32);
const cursor = (sequence) => `${FEED}.${sequence}`;
const changeData = (sequence, change = 'revised') =>
  JSON.stringify({
    cursor: cursor(sequence),
    artifact_id: artifact.id,
    change,
    revision: sequence,
    metadata_revision: 1,
    committed_at_ms: 1,
  });
const changeEvent = (sequence, change = 'revised') => ({
  type: 'change',
  change: {
    cursor: cursor(sequence),
    artifact_id: artifact.id,
    change,
    revision: sequence,
    metadata_revision: 1,
    committed_at_ms: 1,
  },
});

/** An event stream response whose body arrives in `parts` (strings or bytes). */
function stream(parts, { status = 200, onCancel } = {}) {
  const encoder = new TextEncoder();
  const body = new ReadableStream({
    start(controller) {
      for (const part of parts)
        controller.enqueue(
          typeof part === 'string' ? encoder.encode(part) : part,
        );
      if (onCancel === undefined) controller.close();
    },
    cancel: onCancel,
  });
  return new Response(body, {
    status,
    headers: { 'content-type': 'text/event-stream; charset=utf-8' },
  });
}

async function eventsOf(response) {
  const events = [];
  for await (const event of await clientWith(() => response).openChanges())
    events.push(event);
  return events;
}

test('The feed parser follows the event stream rules', async () => {
  const data = changeData(4);
  // A CRLF split after its CR, CR-only and LF line ends, a final CR blank line.
  assert.deepEqual(
    await eventsOf(
      stream([
        'event: change\r',
        `\ndata: ${data}\r`,
        '\n\r',
        `\nevent:change\rdata:${data}\r\r`,
      ]),
    ),
    [changeEvent(4), changeEvent(4)],
  );
  assert.deepEqual(
    await eventsOf(stream([`event: change\ndata: ${data}\r`, '\r'])),
    [changeEvent(4)],
  );
  // A byte order mark and a character split across chunks; comments, unknown
  // fields and events, an event without data, and joined data lines.
  const bytes = new TextEncoder().encode(
    `event: change\ndata: {"cursor":"${cursor(5)}","artifact_id":"${artifact.id}",\ndata: "change":"created","revision":5,"metadata_revision":1,"committed_at_ms":1}\n\n`,
  );
  const split = bytes.indexOf(0x22, 20) + 1;
  assert.deepEqual(
    await eventsOf(
      stream([
        new Uint8Array([0xef, 0xbb, 0xbf]),
        ': heartbeat\n\nretry: 1\nid: x\nevent: future\ndata: {}\n\nevent: change\n\n',
        bytes.subarray(0, split),
        bytes.subarray(split),
      ]),
    ),
    [changeEvent(5, 'created')],
  );
  // An event the body ends before its blank line is dropped.
  assert.deepEqual(
    await eventsOf(stream([`event: change\ndata: ${data}\n`])),
    [],
  );
});

test('Deletes and recoveries arrive as themselves', async () => {
  assert.deepEqual(
    await eventsOf(
      stream([
        `event: change\ndata: ${changeData(7, 'deleted')}\n\n`,
        `event: change\ndata: ${changeData(8, 'recovered')}\n\n`,
      ]),
    ),
    [changeEvent(7, 'deleted'), changeEvent(8, 'recovered')],
  );
});

test('A change kind this client does not know is delivered as unknown', async () => {
  assert.deepEqual(
    await eventsOf(
      stream([`event: change\ndata: ${changeData(7, 'renamed')}\n\n`]),
    ),
    [changeEvent(7, 'unknown')],
  );
  const broken = await failure(
    eventsOf(stream(['event: ready\ndata: {"cursor":7}\n\n'])),
  );
  assert.deepEqual([broken.status, broken.code], [200, 'invalid_response']);
});

/**
 * Follows a feed whose successive connections are answered by `answers`;
 * the last answer repeats. Stops after `events` events, or rejects.
 */
async function followed(answers, { events: wanted = 1, after } = {}) {
  const controller = new AbortController();
  const requests = [];
  const events = [];
  const disconnects = [];
  const client = new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:7410',
    accessToken: 'token',
    fetch: async (url, init) => {
      requests.push(init.headers['last-event-id']);
      const answer = answers.length > 1 ? answers.shift() : answers[0];
      return answer();
    },
  });
  await client.followChanges({
    signal: controller.signal,
    retryDelayMs: { min: 1, max: 2 },
    ...(after === undefined ? {} : { after }),
    onEvent(event) {
      events.push(event);
      if (events.length === wanted) controller.abort();
    },
    onDisconnect(error, retryInMs) {
      disconnects.push([error?.status, error?.code, retryInMs]);
    },
  });
  return { requests, events, disconnects };
}

test('Following retries lost connections, 429, and any 5xx from the last cursor', async () => {
  const result = await followed(
    [
      () => stream([`event: change\ndata: ${changeData(1)}\n\n`]),
      () => {
        throw new TypeError('connection refused');
      },
      () => new Response('', { status: 502 }),
      () => new Response('<h1>Bad gateway</h1>', { status: 504 }),
      () =>
        Response.json(
          { error: { code: 'unavailable', message: 'busy' } },
          { status: 503, headers: { 'retry-after': '0' } },
        ),
      () =>
        Response.json(
          { error: { code: 'rate_limited', message: 'too many streams' } },
          { status: 429, headers: { 'retry-after': '0' } },
        ),
      () => stream([`event: change\ndata: ${changeData(2)}\n\n`]),
    ],
    { events: 2, after: cursor(0) },
  );
  assert.deepEqual(result.events, [changeEvent(1), changeEvent(2)]);
  assert.deepEqual(result.requests, [cursor(0), ...Array(6).fill(cursor(1))]);
  assert.deepEqual(result.disconnects, [
    [undefined, undefined, 1],
    [0, 'network_error', 2],
    [502, 'invalid_response', 2],
    [504, 'invalid_response', 2],
    [503, 'unavailable', 0],
    [429, 'rate_limited', 0],
  ]);
});

test('Following stops on failures a retry cannot fix', async () => {
  const fatal = [
    [
      () =>
        Response.json(
          { error: { code: 'unauthenticated', message: 'no' } },
          { status: 401 },
        ),
      [401, 'unauthenticated'],
    ],
    [
      () =>
        Response.json(
          { error: { code: 'invalid_request', message: 'bad cursor' } },
          { status: 400 },
        ),
      [400, 'invalid_request'],
    ],
    [
      () => new Response('<h1>Not found</h1>', { status: 404 }),
      [404, 'invalid_response'],
    ],
    // A sign-in page or other successful response that is not the feed.
    [
      () =>
        new Response('<html>Sign in</html>', {
          status: 200,
          headers: { 'content-type': 'text/html' },
        }),
      [200, 'invalid_response'],
    ],
    [
      () => stream(['event: change\ndata: {"cursor":1}\n\n']),
      [200, 'invalid_response'],
    ],
  ];
  for (const [answer, expected] of fatal) {
    const error = await failure(followed([answer]));
    assert.deepEqual([error.status, error.code], expected);
  }
});

test('Aborting stops following at once, also during a wait or a stream', async () => {
  // The server asks for a long wait; the abort comes in onDisconnect.
  const client = clientWith(() =>
    Response.json(
      { error: { code: 'rate_limited', message: 'too many streams' } },
      { status: 429, headers: { 'retry-after': '600' } },
    ),
  );
  const controller = new AbortController();
  const started = performance.now();
  await client.followChanges({
    signal: controller.signal,
    onEvent() {},
    onDisconnect(error, retryInMs) {
      assert.deepEqual([error?.code, retryInMs], ['rate_limited', 600_000]);
      controller.abort();
    },
  });
  assert.ok(
    performance.now() - started < 1000,
    'followChanges waited after the abort',
  );

  // An open stream is cancelled when following stops.
  let cancelled;
  const closed = new Promise((resolve) => {
    cancelled = resolve;
  });
  const open = new AbortController();
  await clientWith(() =>
    stream(['event: ready\ndata: {"cursor":"c.0"}\n\n'], {
      onCancel: cancelled,
    }),
  ).followChanges({
    signal: open.signal,
    onEvent() {
      setTimeout(() => open.abort(), 10);
    },
  });
  await closed;
});

test('No event is delivered after an abort, also from the same chunk', async () => {
  const chunk = [1, 2, 3, 4]
    .map((sequence) => `event: change\ndata: ${changeData(sequence)}\n\n`)
    .join('');
  const controller = new AbortController();
  const delivered = [];
  await clientWith(() => stream([chunk])).followChanges({
    signal: controller.signal,
    onEvent(event) {
      delivered.push(event);
      controller.abort();
    },
  });
  assert.deepEqual(delivered, [changeEvent(1)]);

  const reading = new AbortController();
  const read = [];
  for await (const event of await clientWith(() => stream([chunk])).openChanges(
    { signal: reading.signal },
  )) {
    read.push(event);
    reading.abort();
  }
  assert.deepEqual(read, [changeEvent(1)]);
});

test("A proxy's Retry-After is honored although its body is not an API error", async () => {
  const controller = new AbortController();
  const waits = [];
  await clientWith(
    () =>
      new Response('<h1>Service unavailable</h1>', {
        status: 503,
        headers: { 'retry-after': '7' },
      }),
  ).followChanges({
    signal: controller.signal,
    onEvent() {},
    onDisconnect(error, retryInMs) {
      waits.push([error?.code, retryInMs]);
      controller.abort();
    },
  });
  assert.deepEqual(waits, [['invalid_response', 7000]]);
});

test('What onEvent throws ends following, even an API error', async () => {
  let requests = 0;
  const client = new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:7410',
    accessToken: 'token',
    fetch: async () => {
      requests += 1;
      return stream([`event: change\ndata: ${changeData(1)}\n\n`]);
    },
  });
  const thrown = new ArtifreeApiError('handler failed', {
    status: 0,
    code: 'network_error',
  });
  const error = await client
    .followChanges({
      signal: new AbortController().signal,
      onEvent() {
        throw thrown;
      },
    })
    .then(
      () => assert.fail('following must reject'),
      (rejection) => rejection,
    );
  assert.equal(error, thrown);
  assert.equal(requests, 1, 'a handler failure was retried');
});

// A runtime's network error can quote the URL it was given, passwords
// included (decision 0028). The client shows at most origin + path and
// attaches the original error only for a plain http(s) address.
const SECRET = 'hunter2';
const ID = `art_${'1'.repeat(32)}`;
const addresses = [
  ['plain', 'http://127.0.0.1:7410', 'http://127.0.0.1:7410', true],
  [
    'plain with a path',
    'https://api.example/prefix/',
    'https://api.example/prefix',
    true,
  ],
  ['the page origin', '', 'this origin', true],
  [
    'credentials',
    `https://user:${SECRET}@api.example/prefix`,
    'https://api.example/prefix',
    false,
  ],
  [
    'a query',
    `https://api.example/x?key=${SECRET}`,
    'https://api.example/x',
    false,
  ],
  [
    'a fragment',
    `https://api.example/x#${SECRET}`,
    'https://api.example/x',
    false,
  ],
  ['an empty query', 'https://api.example?', 'https://api.example', false],
  [
    'a missing colon',
    `https//user:p%40ss-${SECRET}`,
    'the configured server',
    false,
  ],
  [
    'an opaque URL',
    `user:${SECRET}@api.example`,
    'the configured server',
    false,
  ],
  [
    'another scheme',
    `ftp://user:${SECRET}@api.example`,
    'the configured server',
    false,
  ],
];

for (const [name, baseUrl, label, plain] of addresses) {
  const quoting = new TypeError(`fetch failed for ${baseUrl}/v1/x`);

  test(`a send failure names ${name} as ${label} and ${plain ? 'keeps' : 'drops'} its cause`, async () => {
    const client = new ArtifreeClient({
      baseUrl,
      accessToken: 'token',
      fetch: async () => {
        throw quoting;
      },
    });
    const error = await failure(client.getArtifact(ID));
    assert.equal(error.code, 'network_error');
    assert.equal(
      error.message,
      `Could not reach the Artifree API at ${label}.`,
    );
    assert.equal(error.cause, plain ? quoting : undefined);
    assert.ok(!error.message.includes(SECRET));
  });

  test(`a failure while reading a file names ${name} as ${label} and ${plain ? 'keeps' : 'drops'} its cause`, async () => {
    const body = new TypeError(`terminated ${baseUrl}`);
    const response = new Response('x');
    Object.defineProperty(response, 'arrayBuffer', {
      value: () => Promise.reject(body),
    });
    const client = new ArtifreeClient({
      baseUrl,
      accessToken: 'token',
      fetch: async () => response,
    });
    const error = await failure(client.readFile(ID, 1, 'index.html'));
    assert.equal(error.code, 'network_error');
    assert.equal(
      error.message,
      `The connection to ${label} ended while reading index.html.`,
    );
    assert.equal(error.cause, plain ? body : undefined);
  });

  test(`a failure while reading JSON from ${name} ${plain ? 'keeps' : 'drops'} its cause`, async () => {
    const body = new TypeError(`terminated ${baseUrl}`);
    const response = new Response('{}');
    Object.defineProperty(response, 'text', {
      value: () => Promise.reject(body),
    });
    const client = new ArtifreeClient({
      baseUrl,
      accessToken: 'token',
      fetch: async () => response,
    });
    const error = await failure(client.getArtifact(ID));
    assert.equal(error.code, 'network_error');
    assert.equal(error.cause, plain ? body : undefined);
  });
}
