import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFile, stat } from 'node:fs/promises';
import { createServer, request as httpRequest } from 'node:http';
import { join } from 'node:path';
import { after, before, test } from 'node:test';
import {
  cleanUp,
  createArtifact,
  makeScratch,
  parseCliSuccess,
  runCli,
  startTestServer,
  writeTestFile,
} from '../lib/cli-assets-harness.mjs';

// An unknown outcome and a corrupted read, end to end. A node:http proxy in
// front of the real server either loses the reply to one mutation after the
// server has answered it, or flips one byte of one asset's content. The CLI
// prints the retry hint, and the printed key replays the original result. A
// corrupted read fails with invalid_response and creates no file. Expected
// values are literals, or come from node:crypto over the input bytes; asset
// IDs are the ones the server chose in its receipts.

const sha256 = (bytes) => createHash('sha256').update(bytes).digest('hex');
const LIMITS = { max_assets: 64, max_bytes: 26_214_400 };
const CSV = 'name,score\nada,10\n';

let server;
let scratch;

before(async () => {
  scratch = await makeScratch('artifree-asset-outcome-');
  server = await startTestServer(scratch);
});

after(() => cleanUp({ server, scratch }));

/** Reads a stream to its end. */
async function readAll(stream) {
  const chunks = [];
  for await (const chunk of stream) chunks.push(chunk);
  return Buffer.concat(chunks);
}

/**
 * A node:http proxy on 127.0.0.1, port 0, in front of the API of `server`.
 * Every request is forwarded whole, and its whole upstream response is read
 * before anything is answered, so a request the server committed has been
 * answered in full by then. Only requests with `fault.method` and `fault.path`
 * change:
 *   lost-reply  the client socket is destroyed without an answer;
 *   flip-byte   the answer keeps its status and headers, and the first body
 *               byte has its lowest bit flipped (the length is unchanged).
 * `events` lists each changed request. `close` ends every connection.
 */
async function startProxy(server, fault) {
  const upstream = new URL(server.api_url);
  const events = [];
  const sockets = new Set();
  const upstreamRequests = new Set();

  function exchange(method, path, headers, body) {
    return new Promise((resolve, reject) => {
      const request = httpRequest(
        {
          protocol: upstream.protocol,
          hostname: upstream.hostname,
          port: upstream.port,
          method,
          path,
          headers,
          agent: false,
        },
        (response) => {
          readAll(response).then(
            (bytes) =>
              resolve({
                status: response.statusCode,
                headers: response.headers,
                body: bytes,
              }),
            reject,
          );
        },
      );
      upstreamRequests.add(request);
      request.once('error', reject);
      request.once('close', () => upstreamRequests.delete(request));
      request.end(body);
    });
  }

  async function relay(clientRequest, clientResponse) {
    const body = await readAll(clientRequest);
    const headers = {
      ...clientRequest.headers,
      host: upstream.host,
      'content-length': String(body.byteLength),
    };
    delete headers.connection;
    delete headers['keep-alive'];
    delete headers['transfer-encoding'];
    const { method, url } = clientRequest;
    const { pathname } = new URL(url, upstream);
    const answer = await exchange(method, url, headers, body);
    if (method !== fault.method || pathname !== fault.path) {
      clientResponse.writeHead(answer.status, answer.headers);
      clientResponse.end(answer.body);
    } else if (fault.action === 'lost-reply') {
      events.push({
        action: fault.action,
        method,
        path: pathname,
        upstreamStatus: answer.status,
      });
      clientRequest.socket.destroy();
    } else {
      const flipped = Buffer.from(answer.body);
      flipped[0] ^= 0x01;
      const changedBytes = [...answer.body].filter(
        (byte, index) => byte !== flipped[index],
      ).length;
      events.push({
        action: fault.action,
        method,
        path: pathname,
        upstreamStatus: answer.status,
        changedBytes,
      });
      clientResponse.writeHead(answer.status, answer.headers);
      clientResponse.end(flipped);
    }
  }

  const proxy = createServer((clientRequest, clientResponse) => {
    relay(clientRequest, clientResponse).catch((error) => {
      events.push({ action: 'proxy-error', message: error.message });
      clientRequest.socket.destroy();
    });
  });
  proxy.on('connection', (socket) => {
    sockets.add(socket);
    socket.once('close', () => sockets.delete(socket));
  });
  await new Promise((resolve) => proxy.listen(0, '127.0.0.1', resolve));
  return {
    url: `http://127.0.0.1:${proxy.address().port}`,
    events,
    close() {
      for (const request of upstreamRequests) request.destroy();
      for (const socket of sockets) socket.destroy();
      return new Promise((resolve, reject) =>
        proxy.close((error) => (error ? reject(error) : resolve())),
      );
    },
  };
}

test('A lost upload reply: the printed key replays the one committed asset', async (t) => {
  const artifactId = await createArtifact(server, scratch, 'Lost upload reply');
  const csv = Buffer.from(CSV);
  const csvPath = await writeTestFile(scratch, 'lost-upload.csv', csv);
  const upload = [
    'asset',
    'upload',
    artifactId,
    '--file',
    csvPath,
    '--content-type',
    'text/csv',
    '--base-asset-version',
    '0',
  ];
  const uploadPath = `/v1/artifacts/${artifactId}/assets`;
  const proxy = await startProxy(server, {
    action: 'lost-reply',
    method: 'POST',
    path: uploadPath,
  });
  t.after(() => proxy.close());

  const lost = await runCli(server, upload, { url: proxy.url });
  // The key is the one value only the CLI chooses; the rest is exact.
  const key = /--idempotency-key (\S+) /.exec(lost.stderr)?.[1];
  assert.equal(lost.status, 1);
  assert.equal(lost.stdout, '');
  assert.equal(
    lost.stderr,
    `The outcome is unknown. Retry with --idempotency-key ${key} and the same artifact, --content-type, --base-asset-version and file content (sha256 ${sha256(csv)}, ${csv.byteLength} bytes) to receive the original result.\n` +
      `error[network_error]: Could not reach the Artifree API at ${proxy.url}.\n`,
  );
  // The server answered the upload with 201 before the reply was lost.
  assert.deepEqual(proxy.events, [
    {
      action: 'lost-reply',
      method: 'POST',
      path: uploadPath,
      upstreamStatus: 201,
    },
  ]);

  // The identical command, with the printed key, replays the committed upload.
  const replay = parseCliSuccess(
    await runCli(server, [...upload, '--idempotency-key', key]),
  );
  assert.equal(replay.replayed, true);
  assert.equal(replay.asset_version, 1);
  assert.equal(replay.asset.content_type, 'text/csv');
  assert.equal(replay.asset.size_bytes, csv.byteLength);
  assert.equal(replay.asset.sha256, sha256(csv));
  // The space holds exactly that asset: the first attempt did commit.
  assert.deepEqual(
    parseCliSuccess(await runCli(server, ['asset', 'list', artifactId])),
    {
      asset_version: 1,
      assets: [replay.asset],
      bytes: csv.byteLength,
      ...LIMITS,
    },
  );
});

test('A lost delete reply: the printed key replays the committed delete, and the space moves once', async (t) => {
  const artifactId = await createArtifact(server, scratch, 'Lost delete reply');
  const csv = Buffer.from(CSV);
  const csvPath = await writeTestFile(scratch, 'lost-delete.csv', csv);
  const { asset } = parseCliSuccess(
    await runCli(server, [
      'asset',
      'upload',
      artifactId,
      '--file',
      csvPath,
      '--content-type',
      'text/csv',
      '--base-asset-version',
      '0',
      '--idempotency-key',
      'outcome-delete-setup-1',
    ]),
  );
  const assetPath = `/v1/artifacts/${artifactId}/assets/${asset.id}`;
  const remove = [
    'asset',
    'delete',
    artifactId,
    asset.id,
    '--base-asset-version',
    '1',
  ];
  const proxy = await startProxy(server, {
    action: 'lost-reply',
    method: 'DELETE',
    path: assetPath,
  });
  t.after(() => proxy.close());

  const lost = await runCli(server, remove, { url: proxy.url });
  // The key is the one value only the CLI chooses; the rest is exact.
  const key = /--idempotency-key (\S+) /.exec(lost.stderr)?.[1];
  assert.equal(lost.status, 1);
  assert.equal(lost.stdout, '');
  assert.equal(
    lost.stderr,
    `The outcome is unknown. Retry with --idempotency-key ${key} and the same artifact, asset ID and --base-asset-version to receive the original result.\n` +
      `error[network_error]: Could not reach the Artifree API at ${proxy.url}.\n`,
  );
  // The server answered the delete with 200 before the reply was lost.
  assert.deepEqual(proxy.events, [
    {
      action: 'lost-reply',
      method: 'DELETE',
      path: assetPath,
      upstreamStatus: 200,
    },
  ]);

  // The identical command, with the printed key, replays the committed delete.
  // The space is empty at version 2: the one delete moved the version once.
  assert.deepEqual(
    parseCliSuccess(
      await runCli(server, [...remove, '--idempotency-key', key]),
    ),
    { asset_id: asset.id, asset_version: 2, deleted: true, replayed: true },
  );
  assert.deepEqual(
    parseCliSuccess(await runCli(server, ['asset', 'list', artifactId])),
    { asset_version: 2, assets: [], bytes: 0, ...LIMITS },
  );
});

test('A corrupted asset read fails with invalid_response and creates no file, and the direct read succeeds', async (t) => {
  const artifactId = await createArtifact(server, scratch, 'Corrupted read');
  const csv = Buffer.from(CSV);
  const csvPath = await writeTestFile(scratch, 'corrupted-read.csv', csv);
  const { asset } = parseCliSuccess(
    await runCli(server, [
      'asset',
      'upload',
      artifactId,
      '--file',
      csvPath,
      '--content-type',
      'text/csv',
      '--base-asset-version',
      '0',
      '--idempotency-key',
      'outcome-read-setup-1',
    ]),
  );
  const contentPath = `/v1/artifacts/${artifactId}/assets/${asset.id}/content`;
  const proxy = await startProxy(server, {
    action: 'flip-byte',
    method: 'GET',
    path: contentPath,
  });
  t.after(() => proxy.close());

  const output = join(scratch, 'corrupted-read.out');
  const corrupted = await runCli(
    server,
    ['asset', 'read', artifactId, asset.id, '--output', output],
    { url: proxy.url },
  );
  assert.deepEqual(corrupted, {
    status: 1,
    stdout: '',
    stderr:
      'error[invalid_response]: The asset response does not match its metadata.\n',
  });
  // One content request, answered with a single changed byte.
  assert.deepEqual(proxy.events, [
    {
      action: 'flip-byte',
      method: 'GET',
      path: contentPath,
      upstreamStatus: 200,
      changedBytes: 1,
    },
  ]);
  await assert.rejects(stat(output), { code: 'ENOENT' });

  // The same read straight from the server succeeds, so the proxy changed the bytes.
  assert.deepEqual(
    parseCliSuccess(
      await runCli(server, [
        'asset',
        'read',
        artifactId,
        asset.id,
        '--output',
        output,
      ]),
    ),
    { artifact_id: artifactId, asset_version: 1, asset, output },
  );
  assert.ok(
    (await readFile(output)).equals(csv),
    'the file read directly differs from the uploaded bytes',
  );
});
