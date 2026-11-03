import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import {
  ArtifreeApiError,
  ArtifreeClient,
  MAX_DOWNLOAD_NAME_CHARS,
  RuntimeConnectionReplacedError,
  decodeBase64,
  isDownloadName,
} from '../../packages/client/dist/index.js';
import { scaffoldDescription } from '../../packages/contracts/dist/index.js';

const vectors = JSON.parse(
  await readFile(
    new URL('./fixtures/source-part-vectors.json', import.meta.url),
    'utf8',
  ),
);
const schema = JSON.parse(
  await readFile(
    new URL('../../contracts/generated/api-v1.schema.json', import.meta.url),
    'utf8',
  ),
);

const SESSION = 'e'.repeat(32);
const view = {
  url: `http://127.0.0.1:7411/r/${'b'.repeat(64)}/`,
  enabled: true,
  session_id: SESSION,
  artifact_id: `art_${'0'.repeat(32)}`,
  revision: 3,
  head_revision: 3,
  sdk: 'artifree.app.v1',
  declared: ['source.read', 'source.download'],
  supported: ['metadata.read', 'source.download', 'source.read'],
  granted: ['source.read', 'source.download'],
  grant_version: 1,
  view_id: 'd'.repeat(32),
  generation: 1,
  initialization: 'c'.repeat(64),
  expires_at_ms: 1_000_000,
  lease_ms: 900_000,
};

/** A client whose frame calls are answered by `respond(url, body)`. */
async function clientWith(respond) {
  const requests = [];
  const client = new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:7410',
    accessToken: 'token',
    fetch: async (url, init) => {
      if (url.endsWith('/v1/runtime-sessions'))
        return Response.json({ session_id: SESSION });
      const body = init?.body === undefined ? undefined : JSON.parse(init.body);
      requests.push({ url, method: init?.method, body });
      return respond(url, body);
    },
  });
  await client.openRuntimeSession();
  return { client, requests };
}

const base64 = (length) =>
  Buffer.from(Uint8Array.from({ length }, (_, index) => index % 251)).toString(
    'base64',
  );

function partBody(testCase) {
  return {
    revision: view.revision,
    path: testCase.request.path,
    offset: testCase.request.offset,
    sha256: vectors.digest,
    content: base64(testCase.bytes),
    ...testCase.reply,
  };
}

for (const testCase of vectors.cases) {
  test(`A part is judged by the bytes received: ${testCase.name}`, async () => {
    const { client } = await clientWith(() =>
      Response.json(partBody(testCase)),
    );
    const result = client.runtimeSourceRead(view, testCase.request);
    if (testCase.valid) {
      const part = await result;
      assert.equal(part.bytes.length, testCase.bytes);
      assert.equal(part.nextOffset, testCase.reply.next_offset);
    } else {
      const error = await result.then(
        () => assert.fail('The part must be refused.'),
        (rejection) => rejection,
      );
      assert.ok(error instanceof ArtifreeApiError);
      assert.equal(error.code, 'invalid_response');
      // No peer-supplied text is carried in the message.
      assert.ok(!String(error.message).includes(vectors.digest));
    }
  });
}

test('Parts of a file join to the file; the digest and size repeat on each', async () => {
  const content = Uint8Array.from({ length: 10 }, (_, index) => index + 1);
  const { client, requests } = await clientWith((url, body) => {
    const end = Math.min(body.offset + body.length, content.length);
    return Response.json({
      revision: view.revision,
      path: body.path,
      offset: body.offset,
      next_offset: end < content.length ? end : null,
      size: content.length,
      sha256: vectors.digest,
      content: Buffer.from(content.subarray(body.offset, end)).toString(
        'base64',
      ),
    });
  });
  const joined = [];
  let offset = 0;
  for (;;) {
    const part = await client.runtimeSourceRead(view, {
      path: 'a.bin',
      offset,
      length: 4,
    });
    joined.push(...part.bytes);
    if (part.nextOffset === null) break;
    offset = part.nextOffset;
  }
  assert.deepEqual(joined, [...content]);
  assert.equal(requests.length, 3);
  assert.ok(
    requests[0].url.endsWith(
      `/revisions/3/runtime-views/${view.view_id}/1/source/read`,
    ),
  );
  assert.equal(requests[0].body.initialization, view.initialization);
});

test('A listing names each path once and matches the pinned revision', async () => {
  const file = (path) => ({
    path,
    size: 3,
    sha256: vectors.digest,
    media_type: 'text/plain; charset=utf-8',
  });
  const answer =
    (files, revision = 3) =>
    async () => {
      const { client } = await clientWith(() =>
        Response.json({ revision, files }),
      );
      return client.runtimeSourceList(view);
    };
  assert.equal(
    (await answer([file('a.txt'), file('b.txt')])()).files.length,
    2,
  );
  assert.equal((await answer([])()).files.length, 0);
  for (const bad of [
    answer([file('a.txt'), file('a.txt')]),
    answer([file('a.txt')], 4),
    answer([{ ...file('a.txt'), size: -1 }]),
    answer([{ ...file('a.txt'), sha256: 'F'.repeat(64) }]),
  ])
    await assert.rejects(bad(), (error) => error.code === 'invalid_response');
});

function offerBody(overrides = {}) {
  return {
    kind: 'file',
    path: 'a.txt',
    file_name: 'a.txt',
    size: 3,
    sha256: vectors.digest,
    ...overrides,
  };
}

test('An offer must answer the target that was asked', async () => {
  const check = async (target, body) => {
    const { client } = await clientWith(() => Response.json(body));
    return client.runtimeSourceOffer(view, target);
  };
  const file = { kind: 'file', path: 'a.txt' };
  assert.equal((await check(file, offerBody())).file_name, 'a.txt');
  const archive = { kind: 'archive' };
  assert.equal(
    (
      await check(
        archive,
        offerBody({ kind: 'archive', path: null, size: null, sha256: null }),
      )
    ).kind,
    'archive',
  );
  for (const [target, body] of [
    [file, offerBody({ path: 'b.txt' })],
    [file, offerBody({ kind: 'archive' })],
    [file, offerBody({ file_name: '../a.txt' })],
    [file, offerBody({ file_name: '.hidden' })],
    [file, offerBody({ file_name: 'a'.repeat(MAX_DOWNLOAD_NAME_CHARS + 1) })],
    [file, offerBody({ size: null })],
    [archive, offerBody()],
    [
      archive,
      offerBody({ kind: 'archive', path: 'a.txt', size: null, sha256: null }),
    ],
  ])
    await assert.rejects(
      check(target, body),
      (error) => error.code === 'invalid_response',
    );
});

test('The name bound the client accepts is not tighter than the one the server offers', () => {
  const offered = schema.$defs.RuntimeDownloadOffer.properties.file_name;
  assert.equal(
    offered.maxLength,
    scaffoldDescription.package_rules.max_segment_bytes,
  );
  assert.ok(offered.maxLength <= MAX_DOWNLOAD_NAME_CHARS);
  assert.ok(isDownloadName('a'.repeat(offered.maxLength)));
  assert.ok(!isDownloadName('a'.repeat(MAX_DOWNLOAD_NAME_CHARS + 1)));
  for (const bad of [
    '',
    '.x',
    'a/b',
    'a\\b',
    'a b',
    'a"b',
    'é.txt',
    'a\r\nb',
    7,
  ])
    assert.ok(!isDownloadName(bad), String(bad));
  assert.equal(
    schema.$defs.RuntimeSourceReadRequest.properties.length.maximum,
    1_048_576,
  );
});

test('Base64 is read only in its canonical form', () => {
  assert.deepEqual([...decodeBase64('')], []);
  assert.deepEqual([...decodeBase64('AQI=')], [1, 2]);
  for (const bad of [
    'AQI',
    'AQ=I',
    'A===',
    'AQI= ',
    'AQ I=',
    'AQI\n=',
    '____',
    'AQJ=',
  ])
    assert.equal(decodeBase64(bad), undefined, JSON.stringify(bad));
});

const sha256Of = (bytes) =>
  createHash('sha256').update(Uint8Array.from(bytes)).digest('hex');

/** An offer for the bytes [1, 2, 3], and a response carrying `bytes`. */
function download(overrides = {}, headers = {}, bytes = [1, 2, 3]) {
  const offer = {
    kind: 'file',
    path: 'a.txt',
    file_name: 'a.txt',
    size: 3,
    sha256: sha256Of([1, 2, 3]),
    ...overrides,
  };
  return {
    offer,
    respond: () =>
      new Response(Uint8Array.from(bytes), {
        headers: {
          'content-type': 'application/octet-stream',
          'content-disposition': 'attachment; filename="a.txt"',
          ...headers,
        },
      }),
  };
}

test('A download resolves only as the attachment that was offered', async () => {
  {
    const { offer, respond } = download();
    const { client, requests } = await clientWith(respond);
    assert.deepEqual(
      [...(await client.runtimeSourceDownload(view, offer))],
      [1, 2, 3],
    );
    assert.equal(requests[0].method, 'POST');
    assert.deepEqual(requests[0].body, {
      initialization: view.initialization,
      kind: 'file',
      path: 'a.txt',
    });
  }
  const archive = download(
    {
      kind: 'archive',
      path: null,
      size: null,
      sha256: null,
      file_name: 'x-r3.zip',
    },
    {
      'content-type': 'application/zip',
      'content-disposition': 'attachment; filename="x-r3.zip"',
    },
  );
  const { client } = await clientWith(archive.respond);
  assert.equal(
    (await client.runtimeSourceDownload(view, archive.offer)).length,
    3,
  );
  for (const bad of [
    download({}, { 'content-type': 'text/html' }),
    download({}, { 'content-type': 'application/zip' }),
    download({}, { 'content-disposition': 'inline; filename="a.txt"' }),
    download({}, { 'content-disposition': 'attachment; filename="b.txt"' }),
    download({}, {}, [1, 2]),
    download({}, {}, [1, 2, 3, 4]),
  ]) {
    const { client: other } = await clientWith(bad.respond);
    await assert.rejects(
      other.runtimeSourceDownload(view, bad.offer),
      (error) => error.code === 'invalid_response',
    );
  }
});

test('A file download must be the bytes whose digest was offered', async () => {
  // The same length and a different content is not the offered file.
  const tampered = download({}, {}, [1, 2, 4]);
  const { client } = await clientWith(tampered.respond);
  await assert.rejects(
    client.runtimeSourceDownload(view, tampered.offer),
    (error) =>
      error instanceof ArtifreeApiError &&
      error.code === 'invalid_response' &&
      !String(error.message).includes(tampered.offer.sha256),
  );
  // An archive is built on demand and has no digest: its size is all there is.
  const archive = download(
    { kind: 'archive', path: null, size: null, sha256: null },
    { 'content-type': 'application/zip' },
    [7, 7],
  );
  const { client: zipped } = await clientWith(archive.respond);
  assert.equal(
    (await zipped.runtimeSourceDownload(view, archive.offer)).length,
    2,
  );
});

test('A digest that cannot be computed refuses the download, it does not pass it', async () => {
  const original = Object.getOwnPropertyDescriptor(globalThis, 'crypto');
  const { offer, respond } = download();
  const { client } = await clientWith(respond);
  try {
    for (const replacement of [
      undefined,
      {},
      { subtle: {} },
      {
        subtle: {
          digest: async () => {
            throw new Error('no digest');
          },
        },
      },
    ]) {
      Object.defineProperty(globalThis, 'crypto', {
        value: replacement,
        configurable: true,
      });
      await assert.rejects(
        client.runtimeSourceDownload(view, offer),
        (error) =>
          error instanceof ArtifreeApiError &&
          error.code === 'invalid_response',
        JSON.stringify(replacement),
      );
    }
  } finally {
    Object.defineProperty(globalThis, 'crypto', original);
  }
  assert.equal((await client.runtimeSourceDownload(view, offer)).length, 3);
});

test('A call for a connection that ended carries its own marker', async () => {
  const { client } = await clientWith(() => Response.json({}));
  const other = { ...view, session_id: 'f'.repeat(32) };
  for (const attempt of [
    () => client.runtimeSourceList(other),
    () =>
      client.runtimeSourceRead(other, { path: 'a.txt', offset: 0, length: 1 }),
    () => client.runtimeSourceOffer(other, { kind: 'archive' }),
    () => client.runtimeSourceDownload(other, download().offer),
    () => client.runtimeMetadata(other),
  ]) {
    const error = await attempt().then(
      () => assert.fail('The call must be refused.'),
      (rejection) => rejection,
    );
    assert.ok(error instanceof RuntimeConnectionReplacedError);
    assert.ok(error instanceof ArtifreeApiError);
    assert.equal(error.connectionReplaced, true);
    assert.equal(error.name, 'ArtifreeApiError');
  }
  // A bad request of the client's own is not that marker.
  const plain = new ArtifreeApiError('x', {
    status: 0,
    code: 'invalid_request',
  });
  assert.equal(plain.connectionReplaced, undefined);
});
