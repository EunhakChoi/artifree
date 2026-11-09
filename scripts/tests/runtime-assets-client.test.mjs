import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  ArtifreeApiError,
  ArtifreeClient,
} from '../../packages/client/dist/index.js';
import { parseRuntimeAssetPart } from '../../packages/client/dist/runtime-assets.js';

/*
 * The runtime asset calls (decision 0045, rule 12). The part parser is tested
 * directly. The calls are tested through ArtifreeClient.runtimeCall, the path
 * the frame RPC takes (apps/web/src/artifacts/frame-rpc.ts), with the HTTP
 * answer faked at fetch.
 */

const SESSION = 'e'.repeat(32);
const VIEW_ID = 'd'.repeat(32);
const INIT = 'c'.repeat(64);
const ARTIFACT = `art_${'0'.repeat(32)}`;
const VIEW_ROUTE = `/v1/artifacts/${ARTIFACT}/revisions/3/runtime-views/${VIEW_ID}/1`;
const LIST_URL = `http://127.0.0.1:7410${VIEW_ROUTE}/assets/list`;
const READ_URL = `http://127.0.0.1:7410${VIEW_ROUTE}/assets/read`;

const view = {
  url: `http://127.0.0.1:7411/r/${'b'.repeat(64)}/`,
  enabled: true,
  session_id: SESSION,
  artifact_id: ARTIFACT,
  revision: 3,
  head_revision: 3,
  sdk: 'artifree.app.v1',
  declared: ['assets.read'],
  supported: ['assets.read'],
  granted: ['assets.read'],
  grant_version: 1,
  view_id: VIEW_ID,
  generation: 1,
  initialization: INIT,
  expires_at_ms: 1_000_000,
  lease_ms: 900_000,
};

const ASSET = '0123456789abcdef'.repeat(4);
const OTHER_ASSET = 'f'.repeat(64);
/** SHA-256 of the bytes of "hello": the digest of the whole asset. */
const DIGEST =
  '2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824';
const MAX_PART = 262_144;
const FIVE_MIB = 5 * 1024 * 1024;

/** Base64 of `length` bytes, each one 7. */
function filled(length) {
  return Buffer.alloc(length, 7).toString('base64');
}
const FULL_PART = filled(MAX_PART);

/** A part of ASSET that answers a request for it: the whole of "hello". */
function partOf(overrides = {}) {
  return {
    asset_id: ASSET,
    content_type: 'text/plain',
    offset: 0,
    next_offset: null,
    size: 5,
    sha256: DIGEST,
    content: 'aGVsbG8=',
    ...overrides,
  };
}

/** A request for ASSET from 0, for up to a full part. */
function requestOf(overrides = {}) {
  return { assetId: ASSET, offset: 0, length: MAX_PART, ...overrides };
}

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

const listAssets = (client) =>
  client.runtimeCall(view, (source, frame) =>
    source.listRuntimeAssets(frame, view, undefined),
  );

const readAsset = (client, request) =>
  client.runtimeCall(view, (source, frame) =>
    source.readRuntimeAsset(frame, view, request, undefined),
  );

const refusalOf = (attempt) =>
  attempt.then(
    () => assert.fail('The call must be refused.'),
    (rejection) => rejection,
  );

/** The fields a caller can branch on, of an error from a call. */
const shapeOf = (error) => ({
  status: error.status,
  code: error.code,
  currentRevision: error.currentRevision,
});

const listing = {
  asset_version: 1,
  assets: [
    {
      id: ASSET,
      content_type: 'text/plain',
      size_bytes: 5,
      sha256: DIGEST,
      created_at_ms: 1_700_000_000_000,
    },
  ],
  bytes: 5,
  max_assets: 64,
  max_bytes: 26_214_400,
};

// ---- parsing a part ----

test('A whole part of an asset parses to its bytes, with no next offset', () => {
  assert.deepEqual(parseRuntimeAssetPart(partOf(), requestOf()), {
    assetId: ASSET,
    contentType: 'text/plain',
    offset: 0,
    nextOffset: null,
    size: 5,
    sha256: DIGEST,
    bytes: Uint8Array.from([104, 101, 108, 108, 111]),
  });
});

test('A part that stops before the end names where the next part starts', () => {
  const part = parseRuntimeAssetPart(
    partOf({ next_offset: 4, size: 10, content: filled(4) }),
    requestOf({ length: 4 }),
  );
  assert.equal(part.nextOffset, 4);
  assert.deepEqual([...part.bytes], [7, 7, 7, 7]);
});

for (const contentType of ['text/plain', 'text/csv', 'image/png', 'image/jpeg'])
  test(`A part stored as ${contentType} is accepted`, () => {
    const part = parseRuntimeAssetPart(
      partOf({ content_type: contentType }),
      requestOf(),
    );
    assert.equal(part.contentType, contentType);
  });

test('A part of exactly 262,144 bytes is accepted', () => {
  const part = parseRuntimeAssetPart(
    partOf({ size: MAX_PART + 1, next_offset: MAX_PART, content: FULL_PART }),
    requestOf(),
  );
  assert.equal(part.bytes.length, MAX_PART);
  assert.equal(part.nextOffset, MAX_PART);
});

test('An asset of exactly 5 MiB is accepted', () => {
  const part = parseRuntimeAssetPart(
    partOf({ size: FIVE_MIB, next_offset: MAX_PART, content: FULL_PART }),
    requestOf(),
  );
  assert.equal(part.size, FIVE_MIB);
});

/**
 * Malformed parts: each row changes one thing about the valid part above, and
 * the request is the one the row names (default: the whole valid request).
 */
const refusedParts = [
  ['a sha256 in uppercase hexadecimal', { sha256: DIGEST.toUpperCase() }],
  ['a sha256 of 63 digits', { sha256: DIGEST.slice(1) }],
  ['a sha256 of 65 digits', { sha256: `${DIGEST}0` }],
  [
    'a sha256 with a character that is not hexadecimal',
    { sha256: `g${DIGEST.slice(1)}` },
  ],
  [
    'the media type application/octet-stream',
    { content_type: 'application/octet-stream' },
  ],
  ['the media type image/gif', { content_type: 'image/gif' }],
  ['the media type text/html', { content_type: 'text/html' }],
  ['a media type in uppercase', { content_type: 'TEXT/PLAIN' }],
  [
    'a media type with a parameter',
    { content_type: 'text/plain; charset=utf-8' },
  ],
  ['a size of zero bytes', { size: 0, content: '' }],
  [
    'a size past 5 MiB',
    { size: FIVE_MIB + 1, next_offset: MAX_PART, content: FULL_PART },
  ],
  [
    'a size that is not an integer',
    { size: 1.5, next_offset: 1, content: filled(1) },
    { length: 1 },
  ],
  [
    'a size past the safe integers',
    { size: 2 ** 53, next_offset: MAX_PART, content: FULL_PART },
  ],
  ['an offset other than the one asked', { offset: 1, size: 6 }],
  [
    'a next offset equal to the offset',
    { offset: 4, next_offset: 4, size: 10, content: filled(4) },
    { offset: 4, length: 4 },
  ],
  [
    'a next offset before the offset',
    { offset: 4, next_offset: 2, size: 10, content: filled(4) },
    { offset: 4, length: 4 },
  ],
  [
    'a next offset equal to the size',
    { next_offset: 4, size: 4, content: filled(4) },
    { length: 4 },
  ],
  [
    'a next offset past the size',
    { next_offset: 8, size: 4, content: filled(8) },
    { length: 8 },
  ],
  [
    'a part before the end that is shorter than asked',
    { next_offset: 2, size: 10, content: filled(2) },
    { length: 4 },
  ],
  [
    'a final part that does not reach the size',
    { size: 10, content: filled(5) },
  ],
  [
    'more bytes than the request asked for',
    { content: filled(5) },
    { length: 4 },
  ],
  [
    'more than 262,144 decoded bytes',
    { size: MAX_PART + 1, content: filled(MAX_PART + 1) },
  ],
  [
    'content that is not canonical base64 (wrong length)',
    { content: 'aGVsbG8' },
  ],
  ['content with whitespace', { content: 'aGVs bG8' }],
  ['content with non-zero padding bits', { content: 'aGVsbG9=' }],
  [
    'content with a character outside the base64 alphabet',
    { content: 'aGVs!bG8' },
  ],
  ['an asset ID that is not the one asked', { asset_id: OTHER_ASSET }],
];

for (const [name, overrides, request] of refusedParts)
  test(`A part with ${name} is refused`, () => {
    const part = parseRuntimeAssetPart(partOf(overrides), requestOf(request));
    // A boolean with a fixed message: a wrongly accepted part can hold 256 KiB
    // of bytes, and an assertion would print all of them.
    assert.equal(part === undefined, true, 'The part was accepted.');
  });

// ---- requests the client refuses before anything is sent ----

const invalidReads = [
  [
    'an asset ID in uppercase hexadecimal',
    requestOf({ assetId: 'A'.repeat(64) }),
  ],
  ['an asset ID of 63 digits', requestOf({ assetId: 'a'.repeat(63) })],
  ['an asset ID of 65 digits', requestOf({ assetId: 'a'.repeat(65) })],
  [
    'an asset ID that is not hexadecimal',
    requestOf({ assetId: 'g'.repeat(64) }),
  ],
  ['a length of zero bytes', requestOf({ length: 0 })],
  ['a length past 262,144 bytes', requestOf({ length: MAX_PART + 1 })],
  ['an offset past the safe integers', requestOf({ offset: 2 ** 53 })],
  ['a negative offset', requestOf({ offset: -1 })],
  ['a fractional offset', requestOf({ offset: 1.5 })],
];

for (const [name, request] of invalidReads)
  test(`A read with ${name} is refused before anything is sent`, async () => {
    const { client, requests } = await clientWith(() =>
      Response.json(partOf()),
    );
    const error = await refusalOf(readAsset(client, request));
    assert.ok(error instanceof ArtifreeApiError);
    assert.equal(error.code, 'invalid_request');
    assert.equal(error.status, 0);
    assert.equal(requests.length, 0);
  });

// ---- the routes and bodies the server expects ----

test('A listing is one POST to the view route, carrying only the initialization', async () => {
  const { client, requests } = await clientWith(() => Response.json(listing));
  await listAssets(client);
  assert.deepEqual(requests, [
    { url: LIST_URL, method: 'POST', body: { initialization: INIT } },
  ]);
});

test('A read is one POST to the view route, with the asset ID, offset and length', async () => {
  const { client, requests } = await clientWith(() =>
    Response.json(partOf({ offset: 4, size: 8, content: filled(4) })),
  );
  await readAsset(client, requestOf({ offset: 4, length: 4 }));
  assert.deepEqual(requests, [
    {
      url: READ_URL,
      method: 'POST',
      body: {
        initialization: INIT,
        asset_id: ASSET,
        offset: 4,
        length: 4,
      },
    },
  ]);
});

test('A read resolves to the part the server answered for that request', async () => {
  const { client } = await clientWith(() =>
    Response.json(partOf({ offset: 4, size: 8, content: filled(4) })),
  );
  const part = await readAsset(client, requestOf({ offset: 4, length: 4 }));
  assert.deepEqual(part, {
    assetId: ASSET,
    contentType: 'text/plain',
    offset: 4,
    nextOffset: null,
    size: 8,
    sha256: DIGEST,
    bytes: Uint8Array.from([7, 7, 7, 7]),
  });
});

test('A listing resolves to the asset space the server stated', async () => {
  const { client } = await clientWith(() => Response.json(listing));
  assert.deepEqual(await listAssets(client), listing);
});

test('A listing whose IDs are not in ascending order is refused', async () => {
  const { client } = await clientWith(() =>
    Response.json({
      ...listing,
      asset_version: 2,
      assets: [{ ...listing.assets[0], id: OTHER_ASSET }, listing.assets[0]],
      bytes: 10,
    }),
  );
  const error = await refusalOf(listAssets(client));
  assert.equal(error.code, 'invalid_response');
});

test('A listing whose total is not the sum of its assets is refused', async () => {
  const { client } = await clientWith(() =>
    Response.json({ ...listing, bytes: 6 }),
  );
  const error = await refusalOf(listAssets(client));
  assert.equal(error.code, 'invalid_response');
});

test('A refused part is invalid_response over HTTP, without echoing its digest', async () => {
  const { client } = await clientWith(() =>
    Response.json(partOf({ size: 10, content: filled(5) })),
  );
  const error = await refusalOf(readAsset(client, requestOf()));
  assert.ok(error instanceof ArtifreeApiError);
  assert.equal(error.status, 200);
  assert.equal(error.code, 'invalid_response');
  assert.ok(!String(error.message).includes(DIGEST));
});

// ---- HTTP refusals take the shape the source client gives them ----

const httpRefusals = [
  {
    status: 404,
    body: { error: { code: 'not_found', message: 'asset not found' } },
    expected: { status: 404, code: 'not_found', currentRevision: undefined },
  },
  {
    status: 403,
    body: {
      error: {
        code: 'forbidden',
        message: 'the runtime connection has not granted this capability',
      },
    },
    expected: { status: 403, code: 'forbidden', currentRevision: undefined },
  },
  {
    status: 409,
    body: {
      error: {
        code: 'revision_conflict',
        message: 'the view is older than the current source',
        current_revision: 9,
      },
    },
    expected: { status: 409, code: 'revision_conflict', currentRevision: 9 },
  },
  {
    status: 416,
    headers: { 'content-range': 'bytes */5' },
    body: {
      error: {
        code: 'range_not_satisfiable',
        message: 'the range starts past the end of the asset',
      },
    },
    expected: {
      status: 416,
      code: 'range_not_satisfiable',
      currentRevision: undefined,
    },
  },
];

for (const refusal of httpRefusals)
  test(`An HTTP ${refusal.status} on an asset read is the error a source read gives`, async () => {
    const { client } = await clientWith(() =>
      Response.json(refusal.body, {
        status: refusal.status,
        headers: refusal.headers,
      }),
    );
    const assetError = await refusalOf(readAsset(client, requestOf()));
    const sourceError = await refusalOf(
      client.runtimeSourceRead(view, { path: 'a.txt', offset: 0, length: 4 }),
    );
    assert.ok(assetError instanceof ArtifreeApiError);
    assert.deepEqual(shapeOf(assetError), refusal.expected);
    assert.deepEqual(shapeOf(sourceError), refusal.expected);
  });
