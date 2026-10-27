import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  ArtifreeApiError,
  ArtifreeClient,
  encodeFile,
} from '../../packages/client/dist/index.js';

const artifact = {
  id: `art_${'0'.repeat(32)}`,
  kind: 'html',
  title: 'Demo',
  current_revision: 1,
  created_at_ms: 1,
  updated_at_ms: 1,
};

/** A client whose every request is answered by `respond`. */
function clientWith(respond) {
  return new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:7410',
    accessToken: 'token',
    fetch: async () => respond(),
  });
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
