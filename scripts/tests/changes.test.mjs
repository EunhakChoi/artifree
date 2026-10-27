import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { copyFile, mkdir, mkdtemp, readFile, rm } from 'node:fs/promises';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { after, before, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import {
  Client,
  StreamableHTTPClientTransport,
} from '@modelcontextprotocol/client';
import { ArtifreeClient } from '../../packages/client/dist/index.js';
import { createAjv } from '../lib/schema.mjs';
import { buildServer, startServer } from '../lib/server-process.mjs';

// The change feed of the real binary through the TypeScript client: an
// agent's commits reach a follower once; a follower crosses a crash and
// restart; a cursor of another database is reset; a commit by another
// process on the same database arrives; shutdown ends open streams.
const root = fileURLToPath(new URL('../../', import.meta.url));
let binary;
let scratch;
let validate;

before(async () => {
  binary = buildServer(root);
  scratch = await mkdtemp(join(tmpdir(), 'artifree-changes-test-'));
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
    change: compile('ArtifactChange'),
    position: compile('FeedPosition'),
  };
});

after(async () => {
  await rm(scratch, { recursive: true, force: true });
});

const page = (text) => ({
  title: 'Feed',
  files: [{ path: 'index.html', encoding: 'utf8', content: text }],
});

const put = (base, content) => ({
  base_revision: base,
  changes: [{ op: 'put', path: 'index.html', encoding: 'utf8', content }],
});

/** A loopback port that was free a moment ago, for restarts at one address. */
async function freePort() {
  const server = createServer();
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const { port } = server.address();
  await new Promise((resolve) => server.close(resolve));
  return port;
}

function clientOf(server) {
  return new ArtifreeClient({
    baseUrl: server.api_url,
    accessToken: server.token,
  });
}

/**
 * Follows the feed and records its events. `next(predicate)` resolves with
 * the first event, already received or still to come, that matches.
 */
function follower(client, options = {}) {
  const controller = new AbortController();
  const events = [];
  const disconnects = [];
  const waiting = new Set();
  const done = client.followChanges({
    retryDelayMs: { min: 100, max: 400 },
    ...options,
    signal: controller.signal,
    onEvent(event) {
      events.push(event);
      for (const waiter of waiting)
        if (waiter.predicate(event)) {
          waiting.delete(waiter);
          waiter.resolve(event);
        }
    },
    onDisconnect(error) {
      disconnects.push(error?.code ?? 'ended');
    },
  });
  return {
    events,
    disconnects,
    stop() {
      controller.abort();
      return done;
    },
    next(predicate, label, timeoutMs = 10_000) {
      const seen = events.find(predicate);
      if (seen !== undefined) return Promise.resolve(seen);
      return new Promise((resolve, reject) => {
        const waiter = { predicate, resolve };
        waiting.add(waiter);
        setTimeout(() => {
          if (waiting.delete(waiter))
            reject(
              new Error(
                `No ${label} in ${timeoutMs} ms: ${JSON.stringify(events)}`,
              ),
            );
        }, timeoutMs);
        done.catch(reject);
      });
    },
  };
}

const isType = (type) => (event) => event.type === type;
const changeOf =
  (id, revision) =>
  ({ type, change }) =>
    type === 'change' &&
    change.artifact_id === id &&
    change.revision === revision;

test("An agent's commits reach a follower once; replays and conflicts do not", async () => {
  const server = await startServer(binary, join(scratch, 'agent'));
  const client = clientOf(server);
  const agent = new Client(
    { name: 'artifree-changes-test', version: '0' },
    { versionNegotiation: { mode: { pin: '2026-07-28' } } },
  );
  const watching = follower(client);
  try {
    const ready = await watching.next(isType('ready'), 'ready');
    assert.ok(
      validate.position({ cursor: ready.cursor }),
      JSON.stringify(validate.position.errors),
    );
    const { artifact } = await client.createArtifact(page('<p>1</p>'), {
      idempotencyKey: 'feed-node-create',
    });
    await agent.connect(
      new StreamableHTTPClientTransport(new URL(`${server.api_url}/mcp`), {
        requestInit: { headers: { authorization: `Bearer ${server.token}` } },
      }),
    );
    const save = (key, base) =>
      agent.callTool({
        name: 'save_revision',
        arguments: {
          artifact_id: artifact.id,
          ...put(base, key),
          idempotency_key: key,
        },
      });
    assert.equal((await save('feed-agent-save', 1)).isError, false);
    // Replays over HTTP and MCP, and a stale save, commit nothing.
    await client.createArtifact(page('<p>1</p>'), {
      idempotencyKey: 'feed-node-create',
    });
    assert.equal(
      (await save('feed-agent-save', 1)).structuredContent.replayed,
      true,
    );
    assert.equal((await save('feed-agent-stale', 1)).isError, true);
    assert.equal((await save('feed-agent-third', 2)).isError, false);

    await watching.next(changeOf(artifact.id, 3), 'revision 3');
    const changes = watching.events
      .filter(isType('change'))
      .map(({ change }) => change);
    assert.deepEqual(
      changes.map((change) => [change.change, change.revision]),
      [
        ['created', 1],
        ['revised', 2],
        ['revised', 3],
      ],
    );
    for (const change of changes) {
      assert.ok(
        validate.change(change),
        JSON.stringify(validate.change.errors),
      );
      assert.notEqual(change.cursor, ready.cursor);
    }
  } finally {
    await agent.close();
    await watching.stop();
    await server.stop();
  }
});

test('A follower crosses a crash and restart without missing a commit', async () => {
  const dataDir = join(scratch, 'restart');
  const listen = ['--api-listen', `127.0.0.1:${await freePort()}`];
  let server = await startServer(binary, dataDir, listen);
  const client = clientOf(server);
  // The reconnection waits long enough for the restarted server to commit
  // before the follower is back, so only resuming can deliver that commit.
  let watching = follower(client, { retryDelayMs: { min: 2000, max: 2000 } });
  try {
    await watching.next(isType('ready'), 'ready');
    const { artifact } = await client.createArtifact(page('<p>1</p>'), {
      idempotencyKey: 'feed-restart-create',
    });
    await watching.next(changeOf(artifact.id, 1), 'the create');

    await server.kill();
    server = await startServer(binary, dataDir, listen);
    await client.saveRevision(artifact.id, put(1, '2'), {
      idempotencyKey: 'feed-restart-save-2',
    });
    await watching.next(changeOf(artifact.id, 2), 'the save after the restart');
    assert.ok(
      watching.disconnects.includes('network_error'),
      'the crash went unnoticed',
    );
    assert.ok(
      !watching.events.some(isType('reset')),
      'the same database was reset',
    );

    // Commits made while nobody follows arrive when following resumes.
    const { cursor } = watching.events.at(-1).change;
    await watching.stop();
    await client.saveRevision(artifact.id, put(2, '3'), {
      idempotencyKey: 'feed-restart-save-3',
    });
    watching = follower(client, { after: cursor });
    await watching.next(changeOf(artifact.id, 3), 'the missed save');
    assert.deepEqual(
      watching.events.map(({ type }) => type),
      ['change'],
    );
  } finally {
    await watching.stop();
    await server.stop();
  }
});

test('A cursor of another database is reset, even below its head', async () => {
  const listen = ['--api-listen', `127.0.0.1:${await freePort()}`];
  const first = join(scratch, 'reset-first');
  let server = await startServer(binary, first, listen);
  // The follower is back only after the other database has passed its
  // cursor, so the sequence alone cannot reveal the switch.
  const watching = follower(clientOf(server), {
    retryDelayMs: { min: 2000, max: 2000 },
  });
  try {
    await watching.next(isType('ready'), 'ready');
    await clientOf(server).createArtifact(page('<p>1</p>'), {
      idempotencyKey: 'feed-reset-create',
    });
    await watching.next(isType('change'), 'the create');
    await server.stop();

    // Another database at the same address and with the same token.
    const second = join(scratch, 'reset-second');
    await mkdir(second, { recursive: true, mode: 0o700 });
    await copyFile(join(first, 'access-token'), join(second, 'access-token'));
    server = await startServer(binary, second, listen);
    const other = clientOf(server);
    const { artifact } = await other.createArtifact(page('<p>a</p>'), {
      idempotencyKey: 'feed-other-create',
    });
    await other.saveRevision(artifact.id, put(1, 'b'), {
      idempotencyKey: 'feed-other-save',
    });
    const reset = await watching.next(isType('reset'), 'a reset');
    assert.ok(validate.position({ cursor: reset.cursor }));
    assert.deepEqual(
      watching.events.map(({ type }) => type),
      ['ready', 'change', 'reset'],
      'changes of another database followed the old cursor',
    );
  } finally {
    await watching.stop();
    await server.stop();
  }
});

test("Another process's commit to the same database arrives within the poll interval", async () => {
  const dataDir = join(scratch, 'shared');
  const reader = await startServer(binary, dataDir);
  // A second server on the same database stands in for any other writer.
  const writer = await startServer(binary, dataDir);
  const watching = follower(clientOf(reader));
  try {
    await watching.next(isType('ready'), 'ready');
    const { artifact } = await clientOf(writer).createArtifact(
      page('<p>1</p>'),
      {
        idempotencyKey: 'feed-other-process',
      },
    );
    await watching.next(
      changeOf(artifact.id, 1),
      'the other process commit',
      20_000,
    );
  } finally {
    await watching.stop();
    await writer.stop();
    await reader.stop();
  }
});

// Node can send a graceful stop signal to a child only on Unix; on Windows
// `kill` terminates it. Closing streams is also tested in process by
// crates/artifree-server/tests/change_feed.rs on every platform.
test(
  'Shutdown ends open streams instead of waiting for them',
  {
    skip:
      process.platform === 'win32' &&
      'Windows cannot send a child process a graceful stop signal',
  },
  async () => {
    const server = await startServer(binary, join(scratch, 'shutdown'));
    const watching = follower(clientOf(server));
    try {
      await watching.next(isType('ready'), 'ready');
      const code = await Promise.race([
        server.stop(),
        delay(5000).then(() => 'still running'),
      ]);
      assert.equal(
        code,
        0,
        `shutdown waited for the open stream ${server.stderr()}`,
      );
    } finally {
      await watching.stop();
      await server.kill();
    }
  },
);

test('An invalid stream limit stops the server before it listens', () => {
  const result = spawnSync(
    binary,
    [
      'serve',
      '--data-dir',
      join(scratch, 'bad-streams'),
      '--api-listen',
      '127.0.0.1:0',
      '--content-listen',
      '127.0.0.1:0',
      '--change-streams',
      '0',
    ],
    { encoding: 'utf8', timeout: 20_000 },
  );
  assert.equal(result.status, 1);
  assert.equal(result.stdout, '');
  assert.match(
    result.stderr,
    /--change-streams must be a whole number from 1 to 100000/,
  );
});
