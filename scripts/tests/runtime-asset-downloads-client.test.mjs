import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { test } from 'node:test';
import {
  ArtifreeApiError,
  ArtifreeClient,
} from '../../packages/client/dist/index.js';

/*
 * The runtime asset downloads (decision 0045, rule 12): an offer and a download
 * on behalf of a frame that holds the assets.download grant. As in
 * runtime-asset-writes-client.test.mjs, the calls go through
 * ArtifreeClient.runtimeCall, the path the frame takes
 * (apps/web/src/artifacts/frame-rpc.ts), and the HTTP answer is faked at fetch.
 * A call the client refuses before sending must never reach fetch, so those
 * refusals also assert that no request was sent. A download's bytes are judged
 * only after its response arrived, so those rows assert the error and the one
 * request that was sent, not the absence of a request.
 */

const SESSION = 'e'.repeat(32);
const VIEW_ID = 'd'.repeat(32);
const INIT = 'c'.repeat(64);
const ARTIFACT = `art_${'0'.repeat(32)}`;
const VIEW_ROUTE = `/v1/artifacts/${ARTIFACT}/revisions/3/runtime-views/${VIEW_ID}/1`;
const OFFER_URL = `http://127.0.0.1:7410${VIEW_ROUTE}/assets/offer`;
const DOWNLOAD_URL = `http://127.0.0.1:7410${VIEW_ROUTE}/assets/download`;

const view = {
  url: `http://127.0.0.1:7411/r/${'b'.repeat(64)}/`,
  enabled: true,
  session_id: SESSION,
  artifact_id: ARTIFACT,
  revision: 3,
  head_revision: 3,
  sdk: 'artifree.app.v1',
  declared: ['assets.download'],
  supported: ['assets.download'],
  granted: ['assets.download'],
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
/** The name the server gives a stored text file: `asset-<id>.txt`. */
const FILE_NAME = `asset-${ASSET}.txt`;
/** The most bytes one asset holds: 5 MiB (decision 0045, rule 4). */
const FIVE_MIB = 5_242_880;

/** The bytes of "hello", as a new copy each time. */
const hello = () => Uint8Array.from([104, 101, 108, 108, 111]);

/** The server's offer for `hello`, with changes made. */
function offerAnswer(changes = {}) {
  return {
    asset_id: ASSET,
    file_name: FILE_NAME,
    size: 5,
    sha256: DIGEST,
    ...changes,
  };
}

/** What a frame hands to a download: the offer, once it is checked. */
function offerInput(changes = {}) {
  return {
    assetId: ASSET,
    fileName: FILE_NAME,
    size: 5,
    sha256: DIGEST,
    ...changes,
  };
}

/** The attachment the server answers a download with, headers changed. */
function attachment(headers = {}, bytes = hello()) {
  return new Response(bytes, {
    headers: {
      'content-type': 'application/octet-stream',
      'content-disposition': `attachment; filename="${FILE_NAME}"`,
      ...headers,
    },
  });
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

const runOffer = (client, assetId) =>
  client.runtimeCall(view, (source, frame) =>
    source.offerRuntimeAssetDownload(frame, view, assetId, undefined),
  );

const runDownload = (client, offer) =>
  client.runtimeCall(view, (source, frame) =>
    source.fetchRuntimeAssetDownload(frame, view, offer, undefined),
  );

const refusalOf = (attempt) =>
  attempt.then(
    () => assert.fail('The call must be refused.'),
    (rejection) => rejection,
  );

/** A JSON answer with `status`, as the server gives one. */
const answer = (body, status = 200) => Response.json(body, { status });

/** The server's refusal of a call made without the grant it needs. */
const FORBIDDEN = {
  error: { code: 'forbidden', message: 'The grant is missing.' },
};

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

// ---- an offer that states what the server holds ----

test('An offer resolves to the name, size and digest the server states', async () => {
  const { client } = await clientWith(() => answer(offerAnswer()));
  assert.deepEqual(await runOffer(client, ASSET), {
    assetId: ASSET,
    fileName: FILE_NAME,
    size: 5,
    sha256: DIGEST,
  });
});

test('An offer is one POST to the offer route, with the view secret and the asset ID', async () => {
  const { client, requests } = await clientWith(() => answer(offerAnswer()));
  await runOffer(client, ASSET);
  assert.deepEqual(requests, [
    {
      url: OFFER_URL,
      method: 'POST',
      body: { initialization: INIT, asset_id: ASSET },
    },
  ]);
});

const acceptedOffers = [
  [
    'a name of exactly 100 characters',
    { file_name: 'a'.repeat(100) },
    { assetId: ASSET, fileName: 'a'.repeat(100), size: 5, sha256: DIGEST },
  ],
  [
    'a name with upper case letters, underscores and hyphens',
    { file_name: 'Ab9_-.csv' },
    { assetId: ASSET, fileName: 'Ab9_-.csv', size: 5, sha256: DIGEST },
  ],
  [
    'a size of exactly 5 MiB',
    { size: FIVE_MIB },
    { assetId: ASSET, fileName: FILE_NAME, size: FIVE_MIB, sha256: DIGEST },
  ],
];

for (const [name, changes, expected] of acceptedOffers)
  test(`An offer with ${name} is accepted`, async () => {
    const { client } = await clientWith(() => answer(offerAnswer(changes)));
    assert.deepEqual(await runOffer(client, ASSET), expected);
  });

const refusedOffers = [
  ['an asset ID that is another asset', { asset_id: OTHER_ASSET }],
  ['a digest in upper case hexadecimal', { sha256: DIGEST.toUpperCase() }],
  ['a digest of 63 digits', { sha256: DIGEST.slice(1) }],
  ['a digest that is not hexadecimal', { sha256: 'g'.repeat(64) }],
  ['a size of 5 MiB plus one byte', { size: FIVE_MIB + 1 }],
  ['a size that is negative', { size: -1 }],
  ['a size that is fractional', { size: 1.5 }],
  ['a name of 101 characters', { file_name: 'a'.repeat(101) }],
  ['a name with a slash', { file_name: 'a/b.txt' }],
  ['a name with a backslash', { file_name: 'a\\b.txt' }],
  ['a name with a space', { file_name: 'my file.txt' }],
  ['a name that starts with a dot', { file_name: '.hidden.txt' }],
  ['an empty name', { file_name: '' }],
  ['a field the contract does not define', { extra: true }],
];

for (const [name, changes] of refusedOffers)
  test(`An offer with ${name} is refused`, async () => {
    const { client } = await clientWith(() => answer(offerAnswer(changes)));
    const error = await refusalOf(runOffer(client, ASSET));
    assert.ok(error instanceof ArtifreeApiError);
    assert.equal(error.code, 'invalid_response');
    assert.equal(error.status, 200);
  });

test('An offer without a digest is refused', async () => {
  const { client } = await clientWith(() =>
    answer({ asset_id: ASSET, file_name: FILE_NAME, size: 5 }),
  );
  const error = await refusalOf(runOffer(client, ASSET));
  assert.ok(error instanceof ArtifreeApiError);
  assert.equal(error.code, 'invalid_response');
  assert.equal(error.status, 200);
});

test('An offer whose body is not JSON is refused', async () => {
  const { client } = await clientWith(() => new Response('not json'));
  const error = await refusalOf(runOffer(client, ASSET));
  assert.ok(error instanceof ArtifreeApiError);
  assert.equal(error.code, 'invalid_response');
  assert.equal(error.status, 200);
});

const badAssetIds = [
  ['an asset ID in upper case hexadecimal', 'A'.repeat(64)],
  ['an asset ID of 63 digits', 'a'.repeat(63)],
  ['an asset ID of 65 digits', 'a'.repeat(65)],
  ['an asset ID that is not hexadecimal', 'g'.repeat(64)],
  ['an asset ID that is not text', 64],
];

for (const [name, assetId] of badAssetIds)
  test(`An offer for ${name} is refused before anything is sent`, async () => {
    const { client, requests } = await clientWith(() => answer(offerAnswer()));
    const error = await refusalOf(runOffer(client, assetId));
    assert.ok(error instanceof ArtifreeApiError);
    assert.equal(error.code, 'invalid_request');
    assert.equal(error.status, 0);
    assert.equal(requests.length, 0, 'A request was sent.');
  });

test('An offer the server refuses keeps the status and code it answered with', async () => {
  const { client } = await clientWith(() => answer(FORBIDDEN, 403));
  const error = await refusalOf(runOffer(client, ASSET));
  assert.ok(error instanceof ArtifreeApiError);
  assert.equal(error.status, 403);
  assert.equal(error.code, 'forbidden');
});

// ---- a download that is the attachment offered, with the bytes offered ----

test('A download resolves to the bytes the offer states', async () => {
  const { client } = await clientWith(() => attachment());
  const bytes = await runDownload(client, offerInput());
  assert.deepEqual([...bytes], [104, 101, 108, 108, 111]);
});

test('A download is one POST to the download route, with the view secret and the asset ID', async () => {
  const { client, requests } = await clientWith(() => attachment());
  await runDownload(client, offerInput());
  assert.deepEqual(requests, [
    {
      url: DOWNLOAD_URL,
      method: 'POST',
      body: { initialization: INIT, asset_id: ASSET },
    },
  ]);
});

test('A download of exactly 5 MiB resolves to the same bytes', async () => {
  const bytes = new Uint8Array(FIVE_MIB).map((_, index) => index % 251);
  // The digest comes from node:crypto, not from the Web Crypto path under test.
  const sha256 = createHash('sha256').update(bytes).digest('hex');
  const { client } = await clientWith(() => attachment({}, bytes));
  const received = await runDownload(
    client,
    offerInput({ size: FIVE_MIB, sha256 }),
  );
  assert.equal(received.length, FIVE_MIB);
  assert.equal(
    Buffer.from(received).equals(Buffer.from(bytes)),
    true,
    'The bytes received are not the bytes sent.',
  );
});

const badOffers = [
  ['an asset ID in upper case hexadecimal', { assetId: ASSET.toUpperCase() }],
  ['a name with a slash', { fileName: 'a/b.txt' }],
  ['a name of 101 characters', { fileName: 'a'.repeat(101) }],
  ['a digest in upper case hexadecimal', { sha256: DIGEST.toUpperCase() }],
  ['a size of 5 MiB plus one byte', { size: FIVE_MIB + 1 }],
];

for (const [name, changes] of badOffers)
  test(`A download of an offer with ${name} is refused before anything is sent`, async () => {
    const { client, requests } = await clientWith(() => attachment());
    const error = await refusalOf(runDownload(client, offerInput(changes)));
    assert.ok(error instanceof ArtifreeApiError);
    assert.equal(error.code, 'invalid_request');
    assert.equal(error.status, 0);
    assert.equal(requests.length, 0, 'A request was sent.');
  });

const unusableDigests = [
  ['crypto.subtle is missing', {}],
  ['crypto is missing', undefined],
  ['crypto.subtle is undefined', { subtle: undefined }],
];

for (const [name, replacement] of unusableDigests)
  test(`A download on a page where ${name} is refused before anything is sent`, async () => {
    const { client, requests } = await clientWith(() => attachment());
    const error = await withCrypto(replacement, () =>
      refusalOf(runDownload(client, offerInput())),
    );
    assert.ok(error instanceof ArtifreeApiError);
    assert.equal(error.code, 'unavailable');
    assert.equal(error.status, 0);
    assert.equal(requests.length, 0, 'A request was sent.');
  });

const refusedAttachments = [
  [
    'a content type of text/plain',
    () => attachment({ 'content-type': 'text/plain' }),
  ],
  [
    'a content type of text/html',
    () => attachment({ 'content-type': 'text/html' }),
  ],
  [
    'a content type of application/zip',
    () => attachment({ 'content-type': 'application/zip' }),
  ],
  [
    'no content type',
    () =>
      new Response(hello(), {
        headers: {
          'content-disposition': `attachment; filename="${FILE_NAME}"`,
        },
      }),
  ],
  [
    'an inline disposition',
    () =>
      attachment({ 'content-disposition': `inline; filename="${FILE_NAME}"` }),
  ],
  [
    'a disposition that names another file',
    () =>
      attachment({ 'content-disposition': 'attachment; filename="other.txt"' }),
  ],
  [
    'a disposition without a file name',
    () => attachment({ 'content-disposition': 'attachment' }),
  ],
];

for (const [name, respond] of refusedAttachments)
  test(`A download answered with ${name} is refused`, async () => {
    const { client, requests } = await clientWith(respond);
    const error = await refusalOf(runDownload(client, offerInput()));
    assert.ok(error instanceof ArtifreeApiError);
    assert.equal(error.code, 'invalid_response');
    assert.equal(error.status, 200);
    assert.equal(requests.length, 1, 'The download was not sent once.');
  });

test('A download one byte longer than the size offered is refused, though its digest matches', async () => {
  const { client } = await clientWith(() => attachment());
  const error = await refusalOf(runDownload(client, offerInput({ size: 6 })));
  assert.ok(error instanceof ArtifreeApiError);
  assert.equal(error.code, 'invalid_response');
  assert.equal(error.status, 200);
});

test('A download one byte shorter than the size offered is refused, though its digest matches', async () => {
  const { client } = await clientWith(() => attachment());
  const error = await refusalOf(runDownload(client, offerInput({ size: 4 })));
  assert.ok(error instanceof ArtifreeApiError);
  assert.equal(error.code, 'invalid_response');
  assert.equal(error.status, 200);
});

test('A download of other bytes of the offered size is refused by its digest', async () => {
  const jello = Uint8Array.from([106, 101, 108, 108, 111]);
  const { client } = await clientWith(() => attachment({}, jello));
  const error = await refusalOf(runDownload(client, offerInput()));
  assert.ok(error instanceof ArtifreeApiError);
  assert.equal(error.code, 'invalid_response');
  assert.equal(error.status, 200);
});

test('A download whose digest cannot be computed is refused, it does not pass', async () => {
  const { client } = await clientWith(() => attachment());
  const error = await withCrypto(
    { subtle: { digest: () => Promise.reject(new Error('digest failed')) } },
    () => refusalOf(runDownload(client, offerInput())),
  );
  assert.ok(error instanceof ArtifreeApiError);
  assert.equal(error.code, 'unavailable');
  assert.equal(error.status, 200);
});

test('A download the server refuses keeps the status and code it answered with', async () => {
  const { client } = await clientWith(() => answer(FORBIDDEN, 403));
  const error = await refusalOf(runDownload(client, offerInput()));
  assert.ok(error instanceof ArtifreeApiError);
  assert.equal(error.status, 403);
  assert.equal(error.code, 'forbidden');
});
