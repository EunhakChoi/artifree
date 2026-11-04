import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, before, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import {
  ArtifreeClient,
  followData,
  listDataMembers,
} from '../../packages/client/dist/index.js';
import { createAjv } from '../lib/schema.mjs';
import { buildServer, startServer } from '../lib/server-process.mjs';

// The data members query and the data subscription of the real binary
// through the TypeScript client (decision 0040), with the responses checked
// against the generated schema.
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
    data: compile('AppData'),
    tooLarge: compile('DataStreamTooLarge'),
  };
});

after(async () => {
  await rm(scratch, { recursive: true, force: true });
});

test('Members page through the client and a follower sees snapshots, changes, and a too_large end', async () => {
  const server = await startServer(binary, join(scratch, 'one'));
  const configuration = { baseUrl: server.api_url, accessToken: server.token };
  const client = new ArtifreeClient(configuration);
  const controller = new AbortController();
  try {
    const { artifact } = await client.createArtifact(
      {
        title: 'Data',
        files: [{ path: 'index.html', encoding: 'utf8', content: 'x' }],
      },
      { idempotencyKey: 'stream-create' },
    );
    await client.setData(
      artifact.id,
      {
        path: 'list',
        base_data_version: 0,
        value: { b: 2, a: 1, c: { deep: [1, null] } },
      },
      { idempotencyKey: 'stream-set-1' },
    );
    const first = await listDataMembers(configuration, artifact.id, {
      path: 'list',
      select: 'values',
      limit: 2,
    });
    assert.ok(validate.members(first), JSON.stringify(validate.members.errors));
    assert.deepEqual(
      first.members.map((member) => member.name),
      ['a', 'b'],
    );
    const second = await listDataMembers(configuration, artifact.id, {
      path: 'list',
      select: 'values',
      limit: 2,
      after: first.next_after,
    });
    assert.deepEqual(second.members, [
      { name: 'c', value: { deep: [1, null] } },
    ]);
    assert.equal(second.next_after, undefined);

    const events = [];
    const waiting = [];
    const finished = followData(
      configuration,
      { artifactId: artifact.id, path: 'list' },
      {
        signal: controller.signal,
        retry: { minMs: 50, maxMs: 200 },
        onEvent(event) {
          events.push(event);
          for (const waiter of waiting.splice(0)) waiter();
        },
      },
    );
    const until = async (count) => {
      const deadline = Date.now() + 10_000;
      while (events.length < count && Date.now() < deadline)
        await new Promise((resolve) => {
          waiting.push(resolve);
          setTimeout(resolve, 200);
        });
      assert.ok(events.length >= count, `only ${events.length} events`);
    };
    await until(1);
    assert.equal(events[0].type, 'snapshot');
    assert.equal(events[0].data.data_version, 1);
    await client.setData(
      artifact.id,
      { path: 'list/d', base_data_version: 1, value: 4 },
      { idempotencyKey: 'stream-set-2' },
    );
    await until(2);
    assert.equal(events[1].type, 'change');
    assert.deepEqual(events[1].data.value, {
      a: 1,
      b: 2,
      c: { deep: [1, null] },
      d: 4,
    });
    for (const event of events)
      assert.ok(
        validate.data(event.data),
        JSON.stringify(validate.data.errors),
      );

    await client.setData(
      artifact.id,
      {
        path: 'list/big',
        base_data_version: 2,
        value: 'x'.repeat(140_000),
      },
      { idempotencyKey: 'stream-set-3' },
    );
    assert.equal(await finished, 'too_large');
    const last = events.at(-1);
    assert.equal(last.type, 'too_large');
    assert.ok(
      validate.tooLarge(last.data),
      JSON.stringify(validate.tooLarge.errors),
    );
  } finally {
    controller.abort();
    await server.stop();
  }
});
