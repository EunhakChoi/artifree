import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { test } from 'node:test';
import {
  ArtifreeApiError,
  ArtifreeClient,
} from '../../packages/client/dist/index.js';

/*
 * The runtime asset writes (decision 0045, rule 12): an upload and a delete on
 * behalf of a frame that holds the assets.write grant. As in
 * runtime-assets-client.test.mjs, the calls go through ArtifreeClient.runtimeCall,
 * the path the frame RPC takes (apps/web/src/artifacts/frame-rpc.ts), and the
 * HTTP answer is faked at fetch. A call the client refuses before sending must
 * never reach fetch, so those refusals also assert that no request was sent.
 * A receipt is judged only after its request was answered, so those rows
 * assert the error and not the absence of a request.
 */

const SESSION = 'e'.repeat(32);
const VIEW_ID = 'd'.repeat(32);
const INIT = 'c'.repeat(64);
const ARTIFACT = `art_${'0'.repeat(32)}`;
const VIEW_ROUTE = `/v1/artifacts/${ARTIFACT}/revisions/3/runtime-views/${VIEW_ID}/1`;
const UPLOAD_URL = `http://127.0.0.1:7410${VIEW_ROUTE}/assets/upload`;
const DELETE_URL = `http://127.0.0.1:7410${VIEW_ROUTE}/assets/delete`;

const view = {
  url: `http://127.0.0.1:7411/r/${'b'.repeat(64)}/`,
  enabled: true,
  session_id: SESSION,
  artifact_id: ARTIFACT,
  revision: 3,
  head_revision: 3,
  sdk: 'artifree.app.v1',
  declared: ['assets.write'],
  supported: ['assets.write'],
  granted: ['assets.write'],
  grant_version: 1,
  view_id: VIEW_ID,
  generation: 1,
  initialization: INIT,
  expires_at_ms: 1_000_000,
  lease_ms: 900_000,
};

const ASSET = '0123456789abcdef'.repeat(4);
const OTHER_ASSET = 'f'.repeat(64);
/** SHA-256 of the bytes of "hello", written out rather than computed. */
const DIGEST =
  '2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824';
/** DIGEST with its last digit changed: a digest the bytes sent do not have. */
const NEAR_DIGEST =
  '2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9825';
const CREATED_AT_MS = 1_700_000_000_000;
const RETRY_ID = 'retry-0001';
/** The asset space version the frame last observed; a write moves it by one. */
const BASE = 3;
const MAX_UPLOAD_BYTES = 1_048_576;

/** The bytes of "hello", as a new copy each time. */
const hello = () => Uint8Array.from([104, 101, 108, 108, 111]);

/** What a frame asks to upload: "hello" as text/plain at the base version. */
function uploadInput(overrides = {}) {
  return {
    retryId: RETRY_ID,
    baseAssetVersion: BASE,
    contentType: 'text/plain',
    bytes: hello(),
    ...overrides,
  };
}

/** What a frame asks to delete: the asset, at the base version. */
function deleteInput(overrides = {}) {
  return {
    assetId: ASSET,
    retryId: RETRY_ID,
    baseAssetVersion: BASE,
    ...overrides,
  };
}

/** The asset the server states for `uploadInput()`, unchanged. */
const UPLOADED = {
  id: ASSET,
  content_type: 'text/plain',
  size_bytes: 5,
  sha256: DIGEST,
  created_at_ms: CREATED_AT_MS,
};

/** The receipt the server answers `uploadInput()` with, with changes made. */
function uploadReceipt(assetChanges = {}, receiptChanges = {}) {
  return {
    asset: { ...UPLOADED, ...assetChanges },
    asset_version: BASE + 1,
    replayed: false,
    ...receiptChanges,
  };
}

/** The receipt the server answers `deleteInput()` with, with changes made. */
function deleteReceipt(changes = {}) {
  return {
    asset_id: ASSET,
    asset_version: BASE + 1,
    deleted: true,
    replayed: false,
    ...changes,
  };
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

const upload = (client, input) =>
  client.runtimeCall(view, (source, frame) =>
    source.uploadRuntimeAsset(frame, view, input, undefined),
  );

const deleteAsset = (client, input) =>
  client.runtimeCall(view, (source, frame) =>
    source.deleteRuntimeAsset(frame, view, input, undefined),
  );

const refusalOf = (attempt) =>
  attempt.then(
    () => assert.fail('The call must be refused.'),
    (rejection) => rejection,
  );

/** A JSON answer with `status`, as the server gives one. */
const answer = (body, status = 200) => Response.json(body, { status });

/**
 * Runs `run` with `globalThis.crypto` replaced by `replacement`, then restores
 * the original property, even when `run` fails.
 */
async function withCrypto(replacement, run) {
  const original = Object.getOwnPropertyDescriptor(globalThis, 'crypto');
  Object.defineProperty(globalThis, 'crypto', {
    value: replacement,
    configurable: true,
    writable: true,
  });
  try {
    return await run();
  } finally {
    if (original === undefined) delete globalThis.crypto;
    else Object.defineProperty(globalThis, 'crypto', original);
  }
}

// ---- receipts that match the call ----

test('An upload resolves to the receipt the server states for the bytes sent', async () => {
  const { client } = await clientWith(() => answer(uploadReceipt()));
  assert.deepEqual(await upload(client, uploadInput()), {
    asset: UPLOADED,
    asset_version: BASE + 1,
    replayed: false,
  });
});

test('A replayed upload resolves with replayed set', async () => {
  const { client } = await clientWith(() =>
    answer(uploadReceipt({}, { replayed: true })),
  );
  assert.deepEqual(await upload(client, uploadInput()), {
    asset: UPLOADED,
    asset_version: BASE + 1,
    replayed: true,
  });
});

test('A delete that removed the asset resolves with the version one past the base', async () => {
  const { client } = await clientWith(() => answer(deleteReceipt()));
  assert.deepEqual(await deleteAsset(client, deleteInput()), {
    asset_id: ASSET,
    asset_version: BASE + 1,
    deleted: true,
    replayed: false,
  });
});

test('A delete of an asset the artifact no longer has resolves with deleted false and the version unchanged', async () => {
  const { client } = await clientWith(() =>
    answer(deleteReceipt({ asset_version: BASE, deleted: false })),
  );
  assert.deepEqual(await deleteAsset(client, deleteInput()), {
    asset_id: ASSET,
    asset_version: BASE,
    deleted: false,
    replayed: false,
  });
});

// ---- the routes and bodies the server expects ----

test('An upload is one POST to the upload route, with its retry identity, base, media type and bytes in base64', async () => {
  const { client, requests } = await clientWith(() => answer(uploadReceipt()));
  await upload(client, uploadInput());
  assert.deepEqual(requests, [
    {
      url: UPLOAD_URL,
      method: 'POST',
      body: {
        initialization: INIT,
        retry_id: RETRY_ID,
        base_asset_version: BASE,
        content_type: 'text/plain',
        content_base64: 'aGVsbG8=',
      },
    },
  ]);
});

test('A delete is one POST to the delete route, with its asset ID, retry identity and base', async () => {
  const { client, requests } = await clientWith(() => answer(deleteReceipt()));
  await deleteAsset(client, deleteInput());
  assert.deepEqual(requests, [
    {
      url: DELETE_URL,
      method: 'POST',
      body: {
        initialization: INIT,
        asset_id: ASSET,
        retry_id: RETRY_ID,
        base_asset_version: BASE,
      },
    },
  ]);
});

// ---- the bounds that are inclusive, and the admitted media types ----

for (const [name, length] of [
  ['one byte', 1],
  ['exactly 1 MiB', MAX_UPLOAD_BYTES],
])
  test(`An upload of ${name} is sent, and the bytes sent are the bytes given`, async () => {
    const bytes = new Uint8Array(length).fill(7);
    // The digest comes from node:crypto, not from the Web Crypto path under test.
    const digest = createHash('sha256').update(bytes).digest('hex');
    const { client, requests } = await clientWith(() =>
      answer(uploadReceipt({ size_bytes: length, sha256: digest })),
    );
    const receipt = await upload(client, uploadInput({ bytes }));
    assert.equal(receipt.asset.size_bytes, length);
    assert.equal(requests.length, 1, 'The upload was not sent once.');
    assert.equal(
      Buffer.from(requests[0].body.content_base64, 'base64').equals(bytes),
      true,
      'The bytes sent are not the bytes given.',
    );
  });

for (const contentType of ['text/plain', 'text/csv', 'image/png', 'image/jpeg'])
  test(`An upload declared as ${contentType} is sent`, async () => {
    const { client } = await clientWith(() =>
      answer(uploadReceipt({ content_type: contentType })),
    );
    const receipt = await upload(client, uploadInput({ contentType }));
    assert.equal(receipt.asset.content_type, contentType);
  });

for (const [name, retryId] of [
  ['8', 'abcdefgh'],
  ['36', 'a'.repeat(36)],
])
  test(`An upload with a retry identity of ${name} characters is sent`, async () => {
    const { client, requests } = await clientWith(() =>
      answer(uploadReceipt()),
    );
    await upload(client, uploadInput({ retryId }));
    assert.equal(requests.length, 1, 'The upload was not sent once.');
  });

test('An upload from the highest base version the client accepts is sent and moves to the safe-integer limit', async () => {
  const { client, requests } = await clientWith(() =>
    answer(uploadReceipt({}, { asset_version: 2 ** 53 - 1 })),
  );
  const receipt = await upload(
    client,
    uploadInput({ baseAssetVersion: 2 ** 53 - 2 }),
  );
  assert.equal(receipt.asset_version, 2 ** 53 - 1);
  assert.equal(requests.length, 1, 'The upload was not sent once.');
});

// ---- requests the client refuses before anything is sent ----

const badRetryIds = [
  ['a retry identity of 7 characters', { retryId: 'a'.repeat(7) }],
  ['a retry identity of 37 characters', { retryId: 'a'.repeat(37) }],
  ['a retry identity with a space', { retryId: 'retry 0001' }],
  ['a retry identity with a dot', { retryId: 'retry.0001' }],
  ['a retry identity with a letter outside ASCII', { retryId: 'rétry-0001' }],
];

const badBases = [
  [
    'a base version at the safe-integer limit',
    { baseAssetVersion: 2 ** 53 - 1 },
  ],
  ['a negative base version', { baseAssetVersion: -1 }],
  ['a fractional base version', { baseAssetVersion: 1.5 }],
];

const refusedUploads = [
  ...badRetryIds,
  ...badBases,
  [
    'the media type application/octet-stream',
    { contentType: 'application/octet-stream' },
  ],
  ['the media type image/gif', { contentType: 'image/gif' }],
  ['the media type text/html', { contentType: 'text/html' }],
  [
    'the media type text/plain with a parameter',
    { contentType: 'text/plain; charset=utf-8' },
  ],
  ['the media type TEXT/PLAIN in uppercase', { contentType: 'TEXT/PLAIN' }],
  ['no bytes', { bytes: new Uint8Array(0) }],
  ['1 MiB plus one byte', { bytes: new Uint8Array(MAX_UPLOAD_BYTES + 1) }],
  ['bytes that are not a Uint8Array', { bytes: 'hello' }],
];

const refusedDeletes = [
  ['an asset ID in uppercase hexadecimal', { assetId: 'A'.repeat(64) }],
  ['an asset ID of 63 digits', { assetId: 'a'.repeat(63) }],
  ['an asset ID of 65 digits', { assetId: 'a'.repeat(65) }],
  ['an asset ID that is not hexadecimal', { assetId: 'g'.repeat(64) }],
  ...badRetryIds,
  ...badBases,
];

for (const [name, overrides] of refusedUploads)
  test(`An upload with ${name} is refused before anything is sent`, async () => {
    const { client, requests } = await clientWith(() =>
      answer(uploadReceipt()),
    );
    const error = await refusalOf(upload(client, uploadInput(overrides)));
    assert.ok(error instanceof ArtifreeApiError);
    assert.equal(error.code, 'invalid_request');
    assert.equal(error.status, 0);
    assert.equal(requests.length, 0, 'A request was sent.');
  });

for (const [name, overrides] of refusedDeletes)
  test(`A delete with ${name} is refused before anything is sent`, async () => {
    const { client, requests } = await clientWith(() =>
      answer(deleteReceipt()),
    );
    const error = await refusalOf(deleteAsset(client, deleteInput(overrides)));
    assert.ok(error instanceof ArtifreeApiError);
    assert.equal(error.code, 'invalid_request');
    assert.equal(error.status, 0);
    assert.equal(requests.length, 0, 'A request was sent.');
  });

// ---- a receipt that does not match the call is refused ----

// Each row changes one field of a valid receipt. For the asset ID of an upload
// the client can check only the shape: it has no ID to compare before the call.
const corruptedUploadReceipts = [
  [
    'an asset ID in uppercase hexadecimal',
    uploadReceipt({ id: ASSET.toUpperCase() }),
  ],
  ['an asset ID of 63 digits', uploadReceipt({ id: ASSET.slice(1) })],
  [
    'a media type other than the one sent',
    uploadReceipt({ content_type: 'text/csv' }),
  ],
  ['a size other than the bytes sent', uploadReceipt({ size_bytes: 6 })],
  [
    'a digest other than the bytes sent',
    uploadReceipt({ sha256: NEAR_DIGEST }),
  ],
  ['a creation time that is not a count', uploadReceipt({ created_at_ms: -1 })],
  [
    'a version past the base plus one',
    uploadReceipt({}, { asset_version: BASE + 2 }),
  ],
  ['a version equal to the base', uploadReceipt({}, { asset_version: BASE })],
  [
    'a replayed flag that is not a boolean',
    uploadReceipt({}, { replayed: 'false' }),
  ],
];

const corruptedDeleteReceipts = [
  [
    'an asset ID other than the one asked',
    deleteReceipt({ asset_id: OTHER_ASSET }),
  ],
  [
    'a version equal to the base after a removal',
    deleteReceipt({ asset_version: BASE }),
  ],
  ['a version two past the base', deleteReceipt({ asset_version: BASE + 2 })],
  [
    'a deleted flag of false while the version moved',
    deleteReceipt({ deleted: false }),
  ],
  ['a deleted flag that is not a boolean', deleteReceipt({ deleted: 'true' })],
  [
    'a replayed flag that is not a boolean',
    deleteReceipt({ replayed: 'false' }),
  ],
];

for (const [name, receipt] of corruptedUploadReceipts)
  test(`An upload receipt with ${name} is refused`, async () => {
    const { client } = await clientWith(() => answer(receipt));
    const error = await refusalOf(upload(client, uploadInput()));
    assert.ok(error instanceof ArtifreeApiError);
    assert.equal(error.code, 'invalid_response');
    assert.equal(error.status, 200);
  });

for (const [name, receipt] of corruptedDeleteReceipts)
  test(`A delete receipt with ${name} is refused`, async () => {
    const { client } = await clientWith(() => answer(receipt));
    const error = await refusalOf(deleteAsset(client, deleteInput()));
    assert.ok(error instanceof ArtifreeApiError);
    assert.equal(error.code, 'invalid_response');
    assert.equal(error.status, 200);
  });

// ---- a conflict keeps the version the server stated ----

const CONFLICT = {
  error: {
    code: 'asset_conflict',
    message: 'the asset space has moved on',
    current_asset_version: 7,
  },
};

test('An upload answered 409 asset_conflict keeps the version the server stated in its body', async () => {
  const { client } = await clientWith(() => answer(CONFLICT, 409));
  const error = await refusalOf(upload(client, uploadInput()));
  assert.ok(error instanceof ArtifreeApiError);
  assert.equal(error.status, 409);
  assert.equal(error.code, 'asset_conflict');
  assert.deepEqual(error.body, CONFLICT);
});

test('A delete answered 409 asset_conflict keeps the version the server stated in its body', async () => {
  const { client } = await clientWith(() => answer(CONFLICT, 409));
  const error = await refusalOf(deleteAsset(client, deleteInput()));
  assert.ok(error instanceof ArtifreeApiError);
  assert.equal(error.status, 409);
  assert.equal(error.code, 'asset_conflict');
  assert.deepEqual(error.body, CONFLICT);
});

// ---- the digest is computed before anything is sent ----

const unusableDigests = [
  ['crypto.subtle is missing', {}],
  ['crypto is missing', undefined],
  [
    'the digest fails',
    {
      subtle: { digest: () => Promise.reject(new Error('digest failed')) },
    },
  ],
];

for (const [name, replacement] of unusableDigests)
  test(`An upload on a page where ${name} is refused before anything is sent`, async () => {
    const { client, requests } = await clientWith(() =>
      answer(uploadReceipt()),
    );
    const error = await withCrypto(replacement, () =>
      refusalOf(upload(client, uploadInput())),
    );
    assert.ok(error instanceof ArtifreeApiError);
    assert.equal(error.code, 'unavailable');
    assert.equal(error.status, 0);
    assert.equal(requests.length, 0, 'A request was sent.');
  });
