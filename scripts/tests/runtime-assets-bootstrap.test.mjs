import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { afterEach, mock, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The frame script is bundled as the server embeds it, then run against a
// stand-in window and a real MessageChannel. The host side is the test. The
// fake workspace cuts reads the way the server does
// (crates/artifree-server/src/runtime_assets.rs).
const bundle = await build({
  entryPoints: [
    fileURLToPath(
      new URL('../../packages/runtime-sdk/src/bootstrap.ts', import.meta.url),
    ),
  ],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const { start } = await import(
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
);
// The workspace host's parser, bundled as it ships, to check that every call
// the frame makes is one the host accepts.
const hostBundle = await build({
  entryPoints: [
    fileURLToPath(
      new URL('../../apps/web/src/artifacts/frame-rpc.ts', import.meta.url),
    ),
  ],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const { parseCall } = await import(
  `data:text/javascript;base64,${Buffer.from(hostBundle.outputFiles[0].text).toString('base64')}`
);

const CONFIG = {
  view_id: 'd'.repeat(32),
  generation: 1,
  initialization: 'c'.repeat(64),
  lease_ms: 900_000,
};
/** The most bytes of an asset one part may carry (decision 0045). */
const PART = 262_144;
const ASSET = 'a'.repeat(64);
const OTHER_ASSET = 'b'.repeat(64);
/** A fake workspace stops answering after this many requests. */
const MAX_REQUESTS = 8;

/** The SHA-256 of some bytes, from node:crypto rather than the SDK's own. */
function digestOf(bytes) {
  return createHash('sha256').update(bytes).digest('hex');
}

/** Deterministic bytes that do not repeat one value. */
function bytesOf(length) {
  return Uint8Array.from({ length }, (_, index) => (index * 31 + 7) % 251);
}

/** An asset as the server holds it: its ID, media type, bytes and digest. */
function asset(bytes, overrides = {}) {
  return {
    id: ASSET,
    contentType: 'text/plain',
    bytes,
    sha256: digestOf(bytes),
    ...overrides,
  };
}

/**
 * The part the server sends for `request` of `item`: cut at the offset and the
 * length asked, carrying the whole asset's size and digest. The next offset is
 * null only when the part reaches the end of the asset.
 */
function honestPart(item, request) {
  const size = item.bytes.length;
  const end = Math.min(request.offset + request.length, size);
  return {
    assetId: item.id,
    contentType: item.contentType,
    offset: request.offset,
    nextOffset: end < size ? end : null,
    size,
    sha256: item.sha256,
    bytes: item.bytes.slice(request.offset, end).buffer,
  };
}

/**
 * A workspace that answers each `assets.read` with the part `answer` gives for
 * that request. It stops after MAX_REQUESTS, so a frame that never stops asking
 * fails the test instead of hanging it.
 */
function assetWorkspace(answer) {
  let asked = 0;
  return (message) => {
    asked += 1;
    if (asked > MAX_REQUESTS)
      return { id: message.id, ok: false, code: 'unavailable' };
    return { id: message.id, ok: true, part: answer(message) };
  };
}

async function settle() {
  for (let round = 0; round < 20; round += 1) await Promise.resolve();
  // A message crosses the port on the event loop, not in a microtask.
  for (let round = 0; round < 5; round += 1)
    await new Promise((resolve) => setImmediate(resolve));
}

let current;

/**
 * Starts the script on a stand-in window; the test plays the workspace. Each
 * message the frame sends is recorded, then `respond` may answer it.
 */
function frame(config = CONFIG, respond = () => undefined) {
  const listeners = {};
  const received = [];
  const host = { port: undefined, connect: undefined };
  const window = {
    artifree: undefined,
    addEventListener: (type, listener) => {
      listeners[type] = listener;
    },
    setTimeout: (run, ms) => setTimeout(run, ms),
    parent: {
      postMessage: (message, _origin, ports) => {
        host.connect = message;
        host.port = ports[0];
        host.port.onmessage = (event) => {
          received.push(event.data);
          const reply = respond(event.data);
          if (reply !== undefined) host.port.postMessage(reply);
        };
      },
    },
  };
  globalThis.window = window;
  start(config);
  current = { window, listeners, host };
  return {
    api: window.artifree,
    received,
    host,
    /** Answers call `id` as the workspace. */
    reply: (message) => host.port.postMessage(message),
    hangUp: () => listeners.pagehide(),
  };
}

afterEach(() => {
  current?.listeners.pagehide?.();
  current = undefined;
  delete globalThis.window;
  mock.timers.reset();
});

async function code(promise) {
  const error = await promise.then(
    () => assert.fail('The call must be refused.'),
    (rejection) => rejection,
  );
  assert.equal(error.name, 'RuntimeRequestFailure');
  return error;
}

/**
 * Makes one call, answers the message it sends with `reply`, and says how the
 * call settled: with its value or its error, and with the message the frame
 * sent.
 */
async function answerOnce(call, reply) {
  const f = frame();
  const settled = call(f.api).then(
    (value) => ({ value }),
    (error) => ({ error }),
  );
  await settle();
  assert.equal(f.received.length, 1, 'one call was sent');
  const [sent] = f.received;
  f.reply({ id: sent.id, ...reply });
  const outcome = await settled;
  f.hangUp();
  return { ...outcome, sent };
}

/** Lists the space, with the listing the workspace answers with. */
function listWith(listing) {
  return answerOnce((api) => api.assets.list(), { ok: true, listing });
}

/**
 * Reads ASSET from a workspace that answers each request with `answer`. Every
 * request the frame sent asks for at most one part, refused reads included.
 */
async function readFrom(answer) {
  const f = frame(CONFIG, assetWorkspace(answer));
  const settled = f.api.assets.read({ assetId: ASSET }).then(
    (value) => ({ value }),
    (error) => ({ error }),
  );
  const outcome = await settled;
  f.hangUp();
  assert.deepEqual(
    f.received
      .map((message) => message.length)
      .filter((length) => length > PART),
    [],
    'a request asked for more than one part',
  );
  return { ...outcome, requests: f.received };
}

/** The call was refused as unavailable, with the closed wording. */
function assertUnavailable(outcome, label) {
  // A boolean, not the value itself: a value that should have been refused
  // would otherwise be diffed in full by the failure report.
  assert.equal(
    outcome.value === undefined,
    true,
    `${label}: the call must be refused`,
  );
  assert.equal(outcome.error?.name, 'RuntimeRequestFailure', label);
  assert.equal(outcome.error?.code, 'unavailable', label);
  assert.equal(
    outcome.error?.message,
    'The runtime request is unavailable.',
    label,
  );
}

const DIGEST = digestOf(Uint8Array.of(1, 2, 3));

/** The Nth asset ID, as 64 lowercase hexadecimal digits; IDs ascend with N. */
function idOf(index) {
  return index.toString(16).padStart(64, '0');
}

/** One listed asset, as the workspace reports it. */
function entry(overrides = {}) {
  return {
    id: idOf(1),
    contentType: 'text/plain',
    size: 5,
    sha256: DIGEST,
    createdAtMs: 1_700_000_000_000,
    ...overrides,
  };
}

/** A listing at `assetVersion`, with its assets in the order given. */
function listingOf(assets, assetVersion = 3) {
  return { assetVersion, assets };
}

test('The assets capability offers list, read, upload and delete, and is frozen', () => {
  const f = frame();
  assert.ok(Object.isFrozen(f.api.assets));
  assert.deepEqual(Object.keys(f.api.assets).sort(), [
    'delete',
    'list',
    'read',
    'upload',
  ]);
  f.hangUp();
});

test('A listing returns each asset as the workspace listed it, as a call the host accepts', async () => {
  const listing = listingOf([
    entry({ id: idOf(1) }),
    entry({
      id: idOf(2),
      contentType: 'image/png',
      size: 1_048_576,
      createdAtMs: 1_700_000_100_000,
    }),
  ]);
  const outcome = await listWith(listing);
  assert.equal(outcome.error, undefined);
  assert.deepEqual(outcome.value, listing);
  assert.deepEqual(outcome.sent, { type: 'assets.list', id: 1 });
  assert.deepEqual(parseCall(outcome.sent), { type: 'assets.list', id: 1 });
});

test('A listing holds at most 64 assets', async () => {
  const assetsOf = (count) =>
    Array.from({ length: count }, (_, index) => entry({ id: idOf(index + 1) }));
  const full = listingOf(assetsOf(64));
  assert.deepEqual((await listWith(full)).value, full);
  assertUnavailable(await listWith(listingOf(assetsOf(65))), '65 assets');
});

test('A version of 0 is listed only for a space that holds no asset', async () => {
  for (const listing of [listingOf([], 0), listingOf([entry()], 1)]) {
    assert.deepEqual(
      (await listWith(listing)).value,
      listing,
      `version ${listing.assetVersion}`,
    );
  }
  assertUnavailable(
    await listWith(listingOf([entry()], 0)),
    'version 0 with an asset',
  );
});

test('Listed asset IDs rise strictly, so an ID never repeats', async () => {
  const rising = listingOf([entry({ id: idOf(1) }), entry({ id: idOf(2) })]);
  assert.deepEqual((await listWith(rising)).value, rising);
  assertUnavailable(
    await listWith(listingOf([entry({ id: idOf(1) }), entry({ id: idOf(1) })])),
    'a repeated ID',
  );
  assertUnavailable(
    await listWith(listingOf([entry({ id: idOf(2) }), entry({ id: idOf(1) })])),
    'a descending ID',
  );
});

test('Only the four asset media types are listed', async () => {
  for (const contentType of [
    'text/plain',
    'text/csv',
    'image/png',
    'image/jpeg',
  ]) {
    const listing = listingOf([entry({ contentType })]);
    assert.deepEqual((await listWith(listing)).value, listing, contentType);
  }
  for (const contentType of [
    'application/pdf',
    'text/html',
    'image/gif',
    'text/plain; charset=utf-8',
    'TEXT/PLAIN',
    '',
  ]) {
    assertUnavailable(
      await listWith(listingOf([entry({ contentType })])),
      JSON.stringify(contentType),
    );
  }
});

test('A listed asset of zero bytes is refused, and one of one byte is accepted', async () => {
  assertUnavailable(
    await listWith(listingOf([entry({ size: 0 })])),
    'zero bytes',
  );
  const one = listingOf([entry({ size: 1 })]);
  assert.deepEqual((await listWith(one)).value, one);
});

test('A listed asset above 5 MiB is refused, and one of exactly 5 MiB is accepted', async () => {
  const largest = listingOf([entry({ size: 5_242_880 })]);
  assert.deepEqual((await listWith(largest)).value, largest);
  assertUnavailable(
    await listWith(listingOf([entry({ size: 5_242_881 })])),
    '5 MiB and one byte',
  );
});

test('A listed creation time must be a safe integer', async () => {
  for (const createdAtMs of [0, 1_700_000_000_000, Number.MAX_SAFE_INTEGER]) {
    const listing = listingOf([entry({ createdAtMs })]);
    assert.deepEqual(
      (await listWith(listing)).value,
      listing,
      String(createdAtMs),
    );
  }
  for (const createdAtMs of [
    2 ** 53,
    1.5,
    Number.NaN,
    Number.POSITIVE_INFINITY,
    '1700000000000',
  ]) {
    assertUnavailable(
      await listWith(listingOf([entry({ createdAtMs })])),
      String(createdAtMs),
    );
  }
});

test('A listed creation time is never negative', async () => {
  const negative = listingOf([entry({ createdAtMs: -1 })]);
  assertUnavailable(await listWith(negative), 'a negative creation time');
});

test('A listed asset ID or digest that is not 64 lowercase hexadecimal digits is refused', async () => {
  const valid = listingOf([entry()]);
  assert.deepEqual((await listWith(valid)).value, valid);
  const malformed = {
    'an uppercase ID': entry({ id: 'A'.repeat(64) }),
    'a 63-digit ID': entry({ id: 'a'.repeat(63) }),
    'a 65-digit ID': entry({ id: 'a'.repeat(65) }),
    'a non-hex ID': entry({ id: 'g'.repeat(64) }),
    'an uppercase digest': entry({ sha256: `${DIGEST.slice(0, 63)}A` }),
    'a 63-digit digest': entry({ sha256: DIGEST.slice(1) }),
  };
  for (const [label, item] of Object.entries(malformed)) {
    assertUnavailable(await listWith(listingOf([item])), label);
  }
});

test('A listing whose counts or fields have the wrong shape is refused', async () => {
  const malformed = {
    'a negative version': listingOf([entry()], -1),
    'a fractional version': listingOf([entry()], 1.5),
    'a version as text': listingOf([entry()], '3'),
    'assets that are not a list': { assetVersion: 3, assets: 'a' },
    'no assets field': { assetVersion: 3 },
    'an asset without a media type': listingOf([
      { ...entry(), contentType: undefined },
    ]),
    'an asset without a creation time': listingOf([
      { ...entry(), createdAtMs: undefined },
    ]),
    'no listing at all': undefined,
  };
  for (const [label, listing] of Object.entries(malformed)) {
    assertUnavailable(await listWith(listing), label);
  }
});

test('A 600 KiB asset is read in 256 KiB parts, and its bytes match its digest', async () => {
  const big = asset(bytesOf(614_400));
  const f = frame(
    CONFIG,
    assetWorkspace((request) => honestPart(big, request)),
  );
  const content = await f.api.assets.read({ assetId: ASSET });
  assert.equal(content.assetId, ASSET);
  assert.equal(content.contentType, 'text/plain');
  assert.equal(content.size, 614_400);
  assert.equal(content.sha256, big.sha256);
  assert.ok(content.bytes instanceof Uint8Array);
  assert.ok(
    Buffer.from(content.bytes).equals(Buffer.from(big.bytes)),
    'the assembled bytes differ from the asset',
  );
  assert.deepEqual(f.received, [
    { type: 'assets.read', id: 1, assetId: ASSET, offset: 0, length: PART },
    {
      type: 'assets.read',
      id: 2,
      assetId: ASSET,
      offset: PART,
      length: PART,
    },
    {
      type: 'assets.read',
      id: 3,
      assetId: ASSET,
      offset: 2 * PART,
      length: PART,
    },
  ]);
  for (const message of f.received) {
    assert.deepEqual(parseCall(message), message, JSON.stringify(message));
  }
  f.hangUp();
});

test('A one-byte asset is read in one part', async () => {
  const one = asset(Uint8Array.of(42));
  const outcome = await readFrom((request) => honestPart(one, request));
  assert.equal(outcome.error, undefined);
  assert.deepEqual([...outcome.value.bytes], [42]);
  assert.equal(outcome.value.size, 1);
  assert.equal(outcome.value.sha256, one.sha256);
  assert.deepEqual(outcome.requests, [
    { type: 'assets.read', id: 1, assetId: ASSET, offset: 0, length: PART },
  ]);
});

test('A zero-byte asset is refused, though its one part is an empty final part at offset 0', async () => {
  const empty = asset(new Uint8Array(0));
  const outcome = await readFrom((request) => honestPart(empty, request));
  assertUnavailable(outcome, 'a zero-byte asset');
  assert.equal(outcome.requests.length, 1);
});

test('An asset of exactly two parts is read in two requests, with no empty part after the last', async () => {
  const two = asset(bytesOf(524_288));
  const outcome = await readFrom((request) => honestPart(two, request));
  assert.equal(outcome.error, undefined);
  assert.ok(
    Buffer.from(outcome.value.bytes).equals(Buffer.from(two.bytes)),
    'the assembled bytes differ from the asset',
  );
  assert.deepEqual(
    outcome.requests.map((message) => message.offset),
    [0, PART],
  );
});

test('A part whose next offset is the end of the asset is refused at once', async () => {
  const two = asset(bytesOf(524_288));
  const outcome = await readFrom((request) => {
    const part = honestPart(two, request);
    return request.offset === 0 ? part : { ...part, nextOffset: 524_288 };
  });
  assertUnavailable(outcome, 'a next offset at the end');
  assert.equal(outcome.requests.length, 2);
});

test('A corrupted byte is refused as unavailable once every part has arrived', async () => {
  const big = asset(bytesOf(614_400));
  const flipped = Uint8Array.from(big.bytes);
  flipped[400_000] ^= 0xff;
  const outcome = await readFrom((request) =>
    honestPart({ ...big, bytes: flipped }, request),
  );
  assertUnavailable(outcome, 'a corrupted byte');
  assert.equal(outcome.requests.length, 3);
});

test('A part that states a different size from the first part is refused at once', async () => {
  const big = asset(bytesOf(614_400));
  const shorter = { ...big, bytes: big.bytes.subarray(0, 600_000) };
  const outcome = await readFrom((request) =>
    honestPart(request.offset === 0 ? big : shorter, request),
  );
  assertUnavailable(outcome, 'a changed size');
  assert.equal(outcome.requests.length, 2);
});

test('A part that states a different digest from the first part is refused at once', async () => {
  const big = asset(bytesOf(614_400));
  const other = { ...big, sha256: digestOf(Uint8Array.of(0)) };
  const outcome = await readFrom((request) =>
    honestPart(request.offset === 0 ? big : other, request),
  );
  assertUnavailable(outcome, 'a changed digest');
  assert.equal(outcome.requests.length, 2);
});

test('A part that states a different media type from the first part is refused at once', async () => {
  const big = asset(bytesOf(614_400));
  const other = { ...big, contentType: 'text/csv' };
  const outcome = await readFrom((request) =>
    honestPart(request.offset === 0 ? big : other, request),
  );
  assertUnavailable(outcome, 'a changed media type');
  assert.equal(outcome.requests.length, 2);
});

test('A part that echoes another asset ID is refused at once', async () => {
  const big = asset(bytesOf(614_400));
  const other = { ...big, id: OTHER_ASSET };
  const outcome = await readFrom((request) =>
    honestPart(request.offset === 0 ? big : other, request),
  );
  assertUnavailable(outcome, 'another asset ID');
  assert.equal(outcome.requests.length, 2);
});

test('A part that echoes another offset than the one asked is refused at once', async () => {
  const big = asset(bytesOf(614_400));
  const outcome = await readFrom((request) => {
    const part = honestPart(big, request);
    return request.offset === 0
      ? part
      : { ...part, offset: 0, nextOffset: PART };
  });
  assertUnavailable(outcome, 'another offset');
  assert.equal(outcome.requests.length, 2);
});

test('A part that ends before the asset does is refused at once', async () => {
  const big = asset(bytesOf(614_400));
  const outcome = await readFrom((request) => ({
    ...honestPart(big, request),
    nextOffset: null,
  }));
  assertUnavailable(outcome, 'an early end');
  assert.equal(outcome.requests.length, 1);
});

test('A final part with more bytes than the asset holds is refused', async () => {
  const ten = asset(bytesOf(10));
  const outcome = await readFrom((request) => ({
    ...honestPart(ten, request),
    bytes: Uint8Array.from({ length: 20 }, (_, index) => index).buffer,
  }));
  assertUnavailable(outcome, 'bytes past the end');
  assert.equal(outcome.requests.length, 1);
});

test('A part longer than the bound is refused, even when it claims to be the last', async () => {
  const long = asset(bytesOf(300_000));
  const outcome = await readFrom((request) => ({
    ...honestPart(long, request),
    nextOffset: null,
    bytes: long.bytes.slice().buffer,
  }));
  assertUnavailable(outcome, 'a part over the bound');
  assert.equal(outcome.requests.length, 1);
});

test('A read with a malformed asset ID or a bad argument is refused before anything is sent', async () => {
  // Any call that is sent is answered at once with a refusal, so a broken guard
  // fails fast rather than waiting out the read timeout.
  const f = frame(CONFIG, (message) => ({
    id: message.id,
    ok: false,
    code: 'unavailable',
  }));
  const revoked = Proxy.revocable({}, {});
  revoked.revoke();
  const bad = [
    undefined,
    null,
    5,
    ASSET,
    [ASSET],
    {},
    { assetId: 5 },
    { assetId: null },
    { assetId: undefined },
    { assetId: 'A'.repeat(64) },
    { assetId: 'a'.repeat(63) },
    { assetId: 'a'.repeat(65) },
    { assetId: 'g'.repeat(64) },
    { assetId: ` ${ASSET}` },
    { assetId: ASSET, offset: 0 },
    { assetId: ASSET, extra: 1 },
    { id: ASSET },
    Object.defineProperty({}, 'assetId', { value: ASSET, enumerable: false }),
    {
      get assetId() {
        throw new Error('getter ran');
      },
    },
    new Proxy(
      { assetId: ASSET },
      {
        ownKeys() {
          throw new Error('trap ran');
        },
      },
    ),
    revoked.proxy,
    { [Symbol('marked')]: 1, assetId: ASSET },
  ];
  for (const [index, argument] of bad.entries()) {
    const error = await code(f.api.assets.read(argument));
    assert.equal(error.code, 'invalid_request', `argument ${index}`);
  }
  await settle();
  assert.deepEqual(f.received, [], 'nothing was sent for a refused read');
  f.hangUp();
});

/** Moves the fake clock on by `ms`, then lets the port deliver. */
async function passTime(ms) {
  mock.timers.tick(ms);
  await settle();
}

test('A part refused once as rate-limited is asked again at the same offset, and the read still succeeds', async () => {
  mock.timers.enable({ apis: ['setTimeout'] });
  const big = asset(bytesOf(614_400));
  let refusals = 1;
  const f = frame(CONFIG, (message) => {
    if (message.offset === PART && refusals > 0) {
      refusals -= 1;
      return { id: message.id, ok: false, code: 'rate_limited' };
    }
    return { id: message.id, ok: true, part: honestPart(big, message) };
  });
  const settled = f.api.assets.read({ assetId: ASSET }).then(
    (value) => ({ value }),
    (error) => ({ error }),
  );
  await settle();
  assert.equal(f.received.length, 2, 'the refused part was sent');
  await passTime(249);
  assert.equal(f.received.length, 2, 'no retry before its 250 ms wait ends');
  mock.timers.tick(1);
  const outcome = await settled;
  f.hangUp();
  assert.equal(outcome.error?.code, undefined, 'the read succeeds');
  assert.equal(outcome.value.size, 614_400);
  assert.equal(outcome.value.sha256, big.sha256);
  assert.ok(
    Buffer.from(outcome.value.bytes).equals(Buffer.from(big.bytes)),
    'the assembled bytes differ from the asset',
  );
  assert.deepEqual(
    f.received.map((message) => message.offset),
    [0, PART, PART, 2 * PART],
    'the refused part is asked again at its own offset',
  );
});

test('A part refused as rate-limited every time is asked five times in all, then the read is refused as rate_limited', async () => {
  mock.timers.enable({ apis: ['setTimeout'] });
  const f = frame(CONFIG, (message) => ({
    id: message.id,
    ok: false,
    code: 'rate_limited',
  }));
  // Not awaited: a read that never gives up must fail the assertions below,
  // not hang the test.
  let outcome;
  f.api.assets.read({ assetId: ASSET }).then(
    (value) => {
      outcome = { value };
    },
    (error) => {
      outcome = { error };
    },
  );
  await settle();
  assert.equal(f.received.length, 1, 'the first attempt');
  // The retries come as their waits end: 250, 500, 1000, then 2000 ms.
  await passTime(249);
  assert.equal(f.received.length, 1, 'no retry before 250 ms');
  await passTime(1);
  assert.equal(f.received.length, 2, 'the first retry, at 250 ms');
  await passTime(499);
  assert.equal(f.received.length, 2, 'no second retry before 750 ms');
  await passTime(1);
  assert.equal(f.received.length, 3, 'the second retry, at 750 ms');
  await passTime(999);
  assert.equal(f.received.length, 3, 'no third retry before 1750 ms');
  await passTime(1);
  assert.equal(f.received.length, 4, 'the third retry, at 1750 ms');
  await passTime(1999);
  assert.equal(f.received.length, 4, 'no fourth retry before 3750 ms');
  await passTime(1);
  assert.equal(f.received.length, 5, 'the fourth retry, at 3750 ms');
  assert.deepEqual(
    f.received.map((message) => message.offset),
    [0, 0, 0, 0, 0],
    'every attempt asks for the same offset',
  );
  assert.equal(
    outcome?.error?.code,
    'rate_limited',
    'the read is refused after the fifth attempt',
  );
  f.hangUp();
});

test('A refusal that is not rate-limited is not asked again, however long the backoff would have run', async () => {
  mock.timers.enable({ apis: ['setTimeout'] });
  const f = frame(CONFIG, (message) => ({
    id: message.id,
    ok: false,
    code: 'denied',
  }));
  let outcome;
  f.api.assets.read({ assetId: ASSET }).then(
    (value) => {
      outcome = { value };
    },
    (error) => {
      outcome = { error };
    },
  );
  await settle();
  assert.equal(outcome?.error?.code, 'denied', 'the read is refused at once');
  await passTime(3_750);
  assert.equal(f.received.length, 1, 'one attempt, and no retry');
  f.hangUp();
});
