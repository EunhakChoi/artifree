import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  ArtifreeApiError,
  ArtifreeClient,
} from '../../packages/client/dist/index.js';
import { parseBackground } from '../../packages/client/dist/responses.js';

/** Every field the server may send, with a last exit (decision 0055, rule 5). */
const fullStatus = {
  worker: {
    mode: 'supervised',
    state: 'restarting',
    restarts: 3,
    last_exit: { at_ms: 1_700_000_000_000, outcome: 'failed' },
    dropped_lines: 2,
  },
};

/** A status whose worker runs under supervision and has not exited yet. */
const baseWorker = {
  mode: 'supervised',
  state: 'running',
  restarts: 0,
  dropped_lines: 0,
};

/** A status whose worker is `baseWorker` with `changes` applied. */
function statusWith(changes) {
  return { worker: { ...baseWorker, ...changes } };
}

/** A last exit at `atMs` with the given outcome. */
function exitAt(outcome, atMs = 5) {
  return { at_ms: atMs, outcome };
}

/**
 * A client whose every request is answered by `respond`, as in client.test.mjs.
 * Each request is also pushed to `sent` as `{ url, init }`.
 */
function clientWith(respond, sent = []) {
  return new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:7410',
    accessToken: 'token',
    fetch: async (url, init) => {
      sent.push({ url, init });
      return respond();
    },
  });
}

/** The error that `promise` rejects with; the test fails if it resolves. */
async function rejectionOf(promise) {
  const error = await promise.then(
    () => assert.fail('The request must be refused.'),
    (rejection) => rejection,
  );
  assert.ok(error instanceof ArtifreeApiError, String(error));
  return error;
}

test('getBackground returns a full status exactly as the server sent it', async () => {
  const client = clientWith(() => Response.json(fullStatus));
  assert.deepStrictEqual(await client.getBackground(), fullStatus);
});

test('getBackground sends GET /v1/background with the bearer token and the caller signal', async () => {
  const sent = [];
  const controller = new AbortController();
  const client = clientWith(() => Response.json(fullStatus), sent);
  await client.getBackground({ signal: controller.signal });
  assert.equal(sent.length, 1);
  const [{ url, init }] = sent;
  assert.equal(url, 'http://127.0.0.1:7410/v1/background');
  assert.equal(init.method, 'GET');
  assert.equal(init.body, undefined);
  assert.equal(new Headers(init.headers).get('authorization'), 'Bearer token');
  assert.equal(init.signal, controller.signal);
});

test('A status without last_exit parses with no last_exit key', async () => {
  const body = statusWith({ mode: 'none', state: 'stopped' });
  const status = await clientWith(() => Response.json(body)).getBackground();
  assert.deepStrictEqual(status, body);
  assert.equal(Object.hasOwn(status.worker, 'last_exit'), false);
});

/** Each value of the closed vocabularies, which the parser must accept. */
const acceptedChanges = [
  ['mode supervised', { mode: 'supervised' }],
  ['mode none', { mode: 'none' }],
  ['state running', { state: 'running' }],
  ['state restarting', { state: 'restarting' }],
  ['state stopped', { state: 'stopped' }],
  ['exit outcome clean', { last_exit: exitAt('clean') }],
  ['exit outcome failed', { last_exit: exitAt('failed') }],
  ['exit outcome not_started', { last_exit: exitAt('not_started') }],
];

for (const [name, changes] of acceptedChanges) {
  test(`parseBackground accepts a worker with ${name}`, () => {
    const body = statusWith(changes);
    assert.deepStrictEqual(parseBackground(body), body);
  });
}

/** Bodies outside the closed vocabularies or the counts; each breaks one field. */
const refusedStatuses = [
  ['an unknown mode', statusWith({ mode: 'paused' })],
  ['an unknown state', statusWith({ state: 'idle' })],
  ['an unknown exit outcome', statusWith({ last_exit: exitAt('timeout') })],
  ['a negative restart count', statusWith({ restarts: -1 })],
  ['a negative dropped-line count', statusWith({ dropped_lines: -1 })],
  ['a fractional exit time', statusWith({ last_exit: exitAt('clean', 1.5) })],
  ['an exit that is not an object', statusWith({ last_exit: 'failed' })],
  ['a missing worker', {}],
];

for (const [name, body] of refusedStatuses) {
  test(`A status with ${name} is refused as an invalid response`, async () => {
    assert.equal(parseBackground(body), undefined);
    const error = await rejectionOf(
      clientWith(() => Response.json(body)).getBackground(),
    );
    assert.equal(error.code, 'invalid_response');
    assert.equal(error.status, 200);
  });
}
