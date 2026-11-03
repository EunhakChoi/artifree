import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, before, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import {
  ArtifreeClient,
  followRoom,
  identityOf,
  joinRoom,
  leaveRoom,
  pollRoom,
  sendRoomEvent,
} from '../../packages/client/dist/index.js';
import { createAjv } from '../lib/schema.mjs';
import { buildServer, startServer } from '../lib/server-process.mjs';

// Rooms and presence (decision 0034) of the real binary through the
// TypeScript client: every room response is validated against the published
// schema; two tabs are two runtime connections, a page and a frame are two
// participants of one; a reconnect keeps its participant; an event reaches
// those present and nobody later; a follower crosses a lost participant.
const root = fileURLToPath(new URL('../../', import.meta.url));
let binary;
let scratch;
let server;
let validateSnapshot;
let validateReceipt;
/** Raw room response bodies of the schema check. */
const bodies = [];

before(async () => {
  binary = buildServer(root);
  scratch = await mkdtemp(join(tmpdir(), 'artifree-room-test-'));
  const schema = JSON.parse(
    await readFile(
      join(root, 'contracts/generated/api-v1.schema.json'),
      'utf8',
    ),
  );
  const compile = (name) =>
    createAjv().compile({
      $schema: schema.$schema,
      $defs: schema.$defs,
      $ref: `#/$defs/${name}`,
    });
  validateSnapshot = compile('RoomSnapshot');
  validateReceipt = compile('RoomEventReceipt');
  server = await startServer(binary, join(scratch, 'data'));
});

after(async () => {
  await server?.stop();
  await rm(scratch, { recursive: true, force: true });
});

/** One browser tab: a client with its own runtime connection. */
async function tab(running = server) {
  const client = new ArtifreeClient({
    baseUrl: running.api_url,
    accessToken: running.token,
    fetch: async (url, init) => {
      const response = await fetch(url, init);
      if (new URL(url).pathname.includes('/room/') && response.ok) {
        const text = await response.clone().text();
        if (text !== '')
          bodies.push([init.method, new URL(url).pathname, text]);
      }
      return response;
    },
  });
  await client.openRuntimeSession();
  return client;
}

let counter = 0;
async function artifact(running = server) {
  const client = new ArtifreeClient({
    baseUrl: running.api_url,
    accessToken: running.token,
  });
  const { artifact: created } = await client.createArtifact(
    {
      title: 'Room',
      files: [{ path: 'index.html', encoding: 'utf8', content: '<p>room</p>' }],
    },
    { idempotencyKey: `room-test-create-${(counter += 1)}` },
  );
  return created.id;
}

/** `[participant, is_me, same_tab]` of each member. */
const flags = (snapshot) =>
  snapshot.members.map((m) => [m.participant, m.is_me, m.same_tab]);

test('A page and a frame of one tab are told apart from another tab by isMe and sameTab', async () => {
  const id = await artifact();
  const [one, two] = [await tab(), await tab()];
  const page = await joinRoom(one, id, { slot: 'page' });
  const frame = await joinRoom(one, id, { slot: 'frame' });
  const other = await joinRoom(two, id, { slot: 'page' });
  assert.deepEqual(
    [page, frame, other].map((s) => s.members.length),
    [1, 2, 3],
  );
  const seen = async (client, who) =>
    flags(await pollRoom(client, identityOf(id, who), { after: 0 }));
  assert.deepEqual(await seen(one, page), [
    [page.participant, true, true],
    [frame.participant, false, true],
    [other.participant, false, false],
  ]);
  assert.deepEqual(await seen(one, frame), [
    [page.participant, false, true],
    [frame.participant, true, true],
    [other.participant, false, false],
  ]);
  assert.deepEqual(await seen(two, other), [
    [page.participant, false, false],
    [frame.participant, false, false],
    [other.participant, true, true],
  ]);
  assert.equal(page.tab, frame.tab);
  assert.notEqual(page.tab, other.tab);
});

test('A reconnect keeps its participant, refuses the old connection and never duplicates', async () => {
  const id = await artifact();
  const [one, two] = [await tab(), await tab()];
  const first = await joinRoom(one, id, { slot: 'page' });
  const watcher = await joinRoom(two, id, { slot: 'page' });
  const again = await joinRoom(one, id, { slot: 'page', state: 'back' });
  assert.equal(again.participant, first.participant);
  assert.ok(again.epoch > first.epoch);
  assert.equal(again.members.length, 2);
  await assert.rejects(
    pollRoom(one, identityOf(id, first), { after: 0 }),
    (error) => error.code === 'not_found',
  );
  await assert.rejects(
    sendRoomEvent(one, identityOf(id, first), { kind: 'k', data: 'x' }),
    (error) => error.code === 'not_found',
  );
  const seen = await pollRoom(two, identityOf(id, watcher), { after: 0 });
  assert.deepEqual(
    seen.members.map((m) => [m.participant, m.state]),
    [
      [first.participant, 'back'],
      [watcher.participant, ''],
    ],
  );
});

test('A resumed join hands over what came while its participant was away', async () => {
  const id = await artifact();
  const [one, two] = [await tab(), await tab()];
  const sender = await joinRoom(one, id, { slot: 'page' });
  const reader = await joinRoom(two, id, { slot: 'page' });
  await pollRoom(two, identityOf(id, reader), { after: 0 });
  await sendRoomEvent(one, identityOf(id, sender), { kind: 'k', data: 'away' });
  const back = await joinRoom(two, id, { slot: 'page' });
  assert.equal(back.participant, reader.participant);
  assert.deepEqual(
    back.events.map((e) => [e.seq, e.data, e.is_me, e.same_tab]),
    [[1, 'away', false, false]],
  );
  assert.deepEqual([back.head, back.missed], [1, false]);
});

test('An event reaches those present with the flags of their view, and nobody who joins later', async () => {
  const id = await artifact();
  const [one, two, three] = [await tab(), await tab(), await tab()];
  const sender = await joinRoom(one, id, { slot: 'page' });
  const reader = await joinRoom(two, id, { slot: 'page' });
  assert.equal(
    await sendRoomEvent(one, identityOf(id, sender), {
      kind: 'cursor',
      data: 'a',
    }),
    1,
  );
  const read = await pollRoom(two, identityOf(id, reader), { after: 0 });
  assert.deepEqual(
    read.events.map((e) => [e.seq, e.participant, e.data, e.is_me, e.same_tab]),
    [[1, sender.participant, 'a', false, false]],
  );
  const own = await pollRoom(one, identityOf(id, sender), { after: 0 });
  assert.deepEqual(
    own.events.map((e) => [e.is_me, e.same_tab]),
    [[true, true]],
  );
  const late = await joinRoom(three, id, { slot: 'page' });
  assert.equal(late.head, 1);
  assert.deepEqual(late.events, []);
  const asked = await pollRoom(three, identityOf(id, late), { after: 0 });
  assert.deepEqual(
    asked.events,
    [],
    'asking from the beginning replays nothing',
  );
});

test('Closing a tab removes its participants, and a tab cannot act for another', async () => {
  const id = await artifact();
  const [closing, staying] = [await tab(), await tab()];
  const gone = await joinRoom(closing, id, { slot: 'page' });
  const kept = await joinRoom(staying, id, { slot: 'page' });
  // The staying tab names the closing tab's participant and epoch exactly.
  await assert.rejects(
    pollRoom(staying, identityOf(id, gone), { after: 0 }),
    (error) => error.code === 'not_found',
  );
  assert.equal(
    (await pollRoom(staying, identityOf(id, kept), { after: 0 })).members
      .length,
    2,
  );
  await closing.closeRuntimeSession();
  const view = await pollRoom(staying, identityOf(id, kept), { after: 0 });
  assert.deepEqual(flags(view), [[kept.participant, true, true]]);
  await leaveRoom(staying, identityOf(id, kept));
  await leaveRoom(staying, identityOf(id, kept));
});

test('A follower keeps a tab present, hears the others, and rejoins when its participant is gone', async () => {
  // Its own server: a follower polls steadily, and the rooms budget of an
  // actor is shared by every test that uses the same server.
  const own = await startServer(binary, join(scratch, 'follower-data'));
  const stops = new AbortController();
  try {
    const id = await artifact(own);
    const [followed, other] = [await tab(own), await tab(own)];
    /** Resolves when `onSnapshot` has seen a snapshot satisfying a predicate. */
    const waiting = [];
    const snapshots = [];
    const seen = (predicate) =>
      new Promise((resolve) => {
        const hit = snapshots.find(predicate);
        if (hit !== undefined) resolve(hit);
        else waiting.push({ predicate, resolve });
      });
    const following = followRoom(followed, {
      artifactId: id,
      slot: 'page',
      state: () => 'following',
      signal: stops.signal,
      // Slower than the actor's room budget (15 requests a second) so that the
      // follower does not use up what this test's own calls need.
      intervalMs: 150,
      onSnapshot: (item) => {
        snapshots.push(item);
        for (const wait of waiting.splice(0))
          if (wait.predicate(item)) wait.resolve(item);
          else waiting.push(wait);
      },
    });
    const first = await seen(() => true);
    assert.equal(first.joined, true);
    const mine = first.snapshot.participant;

    // Another tab joins, speaks, and is heard by the follower.
    const peer = await joinRoom(other, id, { slot: 'page' });
    const heard = seen((item) => item.snapshot.members.length === 2);
    await sendRoomEvent(other, identityOf(id, peer), {
      kind: 'cursor',
      data: 'hi',
    });
    const withEvent = await seen((item) => item.snapshot.events.length > 0);
    assert.deepEqual(
      withEvent.snapshot.events.map((e) => [e.data, e.is_me, e.same_tab]),
      [['hi', false, false]],
    );
    assert.equal((await heard).snapshot.participant, mine);

    // The follower's participant is removed by another connection of the same
    // tab: it is gone for the follower, which joins again as a new one.
    await leaveRoom(followed, identityOf(id, withEvent.snapshot));
    const rejoined = await seen((item) => item.joined && item !== first);
    assert.notEqual(rejoined.snapshot.participant, mine);
    assert.equal(rejoined.snapshot.members.length, 2);

    stops.abort();
    await following;
    // It left on abort: the peer sees only itself.
    const left = await pollRoom(other, identityOf(id, peer), { after: 0 });
    assert.deepEqual(flags(left), [[peer.participant, true, true]]);
  } finally {
    stops.abort(); // a failed assertion must not leave the follower polling
    await own.stop();
  }
});

test('Every room response the real server sent obeys the published schema', () => {
  for (const [method, path, text] of bodies) {
    const body = JSON.parse(text);
    const validate = path.endsWith('/events')
      ? validateReceipt
      : validateSnapshot;
    assert.ok(
      validate(body),
      `${method} ${path}: ${JSON.stringify(validate.errors)}`,
    );
  }
  assert.ok(bodies.length > 20, `the check saw ${bodies.length} responses`);
});
