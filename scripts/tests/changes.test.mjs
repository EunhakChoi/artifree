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

// The committed change feed on the real binary through the TypeScript
// client: agent commits reach a follower once, a follower resumes after a
// crash and restart without missing commits, a cursor from another database
// is reset, and shutdown ends open streams.
const root = fileURLToPath(new URL('../../', import.meta.url));
let binary;
let scratch;
let validateChange;

before(async () => {
  binary = buildServer(root);
  scratch = await mkdtemp(join(tmpdir(), 'artifree-changes-test-'));
  const schema = JSON.parse(
    await readFile(
      join(root, 'contracts/generated/api-v1.schema.json'),
      'utf8',
    ),
  );
  validateChange = createAjv().compile({
    $schema: schema.$schema,
    $defs: schema.$defs,
    $ref: '#/$defs/ArtifactChange',
  });
});

after(async () => {
  await rm(scratch, { recursive: true, force: true });
});

const page = (text) => ({
  title: 'Feed',
  files: [{ path: 'index.html', encoding: 'utf8', content: text }],
});

/** A loopback port that was free a moment ago, for restarts at one address. */
async function freePort() {
  const server = createServer();
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const { port } = server.address();
  await new Promise((resolve) => server.close(resolve));
  return port;
}

/**
 * Follows the feed and collects its events. `next(predicate)` resolves with
 * the first event, received or still to come, that matches.
 */
function follower(client, options = {}) {
  const controller = new AbortController();
  const events = [];
  const disconnects = [];
  const waiting = [];
  const done = client.followChanges({
    retryDelayMs: { min: 100, max: 400 },
    ...options,
    signal: controller.signal,
    onEvent(event) {
      events.push(event);
      for (const waiter of waiting.filter(({ predicate }) =>
        predicate(event),
      )) {
        waiting.splice(waiting.indexOf(waiter), 1);
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
    done,
    stop() {
      controller.abort();
      return done;
    },
    next(predicate, label = 'event') {
      const seen = events.find(predicate);
      if (seen !== undefined) return Promise.resolve(seen);
      return new Promise((resolve, reject) => {
        const waiter = { predicate, resolve };
        waiting.push(waiter);
        setTimeout(() => {
          if (waiting.includes(waiter))
            reject(new Error(`No ${label} in 10 s: ${JSON.stringify(events)}`));
        }, 10_000);
      });
    },
  };
}

const changeOf =
  (id, revision) =>
  ({ type, change }) =>
    type === 'change' &&
    change.artifact_id === id &&
    change.revision === revision;

test("An agent's commits reach a follower once; replays and conflicts do not", async () => {
  const server = await startServer(binary, join(scratch, 'agent'));
  const client = new ArtifreeClient({
    baseUrl: server.api_url,
    accessToken: server.token,
  });
  const agent = new Client(
    { name: 'artifree-changes-test', version: '0' },
    { versionNegotiation: { mode: { pin: '2026-07-28' } } },
  );
  const watching = follower(client);
  try {
    const ready = await watching.next((event) => event.type === 'ready');
    assert.deepEqual(ready, { type: 'ready', cursor: 0 });
    const created = await client.createArtifact(page('<p>1</p>'), {
      idempotencyKey: 'feed-node-create',
    });
    const id = created.artifact.id;
    await agent.connect(
      new StreamableHTTPClientTransport(new URL(`${server.api_url}/mcp`), {
        requestInit: { headers: { authorization: `Bearer ${server.token}` } },
      }),
    );
    const save = (key, base) =>
      agent.callTool({
        name: 'save_revision',
        arguments: {
          artifact_id: id,
          base_revision: base,
          changes: [
            { op: 'put', path: 'index.html', encoding: 'utf8', content: key },
          ],
          idempotency_key: key,
        },
      });
    assert.equal((await save('feed-agent-save', 1)).isError, false);
    // A replay over HTTP and MCP, and a stale save, commit nothing.
    await client.createArtifact(page('<p>1</p>'), {
      idempotencyKey: 'feed-node-create',
    });
    assert.equal(
      (await save('feed-agent-save', 1)).structuredContent.replayed,
      true,
    );
    assert.equal((await save('feed-agent-stale', 1)).isError, true);
    assert.equal((await save('feed-agent-third', 2)).isError, false);

    await watching.next(changeOf(id, 3), 'revision 3');
    const changes = watching.events.filter((event) => event.type === 'change');
    assert.deepEqual(
      changes.map(({ change }) => [
        change.cursor,
        change.change,
        change.revision,
      ]),
      [
        [1, 'created', 1],
        [2, 'revised', 2],
        [3, 'revised', 3],
      ],
    );
    for (const { change } of changes)
      assert.ok(validateChange(change), JSON.stringify(validateChange.errors));
  } finally {
    await agent.close();
    await watching.stop();
    await server.stop();
  }
});

test('A follower resumes after a crash and restart without missing commits', async () => {
  const dataDir = join(scratch, 'restart');
  const port = await freePort();
  const listen = ['--api-listen', `127.0.0.1:${port}`];
  let server = await startServer(binary, dataDir, listen);
  const client = new ArtifreeClient({
    baseUrl: server.api_url,
    accessToken: server.token,
  });
  // The first reconnection waits long enough for the restarted server to
  // commit before the follower is back.
  let watching = follower(client, { retryDelayMs: { min: 2000, max: 2000 } });
  try {
    await watching.next((event) => event.type === 'ready');
    const { artifact } = await client.createArtifact(page('<p>1</p>'), {
      idempotencyKey: 'feed-restart-create',
    });
    await watching.next(changeOf(artifact.id, 1), 'the create');

    // A crash: the follower loses its connection; the save lands while it
    // waits to reconnect, so it must resume from its cursor to see it.
    await server.kill();
    server = await startServer(binary, dataDir, listen);
    await client.saveRevision(
      artifact.id,
      {
        base_revision: 1,
        changes: [
          { op: 'put', path: 'index.html', encoding: 'utf8', content: '2' },
        ],
      },
      { idempotencyKey: 'feed-restart-save-2' },
    );
    const second = await watching.next(
      changeOf(artifact.id, 2),
      'the save after the restart',
    );
    assert.equal(second.change.cursor, 2);
    assert.ok(watching.disconnects.length > 0, 'the crash went unnoticed');

    // Commits made while nobody follows arrive when following resumes.
    await watching.stop();
    await client.saveRevision(
      artifact.id,
      {
        base_revision: 2,
        changes: [
          { op: 'put', path: 'index.html', encoding: 'utf8', content: '3' },
        ],
      },
      { idempotencyKey: 'feed-restart-save-3' },
    );
    watching = follower(client, { after: 2 });
    const third = await watching.next(
      changeOf(artifact.id, 3),
      'the missed save',
    );
    assert.equal(third.change.cursor, 3);
    assert.ok(
      watching.events.every((event) => event.type === 'change'),
      'a resumed stream has no ready event',
    );
  } finally {
    await watching.stop();
    await server.stop();
  }
});

test('A cursor from another database is reset to its head', async () => {
  const port = await freePort();
  const listen = ['--api-listen', `127.0.0.1:${port}`];
  const first = join(scratch, 'reset-first');
  let server = await startServer(binary, first, listen);
  const client = new ArtifreeClient({
    baseUrl: server.api_url,
    accessToken: server.token,
  });
  const watching = follower(client);
  try {
    await watching.next((event) => event.type === 'ready');
    await client.createArtifact(page('<p>1</p>'), {
      idempotencyKey: 'feed-reset-create',
    });
    await watching.next((event) => event.type === 'change', 'the create');
    await server.stop();
    // Another, empty database at the same address and with the same token.
    const second = join(scratch, 'reset-second');
    await mkdir(second, { recursive: true, mode: 0o700 });
    await copyFile(join(first, 'access-token'), join(second, 'access-token'));
    server = await startServer(binary, second, listen);
    const reset = await watching.next(
      (event) => event.type === 'reset',
      'a reset',
    );
    assert.deepEqual(reset, { type: 'reset', cursor: 0 });
  } finally {
    await watching.stop();
    await server.stop();
  }
});

test('Shutdown ends open streams instead of waiting for them', async () => {
  const server = await startServer(binary, join(scratch, 'shutdown'));
  const client = new ArtifreeClient({
    baseUrl: server.api_url,
    accessToken: server.token,
  });
  const watching = follower(client);
  try {
    await watching.next((event) => event.type === 'ready');
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
});

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
