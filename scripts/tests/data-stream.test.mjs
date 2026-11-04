import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, before, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import {
  ArtifreeClient,
  followData,
  listDataMembers,
} from '../../packages/client/dist/index.js';
import { buildServer, startServer } from '../lib/server-process.mjs';

// Decision 0040 end to end: the TypeScript client against the real server
// binary. The unit tests of the client use a scripted fetch; the Rust tests
// prove the server. This file proves that the two agree on the wire.
const root = fileURLToPath(new URL('../../', import.meta.url));
let binary;
let scratch;

before(async () => {
  binary = buildServer(root);
  scratch = await mkdtemp(join(tmpdir(), 'artifree-data-stream-test-'));
});

after(async () => {
  await rm(scratch, { recursive: true, force: true });
});

let keys = 0;
const key = () => ({ idempotencyKey: `data-stream-test-${(keys += 1)}` });

async function withServer(name, run) {
  const server = await startServer(binary, join(scratch, name));
  const client = new ArtifreeClient({
    baseUrl: server.api_url,
    accessToken: server.token,
  });
  try {
    await run(client);
  } finally {
    await server.stop();
  }
}

const page = {
  title: 'Data',
  files: [{ path: 'index.html', encoding: 'utf8', content: '<p>x</p>' }],
};

/** Follows a path and lets a test wait for the events it has seen so far. */
function follower(client, id, path) {
  const controller = new AbortController();
  const events = [];
  const waiting = [];
  const done = followData(client, {
    artifactId: id,
    path,
    signal: controller.signal,
    retryDelayMs: { min: 50, max: 200 },
    onEvent(event) {
      events.push(event);
      const ready = waiting.filter((waiter) => events.length >= waiter.count);
      for (const waiter of ready) {
        waiting.splice(waiting.indexOf(waiter), 1);
        waiter.resolve();
      }
    },
  });
  return {
    events,
    done,
    stop() {
      controller.abort();
      return done;
    },
    async reached(count) {
      if (events.length >= count) return;
      await Promise.race([
        new Promise((resolve) => waiting.push({ count, resolve })),
        done.then(() => {
          throw new Error(`Following ended after ${events.length} events.`);
        }),
        new Promise((_, reject) =>
          setTimeout(
            () =>
              reject(
                new Error(
                  `${count} events not seen in 15 s: ${JSON.stringify(events)}`,
                ),
              ),
            15_000,
          ),
        ),
      ]);
    },
  };
}

test('members pages chain through the client in the order of UTF-8 bytes', async () => {
  await withServer('members', async (client) => {
    const { artifact } = await client.createArtifact(page, key());
    // U+FF5E is EF BD 9E and U+10000 is F0 90 80 80: bytes put the first
    // first, UTF-16 the second.
    const names = ['a', 'b', 'c', '～', '\u{10000}', 'z'];
    let version = 0;
    for (const name of names) {
      const written = await client.setData(
        artifact.id,
        {
          path: '',
          value: {
            ...(await client.getData(artifact.id)).value,
            [name]: { n: name },
          },
          base_data_version: version,
        },
        key(),
      );
      version = written.data_version;
    }
    const seen = [];
    let after;
    let pages = 0;
    do {
      const result = await listDataMembers(client, artifact.id, {
        select: 'values',
        limit: 2,
        ...(after === undefined ? {} : { after }),
      });
      assert.equal(result.data_version, version);
      assert.equal(result.limit, 2);
      seen.push(...result.members.map((member) => member.name));
      after = result.next_after;
      pages += 1;
    } while (after !== undefined);
    assert.deepEqual(seen, ['a', 'b', 'c', 'z', '～', '\u{10000}']);
    assert.equal(pages, 3);
    const deep = await listDataMembers(client, artifact.id, { path: 'a' });
    assert.equal(deep.exists, true);
    assert.equal(deep.collection, true);
    assert.deepEqual(deep.members, [{ name: 'n' }]);
    const none = await listDataMembers(client, artifact.id, { path: 'nope' });
    assert.deepEqual(
      [none.exists, none.collection, none.members],
      [false, false, []],
    );
  });
});

test('a following client gets a snapshot and then the new full value of each change', async () => {
  await withServer('follow', async (client) => {
    const { artifact } = await client.createArtifact(page, key());
    const watched = follower(client, artifact.id, 'todos');
    try {
      await watched.reached(1);
      assert.equal(watched.events[0].type, 'snapshot');
      assert.equal(watched.events[0].data.exists, false);
      assert.equal(watched.events[0].data.data_version, 0);
      const first = await client.setData(
        artifact.id,
        { path: 'todos', value: { first: 1 }, base_data_version: 0 },
        key(),
      );
      await watched.reached(2);
      assert.equal(watched.events[1].type, 'change');
      assert.deepEqual(watched.events[1].data.value, { first: 1 });
      assert.equal(watched.events[1].data.data_version, first.data_version);
      // A write elsewhere moves the version but not this path's value.
      const second = await client.setData(
        artifact.id,
        { path: 'other', value: 1, base_data_version: first.data_version },
        key(),
      );
      await watched.reached(3);
      assert.equal(watched.events[2].data.data_version, second.data_version);
      assert.deepEqual(watched.events[2].data.value, { first: 1 });
    } finally {
      await watched.stop();
    }
  });
});

test('moving the artifact to the trash ends the stream and the client stops with not_found', async () => {
  await withServer('trash', async (client) => {
    const { artifact } = await client.createArtifact(page, key());
    const watched = follower(client, artifact.id, '');
    await watched.reached(1);
    await client.updateArtifact(
      artifact.id,
      { base_metadata_revision: artifact.metadata_revision, deleted: true },
      key(),
    );
    await assert.rejects(watched.done, (error) => {
      assert.equal(error.code, 'not_found');
      return true;
    });
    assert.equal(watched.events.length, 1);
  });
});

test('a path whose value is over 128 KiB ends following with too_large', async () => {
  await withServer('large', async (client) => {
    const { artifact } = await client.createArtifact(page, key());
    // 140,000 characters: over the 128 KiB a stream sends, under the 256 KiB a value may hold.
    const big = 'x'.repeat(140_000);
    const written = await client.setData(
      artifact.id,
      { path: 'big', value: big, base_data_version: 0 },
      key(),
    );
    const events = [];
    const controller = new AbortController();
    await assert.rejects(
      followData(client, {
        artifactId: artifact.id,
        path: 'big',
        signal: controller.signal,
        onEvent: (event) => events.push(event),
      }),
      (error) => {
        assert.equal(error.name, 'DataTooLargeError');
        assert.equal(error.dataVersion, written.data_version);
        return true;
      },
    );
    assert.deepEqual(
      events.map((event) => event.type),
      ['too_large'],
    );
  });
});

test('the server refuses a data stream it cannot serve and the client does not retry', async () => {
  await withServer('refused', async (client) => {
    const missing = `art_${'0'.repeat(32)}`;
    const controller = new AbortController();
    await assert.rejects(
      followData(client, {
        artifactId: missing,
        signal: controller.signal,
        onEvent: () => assert.fail('no event'),
      }),
      (error) => error.code === 'not_found',
    );
    await assert.rejects(
      listDataMembers(client, missing),
      (error) => error.code === 'not_found',
    );
  });
});
