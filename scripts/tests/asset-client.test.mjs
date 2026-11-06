import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtemp, rm } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import {
  ArtifreeClient,
  createUser,
  setUserDisabled,
  getAsset,
  listAssets,
  readAsset,
} from '../../packages/client/dist/index.js';
import { buildServer, startServer } from '../lib/server-process.mjs';

const ARTIFACT = `art_${'a'.repeat(32)}`;
const ID = 'b'.repeat(64);
const BYTES = Buffer.from('\ufeffhello 한글\r\n');
const ASSET = {
  id: ID,
  content_type: 'text/plain',
  size_bytes: BYTES.length,
  sha256: createHash('sha256').update(BYTES).digest('hex'),
  created_at_ms: 12,
};
const META = { asset_version: 2, asset: ASSET };
const LIST = {
  asset_version: 2,
  assets: [ASSET],
  bytes: BYTES.length,
  max_assets: 64,
  max_bytes: 25 * 1024 * 1024,
};
function clientWith(respond, clock) {
  return new ArtifreeClient({
    baseUrl: 'http://localhost:7410',
    accessToken: 'token',
    fetch: respond,
    ...(clock === undefined ? {} : { clock }),
  });
}
const invalid = { code: 'invalid_response' };

test('asset list/metadata/content use bounded authenticated no-store redirect-refusing reads', async () => {
  const requests = [];
  const client = clientWith(async (url, init) => {
    requests.push({ url, init });
    if (url.endsWith('/content'))
      return new Response(BYTES, { headers: { 'content-type': 'text/plain' } });
    return Response.json(url.endsWith(`/assets/${ID}`) ? META : LIST);
  });
  assert.deepEqual(await listAssets(client, ARTIFACT), LIST);
  assert.deepEqual(await getAsset(client, ARTIFACT, ID), META);
  const read = await readAsset(client, ARTIFACT, ID);
  assert.deepEqual(Buffer.from(read.bytes), BYTES);
  assert.deepEqual(read.asset, ASSET);
  assert.equal(requests.length, 4);
  for (const { init } of requests) {
    assert.equal(init.method, 'GET');
    assert.equal(init.redirect, 'error');
    assert.equal(init.cache, 'no-store');
    assert.equal(init.headers.authorization, 'Bearer token');
  }
});

const badLists = [
  ['unsafe version', { asset_version: 2 ** 53 }],
  ['fractional version', { asset_version: 1.5 }],
  ['negative version', { asset_version: -1 }],
  ['occupied version zero', { asset_version: 0 }],
  ['wrong total', { bytes: BYTES.length + 1 }],
  ['wrong quota', { max_assets: 65 }],
  ['duplicate id', { assets: [ASSET, ASSET], bytes: 2 * BYTES.length }],
  ['too many entries', { assets: Array(65).fill(ASSET) }],
  ['unknown type', { assets: [{ ...ASSET, content_type: 'text/html' }] }],
  [
    'oversized asset',
    { assets: [{ ...ASSET, size_bytes: 5 * 1024 * 1024 + 1 }] },
  ],
  ['invalid time', { assets: [{ ...ASSET, created_at_ms: 1.5 }] }],
  ['invalid digest', { assets: [{ ...ASSET, sha256: 'z'.repeat(64) }] }],
];
for (const [name, change] of badLists) {
  test(`asset list refuses ${name}`, async () => {
    const client = clientWith(async () =>
      Response.json({ ...LIST, ...change }),
    );
    await assert.rejects(listAssets(client, ARTIFACT), invalid);
  });
}

test('an empty space keeps its version and historical image metadata remains readable', async () => {
  const empty = { ...LIST, assets: [], bytes: 0 };
  assert.deepEqual(
    await listAssets(
      clientWith(async () => Response.json(empty)),
      ARTIFACT,
    ),
    empty,
  );
  const historical = {
    ...META,
    asset: { ...ASSET, content_type: 'image/png' },
  };
  assert.deepEqual(
    await getAsset(
      clientWith(async () => Response.json(historical)),
      ARTIFACT,
      ID,
    ),
    historical,
  );
});

test('metadata must echo the requested asset id before a content request', async () => {
  let requests = 0;
  const client = clientWith(async () => {
    requests += 1;
    return Response.json({ ...META, asset: { ...ASSET, id: 'c'.repeat(64) } });
  });
  await assert.rejects(readAsset(client, ARTIFACT, ID), invalid);
  assert.equal(requests, 1);
});

for (const [name, bytes, contentType] of [
  ['wrong MIME', BYTES, 'text/csv'],
  ['truncated bytes', BYTES.subarray(0, -1), 'text/plain'],
  ['wrong digest', Buffer.alloc(BYTES.length, 97), 'text/plain'],
]) {
  test(`asset content refuses ${name}`, async () => {
    const client = clientWith(async (url) =>
      url.endsWith('/content')
        ? new Response(bytes, { headers: { 'content-type': contentType } })
        : Response.json(META),
    );
    await assert.rejects(readAsset(client, ARTIFACT, ID), invalid);
  });
}

function oversizedResponse(size, status, onCancel) {
  return new Response(
    new ReadableStream({
      start(controller) {
        controller.enqueue(new Uint8Array(size));
        // Leave one queued byte so the limit guard must cancel before EOF.
        controller.enqueue(new Uint8Array(1));
        controller.close();
      },
      cancel: onCancel,
    }),
    { status },
  );
}
for (const operation of ['list', 'metadata', 'content']) {
  test(`${operation} cancels a successful oversized stream before EOF`, async () => {
    let cancelled = false;
    const client = clientWith(async (url) => {
      if (operation === 'content' && !url.endsWith('/content'))
        return Response.json(META);
      return oversizedResponse(
        operation === 'content' ? BYTES.length + 1 : 65_537,
        200,
        () => {
          cancelled = true;
        },
      );
    });
    const call =
      operation === 'list'
        ? listAssets(client, ARTIFACT)
        : operation === 'metadata'
          ? getAsset(client, ARTIFACT, ID)
          : readAsset(client, ARTIFACT, ID);
    await assert.rejects(call, invalid);
    assert.equal(cancelled, true);
  });
  test(`${operation} cancels errors over 4 KiB without consuming them whole`, async () => {
    let cancelled = false;
    const client = clientWith(async (url) => {
      if (operation === 'content' && !url.endsWith('/content'))
        return Response.json(META);
      return oversizedResponse(4097, 403, () => {
        cancelled = true;
      });
    });
    const call =
      operation === 'list'
        ? listAssets(client, ARTIFACT)
        : operation === 'metadata'
          ? getAsset(client, ARTIFACT, ID)
          : readAsset(client, ARTIFACT, ID);
    await assert.rejects(call, { code: 'invalid_response', status: 403 });
    assert.equal(cancelled, true);
  });
}

test('invalid identifiers are refused before dispatch', async () => {
  let requests = 0;
  const client = clientWith(async () => {
    requests += 1;
    return Response.json(META);
  });
  await assert.rejects(getAsset(client, ARTIFACT, '../content'), {
    code: 'invalid_request',
  });
  await assert.rejects(listAssets(client, `${ARTIFACT}/../users`), {
    code: 'invalid_request',
  });
  assert.equal(requests, 0);
});

test('deletion between metadata and content is a 404 with no retry', async () => {
  let requests = 0;
  const client = clientWith(async (url) => {
    requests += 1;
    return url.endsWith('/content')
      ? Response.json(
          { error: { code: 'not_found', message: 'asset not found' } },
          { status: 404 },
        )
      : Response.json(META);
  });
  await assert.rejects(readAsset(client, ARTIFACT, ID), {
    code: 'not_found',
    status: 404,
  });
  assert.equal(requests, 2);
});

test('byte count is checked even when the digest matches the shorter body', async () => {
  const shorter = BYTES.subarray(0, -1);
  const metadata = {
    ...META,
    asset: {
      ...ASSET,
      sha256: createHash('sha256').update(shorter).digest('hex'),
    },
  };
  const client = clientWith(async (url) =>
    url.endsWith('/content')
      ? new Response(shorter, { headers: { 'content-type': 'text/plain' } })
      : Response.json(metadata),
  );
  await assert.rejects(readAsset(client, ARTIFACT, ID), invalid);
});

test('native Fetch refuses same-origin redirects without contacting the target', async () => {
  let targets = 0;
  let contentOnly = false;
  const server = createServer((request, response) => {
    if (
      contentOnly &&
      request.url === `/v1/artifacts/${ARTIFACT}/assets/${ID}`
    ) {
      response.writeHead(200, { 'content-type': 'application/json' });
      response.end(JSON.stringify(META));
    } else if (request.url === '/target') {
      targets += 1;
      response.end('{}');
    } else {
      response.writeHead(302, { location: '/target' });
      response.end();
    }
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  try {
    const client = new ArtifreeClient({
      baseUrl: `http://127.0.0.1:${server.address().port}`,
      accessToken: 'token',
    });
    await assert.rejects(listAssets(client, ARTIFACT), {
      code: 'network_error',
    });
    await assert.rejects(getAsset(client, ARTIFACT, ID), {
      code: 'network_error',
    });
    contentOnly = true;
    await assert.rejects(readAsset(client, ARTIFACT, ID), {
      code: 'network_error',
    });
    assert.equal(targets, 0);
  } finally {
    server.closeAllConnections();
    await new Promise((resolve) => server.close(resolve));
  }
});

test('read-only asset client reads exact text from the real server and denies foreign scope', async () => {
  const root = fileURLToPath(new URL('../../', import.meta.url));
  const binary = buildServer(root);
  const scratch = await mkdtemp(join(tmpdir(), 'artifree-asset-client-'));
  const server = await startServer(binary, scratch);
  try {
    const client = new ArtifreeClient({
      baseUrl: server.api_url,
      accessToken: server.token,
    });
    const created = await client.createArtifact(
      {
        title: 'Asset read',
        files: [
          { path: 'index.html', content: '<h1>Read</h1>', encoding: 'utf8' },
        ],
      },
      { idempotencyKey: 'asset-read-create' },
    );
    const id = created.artifact.id;
    const uploaded = await fetch(
      `${server.api_url}/v1/artifacts/${id}/assets`,
      {
        method: 'POST',
        headers: {
          authorization: `Bearer ${server.token}`,
          'content-type': 'application/json',
          'idempotency-key': 'asset-read-upload',
        },
        body: JSON.stringify({
          base_asset_version: 0,
          content_type: 'text/plain',
          content_base64: BYTES.toString('base64'),
        }),
      },
    );
    assert.equal(uploaded.status, 201);
    const receipt = await uploaded.json();
    const listed = await listAssets(client, id);
    assert.equal(listed.asset_version, 1);
    assert.deepEqual(listed.assets, [receipt.asset]);
    assert.deepEqual(
      Buffer.from((await readAsset(client, id, receipt.asset.id)).bytes),
      BYTES,
    );
    const other = await client.createArtifact(
      {
        title: 'Other',
        files: [
          { path: 'index.html', content: '<h1>Other</h1>', encoding: 'utf8' },
        ],
      },
      { idempotencyKey: 'asset-read-other' },
    );
    await assert.rejects(
      readAsset(client, other.artifact.id, receipt.asset.id),
      { code: 'not_found', status: 404 },
    );
    const alice = await createUser(client, { label: 'Alice' });
    const token = await client.createToken({
      name: 'Alice asset read',
      user_id: alice.id,
    });
    const foreign = new ArtifreeClient({
      baseUrl: server.api_url,
      accessToken: token.secret,
    });
    await assert.rejects(listAssets(foreign, id), {
      code: 'not_found',
      status: 404,
    });
    await assert.rejects(getAsset(foreign, id, receipt.asset.id), {
      code: 'not_found',
      status: 404,
    });
    await assert.rejects(readAsset(foreign, id, receipt.asset.id), {
      code: 'not_found',
      status: 404,
    });
    await setUserDisabled(client, alice.id, true);
    await assert.rejects(listAssets(foreign, id), {
      code: 'unauthenticated',
      status: 401,
    });
  } finally {
    await server.stop();
    await rm(scratch, { recursive: true, force: true });
  }
});

for (const [name, bodySize, status] of [
  ['metadata', 65_536, 200],
  ['error', 4096, 404],
]) {
  test(`accepts exactly ${bodySize} bytes of ${name} JSON`, async () => {
    const value =
      status === 200
        ? LIST
        : { error: { code: 'not_found', message: 'missing' } };
    const text = JSON.stringify(value);
    const body = text.padEnd(bodySize, ' ');
    const client = clientWith(async () => new Response(body, { status }));
    if (status === 200)
      assert.deepEqual(await listAssets(client, ARTIFACT), LIST);
    else
      await assert.rejects(listAssets(client, ARTIFACT), {
        code: 'not_found',
        status: 404,
      });
  });
}

test('raw content accepts exactly 5 MiB and verifies its digest', async () => {
  const bytes = Buffer.alloc(5 * 1024 * 1024, 97);
  const metadata = {
    ...META,
    asset: {
      ...ASSET,
      size_bytes: bytes.length,
      sha256: createHash('sha256').update(bytes).digest('hex'),
    },
  };
  const client = clientWith(async (url) =>
    url.endsWith('/content')
      ? new Response(bytes, { headers: { 'content-type': 'text/plain' } })
      : Response.json(metadata),
  );
  const result = await readAsset(client, ARTIFACT, ID);
  assert.deepEqual(Buffer.from(result.bytes), bytes);
});

test('stalled content cancels through the injected transport clock', async () => {
  let tick;
  let waiting;
  const entered = new Promise((resolve) => {
    waiting = resolve;
  });
  let cancelled = false;
  const clock = {
    after(callback) {
      tick = callback;
      return () => {
        tick = undefined;
      };
    },
  };
  const client = clientWith(
    async (url) =>
      url.endsWith('/content')
        ? new Response(
            new ReadableStream({
              start() {
                waiting();
              },
              cancel() {
                cancelled = true;
              },
            }),
          )
        : Response.json(META),
    clock,
  );
  const read = readAsset(client, ARTIFACT, ID);
  await entered;
  await new Promise((resolve) => setImmediate(resolve));
  // A fired timer leaves the clock before invoking its callback.
  const fire = tick;
  tick = undefined;
  fire();
  await assert.rejects(read, { code: 'network_error' });
  assert.equal(cancelled, true);
  assert.equal(tick, undefined);
});

test('aborting a pending content stream cancels it without returning bytes', async () => {
  const controller = new AbortController();
  let waiting;
  const entered = new Promise((resolve) => {
    waiting = resolve;
  });
  let cancelled = false;
  const client = clientWith(async (url) =>
    url.endsWith('/content')
      ? new Response(
          new ReadableStream({
            start() {
              waiting();
            },
            cancel() {
              cancelled = true;
            },
          }),
        )
      : Response.json(META),
  );
  const read = readAsset(client, ARTIFACT, ID, { signal: controller.signal });
  await entered;
  controller.abort();
  await assert.rejects(read, { code: 'network_error' });
  assert.equal(cancelled, true);
});

test('missing Web Crypto refuses content instead of bypassing digest verification', async () => {
  const descriptor = Object.getOwnPropertyDescriptor(globalThis, 'crypto');
  Object.defineProperty(globalThis, 'crypto', {
    configurable: true,
    value: undefined,
  });
  try {
    const client = clientWith(async (url) =>
      url.endsWith('/content')
        ? new Response(BYTES, { headers: { 'content-type': 'text/plain' } })
        : Response.json(META),
    );
    await assert.rejects(readAsset(client, ARTIFACT, ID), invalid);
  } finally {
    Object.defineProperty(globalThis, 'crypto', descriptor);
  }
});

/** HWM zero prevents pulling again before the consumer requests another byte. */
function byteStream(bytes, reuse) {
  const shared = new Uint8Array(1);
  let offset = 0;
  return new ReadableStream(
    {
      pull(controller) {
        if (offset === bytes.length) {
          controller.close();
          return;
        }
        const chunk = reuse ? shared : new Uint8Array(1);
        chunk[0] = bytes[offset];
        offset += 1;
        controller.enqueue(chunk);
      },
    },
    { highWaterMark: 0 },
  );
}

test('raw reads preserve bytes when a source reuses its one-byte view across buffer growth', async () => {
  const bytes = Buffer.from(
    Uint8Array.from({ length: 256 * 1024 }, (_, index) => 97 + (index % 26)),
  );
  const metadata = {
    ...META,
    asset: {
      ...ASSET,
      size_bytes: bytes.length,
      sha256: createHash('sha256').update(bytes).digest('hex'),
    },
  };
  const client = clientWith(async (url) =>
    url.endsWith('/content')
      ? new Response(byteStream(bytes, true), {
          headers: { 'content-type': 'text/plain' },
        })
      : Response.json(metadata),
  );
  let result;
  await assert.doesNotReject(async () => {
    result = await readAsset(client, ARTIFACT, ID);
  });
  assert.deepEqual(Buffer.from(result.bytes), bytes);
});

test('a maximum-size raw read accepts millions of distinct one-byte chunks', async () => {
  const bytes = Buffer.alloc(5 * 1024 * 1024, 97);
  const metadata = {
    ...META,
    asset: {
      ...ASSET,
      size_bytes: bytes.length,
      sha256: createHash('sha256').update(bytes).digest('hex'),
    },
  };
  const client = clientWith(async (url) =>
    url.endsWith('/content')
      ? new Response(byteStream(bytes, false), {
          headers: { 'content-type': 'text/plain' },
        })
      : Response.json(metadata),
  );
  assert.deepEqual(
    Buffer.from((await readAsset(client, ARTIFACT, ID)).bytes),
    bytes,
  );
});

test('metadata and error JSON also survive a reused one-byte source view', async () => {
  const listBytes = new TextEncoder().encode(JSON.stringify(LIST));
  const client = clientWith(
    async () => new Response(byteStream(listBytes, true)),
  );
  assert.deepEqual(await listAssets(client, ARTIFACT), LIST);
  const failure = { error: { code: 'not_found', message: 'missing' } };
  const errorBytes = new TextEncoder().encode(JSON.stringify(failure));
  const refused = clientWith(
    async () => new Response(byteStream(errorBytes, true), { status: 404 }),
  );
  await assert.rejects(listAssets(refused, ARTIFACT), {
    code: 'not_found',
    status: 404,
  });
});
