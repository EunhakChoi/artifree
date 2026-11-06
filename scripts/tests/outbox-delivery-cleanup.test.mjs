import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { test } from 'node:test';
import { cleanupOutboxDelivery } from '../lib/outbox-delivery-cleanup.mjs';

async function fixture(fail = false) {
  const requests = [];
  const contextsClosed = [];
  const api = createServer(async (request, response) => {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    requests.push({
      method: request.method,
      path: request.url,
      authorization: request.headers.authorization,
      body: Buffer.concat(chunks).toString('utf8'),
    });
    response.statusCode =
      fail && request.url === '/v1/artifacts/first'
        ? 503
        : fail && request.url === '/v1/users/one/disable'
          ? 500
          : 200;
    response.end('{}');
  });
  const other = createServer((_request, response) => response.end('{}'));
  const listeners = [api, other];
  await Promise.all(
    listeners.map(
      (listener) =>
        new Promise((resolve) => {
          listener.listen(0, '127.0.0.1', resolve);
        }),
    ),
  );
  const origin = `http://127.0.0.1:${api.address().port}`;
  const options = {
    contexts: [
      {
        close: async () => {
          contextsClosed.push('first');
        },
      },
      {
        close: async () => {
          contextsClosed.push('second');
        },
      },
    ],
    artifacts: [
      { origin, id: 'first', token: 'alpha' },
      { origin, id: 'second', token: 'beta' },
    ],
    users: ['one', 'two'],
    serverApi: { origin, token: 'bootstrap' },
    listeners,
  };
  return {
    options,
    requests,
    contextsClosed,
    async close() {
      for (const listener of listeners) {
        listener.closeAllConnections();
        if (listener.listening)
          await new Promise((resolve, reject) =>
            listener.close((error) => (error ? reject(error) : resolve())),
          );
      }
    },
  };
}

const EXPECTED_REQUESTS = [
  {
    method: 'PATCH',
    path: '/v1/artifacts/first',
    authorization: 'Bearer alpha',
    body: '{"base_metadata_revision":1,"deleted":true}',
  },
  {
    method: 'POST',
    path: '/v1/artifacts/first/purge',
    authorization: 'Bearer alpha',
    body: '{"base_metadata_revision":2}',
  },
  {
    method: 'PATCH',
    path: '/v1/artifacts/second',
    authorization: 'Bearer beta',
    body: '{"base_metadata_revision":1,"deleted":true}',
  },
  {
    method: 'POST',
    path: '/v1/artifacts/second/purge',
    authorization: 'Bearer beta',
    body: '{"base_metadata_revision":2}',
  },
  {
    method: 'POST',
    path: '/v1/users/one/disable',
    authorization: 'Bearer bootstrap',
    body: '',
  },
  {
    method: 'POST',
    path: '/v1/users/two/disable',
    authorization: 'Bearer bootstrap',
    body: '',
  },
];

test('delivery cleanup uses each artifact owner and closes every context and listener', async () => {
  const scope = await fixture();
  try {
    await cleanupOutboxDelivery(scope.options);
    assert.deepEqual(scope.requests, EXPECTED_REQUESTS);
    assert.deepEqual(scope.contextsClosed, ['first', 'second']);
    assert.deepEqual(
      scope.options.listeners.map((listener) => listener.listening),
      [false, false],
    );
  } finally {
    await scope.close();
  }
});

test('delivery cleanup reports the first HTTP failure, attempts the rest, and closes listeners', async () => {
  const scope = await fixture(true);
  try {
    const [result] = await Promise.allSettled([
      cleanupOutboxDelivery(scope.options),
    ]);
    assert.deepEqual(
      scope.options.listeners.map((listener) => listener.listening),
      [false, false],
    );
    assert.equal(result.status, 'rejected');
    assert.ok(result.reason instanceof AggregateError);
    assert.strictEqual(result.reason.cause, result.reason.errors[0]);
    assert.deepEqual(
      result.reason.errors.map((failure) => failure.actual),
      [503, 500],
    );
    assert.deepEqual(scope.requests, EXPECTED_REQUESTS);
    assert.deepEqual(
      scope.options.listeners.map((listener) => listener.listening),
      [false, false],
    );
  } finally {
    await scope.close();
  }
});

test('delivery cleanup preserves an earlier scenario failure alongside cleanup failures', async () => {
  const scope = await fixture(true);
  const primary = new Error('original scenario failed');
  try {
    await assert.rejects(
      cleanupOutboxDelivery(scope.options, primary),
      (error) => {
        assert.ok(error instanceof AggregateError);
        assert.strictEqual(error.cause, primary);
        assert.strictEqual(error.errors[0], primary);
        assert.deepEqual(
          error.errors.slice(1).map((failure) => failure.actual),
          [503, 500],
        );
        return true;
      },
    );
    assert.deepEqual(scope.requests, EXPECTED_REQUESTS);
    assert.deepEqual(
      scope.options.listeners.map((listener) => listener.listening),
      [false, false],
    );
  } finally {
    await scope.close();
  }
});
