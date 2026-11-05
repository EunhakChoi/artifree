import assert from 'node:assert/strict';
import { test } from 'node:test';
import { ArtifreeClient } from '../../packages/client/dist/index.js';

const page = {
  artifact_id: 'artifact',
  path: '',
  data_version: 3,
  exists: true,
  names: ['first'],
};

function client(fetch, clock) {
  return new ArtifreeClient({
    baseUrl: 'http://localhost',
    accessToken: 'token',
    fetch,
    clock,
  });
}

test('lists one page with encoded options and a continuation token', async () => {
  let request;
  const api = client(async (url, init) => {
    request = { url: new URL(url), init };
    return Response.json({ ...page, next_page_token: 'next' });
  });
  const result = await api.listDataMembers('artifact', {
    path: 'a/b',
    limit: 5,
    pageToken: 'a+b',
    atDataVersion: 3,
  });
  assert.equal(request.url.pathname, '/v1/artifacts/artifact/data/members');
  assert.deepEqual(Object.fromEntries(request.url.searchParams), {
    path: 'a/b',
    limit: '5',
    page_token: 'a+b',
    at_data_version: '3',
  });
  assert.equal(request.init.method, 'GET');
  assert.equal(result.next_page_token, 'next');
});

test('rejects a wrong artifact, invalid shape, and absent-value tokens', async () => {
  for (const bad of [
    { ...page, artifact_id: 'other' },
    { ...page, names: [1] },
    { ...page, data_version: Number.MAX_SAFE_INTEGER + 1 },
    { ...page, next_page_token: '' },
    { ...page, next_page_token: null },
  ]) {
    const api = client(async () => Response.json(bad));
    await assert.rejects(api.listDataMembers('artifact'), {
      code: 'invalid_response',
    });
  }
});

test('accepts server-owned name and path semantics without adding client rules', async () => {
  const answer = {
    ...page,
    path: 'unexpected',
    names: ['duplicate', 'duplicate', ''],
  };
  const api = client(async () => Response.json(answer));
  assert.deepEqual(await api.listDataMembers('artifact'), answer);
});

test('accepts a safe integer data version without enforcing server data rules', async () => {
  const answer = { ...page, data_version: -1 };
  const api = client(async () => Response.json(answer));
  assert.deepEqual(await api.listDataMembers('artifact'), answer);
});

test('an already aborted request sends nothing and preserves the reason', async () => {
  const controller = new AbortController();
  const reason = new Error('stopped');
  controller.abort(reason);
  let calls = 0;
  const api = client(async () => {
    calls++;
    return Response.json(page);
  });
  await assert.rejects(
    api.listDataMembers('artifact', { signal: controller.signal }),
    (error) => error === reason,
  );
  assert.equal(calls, 0);
});

test('oversized success and HTTP failures reject without retry', async () => {
  let calls = 0;
  const api = client(async () => {
    calls++;
    return new Response('x'.repeat(1_064_961));
  });
  await assert.rejects(api.listDataMembers('artifact'), {
    code: 'invalid_response',
  });
  assert.equal(calls, 1);
  const denied = client(
    async () => new Response('proxy error', { status: 503 }),
  );
  await assert.rejects(denied.listDataMembers('artifact'), { status: 503 });
});

test('accepts a success body exactly at the byte limit', async () => {
  const empty = JSON.stringify({ ...page, padding: '' });
  const body = JSON.stringify({
    ...page,
    padding: 'x'.repeat(1_064_960 - empty.length),
  });
  assert.equal(Buffer.byteLength(body), 1_064_960);
  const api = client(async () => new Response(body));
  assert.deepEqual(await api.listDataMembers('artifact'), page);
});

test('an oversized error body is cancelled and its HTTP status is retained', async () => {
  let cancelled = 0;
  const body = new ReadableStream({
    pull(controller) {
      controller.enqueue(new Uint8Array(65_537));
    },
    cancel() {
      cancelled++;
    },
  });
  const api = client(async () => new Response(body, { status: 404 }));
  await assert.rejects(api.listDataMembers('artifact'), { status: 404 });
  assert.equal(cancelled, 1);
});

for (const status of [401, 404]) {
  test(`a stalled ${status} error body retains its status`, async () => {
    let timeout;
    let cancelled = 0;
    const clock = {
      after(callback) {
        timeout = callback;
        return () => {};
      },
    };
    const body = new ReadableStream({
      cancel() {
        cancelled++;
      },
    });
    const api = client(async () => new Response(body, { status }), clock);
    const result = api.listDataMembers('artifact');
    await new Promise((resolve) => setImmediate(resolve));
    timeout();
    await assert.rejects(result, { status });
    assert.equal(cancelled, 1);
  });
}

test('aborting before headers preserves the reason without a retry', async () => {
  const controller = new AbortController();
  const reason = new Error('abort before headers');
  let calls = 0;
  const api = client(async (_url, init) => {
    calls++;
    return new Promise((_resolve, reject) => {
      init.signal.addEventListener('abort', () => reject(init.signal.reason));
    });
  });
  const result = api.listDataMembers('artifact', { signal: controller.signal });
  controller.abort(reason);
  await assert.rejects(result, (error) => error === reason);
  assert.equal(calls, 1);
});

test('a stalled success body rejects and cancels its reader', async () => {
  let timeout;
  let cancelled = 0;
  const clock = {
    after(callback, milliseconds) {
      assert.equal(milliseconds, 45_000);
      timeout = callback;
      return () => {
        timeout = undefined;
      };
    },
  };
  const stream = new ReadableStream({
    cancel() {
      cancelled++;
    },
  });
  const api = client(async () => new Response(stream), clock);
  const result = api.listDataMembers('artifact');
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(typeof timeout, 'function');
  timeout();
  await assert.rejects(result, { code: 'network_error' });
  assert.equal(cancelled, 1);
});

test('aborting a success body preserves its exact reason and cancels the reader', async () => {
  const controller = new AbortController();
  const reason = new Error('abort body');
  let cancelled = 0;
  const stream = new ReadableStream({
    cancel() {
      cancelled++;
    },
  });
  const api = client(async () => new Response(stream));
  const result = api.listDataMembers('artifact', { signal: controller.signal });
  await new Promise((resolve) => setImmediate(resolve));
  controller.abort(reason);
  await assert.rejects(result, (error) => error === reason);
  assert.equal(cancelled, 1);
});

test('a stalled error body keeps its status and Retry-After', async () => {
  let timeout;
  let cancelled = 0;
  const clock = {
    after(callback, milliseconds) {
      assert.equal(milliseconds, 45_000);
      timeout = callback;
      return () => {
        timeout = undefined;
      };
    },
  };
  const body = new ReadableStream({
    cancel() {
      cancelled++;
    },
  });
  const api = client(
    async () =>
      new Response(body, {
        status: 429,
        headers: { 'retry-after': '7' },
      }),
    clock,
  );
  const result = api.listDataMembers('artifact');
  await new Promise((resolve) => setImmediate(resolve));
  timeout();
  await assert.rejects(result, {
    status: 429,
    code: 'rate_limited',
    retryAfterSeconds: 7,
  });
  assert.equal(cancelled, 1);
});

test('aborting an error body preserves the reason and cancels the reader', async () => {
  const controller = new AbortController();
  const reason = new Error('abort error body');
  let cancelled = 0;
  const body = new ReadableStream({
    cancel() {
      cancelled++;
    },
  });
  const api = client(async () => new Response(body, { status: 404 }));
  const result = api.listDataMembers('artifact', { signal: controller.signal });
  await new Promise((resolve) => setImmediate(resolve));
  controller.abort(reason);
  await assert.rejects(result, (error) => error === reason);
  assert.equal(cancelled, 1);
});

test('an abort racing an error response wins over the HTTP status', async () => {
  const controller = new AbortController();
  const reason = new Error('abort at response');
  const api = client(async () => {
    controller.abort(reason);
    return new Response('not found', { status: 404 });
  });
  await assert.rejects(
    api.listDataMembers('artifact', { signal: controller.signal }),
    (error) => error === reason,
  );
});

test('waiting for headers installs no stall timer', async () => {
  const controller = new AbortController();
  const reason = new Error('stop waiting');
  const clock = {
    after() {
      throw new Error('timer before headers');
    },
  };
  const api = client(
    async (_url, init) =>
      new Promise((_resolve, reject) => {
        init.signal.addEventListener('abort', () => reject(init.signal.reason));
      }),
    clock,
  );
  const result = api.listDataMembers('artifact', { signal: controller.signal });
  await new Promise((resolve) => setImmediate(resolve));
  controller.abort(reason);
  await assert.rejects(result, (error) => error === reason);
});
