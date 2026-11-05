import assert from 'node:assert/strict';
import { createHook } from 'node:async_hooks';
import { cp, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { pathToFileURL } from 'node:url';
import {
  ArtifreeApiError,
  ArtifreeClient,
} from '../../packages/client/dist/index.js';
import { followData } from '../../packages/client/dist/follow-data.js';

const artifact = `art_${'0'.repeat(32)}`;
const cursor = (sequence) => `${'f'.repeat(32)}.${sequence}`;
const flush = () => new Promise((resolve) => setImmediate(resolve));

for (const loadFails of [false, true])
  test(`abort settles while the follower module loads, even if loading ${loadFails ? 'fails' : 'succeeds'}`, async () => {
    const dir = await mkdtemp(join(tmpdir(), 'artifree-follower-'));
    try {
      await cp(new URL('../../packages/client/dist/', import.meta.url), dir, {
        recursive: true,
      });
      await writeFile(join(dir, 'package.json'), '{"type":"module"}');
      await writeFile(
        join(dir, 'gate.mjs'),
        `export let release, fail;
       export const gate = new Promise((resolve, reject) => { release = resolve; fail = reject; });
       export let entered;
       export const started = new Promise((resolve) => { entered = resolve; });
       export let calls = 0;
       export function called() { calls++; }`,
      );
      await writeFile(
        join(dir, 'follow-data.js'),
        `import { gate, entered, called } from './gate.mjs';
       entered();
       await gate;
       export function followData() { called(); }`,
      );
      const gate = await import(pathToFileURL(join(dir, 'gate.mjs')).href);
      const { ArtifreeClient: Client } = await import(
        pathToFileURL(join(dir, 'client.js')).href
      );
      const controller = new AbortController();
      const api = new Client({
        baseUrl: 'http://localhost',
        accessToken: 'token',
      });
      let settled = false;
      const finished = api
        .followData(artifact, { signal: controller.signal, onData() {} })
        .then(() => {
          settled = true;
        });
      await gate.started;
      controller.abort();
      await flush();
      const promptly = settled;
      if (loadFails) gate.fail(new Error('chunk unavailable'));
      else gate.release();
      await finished;
      await flush();
      assert.equal(promptly, true, 'abort settles before the module loads');
      assert.equal(gate.calls, 0, 'the loaded follower is never started');
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  });

function feed() {
  let controller;
  let cancelled = 0;
  const body = new ReadableStream({
    start(value) {
      controller = value;
    },
    cancel() {
      cancelled++;
    },
  });
  return {
    body,
    send(type, value) {
      controller.enqueue(
        new TextEncoder().encode(
          `event: ${type}\ndata: ${JSON.stringify(value)}\n\n`,
        ),
      );
    },
    get cancelled() {
      return cancelled;
    },
    comment() {
      controller.enqueue(new TextEncoder().encode(': ping\n\n'));
    },
    fail(error) {
      controller.error(error);
    },
  };
}

function changed(version, sequence = version) {
  return {
    cursor: cursor(sequence),
    artifact_id: artifact,
    change: 'data_changed',
    revision: 1,
    metadata_revision: 1,
    committed_at_ms: 1,
    data_version: version,
  };
}

function snapshot(version, value = {}) {
  return {
    artifact_id: artifact,
    path: '',
    data_version: version,
    exists: true,
    value,
  };
}

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((done, fail) => {
    resolve = done;
    reject = fail;
  });
  return { promise, resolve, reject };
}

class FakeClock {
  now = 0;
  timers = new Map();
  next = 0;

  after(callback, milliseconds) {
    const id = this.next++;
    this.timers.set(id, { callback, at: this.now + milliseconds });
    return () => this.timers.delete(id);
  }

  get nextIn() {
    const times = [...this.timers.values()].map((timer) => timer.at);
    return times.length === 0 ? undefined : Math.min(...times) - this.now;
  }

  async advance(milliseconds) {
    const target = this.now + milliseconds;
    for (;;) {
      let due;
      for (const [id, timer] of this.timers)
        if (timer.at <= target && (due === undefined || timer.at < due.at))
          due = { id, ...timer };
      if (due === undefined) break;
      this.timers.delete(due.id);
      this.now = due.at;
      due.callback();
      await flush();
    }
    this.now = target;
    await flush();
  }
}

test('a change during the snapshot triggers one subsequent read', async () => {
  const source = feed();
  const firstRead = deferred();
  const readStarted = deferred();
  const controller = new AbortController();
  const delivered = [];
  let reads = 0;
  const api = new ArtifreeClient({
    baseUrl: 'http://localhost',
    accessToken: 'token',
    fetch: async (url) => {
      if (url.endsWith('/v1/changes'))
        return new Response(source.body, {
          headers: { 'content-type': 'text/event-stream' },
        });
      reads++;
      if (reads === 1) {
        readStarted.resolve();
        return firstRead.promise;
      }
      return Response.json(snapshot(2, { version: 2 }));
    },
  });
  const finished = api.followData(artifact, {
    signal: controller.signal,
    onData(event) {
      delivered.push(event);
      if (event.type === 'update') controller.abort();
    },
  });
  source.send('ready', { cursor: cursor(0) });
  await readStarted.promise;
  source.send('change', changed(2));
  await flush();
  firstRead.resolve(Response.json(snapshot(1, { version: 1 })));
  try {
    await flush();
    assert.equal(reads, 2, 'the recorded change triggers a second read');
    await finished;
  } finally {
    controller.abort();
  }
  assert.deepEqual(delivered, [
    { type: 'snapshot', data: snapshot(1, { version: 1 }) },
    { type: 'update', data: snapshot(2, { version: 2 }) },
  ]);
  assert.equal(reads, 2);
  assert.equal(source.cancelled, 1);
});

test('a change before ready rejects without reading data', async () => {
  const source = feed();
  const controller = new AbortController();
  let reads = 0;
  const api = new ArtifreeClient({
    baseUrl: 'http://localhost',
    accessToken: 'token',
    fetch: async (url) => {
      if (url.endsWith('/v1/changes'))
        return new Response(source.body, {
          headers: { 'content-type': 'text/event-stream' },
        });
      reads++;
      return Response.json(snapshot(1));
    },
  });
  const finished = api.followData(artifact, {
    signal: controller.signal,
    onData() {},
  });
  const rejected = assert.rejects(finished, { code: 'invalid_response' });
  source.send('change', changed(1));
  await flush();
  controller.abort();
  await rejected;
  assert.equal(reads, 0);
});

test('a missing artifact delivers unavailable once and ends', async () => {
  const source = feed();
  const delivered = [];
  const controller = new AbortController();
  const api = new ArtifreeClient({
    baseUrl: 'http://localhost',
    accessToken: 'token',
    fetch: async (url) =>
      url.endsWith('/v1/changes')
        ? new Response(source.body, {
            headers: { 'content-type': 'text/event-stream' },
          })
        : new Response('missing', { status: 404 }),
  });
  const finished = api.followData(artifact, {
    signal: controller.signal,
    onData(event) {
      delivered.push(event);
    },
  });
  source.send('ready', { cursor: cursor(0) });
  await finished;
  assert.deepEqual(delivered, [{ type: 'unavailable' }]);
  assert.equal(source.cancelled, 1);
});

test('abort resolves even when the initial fetch ignores cancellation', async () => {
  const started = deferred();
  const controller = new AbortController();
  const clock = new FakeClock();
  const api = new ArtifreeClient({
    baseUrl: 'http://localhost',
    accessToken: 'token',
    clock,
    fetch: async () => {
      started.resolve();
      return new Promise(() => {});
    },
  });
  const finished = api.followData(artifact, {
    signal: controller.signal,
    onData() {
      assert.fail('unexpected delivery');
    },
  });
  await started.promise;
  await clock.advance(3_600_000);
  assert.equal(clock.timers.size, 0);
  controller.abort();
  await finished;
});

test('duplicate versions do not read and a reset can start an older epoch', async () => {
  const source = feed();
  const controller = new AbortController();
  const delivered = [];
  let reads = 0;
  const api = new ArtifreeClient({
    baseUrl: 'http://localhost',
    accessToken: 'token',
    fetch: async (url) => {
      if (url.endsWith('/v1/changes'))
        return new Response(source.body, {
          headers: { 'content-type': 'text/event-stream' },
        });
      reads++;
      return Response.json(snapshot(reads === 2 ? 2 : 1));
    },
  });
  const finished = api.followData(artifact, {
    signal: controller.signal,
    onData(event) {
      delivered.push(event);
      if (delivered.length === 1) {
        source.send('change', changed(1));
        source.send('change', changed(2));
      } else if (delivered.length === 2)
        source.send('reset', { cursor: cursor(3) });
      else controller.abort();
    },
  });
  source.send('ready', { cursor: cursor(0) });
  await finished;
  assert.deepEqual(delivered, [
    { type: 'snapshot', data: snapshot(1) },
    { type: 'update', data: snapshot(2) },
    { type: 'snapshot', data: snapshot(1) },
  ]);
  assert.equal(reads, 3);
});

test('a read sent after a change cannot answer below the named version', async () => {
  const source = feed();
  const firstDelivered = deferred();
  const controller = new AbortController();
  let reads = 0;
  let deliveries = 0;
  const api = new ArtifreeClient({
    baseUrl: 'http://localhost',
    accessToken: 'token',
    fetch: async (url) => {
      if (url.endsWith('/v1/changes'))
        return new Response(source.body, {
          headers: { 'content-type': 'text/event-stream' },
        });
      reads++;
      return Response.json(snapshot(reads === 1 ? 1 : 2));
    },
  });
  const finished = api.followData(artifact, {
    signal: controller.signal,
    onData() {
      deliveries++;
      firstDelivered.resolve();
      if (deliveries > 1) controller.abort();
    },
  });
  source.send('ready', { cursor: cursor(0) });
  await firstDelivered.promise;
  source.send('change', changed(3));
  await assert.rejects(finished, { code: 'invalid_response' });
  assert.equal(reads, 2);
});

test('deletion ends once and discards a late pending read', async () => {
  const source = feed();
  const pending = deferred();
  const started = deferred();
  const controller = new AbortController();
  const delivered = [];
  const api = new ArtifreeClient({
    baseUrl: 'http://localhost',
    accessToken: 'token',
    fetch: async (url) => {
      if (url.endsWith('/v1/changes'))
        return new Response(source.body, {
          headers: { 'content-type': 'text/event-stream' },
        });
      started.resolve();
      return pending.promise;
    },
  });
  const finished = api.followData(artifact, {
    signal: controller.signal,
    onData(event) {
      delivered.push(event);
    },
  });
  source.send('ready', { cursor: cursor(0) });
  await started.promise;
  source.send('change', {
    ...changed(2),
    change: 'deleted',
    data_version: undefined,
  });
  await finished;
  pending.resolve(Response.json(snapshot(9)));
  await flush();
  assert.deepEqual(delivered, [{ type: 'unavailable' }]);
});

test('a reset discards an old read even if its fetch ignores abort', async () => {
  const source = feed();
  const pending = deferred();
  const started = deferred();
  const controller = new AbortController();
  const delivered = [];
  let reads = 0;
  const api = new ArtifreeClient({
    baseUrl: 'http://localhost',
    accessToken: 'token',
    fetch: async (url) => {
      if (url.endsWith('/v1/changes'))
        return new Response(source.body, {
          headers: { 'content-type': 'text/event-stream' },
        });
      reads++;
      if (reads === 1) {
        started.resolve();
        return pending.promise;
      }
      return Response.json(snapshot(4));
    },
  });
  const finished = api.followData(artifact, {
    signal: controller.signal,
    onData(event) {
      delivered.push(event);
      controller.abort();
    },
  });
  source.send('ready', { cursor: cursor(0) });
  await started.promise;
  source.send('reset', { cursor: cursor(1) });
  await finished;
  pending.resolve(Response.json(snapshot(10)));
  await flush();
  assert.deepEqual(delivered, [{ type: 'snapshot', data: snapshot(4) }]);
  assert.equal(reads, 2);
});

test('a dropped stream backs off then starts a new snapshot without a cursor', async () => {
  const first = feed();
  const second = feed();
  const firstDelivered = deferred();
  const secondOpened = deferred();
  const controller = new AbortController();
  const clock = new FakeClock();
  const delivered = [];
  const headers = [];
  let opens = 0;
  const api = new ArtifreeClient({
    baseUrl: 'http://localhost',
    accessToken: 'token',
    clock,
    fetch: async (url, init) => {
      if (url.endsWith('/v1/changes')) {
        opens++;
        headers.push(init.headers);
        if (opens === 2) secondOpened.resolve();
        return new Response(opens === 1 ? first.body : second.body, {
          headers: { 'content-type': 'text/event-stream' },
        });
      }
      return Response.json(snapshot(1));
    },
  });
  const finished = api.followData(artifact, {
    signal: controller.signal,
    random: () => 0,
    onData(event) {
      delivered.push(event);
      if (delivered.length === 1) firstDelivered.resolve();
      else controller.abort();
    },
  });
  first.send('ready', { cursor: cursor(0) });
  await firstDelivered.promise;
  first.fail(new Error('connection lost'));
  await flush();
  assert.equal(clock.nextIn, 250);
  await clock.advance(249);
  assert.equal(opens, 1);
  await clock.advance(1);
  await secondOpened.promise;
  second.send('ready', { cursor: cursor(0) });
  await finished;
  assert.deepEqual(delivered, [
    { type: 'snapshot', data: snapshot(1) },
    { type: 'snapshot', data: snapshot(1) },
  ]);
  assert.equal(headers[0]['last-event-id'], undefined);
  assert.equal(headers[1]['last-event-id'], undefined);
  assert.equal(clock.timers.size, 0);
});

test('an onData callback that aborts then throws resolves', async () => {
  const source = feed();
  const controller = new AbortController();
  const api = new ArtifreeClient({
    baseUrl: 'http://localhost',
    accessToken: 'token',
    fetch: async (url) =>
      url.endsWith('/v1/changes')
        ? new Response(source.body, {
            headers: { 'content-type': 'text/event-stream' },
          })
        : Response.json(snapshot(1)),
  });
  const finished = api.followData(artifact, {
    signal: controller.signal,
    onData() {
      controller.abort();
      throw new Error('abort wins');
    },
  });
  source.send('ready', { cursor: cursor(0) });
  await finished;
});

for (const status of [429, 500]) {
  test(`a ${status} data response reconnects with the required delay`, async () => {
    const first = feed();
    const second = feed();
    const opened = deferred();
    const controller = new AbortController();
    const clock = new FakeClock();
    const delivered = [];
    let opens = 0;
    let reads = 0;
    const api = new ArtifreeClient({
      baseUrl: 'http://localhost',
      accessToken: 'token',
      clock,
      fetch: async (url) => {
        if (url.endsWith('/v1/changes')) {
          opens++;
          if (opens === 2) opened.resolve();
          return new Response(opens === 1 ? first.body : second.body, {
            headers: { 'content-type': 'text/event-stream' },
          });
        }
        reads++;
        if (reads === 1)
          return new Response('unavailable', {
            status,
            headers: status === 429 ? { 'retry-after': '120' } : {},
          });
        return Response.json(snapshot(1));
      },
    });
    const finished = api.followData(artifact, {
      signal: controller.signal,
      random: () => 0,
      onData(event) {
        delivered.push(event);
        controller.abort();
      },
    });
    first.send('ready', { cursor: cursor(0) });
    await flush();
    await flush();
    const delay = status === 429 ? 120_000 : 250;
    assert.equal(clock.nextIn, delay);
    await clock.advance(delay - 1);
    assert.equal(opens, 1);
    await clock.advance(1);
    await opened.promise;
    second.send('ready', { cursor: cursor(0) });
    await finished;
    assert.deepEqual(delivered, [{ type: 'snapshot', data: snapshot(1) }]);
    assert.equal(reads, 2);
    assert.equal(clock.timers.size, 0);
  });
}

test('a callback failure is not confused with a retryable transport failure', async () => {
  const source = feed();
  const controller = new AbortController();
  const reason = new ArtifreeApiError('callback failed', {
    status: 503,
    code: 'network_error',
  });
  let opens = 0;
  const api = new ArtifreeClient({
    baseUrl: 'http://localhost',
    accessToken: 'token',
    fetch: async (url) => {
      if (url.endsWith('/v1/changes')) {
        opens++;
        return new Response(source.body, {
          headers: { 'content-type': 'text/event-stream' },
        });
      }
      return Response.json(snapshot(1));
    },
  });
  const finished = api.followData(artifact, {
    signal: controller.signal,
    onData() {
      throw reason;
    },
  });
  source.send('ready', { cursor: cursor(0) });
  await assert.rejects(finished, (error) => error === reason);
  assert.equal(opens, 1);
});

test('a pending promise returned by the callback does not delay updates', async () => {
  const source = feed();
  const controller = new AbortController();
  const delivered = [];
  let reads = 0;
  const api = new ArtifreeClient({
    baseUrl: 'http://localhost',
    accessToken: 'token',
    fetch: async (url) =>
      url.endsWith('/v1/changes')
        ? new Response(source.body, {
            headers: { 'content-type': 'text/event-stream' },
          })
        : Response.json(snapshot(++reads)),
  });
  const finished = api.followData(artifact, {
    signal: controller.signal,
    onData(event) {
      delivered.push(event);
      if (delivered.length === 1) source.send('change', changed(2));
      else controller.abort();
      return new Promise(() => {});
    },
  });
  source.send('ready', { cursor: cursor(0) });
  await finished;
  assert.deepEqual(delivered, [
    { type: 'snapshot', data: snapshot(1) },
    { type: 'update', data: snapshot(2) },
  ]);
});

test('simultaneous stream and read failures produce one reconnect', async () => {
  const first = feed();
  const second = feed();
  const firstRead = deferred();
  const started = deferred();
  const secondOpened = deferred();
  const controller = new AbortController();
  const clock = new FakeClock();
  let opens = 0;
  let reads = 0;
  const api = new ArtifreeClient({
    baseUrl: 'http://localhost',
    accessToken: 'token',
    clock,
    fetch: async (url) => {
      if (url.endsWith('/v1/changes')) {
        opens++;
        if (opens === 2) secondOpened.resolve();
        return new Response(opens === 1 ? first.body : second.body, {
          headers: { 'content-type': 'text/event-stream' },
        });
      }
      reads++;
      if (reads === 1) {
        started.resolve();
        return firstRead.promise;
      }
      return Response.json(snapshot(2));
    },
  });
  const finished = api.followData(artifact, {
    signal: controller.signal,
    random: () => 0,
    onData() {
      controller.abort();
    },
  });
  first.send('ready', { cursor: cursor(0) });
  await started.promise;
  first.fail(new Error('stream lost'));
  firstRead.reject(new Error('read lost'));
  await flush();
  await flush();
  assert.equal(clock.nextIn, 250);
  await clock.advance(250);
  await secondOpened.promise;
  second.send('ready', { cursor: cursor(0) });
  await finished;
  assert.equal(opens, 2);
  assert.equal(reads, 2);
  assert.equal(clock.timers.size, 0);
});

test('a stalled data body reconnects even when stream comments keep arriving', async () => {
  const first = feed();
  const second = feed();
  const firstRead = deferred();
  const secondOpened = deferred();
  const controller = new AbortController();
  const clock = new FakeClock();
  let opens = 0;
  let reads = 0;
  let cancelled = 0;
  const api = new ArtifreeClient({
    baseUrl: 'http://localhost',
    accessToken: 'token',
    clock,
    fetch: async (url) => {
      if (url.endsWith('/v1/changes')) {
        opens++;
        if (opens === 2) secondOpened.resolve();
        return new Response(opens === 1 ? first.body : second.body, {
          headers: { 'content-type': 'text/event-stream' },
        });
      }
      reads++;
      if (reads === 1) {
        firstRead.resolve();
        return new Response(
          new ReadableStream({
            cancel() {
              cancelled++;
            },
          }),
        );
      }
      return Response.json(snapshot(2));
    },
  });
  const finished = api.followData(artifact, {
    signal: controller.signal,
    random: () => 0,
    onData() {
      controller.abort();
    },
  });
  first.send('ready', { cursor: cursor(0) });
  await firstRead.promise;
  await flush();
  await clock.advance(15_000);
  first.comment();
  await flush();
  await clock.advance(15_000);
  first.comment();
  await flush();
  await clock.advance(15_000);
  await flush();
  assert.equal(cancelled, 1);
  assert.equal(clock.nextIn, 250);
  await clock.advance(250);
  await secondOpened.promise;
  second.send('ready', { cursor: cursor(0) });
  await finished;
  assert.equal(opens, 2);
  assert.equal(clock.timers.size, 0);
});

test('an oversized data body rejects rather than reconnecting', async () => {
  const source = feed();
  const controller = new AbortController();
  let opens = 0;
  const api = new ArtifreeClient({
    baseUrl: 'http://localhost',
    accessToken: 'token',
    fetch: async (url) => {
      if (url.endsWith('/v1/changes')) {
        opens++;
        return new Response(source.body, {
          headers: { 'content-type': 'text/event-stream' },
        });
      }
      return new Response('x'.repeat(1_064_961));
    },
  });
  const finished = api.followData(artifact, {
    signal: controller.signal,
    onData() {
      assert.fail('unexpected delivery');
    },
  });
  source.send('ready', { cursor: cursor(0) });
  await assert.rejects(finished, { code: 'invalid_response' });
  assert.equal(opens, 1);
});

test('a connection resets the retry count only after its first read stays open thirty seconds', async () => {
  const sources = [feed(), feed(), feed()];
  const firstDelivered = deferred();
  const secondDelivered = deferred();
  const secondOpened = deferred();
  const thirdOpened = deferred();
  const controller = new AbortController();
  const clock = new FakeClock();
  let opens = 0;
  let deliveries = 0;
  const api = new ArtifreeClient({
    baseUrl: 'http://localhost',
    accessToken: 'token',
    clock,
    fetch: async (url) => {
      if (!url.endsWith('/v1/changes')) return Response.json(snapshot(1));
      const source = sources[opens++];
      if (opens === 2) secondOpened.resolve();
      if (opens === 3) thirdOpened.resolve();
      return new Response(source.body, {
        headers: { 'content-type': 'text/event-stream' },
      });
    },
  });
  const finished = api.followData(artifact, {
    signal: controller.signal,
    random: () => 0,
    onData() {
      deliveries++;
      if (deliveries === 1) firstDelivered.resolve();
      else if (deliveries === 2) secondDelivered.resolve();
      else controller.abort();
    },
  });
  sources[0].send('ready', { cursor: cursor(0) });
  await firstDelivered.promise;
  sources[0].fail(new Error('first dropped'));
  await flush();
  assert.equal(clock.nextIn, 250);
  await clock.advance(250);
  await secondOpened.promise;
  sources[1].send('ready', { cursor: cursor(0) });
  await secondDelivered.promise;
  await clock.advance(30_000);
  sources[1].fail(new Error('second dropped'));
  await flush();
  assert.equal(clock.nextIn, 250);
  await clock.advance(250);
  await thirdOpened.promise;
  sources[2].send('ready', { cursor: cursor(0) });
  await finished;
  assert.equal(opens, 3);
  assert.equal(deliveries, 3);
  assert.equal(clock.timers.size, 0);
});

test('changes during a read coalesce and cause a further read when needed', async () => {
  const source = feed();
  const second = deferred();
  const secondStarted = deferred();
  const controller = new AbortController();
  const delivered = [];
  let reads = 0;
  const api = new ArtifreeClient({
    baseUrl: 'http://localhost',
    accessToken: 'token',
    fetch: async (url) => {
      if (url.endsWith('/v1/changes'))
        return new Response(source.body, {
          headers: { 'content-type': 'text/event-stream' },
        });
      reads++;
      if (reads === 2) {
        secondStarted.resolve();
        return second.promise;
      }
      return Response.json(snapshot(reads === 1 ? 1 : 4));
    },
  });
  const firstDelivered = deferred();
  const finished = api.followData(artifact, {
    signal: controller.signal,
    onData(event) {
      delivered.push(event);
      if (delivered.length === 1) firstDelivered.resolve();
      if (delivered.length === 3) controller.abort();
    },
  });
  source.send('ready', { cursor: cursor(0) });
  await firstDelivered.promise;
  source.send('change', changed(2));
  await secondStarted.promise;
  source.send('change', changed(3));
  source.send('change', changed(4));
  await flush();
  second.resolve(Response.json(snapshot(2)));
  await finished;
  assert.deepEqual(delivered, [
    { type: 'snapshot', data: snapshot(1) },
    { type: 'update', data: snapshot(2) },
    { type: 'update', data: snapshot(4) },
  ]);
  assert.equal(reads, 3);
});

test('a stream-open 404 rejects instead of delivering unavailable', async () => {
  const controller = new AbortController();
  const events = [];
  const api = new ArtifreeClient({
    baseUrl: 'http://localhost',
    accessToken: 'token',
    fetch: async () => new Response('missing', { status: 404 }),
  });
  await assert.rejects(
    api.followData(artifact, {
      signal: controller.signal,
      onData(event) {
        events.push(event);
      },
    }),
    { status: 404 },
  );
  assert.deepEqual(events, []);
});

test('a wrong echoed artifact ID rejects without retry', async () => {
  const source = feed();
  const controller = new AbortController();
  let opens = 0;
  const api = new ArtifreeClient({
    baseUrl: 'http://localhost',
    accessToken: 'token',
    fetch: async (url) => {
      if (url.endsWith('/v1/changes')) {
        opens++;
        return new Response(source.body, {
          headers: { 'content-type': 'text/event-stream' },
        });
      }
      return Response.json({ ...snapshot(1), artifact_id: 'other' });
    },
  });
  const finished = api.followData(artifact, {
    signal: controller.signal,
    onData() {
      assert.fail('unexpected delivery');
    },
  });
  source.send('ready', { cursor: cursor(0) });
  await assert.rejects(finished, { code: 'invalid_response' });
  assert.equal(opens, 1);
});

test('a follower accepts response shape without imposing server data semantics', async () => {
  const source = feed();
  const controller = new AbortController();
  const answer = {
    artifact_id: artifact,
    path: 'different',
    data_version: 0,
    exists: false,
    value: 17,
  };
  const delivered = [];
  const api = new ArtifreeClient({
    baseUrl: 'http://localhost',
    accessToken: 'token',
    fetch: async (url) =>
      url.endsWith('/v1/changes')
        ? new Response(source.body, {
            headers: { 'content-type': 'text/event-stream' },
          })
        : Response.json(answer),
  });
  const finished = api.followData(artifact, {
    signal: controller.signal,
    onData(event) {
      delivered.push(event);
      controller.abort();
    },
  });
  source.send('ready', { cursor: cursor(0) });
  await finished;
  assert.deepEqual(delivered, [{ type: 'snapshot', data: answer }]);
});

for (const dataVersion of [1.5, Number.MAX_SAFE_INTEGER + 1]) {
  test(`a non-safe integer version ${dataVersion} rejects without retry`, async () => {
    const source = feed();
    const controller = new AbortController();
    let opens = 0;
    const api = new ArtifreeClient({
      baseUrl: 'http://localhost',
      accessToken: 'token',
      fetch: async (url) => {
        if (url.endsWith('/v1/changes')) {
          opens++;
          return new Response(source.body, {
            headers: { 'content-type': 'text/event-stream' },
          });
        }
        return Response.json(snapshot(dataVersion));
      },
    });
    const finished = api.followData(artifact, {
      signal: controller.signal,
      onData() {
        assert.fail('unexpected delivery');
      },
    });
    source.send('ready', { cursor: cursor(0) });
    await assert.rejects(finished, { code: 'invalid_response' });
    assert.equal(opens, 1);
  });
}

test('unrelated artifact and non-data changes do not trigger reads', async () => {
  const source = feed();
  const firstDelivered = deferred();
  const controller = new AbortController();
  let reads = 0;
  const delivered = [];
  const api = new ArtifreeClient({
    baseUrl: 'http://localhost',
    accessToken: 'token',
    fetch: async (url) =>
      url.endsWith('/v1/changes')
        ? new Response(source.body, {
            headers: { 'content-type': 'text/event-stream' },
          })
        : Response.json(snapshot(++reads)),
  });
  const finished = api.followData(artifact, {
    signal: controller.signal,
    onData(event) {
      delivered.push(event);
      if (delivered.length === 1) firstDelivered.resolve();
      else controller.abort();
    },
  });
  source.send('ready', { cursor: cursor(0) });
  await firstDelivered.promise;
  source.send('change', {
    ...changed(2),
    artifact_id: `art_${'1'.repeat(32)}`,
  });
  source.send('change', {
    ...changed(2),
    change: 'revised',
    data_version: undefined,
  });
  await flush();
  assert.equal(reads, 1);
  source.send('change', changed(2));
  await finished;
  assert.equal(reads, 2);
  assert.deepEqual(
    delivered.map((event) => event.type),
    ['snapshot', 'update'],
  );
});

test('reset while following abandons the update and emits a new snapshot', async () => {
  const source = feed();
  const firstDelivered = deferred();
  const secondStarted = deferred();
  const second = deferred();
  const controller = new AbortController();
  const delivered = [];
  let reads = 0;
  const api = new ArtifreeClient({
    baseUrl: 'http://localhost',
    accessToken: 'token',
    fetch: async (url) => {
      if (url.endsWith('/v1/changes'))
        return new Response(source.body, {
          headers: { 'content-type': 'text/event-stream' },
        });
      reads++;
      if (reads === 2) {
        secondStarted.resolve();
        return second.promise;
      }
      return Response.json(snapshot(1));
    },
  });
  const finished = api.followData(artifact, {
    signal: controller.signal,
    onData(event) {
      delivered.push(event);
      if (delivered.length === 1) firstDelivered.resolve();
      else controller.abort();
    },
  });
  source.send('ready', { cursor: cursor(0) });
  await firstDelivered.promise;
  source.send('change', changed(2));
  await secondStarted.promise;
  source.send('reset', { cursor: cursor(3) });
  await finished;
  second.resolve(Response.json(snapshot(2)));
  await flush();
  assert.deepEqual(delivered, [
    { type: 'snapshot', data: snapshot(1) },
    { type: 'snapshot', data: snapshot(1) },
  ]);
  assert.equal(reads, 3);
});

test('a data 401 rejects without retry or unavailable delivery', async () => {
  const source = feed();
  const controller = new AbortController();
  const delivered = [];
  let opens = 0;
  const api = new ArtifreeClient({
    baseUrl: 'http://localhost',
    accessToken: 'token',
    fetch: async (url) => {
      if (url.endsWith('/v1/changes')) {
        opens++;
        return new Response(source.body, {
          headers: { 'content-type': 'text/event-stream' },
        });
      }
      return new Response('unauthorized', { status: 401 });
    },
  });
  const finished = api.followData(artifact, {
    signal: controller.signal,
    onData(event) {
      delivered.push(event);
    },
  });
  source.send('ready', { cursor: cursor(0) });
  await assert.rejects(finished, { status: 401 });
  assert.equal(opens, 1);
  assert.deepEqual(delivered, []);
});

test('many events do not add handlers to a held read', async () => {
  const controller = new AbortController();
  let subscriptions = 0;
  let captureHeld = true;
  let heldId;
  const descendants = new Set();
  const hook = createHook({
    init(id, type, trigger) {
      if (type !== 'PROMISE') return;
      if (captureHeld) {
        heldId = id;
        captureHeld = false;
      } else if (trigger === heldId || descendants.has(trigger)) {
        subscriptions++;
        descendants.add(id);
      }
    },
  });
  hook.enable();
  const held = new Promise(() => {});
  async function* events() {
    yield { type: 'ready', cursor: cursor(0) };
    for (let version = 1; version <= 200; version++)
      yield { type: 'change', change: changed(version) };
    controller.abort();
  }
  try {
    await followData(
      {
        clock: new FakeClock(),
        open: async () => events(),
        read: () => held,
      },
      artifact,
      {
        signal: controller.signal,
        onData() {
          assert.fail('unexpected delivery');
        },
      },
    );
    assert.equal(subscriptions, 1);
  } finally {
    hook.disable();
  }
});

test('successful reads on short connections do not reset increasing retry delays', async () => {
  const sources = [feed(), feed(), feed(), feed()];
  const opened = sources.map(() => deferred());
  const delivered = sources.map(() => deferred());
  const controller = new AbortController();
  const clock = new FakeClock();
  let opens = 0;
  let deliveries = 0;
  const api = new ArtifreeClient({
    baseUrl: 'http://localhost',
    accessToken: 'token',
    clock,
    fetch: async (url) => {
      if (!url.endsWith('/v1/changes')) return Response.json(snapshot(1));
      const index = opens++;
      opened[index].resolve();
      return new Response(sources[index].body, {
        headers: { 'content-type': 'text/event-stream' },
      });
    },
  });
  const finished = api.followData(artifact, {
    signal: controller.signal,
    random: () => 0,
    onData() {
      delivered[deliveries++].resolve();
    },
  });
  for (let index = 0; index < 3; index++) {
    await opened[index].promise;
    sources[index].send('ready', { cursor: cursor(0) });
    await delivered[index].promise;
    await clock.advance(1_000);
    sources[index].fail(new Error('short connection'));
    await flush();
    const delay = [250, 500, 1_000][index];
    assert.equal(clock.nextIn, delay);
    await clock.advance(delay);
  }
  await opened[3].promise;
  assert.equal(opens, 4);
  controller.abort();
  await finished;
  assert.equal(clock.timers.size, 0);
});

test('a connection dropped at 29,999 ms does not reset the retry count', async () => {
  const sources = [feed(), feed()];
  const opened = [deferred(), deferred()];
  const delivered = [deferred(), deferred()];
  const controller = new AbortController();
  const clock = new FakeClock();
  let opens = 0;
  let deliveries = 0;
  const api = new ArtifreeClient({
    baseUrl: 'http://localhost',
    accessToken: 'token',
    clock,
    fetch: async (url) => {
      if (!url.endsWith('/v1/changes')) return Response.json(snapshot(1));
      const index = opens++;
      opened[index].resolve();
      return new Response(sources[index].body, {
        headers: { 'content-type': 'text/event-stream' },
      });
    },
  });
  const finished = api.followData(artifact, {
    signal: controller.signal,
    random: () => 0,
    onData() {
      delivered[deliveries++].resolve();
    },
  });
  await opened[0].promise;
  sources[0].send('ready', { cursor: cursor(0) });
  await delivered[0].promise;
  sources[0].fail(new Error('first drop'));
  await flush();
  await clock.advance(250);
  await opened[1].promise;
  sources[1].send('ready', { cursor: cursor(0) });
  await delivered[1].promise;
  await clock.advance(29_999);
  sources[1].fail(new Error('second drop'));
  await flush();
  assert.equal(clock.nextIn, 500);
  controller.abort();
  await finished;
  assert.equal(clock.timers.size, 0);
});
