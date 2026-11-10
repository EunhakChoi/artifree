import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFile, readdir } from 'node:fs/promises';
import { test } from 'node:test';
import { compareBytes } from '../../packages/client/dist/runtime-bytes.js';
import {
  FRAME_ROUTES,
  FRAME_ROUTE_TAILS,
  frameBody,
  frameRoute,
} from '../../packages/client/dist/runtime-frame.js';

/*
 * The client frame transport (decision 0052, rule 5): the tails that the frame
 * routes are built from, the route and body of a frame request, and the
 * comparison of received bytes with the size and SHA-256 an offer stated. The
 * expected paths are written out, and the expected digests come from
 * node:crypto, never from the functions under test.
 */

const ARTIFACT = `art_${'0'.repeat(32)}`;
const VIEW_ID = 'd'.repeat(32);
const INIT = 'c'.repeat(64);

/** A view at revision 3 and generation 1. */
const view = {
  artifact_id: ARTIFACT,
  revision: 3,
  view_id: VIEW_ID,
  generation: 1,
  initialization: INIT,
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

/** The lowercase hexadecimal SHA-256 of `data`, computed with node:crypto. */
function sha256Hex(data) {
  return createHash('sha256').update(data).digest('hex');
}

/** The SHA-256 of `data` as the ArrayBuffer that Web Crypto returns. */
function sha256Buffer(data) {
  return new Uint8Array(createHash('sha256').update(data).digest()).buffer;
}

/** Compares 'hello' with its own size and digest, with `crypto` replaced. */
function compareHelloWithCrypto(replacement) {
  const bytes = new TextEncoder().encode('hello');
  const sha256 = sha256Hex(bytes);
  return withCrypto(replacement, () => compareBytes(bytes, 5, sha256));
}

// ---- the route tails ----

test('The frame route tails are the twelve recorded tails, in order, each named once', () => {
  const recorded = [
    'metadata',
    'source/list',
    'source/read',
    'source/offer',
    'source/download',
    'source/write',
    'assets/list',
    'assets/read',
    'assets/upload',
    'assets/delete',
    'assets/offer',
    'assets/download',
  ];
  assert.deepEqual([...FRAME_ROUTE_TAILS], recorded);
  assert.equal(new Set(FRAME_ROUTE_TAILS).size, 12);
  assert.deepEqual(Object.values(FRAME_ROUTES), recorded);
});

// ---- routes and bodies ----

test('A frame route is the view path followed by the tail', () => {
  assert.equal(
    frameRoute(view, 'assets/offer'),
    `/v1/artifacts/art_${'0'.repeat(32)}/revisions/3/runtime-views/${'d'.repeat(32)}/1/assets/offer`,
  );
  assert.equal(
    frameRoute(view, FRAME_ROUTES.metadata),
    `/v1/artifacts/art_${'0'.repeat(32)}/revisions/3/runtime-views/${'d'.repeat(32)}/1/metadata`,
  );
});

test('An artifact id is percent-encoded in a frame route', () => {
  assert.equal(
    frameRoute({ ...view, artifact_id: 'a/b' }, 'metadata'),
    `/v1/artifacts/a%2Fb/revisions/3/runtime-views/${'d'.repeat(32)}/1/metadata`,
  );
});

test('A frame body starts with the initialization, then the call fields', () => {
  const body = frameBody(view, { asset_id: 'x' });
  assert.deepEqual(body, { initialization: INIT, asset_id: 'x' });
  assert.equal(Object.keys(body)[0], 'initialization');
  assert.deepEqual(frameBody(view, {}), { initialization: INIT });
});

// ---- comparing received bytes ----

test('A length that differs from the stated size is a mismatch, decided before any digest', async () => {
  const bytes = new Uint8Array([1, 2, 3]);
  const sha256 = sha256Hex(bytes);
  let digests = 0;
  const replacement = {
    subtle: {
      digest: async (_algorithm, data) => {
        digests += 1;
        return sha256Buffer(data);
      },
    },
  };
  const compare = () => compareBytes(bytes, 4, sha256);
  const comparison = await withCrypto(replacement, compare);
  assert.deepEqual(comparison, { outcome: 'mismatch' });
  assert.equal(digests, 0);
});

test('With the stated length, the digest decides between a match and a mismatch', async () => {
  const bytes = new TextEncoder().encode('hello');
  const sha256 = sha256Hex(bytes);
  const matching = await compareBytes(bytes, 5, sha256);
  const other = await compareBytes(bytes, 5, '0'.repeat(64));
  assert.deepEqual(matching, { outcome: 'match' });
  assert.deepEqual(other, { outcome: 'mismatch' });
});

test('A missing crypto object makes the digest unavailable, never a match', async () => {
  const comparison = await compareHelloWithCrypto(undefined);
  assert.equal(comparison.outcome, 'unavailable');
  assert.ok(comparison.cause instanceof TypeError);
});

test('A missing crypto.subtle makes the digest unavailable, never a match', async () => {
  const comparison = await compareHelloWithCrypto({});
  assert.equal(comparison.outcome, 'unavailable');
  assert.ok(comparison.cause instanceof TypeError);
});

test('A rejected digest is unavailable with the rejection as its cause', async () => {
  const failure = new Error('The digest was rejected.');
  const replacement = {
    subtle: {
      digest: async () => {
        throw failure;
      },
    },
  };
  const comparison = await compareHelloWithCrypto(replacement);
  assert.equal(comparison.outcome, 'unavailable');
  assert.equal(comparison.cause, failure);
});

test('Only the two on-demand runtime modules import the byte comparison', async () => {
  const directory = new URL('../../packages/client/src/', import.meta.url);
  const importers = [];
  for (const name of await readdir(directory)) {
    if (!name.endsWith('.ts')) continue;
    const source = await readFile(new URL(name, directory), 'utf8');
    // An import or a re-export, not a mention in a comment.
    if (/['"]\.\/runtime-bytes\.js['"]/.test(source)) importers.push(name);
  }
  assert.deepEqual(importers.sort(), [
    'runtime-assets.ts',
    'runtime-source.ts',
  ]);
});
