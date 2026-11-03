import assert from 'node:assert/strict';
import { readFile, mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { after, before, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import {
  ArtifreeApiError,
  ArtifreeClient,
  applyDataEvent,
  createUser,
  followData,
  listDataMembers,
  newIdempotencyKey,
  openData,
  setUserDisabled,
} from '../../packages/client/dist/index.js';
import { createAjv } from '../lib/schema.mjs';
import { buildServer, freePort, startServer } from '../lib/server-process.mjs';

// Data queries and subscriptions (decision 0038) of the real binary through
// the TypeScript client: a follower mirrors the server's value across every
// kind of write, pages list what the space holds, a follower crosses a crash
// and a restart and is reset exactly when the server cannot continue its
// cursor, and a user cannot see or keep a stream on another user's data.
const root = fileURLToPath(new URL('../../', import.meta.url));
let binary;
let scratch;
let validate;

before(async () => {
  binary = buildServer(root);
  scratch = await mkdtemp(join(tmpdir(), 'artifree-data-test-'));
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
  validate = {
    members: compile('DataMembers'),
    snapshot: compile('DataSnapshotEvent'),
    change: compile('DataChangeEvent'),
    position: compile('DataPosition'),
  };
});

after(async () => {
  await rm(scratch, { recursive: true, force: true });
});

const page = {
  title: 'Data',
  files: [{ path: 'index.html', encoding: 'utf8', content: '<p>x</p>' }],
};

function clientOf(server, accessToken = server.token) {
  return new ArtifreeClient({ baseUrl: server.api_url, accessToken });
}

async function artifactOf(client) {
  const { artifact } = await client.createArtifact(page, {
    idempotencyKey: newIdempotencyKey(),
  });
  return artifact.id;
}

/** Writes to one artifact's data, tracking the version like an application. */
function writer(client, id) {
  let version = 0;
  const done = (reply) => {
    version = reply.data_version;
    return version;
  };
  const options = () => ({ idempotencyKey: newIdempotencyKey() });
  return {
    set: async (path, value) =>
      done(
        await client.setData(
          id,
          { path, value, base_data_version: version },
          options(),
        ),
      ),
    patch: async (path, patch) =>
      done(
        await client.updateData(
          id,
          { path, patch, base_data_version: version },
          options(),
        ),
      ),
    remove: async (path) =>
      done(
        await client.deleteData(
          id,
          { path, base_data_version: version },
          options(),
        ),
      ),
  };
}

/**
 * Follows a path and mirrors it with the reducer. `waitFor(predicate)`
 * resolves with the view once `predicate(view, events)` holds after an event
 * (or at once); `reach(version)` waits for a data version.
 */
function watcher(client, id, options = {}) {
  const controller = new AbortController();
  const events = [];
  const disconnects = [];
  const waiting = new Set();
  let view;
  const done = followData(client, id, {
    retryDelayMs: { min: 100, max: 400 },
    ...options,
    signal: controller.signal,
    onEvent(event) {
      events.push(event);
      view = applyDataEvent(view, event);
      for (const waiter of waiting)
        if (waiter.predicate(view, events)) {
          waiting.delete(waiter);
          waiter.resolve(view);
        }
    },
    onDisconnect(error) {
      disconnects.push(error?.code ?? 'ended');
    },
  });
  done.catch(() => undefined);
  const waitFor = (predicate, label, timeoutMs = 10_000) => {
    if (view !== undefined && predicate(view, events))
      return Promise.resolve(view);
    return new Promise((resolve, reject) => {
      const waiter = { predicate, resolve };
      waiting.add(waiter);
      const timer = setTimeout(() => {
        if (waiting.delete(waiter))
          reject(
            new Error(
              `No ${label} in ${timeoutMs} ms: ${JSON.stringify(events)}`,
            ),
          );
      }, timeoutMs);
      timer.unref();
      done.catch(reject);
    });
  };
  return {
    events,
    disconnects,
    done,
    view: () => view,
    waitFor,
    reach: (version) =>
      waitFor((seen) => seen.dataVersion >= version, `data version ${version}`),
    stop() {
      controller.abort();
      return done;
    },
  };
}

/** The shape every event of a stream must have, by its schema. */
function assertContract(events) {
  for (const { type, data } of events) {
    const check = {
      snapshot: validate.snapshot,
      reset: validate.snapshot,
      change: validate.change,
      position: validate.position,
    }[type];
    assert.ok(check(data), `${type}: ${JSON.stringify(check.errors)}`);
  }
}

const SIMPLE = { retryDelayMs: { min: 100, max: 400 } };

test('A follower mirrors the value across set, merge patch, and remove, as a read reports it', async () => {
  const server = await startServer(binary, join(scratch, 'mirror'));
  const client = clientOf(server);
  const id = await artifactOf(client);
  const write = writer(client, id);
  const whole = watcher(client, id);
  const part = watcher(client, id, { path: 'todos/t2' });
  try {
    // Both streams are open before the first write, so each write is an event.
    await whole.reach(0);
    await part.reach(0);
    await write.set('todos/t1', { title: 'one', done: false });
    await write.patch('todos/t1', { done: true, note: 'x' });
    await write.set('todos/t2', { title: 'two', tags: ['a'] });
    await write.patch('todos/t1', { note: null, extra: { deep: [1] } });
    await write.set('stored/null', null);
    await write.remove('todos/t1/title');
    await write.set('__proto__/inert', 1);
    const last = await write.patch('todos/t2', { done: false, tags: null });

    const view = await whole.reach(last);
    const read = await client.getData(id);
    assert.equal(read.data_version, last);
    assert.deepEqual(view.value, read.value);
    assert.equal(view.dataVersion, read.data_version);
    assert.equal(Object.getPrototypeOf(view.value), Object.prototype);

    // The follower of one task sees only what touches it, and ends equal.
    const mine = await part.reach(last);
    const task = await client.getData(id, 'todos/t2');
    assert.deepEqual(mine.value, task.value);
    assert.deepEqual(
      part.events.map(({ type }) => type).filter((type) => type !== 'position'),
      ['snapshot', 'change', 'change'],
      'writes that do not touch the path were sent',
    );
    assertContract(whole.events);
    assertContract(part.events);
    assert.deepEqual(
      whole.events.map(({ type }) => type),
      ['snapshot', ...Array(8).fill('change')],
      'one change per write, once, after the snapshot',
    );

    // Removing what is watched leaves nothing, and a read agrees.
    const removed = await write.remove('todos/t2');
    assert.equal((await part.reach(removed)).exists, false);
    assert.equal((await client.getData(id, 'todos/t2')).exists, false);
  } finally {
    await whole.stop();
    await part.stop();
    await server.stop();
  }
});

test('Pages list the members by name bytes and a cursor walks them without repeats', async () => {
  const server = await startServer(binary, join(scratch, 'members'));
  const client = clientOf(server);
  const id = await artifactOf(client);
  const write = writer(client, id);
  try {
    const names = ['b2', 'a1', 'a10', 'a2', 'é', 'c', 'B'];
    await write.set('items', Object.fromEntries(names.map((n, i) => [n, i])));
    const walked = [];
    let after;
    let pages = 0;
    do {
      const result = await listDataMembers(client, id, {
        path: 'items',
        limit: 3,
        select: 'values',
        ...(after === undefined ? {} : { after }),
      });
      assert.ok(
        validate.members(result),
        JSON.stringify(validate.members.errors),
      );
      assert.equal(result.canonical.data_version, 1);
      walked.push(...result.canonical.members);
      after = result.canonical.next_after;
      pages += 1;
    } while (after !== undefined);
    assert.equal(pages, 3);
    assert.deepEqual(
      walked.map(({ name }) => name),
      ['B', 'a1', 'a10', 'a2', 'b2', 'c', 'é'],
      'ordered by UTF-8 bytes: capitals, then ASCII, then multi-byte',
    );
    assert.deepEqual(
      walked.map(({ value }) => value),
      [6, 1, 2, 3, 0, 5, 4],
    );

    const prefixed = await listDataMembers(client, id, {
      path: 'items',
      prefix: 'a',
    });
    assert.deepEqual(
      prefixed.canonical.members.map(({ name }) => name),
      ['a1', 'a10', 'a2'],
    );
    const nothing = await listDataMembers(client, id, { path: 'nowhere' });
    assert.deepEqual(nothing.canonical, {
      data_version: 1,
      exists: false,
      members: [],
    });
    const error = await listDataMembers(client, id, { limit: 101 }).catch(
      (failure) => failure,
    );
    assert.ok(error instanceof ArtifreeApiError);
    assert.deepEqual([error.status, error.code], [422, 'invalid_request']);
  } finally {
    await server.stop();
  }
});

test('A follower crosses a crash and a restart; the server resets it only when it cannot continue', async () => {
  const dataDir = join(scratch, 'restart');
  const listen = ['--api-listen', `127.0.0.1:${await freePort()}`];
  let server = await startServer(binary, dataDir, listen);
  const client = clientOf(server);
  const id = await artifactOf(client);
  const write = writer(client, id);
  const watching = watcher(client, id, SIMPLE);
  try {
    await write.set('n', 1);
    const v2 = await write.set('n', 2);
    await watching.reach(v2);

    // Nothing was written during the crash: the cursor continues.
    await server.kill();
    server = await startServer(binary, dataDir, listen);
    // Write only once the follower is back, or the cursor would be behind a
    // write that the restart made unreplayable.
    await watching.waitFor(
      (_, events) => events.at(-1).type === 'position',
      'the position that continues the cursor',
    );
    const v3 = await write.set('n', 3);
    const view = await watching.reach(v3);
    assert.deepEqual(view.value, { n: 3 });
    assert.ok(watching.disconnects.includes('network_error'));
    assert.ok(
      !watching.events.some(({ type }) => type === 'reset'),
      'a cursor that could be continued was reset',
    );

    // A write made while nobody follows, then a restart that loses the
    // window: the old cursor is behind a write and gets a reset.
    const stopped = watching.view().cursor;
    await watching.stop();
    const v4 = await write.set('n', 4);
    await server.stop();
    server = await startServer(binary, dataDir, listen);
    const resumed = watcher(client, id, { ...SIMPLE, after: stopped });
    try {
      const mirrored = await resumed.reach(v4);
      assert.deepEqual(
        resumed.events.map(({ type }) => type),
        ['reset'],
      );
      assertContract(resumed.events);
      assert.deepEqual(mirrored.value, (await client.getData(id)).value);
    } finally {
      await resumed.stop();
    }
  } finally {
    await watching.stop();
    await server.stop();
  }
});

test('A cursor from the change feed is refused rather than followed', async () => {
  const server = await startServer(binary, join(scratch, 'cursors'));
  const client = clientOf(server);
  const id = await artifactOf(client);
  try {
    const feed = await client.openChanges();
    const ready = (await feed.next()).value;
    await feed.return(undefined);
    const error = await openData(client, id, { after: ready.cursor }).catch(
      (failure) => failure,
    );
    assert.ok(error instanceof ArtifreeApiError, String(error));
    assert.deepEqual([error.status, error.code], [400, 'invalid_request']);
  } finally {
    await server.stop();
  }
});

test('A caller holds eight data streams; aborting one frees its place', async () => {
  const server = await startServer(binary, join(scratch, 'limit'));
  const client = clientOf(server);
  const id = await artifactOf(client);
  const aborts = Array.from({ length: 8 }, () => new AbortController());
  try {
    for (const controller of aborts) {
      const events = await openData(client, id, { signal: controller.signal });
      assert.equal((await events.next()).value.type, 'snapshot');
    }
    const error = await openData(client, id).catch((failure) => failure);
    assert.ok(error instanceof ArtifreeApiError, String(error));
    assert.deepEqual([error.status, error.code], [429, 'rate_limited']);
    assert.equal(error.retryAfterSeconds, 5);

    aborts[0].abort();
    // The server notices the closed connection on its own schedule.
    let reopened;
    for (
      let attempt = 0;
      attempt < 50 && reopened === undefined;
      attempt += 1
    ) {
      try {
        reopened = await openData(client, id);
      } catch (failure) {
        assert.equal(failure.code, 'rate_limited');
        await delay(100);
      }
    }
    assert.ok(reopened, 'the place of an aborted stream was never freed');
    assert.equal((await reopened.next()).value.type, 'snapshot');
    await reopened.return(undefined);
  } finally {
    for (const controller of aborts) controller.abort();
    await server.stop();
  }
});

test("A user cannot read or follow another user's data, and disabling the user ends the stream", async () => {
  const server = await startServer(binary, join(scratch, 'users'));
  const owner = clientOf(server);
  const alice = await createUser(owner, { label: 'Alice' });
  const bob = await createUser(owner, { label: 'Bob' });
  const secretOf = async (user) =>
    (await owner.createToken({ name: `${user.label}-token`, user_id: user.id }))
      .secret;
  const aliceClient = clientOf(server, await secretOf(alice));
  const bobClient = clientOf(server, await secretOf(bob));
  const aliceArtifact = await artifactOf(aliceClient);
  const bobArtifact = await artifactOf(bobClient);
  const aliceWrites = writer(aliceClient, aliceArtifact);
  const bobWrites = writer(bobClient, bobArtifact);
  const hers = watcher(aliceClient, aliceArtifact);
  const his = watcher(bobClient, bobArtifact);
  try {
    await aliceWrites.set('secret', 'alice-only');
    await bobWrites.set('mine', 1);

    // What is refused is the same as what does not exist.
    const missing = `art_${'f'.repeat(32)}`;
    const refusals = [];
    for (const target of [aliceArtifact, missing]) {
      const listing = await listDataMembers(bobClient, target).catch(
        (failure) => failure,
      );
      const stream = await openData(bobClient, target).catch(
        (failure) => failure,
      );
      for (const error of [listing, stream]) {
        assert.ok(error instanceof ArtifreeApiError, String(error));
        refusals.push([
          error.status,
          error.code,
          JSON.stringify(error.body).replaceAll(target, 'ID'),
        ]);
      }
    }
    assert.equal(refusals[0][0], 404);
    assert.deepEqual(
      refusals.slice(0, 2),
      refusals.slice(2),
      'a user could tell',
    );
    assert.deepEqual(refusals[0], refusals[1]);

    // Bob's follower never saw Alice's write; Alice's saw only her own.
    assert.deepEqual((await his.reach(1)).value, { mine: 1 });
    assert.deepEqual((await hers.reach(1)).value, { secret: 'alice-only' });

    await setUserDisabled(owner, alice.id, true);
    const error = await hers.done.then(
      () => undefined,
      (failure) => failure,
    );
    assert.ok(error instanceof ArtifreeApiError, String(error));
    assert.deepEqual([error.status, error.code], [401, 'unauthenticated']);

    // Bob's stream goes on.
    const next = await bobWrites.set('more', 2);
    assert.deepEqual((await his.reach(next)).value, { mine: 1, more: 2 });
  } finally {
    // Her following already ended with the failure asserted above.
    await hers.stop().catch(() => undefined);
    await his.stop();
    await server.stop();
  }
});
