import assert from 'node:assert/strict';
import { test } from 'node:test';
import * as vm from 'node:vm';
import { recordRuntimeViews } from '../lib/runtime-view-recorder.mjs';

const path = '/v1/artifacts/artifact/revisions/1/runtime-views';
const view = {
  url: 'http://localhost:9001/p/ticket/index.html',
  generation: 2,
};

/** Runs the installed browser script with only fetch replaced at its boundary. */
async function recorder(send) {
  const context = vm.createContext({
    fetch: send,
    Request,
    URL,
    location: { href: 'http://localhost:9000/' },
  });
  vm.runInContext(
    'globalThis.window = globalThis; window.top = window;',
    context,
  );
  const read = await recordRuntimeViews({
    addInitScript: async (script) => {
      vm.runInContext(`(${script.toString()})()`, context);
    },
    evaluate: async (script) =>
      structuredClone(vm.runInContext(`(${script.toString()})()`, context)),
  });
  return { read, fetch: context.fetch };
}

test('records the exact response before delivering it without consuming the application body', async () => {
  let release;
  const body = new ReadableStream({
    start(controller) {
      release = () => {
        controller.enqueue(new TextEncoder().encode(JSON.stringify(view)));
        controller.close();
      };
    },
  });
  const original = new Response(body, { status: 201 });
  const recorded = await recorder(async () => original);
  let delivered = false;
  const pending = recorded.fetch(path, { method: 'POST' }).then((response) => {
    delivered = true;
    return response;
  });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(delivered, false, 'The application must not outrun its record.');
  assert.deepEqual(await recorded.read(), []);
  release();
  const response = await pending;
  assert.equal(response, original);
  assert.deepEqual(await recorded.read(), [view]);
  assert.deepEqual(await response.json(), view);
});

test('keeps a failed body read observable with request and status while returning the application response', async () => {
  const original = new Response(
    new ReadableStream({
      start(controller) {
        controller.error(new Error('body cut off'));
      },
    }),
    { status: 201 },
  );
  const recorded = await recorder(async () => original);
  assert.equal(await recorded.fetch(path, { method: 'POST' }), original);
  await assert.rejects(recorded.read(), {
    message:
      /Runtime view recording failed:.*runtime-views.*"status":201.*body cut off/,
  });
  await assert.rejects(original.json(), { message: 'body cut off' });
});

test('reports invalid JSON instead of silently dropping a successful creation', async () => {
  const recorded = await recorder(
    async () => new Response('invalid JSON', { status: 201 }),
  );
  await recorded.fetch(path, { method: 'POST' });
  await assert.rejects(recorded.read(), {
    message:
      /Runtime view recording failed:.*runtime-views.*"status":201.*JSON/,
  });
});

test('records a Request input but ignores other methods, routes, and refused creations', async () => {
  const recorded = await recorder(async (_input, init) =>
    Response.json(view, { status: init?.refused ? 403 : 201 }),
  );
  await recorded.fetch(path);
  await recorded.fetch('/v1/runtime-sessions', { method: 'POST' });
  await recorded.fetch(path, { method: 'POST', refused: true });
  assert.deepEqual(await recorded.read(), []);
  await recorded.fetch(
    new Request(`http://localhost:9000${path}`, { method: 'POST' }),
  );
  assert.deepEqual(await recorded.read(), [view]);
});
