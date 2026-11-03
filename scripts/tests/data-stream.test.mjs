import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, before, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import {
  ArtifreeApiError,
  ArtifreeClient,
  followData,
  listDataMembers,
} from '../../packages/client/dist/index.js';
import { createAjv } from '../lib/schema.mjs';
import { buildServer, freePort, startServer } from '../lib/server-process.mjs';

// Data queries and subscriptions (decision 0040) end to end: the real binary,
// the TypeScript client, and every response checked against the published
// schema. A page is listed by cursor, a follower receives a snapshot and then
// the newest state, and after the server is killed and started again on the
// same database a new connection begins with a new snapshot.
const root = fileURLToPath(new URL('../../', import.meta.url));
let binary;
let scratch;
let validate;

before(async () => {
  binary = buildServer(root);
  scratch = await mkdtemp(join(tmpdir(), 'artifree-data-stream-test-'));
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
    state: compile('AppData'),
  };
});

after(async () => {
  await rm(scratch, { recursive: true, force: true });
});

const page = {
  title: 'Data',
  files: [{ path: 'index.html', encoding: 'utf8', content: '<p>1</p>' }],
};

function clientOf(server) {
  return new ArtifreeClient({
    baseUrl: server.api_url,
    accessToken: server.token,
  });
}

/** Follows a path and lets a test wait for the event it expects. */
function follower(server, artifactId, path) {
  const controller = new AbortController();
  const events = [];
  const waiting = new Set();
  const done = followData(
    { baseUrl: server.api_url, accessToken: server.token },
    artifactId,
    {
      path,
      signal: controller.signal,
      onEvent(event) {
        events.push(event);
        for (const waiter of waiting)
          if (waiter.predicate(event)) {
            waiting.delete(waiter);
            waiter.resolve(event);
          }
      },
    },
  );
  return {
    events,
    stop() {
      controller.abort();
      return done;
    },
    next(predicate, label, timeoutMs = 20_000) {
      const seen = events.find(predicate);
      if (seen !== undefined) return Promise.resolve(seen);
      return new Promise((resolve, reject) => {
        const waiter = { predicate, resolve };
        waiting.add(waiter);
        setTimeout(() => {
          if (waiting.delete(waiter))
            reject(new Error(`No ${label} in ${timeoutMs} ms`));
        }, timeoutMs);
        done.catch(reject);
      });
    },
  };
}

test('Members are listed by cursor through the client, and every page matches the schema', async () => {
  const server = await startServer(binary, join(scratch, 'members'));
  const client = clientOf(server);
  try {
    const { artifact } = await client.createArtifact(page, {
      idempotencyKey: 'data-members-create',
    });
    const names = ['B', 'a', 'ab', 'é', '～', '\u{10000}'];
    await client.setData(
      artifact.id,
      {
        path: '',
        base_data_version: 0,
        value: {
          list: Object.fromEntries(names.map((name, index) => [name, index])),
        },
      },
      { idempotencyKey: 'data-members-set' },
    );
    const seen = [];
    let after;
    do {
      const result = await listDataMembers(client, artifact.id, {
        path: 'list',
        select: 'values',
        limit: 4,
        ...(after === undefined ? {} : { after }),
      });
      assert.ok(
        validate.members(result),
        JSON.stringify(validate.members.errors),
      );
      assert.equal(result.data_version, 1);
      seen.push(...result.members.map((member) => member.name));
      after = result.next_after;
    } while (after !== undefined);
    assert.deepEqual(seen, names, 'UTF-8 byte order, each name once');
    const none = await listDataMembers(client, artifact.id, { path: 'list/a' });
    assert.equal(none.collection, false);
    await assert.rejects(
      listDataMembers(client, 'art_00000000000000000000000000000000'),
      (error) =>
        error instanceof ArtifreeApiError && error.code === 'not_found',
    );
  } finally {
    await server.stop();
  }
});

test('A follower gets a snapshot, then the newest state, and a fresh snapshot after a restart', async () => {
  const dataDir = join(scratch, 'follow');
  const listen = ['--api-listen', `127.0.0.1:${await freePort()}`];
  let server = await startServer(binary, dataDir, listen);
  const client = clientOf(server);
  let watching;
  try {
    const { artifact } = await client.createArtifact(page, {
      idempotencyKey: 'data-follow-create',
    });
    watching = follower(server, artifact.id, 'score');
    const snapshot = await watching.next(
      (event) => event.type === 'snapshot',
      'a snapshot',
    );
    assert.ok(validate.state(snapshot.data));
    assert.equal(snapshot.data.exists, false);
    assert.equal(snapshot.data.data_version, 0);

    await client.setData(
      artifact.id,
      { path: 'score', base_data_version: 0, value: 10 },
      { idempotencyKey: 'data-follow-1' },
    );
    const first = await watching.next(
      (event) => event.type === 'change' && event.data.data_version === 1,
      'the first change',
    );
    assert.equal(first.data.value, 10);
    assert.ok(validate.state(first.data));

    // The server dies; a write is made after it is back, and the follower
    // reconnects by itself and is told where things stand.
    await server.kill();
    server = await startServer(binary, dataDir, listen);
    const restarted = clientOf(server);
    await restarted.setData(
      artifact.id,
      { path: 'score', base_data_version: 1, value: 11 },
      { idempotencyKey: 'data-follow-2' },
    );
    const latest = await watching.next(
      (event) => event.data.data_version === 2,
      'the write made after the restart',
    );
    assert.equal(latest.data.value, 11);
    // The first event of the new connection is a snapshot, whether it saw
    // the write or the write followed it; nothing was replayed.
    const types = watching.events.map((event) => event.type);
    assert.deepEqual(types.slice(0, 3), ['snapshot', 'change', 'snapshot']);
    assert.ok(
      watching.events[2].data.data_version >= 1,
      'the snapshot after the restart names where things stand',
    );
    assert.equal(
      watching.events.filter((event) => event.data.data_version === 0).length,
      1,
      'version 0 was replayed',
    );
  } finally {
    await watching?.stop();
    await server.stop();
  }
});
