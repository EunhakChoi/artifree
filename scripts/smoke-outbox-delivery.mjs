import assert from 'node:assert/strict';
import { createServer, request as httpRequest } from 'node:http';
import { once } from 'node:events';
import { build } from 'esbuild';
import { cleanupOutboxDelivery } from './lib/outbox-delivery-cleanup.mjs';
import { checkOutboxScheduler } from './smoke-outbox-scheduler.mjs';

const ARTIFACT = `art_${'a'.repeat(32)}`;
const EPOCH = 'b'.repeat(32);

/** Native Fetch/IDB one-attempt kernel. Public browser delivery stays disabled. */
export async function checkOutboxDelivery(browser, serverApi) {
  // Owned response/IDB fixtures do not consume real-server authentication or
  // artifact budgets. Each real Rust slice below uses its own actor as well.
  let live;
  const isolatedUsers = [];
  const bundle = await build({
    stdin: {
      contents: `import {DeliveryKernel} from './packages/client/src/outbox/delivery.ts';
      import {openStore} from './packages/client/src/outbox/store.ts';
      import {readBinding} from './packages/client/src/outbox/binding.ts';
      import {OutboxError} from './packages/client/src/outbox/types.ts';
      import {ArtifreeApiError} from './packages/client/src/errors.ts';
      import {DeliveryScheduler} from './packages/client/src/outbox/scheduler.ts';
      globalThis.deliveryApi = {DeliveryKernel, DeliveryScheduler, openStore, readBinding, OutboxError, ArtifreeApiError};`,
      resolveDir: process.cwd(),
      loader: 'ts',
    },
    bundle: true,
    format: 'esm',
    platform: 'browser',
    write: false,
  });
  let mode = 'normal';
  let status = 409;
  let errorBody = {
    error: {
      code: 'data_conflict',
      message: 'private-response-marker',
      current_data_version: 5,
    },
  };
  let redirectStatus = 307;
  let redirectTarget = '/collect';
  let requests = [];
  let collected = [];
  const receipts = new Map();
  const createdArtifacts = [];
  let held;
  let arrived;
  let onArrived;
  const responses = new Set();
  const target = createServer(async (request, response) => {
    const body = await bodyText(request);
    collected.push({ method: request.method, body, headers: request.headers });
    response.setHeader('Access-Control-Allow-Origin', '*');
    response.setHeader(
      'Access-Control-Allow-Headers',
      'authorization,content-type,idempotency-key,artifree-outbox-actor,artifree-outbox-epoch,artifree-outbox-actor-generation',
    );
    response.setHeader(
      'Access-Control-Allow-Methods',
      'PUT,PATCH,DELETE,OPTIONS',
    );
    response.end('{}');
  });
  target.listen(0, '127.0.0.1');
  await once(target, 'listening');
  const crossOrigin = `http://127.0.0.1:${target.address().port}/collect`;
  const server = createServer(async (request, response) => {
    response.on('close', () => responses.delete(response));
    responses.add(response);
    if (request.url === '/module.js') {
      response.setHeader('Content-Type', 'text/javascript');
      response.end(bundle.outputFiles[0].contents);
      return;
    }
    if (request.url === '/collect') {
      collected.push({
        method: request.method,
        body: await bodyText(request),
        headers: request.headers,
      });
      response.end('{}');
      return;
    }
    if (request.url === '/v1/data-outbox/binding') {
      if (live !== undefined) {
        forward(request, response, '');
        return;
      }
      response.setHeader('Content-Type', 'application/json');
      response.end(
        JSON.stringify({
          actor_id: 'local',
          actor_generation: '1',
          storage_epoch: EPOCH,
        }),
      );
      return;
    }
    if (!request.url?.endsWith('/data') || request.method === 'GET') {
      response.setHeader('Content-Type', 'text/html');
      response.end(
        '<!doctype html><script type="module" src="/module.js"></script>',
      );
      return;
    }
    const body = await bodyText(request);
    requests.push({
      method: request.method,
      url: request.url,
      body,
      headers: request.headers,
    });
    onArrived?.();
    onArrived = undefined;
    if (mode === 'hold_forward') {
      held = () => forward(request, response, body);
      return;
    }
    if (mode === 'headers_hold') return;
    if (mode === 'binding_changed') {
      response.writeHead(409, { 'Content-Type': 'application/json' });
      response.end(
        JSON.stringify({
          error: {
            code: 'outbox_binding_changed',
            message: 'fixed binding refusal',
          },
        }),
      );
      return;
    }
    if (mode === 'headers_hold_status') {
      held = () => {
        response.writeHead(429, { 'Retry-After': '999999' });
        response.end('{}');
      };
      return;
    }
    if (mode === 'redirect') {
      response.writeHead(redirectStatus, { Location: redirectTarget });
      response.end();
      return;
    }
    if (mode === 'status' || mode === 'error_body_hold') {
      response.writeHead(status, {
        'Content-Type': 'application/json',
        'Retry-After': '999999',
      });
      if (mode === 'error_body_hold') response.write('{"error":');
      else
        response.end(
          typeof errorBody === 'string' ? errorBody : JSON.stringify(errorBody),
        );
      return;
    }
    if (mode === 'body_hold') {
      const input = JSON.parse(body);
      const receipt = JSON.stringify({
        artifact_id: request.url.split('/')[3],
        path: input.path,
        data_version: input.base_data_version + 1,
        changed: true,
        replayed: false,
      });
      response.writeHead(200, { 'Content-Type': 'application/json' });
      response.write(receipt.slice(0, 1));
      held = () => response.end(receipt.slice(1));
      return;
    }
    if (
      live !== undefined &&
      (mode === 'normal' ||
        mode === 'lost_response' ||
        mode === 'commit_redirect' ||
        mode === 'invalid_committed_receipt')
    ) {
      forward(request, response, body);
      return;
    }
    const input = JSON.parse(body);
    const artifact = request.url.split('/')[3];
    let receipt = {
      artifact_id: artifact,
      path: input.path,
      data_version: input.base_data_version + 1,
      changed: true,
      replayed: false,
    };
    const previous = receipts.get(request.headers['idempotency-key']);
    if (previous) receipt = { ...previous, replayed: true };
    else receipts.set(request.headers['idempotency-key'], receipt);
    if (mode === 'lost_response') {
      response.destroy();
      return;
    }
    if (mode === 'mismatched') receipt.data_version += 1;
    if (mode === 'wrong_path') receipt.path = 'wrong';
    if (mode === 'extra_success') receipt.extra = 'private-response-marker';
    if (mode === 'wrong_artifact')
      receipt.artifact_id = `art_${'b'.repeat(32)}`;
    response.statusCode = mode === 'unexpected_2xx' ? 201 : 200;
    response.setHeader('Content-Type', 'application/json');
    if (mode === 'invalid_json') {
      response.end('{');
      return;
    }
    if (mode === 'invalid_utf8') {
      const text = JSON.stringify({ ...receipt, path: '�' });
      const index = text.indexOf('�');
      response.end(
        Buffer.concat([
          Buffer.from(text.slice(0, index)),
          Buffer.from([255]),
          Buffer.from(text.slice(index + 1)),
        ]),
      );
      return;
    }
    response.end(
      JSON.stringify(receipt) +
        (mode === 'huge_success' ? ' '.repeat(1_064_961) : ''),
    );
  });
  function forward(request, response, body) {
    const upstream = httpRequest(
      new URL(request.url, live.origin),
      {
        method: request.method,
        headers: { ...request.headers, host: new URL(live.origin).host },
      },
      (reply) => {
        const chunks = [];
        reply.on('data', (chunk) => chunks.push(chunk));
        reply.on('end', () => {
          if (mode === 'lost_response') {
            response.destroy();
            return;
          }
          if (mode === 'commit_redirect') {
            response.writeHead(307, { Location: crossOrigin });
            response.end();
            return;
          }
          if (mode === 'invalid_committed_receipt') {
            const receipt = JSON.parse(Buffer.concat(chunks).toString('utf8'));
            response.writeHead(200, { 'Content-Type': 'application/json' });
            response.end(
              JSON.stringify({ ...receipt, path: 'private-mismatched-path' }),
            );
            return;
          }
          response.writeHead(reply.statusCode, reply.headers);
          response.end(Buffer.concat(chunks));
        });
      },
    );
    upstream.on('error', () => response.destroy());
    upstream.end(body);
  }
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const origin = `http://127.0.0.1:${server.address().port}`;
  const contexts = [];
  async function setup({
    path = 'marker',
    operation = 'set',
    text = '{"original":1}',
    artifact = ARTIFACT,
    base = 0,
    immediateClock = false,
  } = {}) {
    const context = await browser.newContext();
    contexts.push(context);
    const page = await context.newPage();
    await page.goto(origin);
    await page.waitForFunction(() => globalThis.deliveryApi !== undefined);
    await page.evaluate(
      async ({
        path,
        operation,
        text,
        artifact,
        base,
        token,
        key,
        immediateClock,
      }) => {
        const {
          openStore,
          readBinding,
          DeliveryKernel,
          OutboxError,
          ArtifreeApiError,
        } = deliveryApi;
        globalThis.owner = 'a'.repeat(32);
        globalThis.auth = await readBinding(location.origin, token);
        globalThis.store = await openStore();
        globalThis.lane = [
          owner,
          location.origin,
          auth.actor_id,
          auth.storage_epoch,
          artifact,
        ];
        globalThis.stop = new AbortController();
        globalThis.authority = Symbol();
        globalThis.currentAuthority = authority;
        globalThis.lockHeld = false;
        sessionStorage.setItem('artifree-data-outbox-owner-v1', owner);
        let acquired;
        const ready = new Promise((resolve) => {
          acquired = resolve;
        });
        globalThis.lockLife = navigator.locks.request(
          'artifree-data-outbox:' + owner,
          async () => {
            lockHeld = true;
            acquired();
            await new Promise((resolve) => {
              globalThis.releaseLock = resolve;
            });
            lockHeld = false;
          },
        );
        await ready;
        globalThis.timers = [];
        const clock = {
          after(callback, milliseconds) {
            if (immediateClock) {
              callback();
              return () => undefined;
            }
            const timer = { callback, milliseconds, active: true };
            timers.push(timer);
            return () => {
              timer.active = false;
            };
          },
        };
        globalThis.authorityException = undefined;
        globalThis.authenticationInvalidationException = undefined;
        globalThis.makeAuthorityException = () =>
          new Error('private-authority-exception-message', {
            cause: new Error('private-authority-exception-cause'),
          });
        globalThis.authorize = () => {
          if (authorityException !== undefined) throw authorityException;
          if (
            !lockHeld ||
            authority !== currentAuthority ||
            sessionStorage.getItem('artifree-data-outbox-owner-v1') !== owner
          )
            throw new OutboxError('owner_not_ready');
        };
        globalThis.locallyAuthenticated = true;
        globalThis.authInvalidationAborts = true;
        const invalidateAuthentication = () => {
          if (authenticationInvalidationException !== undefined)
            throw authenticationInvalidationException;
          locallyAuthenticated = false;
          currentAuthority = Symbol();
          if (authInvalidationAborts) stop.abort();
        };
        globalThis.deliveryContext = {
          base: location.origin,
          owner,
          accessToken: token,
          binding: auth,
          signal: stop.signal,
          authorize,
          invalidateAuthentication,
          clock,
        };
        globalThis.kernel = new DeliveryKernel(store, deliveryContext);
        globalThis.installScheduler = () => {
          globalThis.events = [];
          globalThis.revalidationCalls = 0;
          globalThis.failRevalidation = false;
          globalThis.scheduler = new deliveryApi.DeliveryScheduler(store, {
            context: deliveryContext,
            random: () => 0,
            observe: (event) => {
              events.push(event);
              globalThis.onSchedulerEvent?.(event);
            },
            revalidate: async (lane, signal) => {
              revalidationCalls += 1;
              if (failRevalidation)
                throw new ArtifreeApiError('network_error', {
                  code: 'network_error',
                  status: 0,
                });
              const current = await readBinding(location.origin, token, signal);
              if (JSON.stringify(current) !== JSON.stringify(auth))
                throw new OutboxError('owner_not_ready');
            },
          });
        };
        globalThis.head = await store.enqueue({
          format: 2,
          state: 'unprepared',
          version: 1,
          key,
          owner,
          binding: lane.slice(0, 4),
          lane,
          artifactId: artifact,
          admittedGeneration: auth.actor_generation,
          authorizedGeneration: auth.actor_generation,
          operation,
          path,
          text: operation === 'delete' ? null : text,
          base: { version: base },
        });
        globalThis.enqueueLikeHead = (changes) => {
          const intent = { ...head, ...changes };
          delete intent.sequence;
          delete intent.bytes;
          return store.enqueue(intent);
        };
        globalThis.start = () => {
          globalThis.pending = kernel.run(lane).then(
            (value) => ({ value }),
            (error) => ({
              error: error.code ?? error.name,
              dispatched: error.dispatched,
            }),
          );
        };
        globalThis.captureAttempt = async () => {
          try {
            return { value: await kernel.run(lane) };
          } catch (error) {
            return {
              name: error.name,
              code: error.code ?? null,
              message: error.message,
              dispatched: error.dispatched ?? null,
              hasCause: Object.hasOwn(error, 'cause'),
              diagnostics: [
                String(error),
                error.stack,
                JSON.stringify(error, Object.getOwnPropertyNames(error)),
              ].join('\n'),
            };
          }
        };
        globalThis.fire = (milliseconds) => {
          const timer = timers.find(
            (timer) => timer.active && timer.milliseconds === milliseconds,
          );
          if (!timer) throw new Error('missing active deadline');
          timer.active = false;
          timer.callback();
        };
        globalThis.snapshot = () => store.lane(lane);
        globalThis.storageSnapshot = async () => {
          const opening = indexedDB.open('artifree-data-outbox');
          const db = await new Promise((resolve, reject) => {
            opening.onsuccess = () => resolve(opening.result);
            opening.onerror = () => reject(opening.error);
          });
          try {
            return await new Promise((resolve, reject) => {
              const tx = db.transaction(['intents', 'checkpoints', 'usage']);
              const rows = tx.objectStore('intents').getAll();
              const checkpoints = tx.objectStore('checkpoints').getAll();
              const usage = tx.objectStore('usage').get('global');
              tx.oncomplete = () =>
                resolve({
                  rows: rows.result,
                  checkpoints: checkpoints.result,
                  usage: usage.result,
                });
              tx.onabort = () => reject(tx.error);
            });
          } finally {
            db.close();
          }
        };
        globalThis.fenceRequest = (
          table,
          method,
          state,
          at,
          abort,
          commit = false,
        ) => {
          const original = IDBObjectStore.prototype[method];
          const originalTransaction = IDBDatabase.prototype.transaction;
          const selected = new WeakSet();
          const invalidate = () => {
            currentAuthority = Symbol();
            if (abort) stop.abort();
          };
          if (commit)
            IDBDatabase.prototype.transaction = function (...args) {
              const tx = Reflect.apply(originalTransaction, this, args);
              // Register before the store's terminal listener. Browsers may run
              // promise continuations between separate native event listeners.
              tx.addEventListener(
                'complete',
                () => {
                  if (selected.has(tx)) invalidate();
                },
                { once: true },
              );
              return tx;
            };
          let count = 0;
          IDBObjectStore.prototype[method] = function (...args) {
            const request = Reflect.apply(original, this, args);
            if (
              this.name === table &&
              this.transaction.mode === 'readwrite' &&
              (state === undefined || args[0]?.state === state)
            ) {
              count += 1;
              if (count === at) {
                if (commit) selected.add(this.transaction);
                else
                  request.addEventListener('success', invalidate, {
                    once: true,
                  });
              }
            }
            return request;
          };
          globalThis.restoreFence = () => {
            IDBObjectStore.prototype[method] = original;
            IDBDatabase.prototype.transaction = originalTransaction;
          };
        };
        globalThis.faultPut = (table, state) => {
          const original = IDBObjectStore.prototype.put;
          IDBObjectStore.prototype.put = function (value, ...args) {
            const request = Reflect.apply(original, this, [value, ...args]);
            if (
              this.name === table &&
              (state === undefined || value.state === state)
            )
              request.addEventListener(
                'success',
                () => this.transaction.abort(),
                { once: true },
              );
            return request;
          };
          globalThis.restorePut = () => {
            IDBObjectStore.prototype.put = original;
          };
        };
      },
      {
        path,
        operation,
        text,
        artifact,
        base,
        token: live?.token ?? 'test-token',
        immediateClock,
        key:
          live === undefined
            ? '1'.repeat(32)
            : crypto.randomUUID().replaceAll('-', ''),
      },
    );
    return page;
  }
  async function run(page) {
    return page.evaluate(() => kernel.run(lane));
  }
  async function close(page) {
    await page.evaluate(async () => {
      stop.abort();
      await globalThis.pending?.catch(() => undefined);
      store.close();
      releaseLock();
      await lockLife;
    });
    await page.context().close();
  }
  function waiting() {
    arrived = new Promise((resolve) => {
      onArrived = resolve;
    });
    return arrived;
  }
  let scenarioFailure;
  try {
    await checkOutboxScheduler({
      setup,
      close,
      waitRequest: waiting,
      releaseHeld() {
        held();
      },
      setMode(nextMode) {
        mode = nextMode;
      },
      configure(nextMode, nextStatus) {
        mode = nextMode;
        if (nextStatus !== undefined) status = nextStatus;
        requests = [];
        receipts.clear();
      },
      requests: () => requests,
    });
    // Failure to prepare never dispatches. The same accepted row remains available.
    let page = await setup();
    requests = [];
    const original = await page.evaluate(() => snapshot());
    await page.evaluate(() => faultPut('intents', 'prepared'));
    assert.deepEqual(await run(page), {
      kind: 'preparation_storage_failed',
      durablyEnqueued: true,
      prepared: false,
    });
    assert.equal(requests.length, 0);
    assert.deepEqual(await page.evaluate(() => snapshot()), original);
    await page.evaluate(() => restorePut());
    await close(page);

    await checkHeldRequest();

    for (const failureMode of [
      'invalid_json',
      'mismatched',
      'wrong_path',
      'wrong_artifact',
      'extra_success',
      'unexpected_2xx',
      'huge_success',
    ]) {
      page = await setup();
      mode = failureMode;
      requests = [];
      const result = await run(page);
      assert.deepEqual(result, {
        kind: 'delivery_failed',
        failure: {
          reason: 'invalid_response',
          status: failureMode === 'unexpected_2xx' ? 201 : 200,
        },
        outcome: 'unknown',
      });
      assert.equal(
        (await page.evaluate(() => snapshot()))[0].state,
        'prepared',
      );
      assert.equal(requests.length, 1);
      assert.equal(
        await page.evaluate(
          () => timers.filter((timer) => timer.active).length,
        ),
        0,
      );
      await close(page);
    }
    page = await setup({ path: '�' });
    mode = 'invalid_utf8';
    assert.equal((await run(page)).failure.reason, 'invalid_response');
    await close(page);

    const errors = [
      [401, '{', { reason: 'access_unavailable', status: 401 }],
      [403, 'x'.repeat(70_000), { reason: 'access_unavailable', status: 403 }],
      [404, '{', { reason: 'access_unavailable', status: 404 }],
      [
        429,
        '{',
        { reason: 'rate_limited', status: 429, retryAfterSeconds: 3600 },
      ],
      [
        503,
        'x'.repeat(70_000),
        { reason: 'server_error', status: 503, retryAfterSeconds: 3600 },
      ],
      [
        409,
        {
          error: {
            code: 'data_conflict',
            message: 'private-response-marker',
            current_data_version: 5,
          },
        },
        { reason: 'data_conflict', status: 409, currentDataVersion: 5 },
      ],
      [
        409,
        {
          error: {
            code: 'outbox_binding_changed',
            message: 'private-response-marker',
          },
        },
        { reason: 'binding_changed', status: 409 },
      ],
      [409, '{', { reason: 'unclassified_error_response', status: 409 }],
      [
        409,
        {
          error: { code: 'data_conflict', message: 'private-response-marker' },
        },
        { reason: 'unclassified_error_response', status: 409 },
      ],
      [
        400,
        {
          error: {
            code: 'data_conflict',
            message: 'private-response-marker',
            current_data_version: 5,
          },
        },
        { reason: 'unclassified_error_response', status: 400 },
      ],
      [
        422,
        {
          error: {
            code: 'request_key_reused',
            message: 'private-response-marker',
          },
        },
        { reason: 'rejected', status: 422, code: 'request_key_reused' },
      ],
      [
        409,
        {
          error: {
            code: 'data_path_conflict',
            message: 'private-response-marker',
          },
        },
        { reason: 'rejected', status: 409, code: 'data_path_conflict' },
      ],
      [
        422,
        {
          error: {
            code: 'invalid_request',
            message: 'private-response-marker',
          },
        },
        { reason: 'rejected', status: 422, code: 'invalid_request' },
      ],
      [
        413,
        {
          error: {
            code: 'payload_too_large',
            message: 'private-response-marker',
          },
        },
        { reason: 'rejected', status: 413, code: 'payload_too_large' },
      ],
      [
        422,
        {
          error: { code: 'data_too_large', message: 'private-response-marker' },
        },
        { reason: 'rejected', status: 422, code: 'data_too_large' },
      ],
      [
        400,
        {
          error: {
            code: 'invalid_request',
            message: 'private-response-marker',
          },
        },
        { reason: 'rejected', status: 400, code: 'invalid_request' },
      ],
      [
        418,
        {
          error: {
            code: 'invalid_request',
            message: 'private-response-marker',
          },
        },
        { reason: 'unclassified_error_response', status: 418 },
      ],
      [
        409,
        'x'.repeat(70_000),
        { reason: 'unclassified_error_response', status: 409 },
      ],
      [
        409,
        {
          error: {
            code: 'data_conflict',
            message: 'private-response-marker',
            current_data_version: 5,
            extra: true,
          },
        },
        { reason: 'unclassified_error_response', status: 409 },
      ],
      [
        409,
        {
          error: {
            code: 'data_conflict',
            message: 'private-response-marker',
            current_data_version: 5,
          },
          extra: true,
        },
        { reason: 'unclassified_error_response', status: 409 },
      ],
      [
        400,
        {
          error: {
            code: 'invalid_request',
            message: 'private-response-marker',
            conflicting_files_total: 'wrong',
          },
        },
        { reason: 'unclassified_error_response', status: 400 },
      ],
      [304, '{', { reason: 'unclassified_error_response', status: 304 }],
    ];
    for (const [httpStatus, body, expected] of errors) {
      page = await setup();
      mode = 'status';
      status = httpStatus;
      errorBody = body;
      requests = [];
      const result = await run(page);
      assert.deepEqual(result, {
        kind: 'delivery_failed',
        failure: expected,
        outcome: 'unknown',
      });
      assert.equal(
        JSON.stringify(result).includes('private-response-marker'),
        false,
      );
      assert.equal(
        (await page.evaluate(() => snapshot()))[0].state,
        'prepared',
      );
      assert.equal(requests.length, 1);
      if (httpStatus === 401) {
        assert.equal(await page.evaluate(() => locallyAuthenticated), false);
        await assert.rejects(run(page), /owner_not_ready/);
        assert.equal(requests.length, 1);
      }
      await close(page);
    }
    // The kernel fences itself even when the host revokes its proof without aborting.
    page = await setup();
    mode = 'status';
    status = 401;
    errorBody = '{';
    requests = [];
    await page.evaluate(() => {
      authInvalidationAborts = false;
    });
    assert.deepEqual((await run(page)).failure, {
      reason: 'access_unavailable',
      status: 401,
    });
    await page.evaluate(() => {
      currentAuthority = authority;
    });
    await assert.rejects(run(page), /owner_not_ready/);
    assert.equal(requests.length, 1);
    await close(page);
    // Retained statuses never depend on an error body finishing.
    for (const httpStatus of [401, 403, 404, 429, 503]) {
      page = await setup();
      mode = 'error_body_hold';
      status = httpStatus;
      requests = [];
      const result = await run(page);
      assert.equal(result.failure.status, httpStatus);
      assert.equal(
        result.failure.reason,
        httpStatus === 429
          ? 'rate_limited'
          : httpStatus === 503
            ? 'server_error'
            : 'access_unavailable',
      );
      assert.equal(requests.length, 1);
      assert.equal(
        await page.evaluate(
          () => timers.filter((timer) => timer.active).length,
        ),
        0,
      );
      await close(page);
    }
    page = await setup();
    mode = 'headers_hold';
    requests = [];
    const beforeHeaders = waiting();
    await page.evaluate(() => start());
    await beforeHeaders;
    await page.evaluate(() => fire(15000));
    assert.deepEqual((await page.evaluate(() => pending)).value.failure, {
      reason: 'network_error',
      status: 0,
    });
    assert.equal(requests.length, 1);
    await close(page);
    for (const deadline of [45000, 60000]) {
      page = await setup();
      mode = 'body_hold';
      requests = [];
      await page.evaluate(() => start());
      await page.waitForFunction(() =>
        timers.some((timer) => timer.active && timer.milliseconds === 45000),
      );
      assert.equal(
        await page.evaluate(() =>
          timers.some((timer) => timer.active && timer.milliseconds === 15000),
        ),
        false,
      );
      await page.evaluate((milliseconds) => fire(milliseconds), deadline);
      assert.deepEqual((await page.evaluate(() => pending)).value.failure, {
        reason: 'network_error',
        status: 200,
      });
      held();
      assert.equal(
        (await page.evaluate(() => snapshot()))[0].state,
        'prepared',
      );
      assert.equal(requests.length, 1);
      await close(page);
    }
    page = await setup();
    mode = 'normal';
    requests = [];
    await page.evaluate(async () => {
      releaseLock();
      await lockLife;
    });
    await assert.rejects(run(page), (error) =>
      error.message.includes('owner_not_ready'),
    );
    assert.equal(requests.length, 0);
    await close(page);
    page = await setup({ immediateClock: true });
    mode = 'normal';
    requests = [];
    assert.deepEqual(
      await page.evaluate(async () => {
        try {
          await kernel.run(lane);
          return { unexpected: true };
        } catch (error) {
          return { code: error.code, dispatched: error.dispatched };
        }
      }),
      { code: 'owner_not_ready', dispatched: false },
    );
    assert.equal(requests.length, 0);
    await close(page);
    // Error-body deadlines preserve the already observed status; a late body cannot ACK.
    page = await setup();
    mode = 'error_body_hold';
    status = 409;
    requests = [];
    await page.evaluate(() => start());
    await page.waitForFunction(() =>
      timers.some((timer) => timer.active && timer.milliseconds === 45000),
    );
    await page.evaluate(() => fire(60000));
    assert.deepEqual((await page.evaluate(() => pending)).value.failure, {
      reason: 'unclassified_error_response',
      status: 409,
    });
    assert.equal(requests.length, 1);
    await close(page);
    page = await setup();
    mode = 'body_hold';
    requests = [];
    await page.evaluate(() => start());
    await page.waitForFunction(() =>
      timers.some((timer) => timer.active && timer.milliseconds === 45000),
    );
    await page.evaluate(() => {
      currentAuthority = Symbol();
    });
    held();
    assert.deepEqual(await page.evaluate(() => pending), {
      error: 'owner_not_ready',
      dispatched: true,
    });
    assert.equal((await page.evaluate(() => snapshot()))[0].state, 'prepared');
    await assert.rejects(run(page), /owner_not_ready/);
    assert.equal(requests.length, 1);
    await close(page);

    await checkAuthorityException({
      phase: 'before_dispatch',
      dispatched: false,
      state: 'unprepared',
      expectedRequests: 0,
    });
    await checkAuthorityException({
      phase: 'after_dispatch',
      dispatched: true,
      state: 'prepared',
      expectedRequests: 1,
    });
    await checkAuthorityException({
      phase: 'authentication_invalidation',
      dispatched: true,
      state: 'prepared',
      expectedRequests: 1,
    });

    // All native redirect variants reject before reaching either collector.
    for (const targetUrl of ['/collect', crossOrigin])
      for (const code of [301, 302, 303, 307, 308]) {
        page = await setup();
        mode = 'redirect';
        redirectTarget = targetUrl;
        redirectStatus = code;
        collected = [];
        assert.deepEqual((await run(page)).failure, {
          reason: 'network_error',
          status: 0,
        });
        assert.equal(collected.length, 0);
        await close(page);
      }
    // Positive control proves the collector accepts a follow, including frozen data and fences.
    page = await setup();
    mode = 'redirect';
    redirectTarget = crossOrigin;
    redirectStatus = 307;
    collected = [];
    await page.evaluate(async () => {
      const row = await store.prepare(head, authorize, stop.signal);
      await fetch(
        location.origin + '/v1/artifacts/' + row.artifactId + '/data',
        {
          method: row.request.method,
          body: row.request.body,
          redirect: 'follow',
          headers: {
            'Content-Type': 'application/json',
            'Idempotency-Key': row.key,
            'Artifree-Outbox-Actor': 'local',
            'Artifree-Outbox-Epoch': auth.storage_epoch,
            'Artifree-Outbox-Actor-Generation': auth.actor_generation,
          },
        },
      );
    });
    assert.equal(collected.at(-1).method, 'PUT');
    assert.equal(
      collected.at(-1).headers['idempotency-key'],
      await page.evaluate(() => head.key),
    );
    assert.equal(
      collected.at(-1).body,
      '{"base_data_version":0,"path":"marker","value":{"original":1}}',
    );
    await close(page);

    // Native transaction terminal events distinguish rollback from a completed
    // write whose authority became stale before the caller resumed.
    for (const abort of [false, true]) {
      for (const [stage, table, method, state, at] of [
        ['prepare', 'checkpoints', 'get', undefined, 1],
        ['prepare', 'checkpoints', 'get', undefined, 2],
        ['prepare', 'intents', 'put', 'prepared', 1],
        ['prepare', 'checkpoints', 'put', undefined, 1],
        ['ack', 'checkpoints', 'get', undefined, 1],
        ['ack', 'checkpoints', 'get', undefined, 2],
        ['ack', 'intents', 'put', 'acknowledged', 1],
        ['ack', 'checkpoints', 'put', undefined, 1],
        ['trim', 'checkpoints', 'get', undefined, 1],
        ['trim', 'intents', 'delete', undefined, 1],
        ['trim', 'checkpoints', 'get', undefined, 2],
        ['trim', 'checkpoints', 'delete', undefined, 1],
        ['trim', 'usage', 'put', undefined, 1],
      ]) {
        await checkStorageFence({ stage, table, method, state, at, abort });
      }
      for (const [stage, table, method, state] of [
        ['prepare', 'intents', 'put', 'prepared'],
        ['ack', 'intents', 'put', 'acknowledged'],
        ['trim', 'usage', 'put', undefined],
      ]) {
        await checkStorageFence({
          stage,
          table,
          method,
          state,
          at: 1,
          abort,
          commit: true,
        });
      }
    }
    if (serverApi !== undefined) {
      await selectActor('Outbox ACK');
      await checkHeldRequest();
      // Keep all ACK authority interleavings against actual Rust mutations.
      // Preparation/seeded-ACK trim cases above never dispatch HTTP.
      for (const abort of [false, true]) {
        for (const [table, method, state, at, commit] of [
          ['checkpoints', 'get', undefined, 1, false],
          ['checkpoints', 'get', undefined, 2, false],
          ['intents', 'put', 'acknowledged', 1, false],
          ['checkpoints', 'put', undefined, 1, false],
          ['intents', 'put', 'acknowledged', 1, true],
        ]) {
          await checkStorageFence({
            stage: 'ack',
            table,
            method,
            state,
            at,
            abort,
            commit,
          });
        }
      }
      await selectActor('Outbox replay');
      await checkRealServer();
      await selectActor('Outbox scheduler');
      await checkOutboxScheduler({
        setup,
        close,
        configure(nextMode) {
          mode = nextMode;
          requests = [];
        },
        requests: () => requests,
        createArtifact: createRealArtifact,
      });
    }
    console.log(
      'outbox delivery: native exact request, bounded status/receipt, redirect and authority fences passed; public sender disabled',
    );
  } catch (error) {
    // Cleanup rethrows this first, after settling the other resources.
    scenarioFailure =
      error instanceof Error
        ? error
        : new Error('Outbox scenario failed', { cause: error });
  } finally {
    await cleanupOutboxDelivery(
      {
        contexts,
        artifacts: createdArtifacts,
        users: isolatedUsers,
        serverApi,
        listeners: [server, target],
      },
      scenarioFailure,
    );
  }
  async function checkAuthorityException({
    phase,
    dispatched,
    state,
    expectedRequests,
  }) {
    const page = await setup();
    requests = [];
    mode =
      phase === 'after_dispatch'
        ? 'headers_hold'
        : phase === 'authentication_invalidation'
          ? 'status'
          : 'normal';
    status = 401;
    errorBody = '{';
    const arrival = phase === 'after_dispatch' ? waiting() : undefined;
    await page.evaluate((phase) => {
      if (phase === 'before_dispatch')
        authorityException = makeAuthorityException();
      if (phase === 'authentication_invalidation')
        authenticationInvalidationException = makeAuthorityException();
      globalThis.capturedPending = captureAttempt();
    }, phase);
    if (phase === 'after_dispatch') {
      await arrival;
      await page.evaluate(() => {
        authorityException = makeAuthorityException();
        fire(15000);
      });
    }
    const { diagnostics, ...failure } = await page.evaluate(
      () => capturedPending,
    );
    assert.equal(typeof diagnostics, 'string', `${phase}: attempt must fail`);
    assert.equal(
      diagnostics.includes('private-authority-exception-message'),
      false,
      `${phase}: callback message must stay private`,
    );
    assert.equal(
      diagnostics.includes('private-authority-exception-cause'),
      false,
      `${phase}: callback cause must stay private`,
    );
    assert.deepEqual(
      failure,
      {
        name: 'DeliveryAttemptError',
        code: 'owner_not_ready',
        message: 'owner_not_ready',
        dispatched,
        hasCause: false,
      },
      phase,
    );
    assert.equal((await page.evaluate(() => snapshot()))[0].state, state);
    assert.equal(requests.length, expectedRequests);
    await page.evaluate(() => {
      authorityException = undefined;
      authenticationInvalidationException = undefined;
    });
    mode = 'normal';
    await assert.rejects(run(page), /owner_not_ready/);
    assert.equal(requests.length, expectedRequests);
    await close(page);
  }
  async function checkHeldRequest() {
    // Inspect real committed IDB while the owned server holds the request before handling it.
    const page = await setup({
      artifact: live === undefined ? ARTIFACT : await createRealArtifact(),
    });
    requests = [];
    mode = 'hold_forward';
    if (live === undefined) mode = 'headers_hold';
    await page.evaluate(() => {
      head.text = '{"tampered":2}';
    });
    const atRequest = waiting();
    await page.evaluate(() => start());
    await atRequest;
    const prepared = (await page.evaluate(() => snapshot()))[0];
    assert.equal(prepared.state, 'prepared');
    assert.equal(Object.hasOwn(prepared, 'text'), false);
    assert.equal(requests[0].body, prepared.request.body);
    assert.equal(
      requests[0].body,
      '{"base_data_version":0,"path":"marker","value":{"original":1}}',
    );
    assert.equal(requests[0].headers['idempotency-key'], prepared.key);
    assert.equal(
      requests[0].headers['artifree-outbox-actor-generation'],
      prepared.authorizedGeneration,
    );
    await assert.rejects(run(page), /owner_not_ready/);
    if (live !== undefined) {
      mode = 'normal';
      held();
      assert.equal(
        (await page.evaluate(() => pending)).value.kind,
        'server_acknowledged',
      );
    } else {
      await page.evaluate(() => fire(15000));
      assert.deepEqual((await page.evaluate(() => pending)).value.failure, {
        reason: 'network_error',
        status: 0,
      });
    }
    assert.equal(requests.length, 1);
    await close(page);
    mode = 'normal';
  }
  async function selectActor(label) {
    const user = await adminPost(
      '/v1/users',
      {
        label: `${label} ${crypto.randomUUID()}`,
      },
      201,
    );
    assert.match(user.id, /^u-[0-9a-f]{32}$/);
    isolatedUsers.push(user.id);
    const issued = await adminPost(
      '/v1/tokens',
      {
        name: `${label} smoke`,
        user_id: user.id,
      },
      201,
    );
    assert.equal(issued.token.user_id, user.id);
    assert.equal(typeof issued.secret, 'string');
    assert.notEqual(issued.secret, '');
    live = { origin: serverApi.origin, token: issued.secret };
  }
  async function adminPost(path, body, expectedStatus = 200) {
    const response = await fetch(serverApi.origin + path, {
      method: 'POST',
      headers: {
        Authorization: 'Bearer ' + serverApi.token,
        'Content-Type': 'application/json',
      },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    });
    assert.equal(response.status, expectedStatus);
    return response.json();
  }
  async function checkStorageFence({
    stage,
    table,
    method,
    state,
    at,
    abort,
    commit = false,
  }) {
    const artifact = live === undefined ? ARTIFACT : await createRealArtifact();
    const page = await setup({ artifact });
    mode = 'normal';
    requests = [];
    receipts.clear();
    const receipt = {
      artifact_id: artifact,
      path: 'marker',
      data_version: 1,
      changed: true,
      replayed: false,
    };
    await page.evaluate(
      async ({ stage, receipt }) => {
        if (stage === 'prepare') return;
        const prepared = await store.prepare(head, authorize, stop.signal);
        if (stage === 'trim')
          await store.acknowledge(prepared, receipt, authorize, stop.signal);
      },
      { stage, receipt },
    );
    const before = await page.evaluate(() => storageSnapshot());
    await page.evaluate(
      ({ table, method, state, at, abort, commit }) => {
        fenceRequest(table, method, state, at, abort, commit);
        start();
      },
      { table, method, state, at, abort, commit },
    );
    assert.deepEqual(
      await page.evaluate(() => pending),
      {
        error: 'owner_not_ready',
        dispatched: stage === 'ack',
      },
      `${stage} ${table}.${method} ${at} abort=${abort} commit=${commit}`,
    );
    await page.evaluate(() => restoreFence());
    let expected = before;
    if (commit && stage === 'prepare') {
      const { text, ...identity } = before.rows[0];
      assert.equal(text, '{"original":1}');
      expected = {
        ...before,
        rows: [
          {
            ...identity,
            state: 'prepared',
            version: 2,
            request: {
              method: 'PUT',
              body: '{"base_data_version":0,"path":"marker","value":{"original":1}}',
              base: 0,
            },
          },
        ],
      };
    } else if (commit && stage === 'ack') {
      expected = {
        ...before,
        rows: [
          { ...before.rows[0], state: 'acknowledged', version: 3, receipt },
        ],
        checkpoints: [
          {
            ...before.checkpoints[0],
            predecessor: {
              sequence: before.rows[0].sequence,
              dataVersion: 1,
            },
          },
        ],
      };
    } else if (commit && stage === 'trim') {
      expected = {
        rows: [],
        checkpoints: [],
        usage: { ...before.usage, rows: 0, bytes: 0 },
      };
    }
    assert.deepEqual(await page.evaluate(() => storageSnapshot()), expected);
    // Restoring the host's identity cannot revive this captured kernel.
    await page.evaluate(() => {
      currentAuthority = authority;
    });
    await assert.rejects(run(page), /owner_not_ready/);
    assert.equal(requests.length, stage === 'ack' ? 1 : 0);
    await close(page);
  }
  async function createRealArtifact() {
    const response = await fetch(live.origin + '/v1/artifacts', {
      method: 'POST',
      headers: {
        Authorization: 'Bearer ' + live.token,
        'Content-Type': 'application/json',
        'Idempotency-Key': crypto.randomUUID(),
      },
      body: JSON.stringify({
        title: 'Internal delivery kernel',
        files: [
          {
            path: 'index.html',
            encoding: 'utf8',
            content: '<p>Original kernel fixture</p>',
          },
        ],
      }),
    });
    assert.equal(response.status, 201);
    const id = (await response.json()).artifact.id;
    createdArtifacts.push({ origin: live.origin, id, token: live.token });
    return id;
  }

  async function checkRealServer() {
    for (const loss of [
      'lost_response',
      'commit_redirect',
      'invalid_committed_receipt',
    ]) {
      const artifact = await createRealArtifact();
      let page = await setup({ artifact });
      requests = [];
      collected = [];
      mode = loss;
      assert.deepEqual(
        (await run(page)).failure,
        loss === 'invalid_committed_receipt'
          ? { reason: 'invalid_response', status: 200 }
          : { reason: 'network_error', status: 0 },
      );
      const frozen = (await page.evaluate(() => snapshot()))[0];
      const state = await fetch(
        live.origin + '/v1/artifacts/' + artifact + '/data',
        { headers: { Authorization: 'Bearer ' + live.token } },
      ).then((response) => response.json());
      assert.equal(state.data_version, 1);
      assert.equal(collected.length, 0);
      const external = await fetch(
        live.origin + '/v1/artifacts/' + artifact + '/data',
        {
          method: 'PUT',
          headers: {
            Authorization: 'Bearer ' + live.token,
            'Content-Type': 'application/json',
            'Idempotency-Key': crypto.randomUUID(),
          },
          body: JSON.stringify({
            path: '',
            base_data_version: 1,
            value: { canonical: 2 },
          }),
        },
      );
      assert.equal(external.status, 200);
      mode = 'normal';
      const replay = await run(page);
      assert.equal(replay.kind, 'server_acknowledged');
      assert.equal(replay.receipt.replayed, true);
      assert.equal(replay.receipt.data_version, 1);
      assert.deepEqual(
        requests.map((request) => [
          request.method,
          request.body,
          request.headers['idempotency-key'],
        ]),
        [
          [frozen.request.method, frozen.request.body, frozen.key],
          [frozen.request.method, frozen.request.body, frozen.key],
        ],
      );
      assert.deepEqual(await page.evaluate(() => snapshot()), []);
      const canonical = await fetch(
        live.origin + '/v1/artifacts/' + artifact + '/data',
        { headers: { Authorization: 'Bearer ' + live.token } },
      ).then((response) => response.json());
      assert.equal(canonical.data_version, 2);
      assert.deepEqual(canonical.value, { canonical: 2 });
      await close(page);
    }
    let artifact = await createRealArtifact();
    let page = await setup({ artifact });
    mode = 'normal';
    requests = [];
    await page.evaluate(async () => {
      await store.prepare(head, authorize, stop.signal);
      faultPut('checkpoints');
    });
    const failed = await run(page);
    assert.equal(failed.kind, 'ack_storage_failed');
    assert.equal(failed.ackDurable, false);
    assert.equal(failed.observedReceipt.replayed, false);
    assert.equal(requests.length, 1);
    await page.evaluate(() => restorePut());
    const replay = await run(page);
    assert.equal(replay.receipt.replayed, true);
    assert.equal(requests.length, 2);
    await close(page);
    artifact = await createRealArtifact();
    page = await setup({ artifact });
    requests = [];
    await page.evaluate(() => faultPut('usage'));
    assert.equal((await run(page)).kind, 'acknowledged_cleanup_pending');
    assert.equal(
      (await page.evaluate(() => snapshot()))[0].state,
      'acknowledged',
    );
    await page.evaluate(() => restorePut());
    assert.deepEqual(await run(page), { kind: 'cleanup_complete' });
    assert.equal(requests.length, 1);
    await close(page);
    artifact = await createRealArtifact();
    page = await setup({
      artifact,
      path: 'raw//invalid',
      text: '{"n":9007199254740993}',
    });
    requests = [];
    const invalid = await run(page);
    assert.deepEqual(invalid.failure, {
      reason: 'rejected',
      status: 422,
      code: 'invalid_request',
    });
    assert.equal(
      requests[0].body,
      '{"base_data_version":0,"path":"raw//invalid","value":{"n":9007199254740993}}',
    );
    assert.equal((await page.evaluate(() => snapshot()))[0].state, 'prepared');
    await close(page);
    artifact = await createRealArtifact();
    page = await setup({ artifact });
    mode = 'normal';
    requests = [];
    await page.evaluate(async () => {
      const { sequence, bytes, ...identity } = head;
      void bytes;
      const second = await store.enqueue({
        ...identity,
        key: crypto.randomUUID().replaceAll('-', ''),
        base: { after: sequence },
      });
      await store.enqueue({
        ...identity,
        key: crypto.randomUUID().replaceAll('-', ''),
        operation: 'update',
        path: 'next',
        text: '{"second":2}',
        base: { after: second.sequence },
      });
    });
    assert.equal((await run(page)).receipt.data_version, 1);
    const unchanged = await run(page);
    assert.equal(unchanged.receipt.changed, false);
    assert.equal(unchanged.receipt.data_version, 1);
    assert.equal((await run(page)).receipt.data_version, 2);
    assert.deepEqual(
      requests.map((request) => JSON.parse(request.body).base_data_version),
      [0, 1, 1],
    );
    await page.evaluate(async () => {
      const { sequence, bytes, ...identity } = head;
      void sequence;
      void bytes;
      await store.enqueue({
        ...identity,
        key: crypto.randomUUID().replaceAll('-', ''),
        base: { version: 0 },
      });
    });
    assert.deepEqual((await run(page)).failure, {
      reason: 'data_conflict',
      status: 409,
      currentDataVersion: 2,
    });
    assert.equal(requests.length, 4);
    await close(page);
    for (const [text, status] of [
      ['{', 400],
      ['{"a":1,"a":2}', 422],
      ['{"n":9007199254740993}', 422],
    ]) {
      artifact = await createRealArtifact();
      page = await setup({ artifact, text });
      mode = 'normal';
      requests = [];
      assert.deepEqual((await run(page)).failure, {
        reason: 'rejected',
        status,
        code: 'invalid_request',
      });
      assert.equal(
        requests[0].body,
        '{"base_data_version":0,"path":"marker","value":' + text + '}',
      );
      assert.equal(
        (await page.evaluate(() => snapshot()))[0].state,
        'prepared',
      );
      await close(page);
    }
    console.log(
      'outbox delivery: real Rust commit/lost response, exact-key replay, durable ACK/trim faults and raw rejection passed',
    );
  }
}

async function bodyText(request) {
  const chunks = [];
  for await (const chunk of request) chunks.push(chunk);
  return Buffer.concat(chunks).toString('utf8');
}
