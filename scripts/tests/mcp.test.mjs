import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, before, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import {
  Client,
  ProtocolError,
  StreamableHTTPClientTransport,
} from '@modelcontextprotocol/client';
import { Client as V1Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StreamableHTTPClientTransport as V1Transport } from '@modelcontextprotocol/sdk/client/streamableHttp.js';
import { createAjv } from '../lib/schema.mjs';
import { buildServer, startServer } from '../lib/server-process.mjs';

// The official MCP TypeScript client drives the real server binary, in the
// legacy initialize era and in the 2026-07-28 stateless era. The 1.x SDK,
// which many agents still embed, validates every structured result against
// the tool's output schema, including errors.
const root = fileURLToPath(new URL('../../', import.meta.url));
const ERAS = {
  legacy: {},
  modern: { versionNegotiation: { mode: { pin: '2026-07-28' } } },
};
let binary;
let scratch;
let server;
let catalog;
let apiSchema;

before(async () => {
  binary = buildServer(root);
  scratch = await mkdtemp(join(tmpdir(), 'artifree-mcp-test-'));
  // One actor makes every request of this suite, more at once than the
  // default burst allows; limits.test.mjs checks the limits themselves.
  server = await startServer(binary, join(scratch, 'data'), [
    '--request-burst',
    '1000',
  ]);
  catalog = JSON.parse(
    await readFile(join(root, 'contracts/generated/mcp-tools.json'), 'utf8'),
  );
  apiSchema = JSON.parse(
    await readFile(
      join(root, 'contracts/generated/api-v1.schema.json'),
      'utf8',
    ),
  );
});

after(async () => {
  await server?.stop();
  await rm(scratch, { recursive: true, force: true });
});

async function connect(era = 'modern', token = server.token) {
  const client = new Client({ name: 'artifree-test', version: '0' }, ERAS[era]);
  const transport = new StreamableHTTPClientTransport(
    new URL(`${server.api_url}/mcp`),
    { requestInit: { headers: { authorization: `Bearer ${token}` } } },
  );
  await client.connect(transport);
  return client;
}

async function withClient(run, era) {
  const client = await connect(era);
  try {
    return await run(client);
  } finally {
    await client.close();
  }
}

let keys = 0;
function freshKey() {
  keys += 1;
  return `mcp-test-${process.pid}-${keys}`;
}

/** Calls a tool that must succeed and returns its structured result. */
async function ok(client, name, args) {
  const result = await client.callTool({ name, arguments: args });
  assert.equal(result.isError, false, `${name}: ${JSON.stringify(result)}`);
  // The text block repeats the structured result for older clients.
  assert.deepEqual(
    JSON.parse(result.content[0].text),
    result.structuredContent,
  );
  return result.structuredContent;
}

/** Calls a tool that must fail and returns its contract error body. */
async function refused(client, name, args, code) {
  const result = await client.callTool({ name, arguments: args });
  assert.equal(
    result.isError,
    true,
    `${name} must fail: ${JSON.stringify(result)}`,
  );
  const { error } = result.structuredContent;
  assert.equal(error.code, code, `${name}: ${error.message}`);
  return error;
}

function utf8(path, content) {
  return { path, encoding: 'utf8', content };
}

function base64(path, bytes) {
  return {
    path,
    encoding: 'base64',
    content: Buffer.from(bytes).toString('base64'),
  };
}

async function api(path, init = {}) {
  const response = await fetch(`${server.api_url}${path}`, {
    ...init,
    headers: { authorization: `Bearer ${server.token}`, ...init.headers },
  });
  return { status: response.status, body: await response.json() };
}

test('a user stages a pinned selection for one registered MCP inbox', async () => {
  await withClient(async (client) => {
    const source = '\uFEFFa😀\r\nb';
    const created = await ok(client, 'create_artifact', {
      title: 'Selection handoff',
      files: [utf8('index.html', source)],
      idempotency_key: freshKey(),
    });
    const agent = await ok(client, 'register_handoff_session', {
      name: 'Editor',
    });
    const other = await ok(client, 'register_handoff_session', {
      name: 'Other',
    });
    const poll = (identity) => ok(client, 'poll_handoffs', identity);
    const workspace = await api('/v1/runtime-sessions', { method: 'POST' });
    assert.equal(workspace.status, 200);
    const connection = workspace.body.session_id;
    const otherWorkspace = await api('/v1/runtime-sessions', {
      method: 'POST',
    });
    const listing = await api(
      `/v1/agent-sessions?workspace_session_id=${connection}`,
    );
    assert.equal(listing.status, 200);
    assert.ok(
      listing.body.sessions.some(
        ({ id, name }) => id === agent.session_id && name === 'Editor',
      ),
    );
    assert.equal(
      JSON.stringify(listing.body).includes(agent.session_secret),
      false,
    );
    await refused(
      client,
      'poll_handoffs',
      {
        session_id: agent.session_id,
        session_secret: other.session_secret,
      },
      'not_found',
    );

    const proposal = {
      workspace_session_id: connection,
      agent_session_id: agent.session_id,
      artifact_id: created.artifact.id,
      revision: 1,
      path: 'index.html',
      file_sha256: createHash('sha256').update(source).digest('hex'),
      start: 2,
      end: 5,
      intent: '\uFEFF Clarify the selection \uFEFF',
    };
    const post = (path, body) =>
      api(path, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(body),
      });
    assert.equal(
      (await post('/v1/handoffs', { ...proposal, file_sha256: '0'.repeat(64) }))
        .status,
      409,
    );
    assert.equal(
      (await post('/v1/handoffs', { ...proposal, start: 3 })).status,
      422,
    );
    const first = await post('/v1/handoffs', proposal);
    assert.equal(first.status, 201);
    assert.equal(first.body.state, 'staged');
    assert.deepEqual(first.body.selection, {
      artifact_id: created.artifact.id,
      revision: 1,
      path: 'index.html',
      file_sha256: proposal.file_sha256,
      start: 2,
      end: 5,
      text: '😀\n',
    });
    assert.equal(first.body.intent, proposal.intent);
    assert.equal(
      (
        await api(
          `/v1/handoffs/${first.body.id}?workspace_session_id=${otherWorkspace.body.session_id}`,
        )
      ).status,
      404,
    );
    assert.deepEqual((await poll(agent)).deliveries, []);
    await refused(
      client,
      'acknowledge_handoff',
      { ...agent, delivery_id: first.body.id },
      'invalid_request',
    );
    const action = { workspace_session_id: connection };
    assert.equal(
      (await post(`/v1/handoffs/${first.body.id}/cancel`, action)).body.state,
      'cancelled',
    );
    assert.equal(
      (await post(`/v1/handoffs/${first.body.id}/send`, action)).status,
      409,
    );
    assert.deepEqual((await poll(agent)).deliveries, []);

    const staged = await post('/v1/handoffs', proposal);
    assert.equal(staged.status, 201);
    const send = () => post(`/v1/handoffs/${staged.body.id}/send`, action);
    assert.equal((await send()).body.state, 'queued');
    assert.equal((await send()).body.state, 'queued');
    assert.deepEqual((await poll(other)).deliveries, []);
    const offered = (await poll(agent)).deliveries;
    assert.equal(offered.length, 1);
    assert.equal(offered[0].id, staged.body.id);
    assert.equal(offered[0].state, 'offered');
    assert.equal(offered[0].selection.text, '😀\n');
    assert.equal(offered[0].intent, proposal.intent);
    assert.deepEqual((await poll(agent)).deliveries, offered);
    assert.equal(
      (await post(`/v1/handoffs/${staged.body.id}/cancel`, action)).status,
      409,
    );
    await refused(
      client,
      'acknowledge_handoff',
      { ...other, delivery_id: staged.body.id },
      'not_found',
    );
    const receipt = await ok(client, 'acknowledge_handoff', {
      ...agent,
      delivery_id: staged.body.id,
    });
    assert.equal(receipt.state, 'acknowledged');
    assert.deepEqual(
      await ok(client, 'acknowledge_handoff', {
        ...agent,
        delivery_id: staged.body.id,
      }),
      receipt,
    );
    assert.deepEqual((await poll(agent)).deliveries, []);
    assert.equal(
      (
        await api(
          `/v1/handoffs/${staged.body.id}?workspace_session_id=${connection}`,
        )
      ).body.state,
      'acknowledged',
    );

    const pending = await post('/v1/handoffs', proposal);
    assert.equal(
      (await post(`/v1/handoffs/${pending.body.id}/send`, action)).body.state,
      'queued',
    );
    const close = await fetch(
      `${server.api_url}/v1/runtime-sessions/${connection}`,
      {
        method: 'DELETE',
        headers: { authorization: `Bearer ${server.token}` },
      },
    );
    assert.equal(close.status, 204);
    assert.equal(
      (
        await api(
          `/v1/handoffs/${pending.body.id}?workspace_session_id=${connection}`,
        )
      ).body.state,
      'expired',
    );
    assert.deepEqual((await poll(agent)).deliveries, []);

    const active = otherWorkspace.body.session_id;
    const currentProposal = { ...proposal, workspace_session_id: active };
    const unsent = await post('/v1/handoffs', currentProposal);
    const queued = await post('/v1/handoffs', currentProposal);
    const activeAction = { workspace_session_id: active };
    assert.equal(
      (await post(`/v1/handoffs/${queued.body.id}/send`, activeAction)).body
        .state,
      'queued',
    );
    const deleted = await ok(client, 'update_artifact', {
      artifact_id: created.artifact.id,
      base_metadata_revision: created.artifact.metadata_revision,
      deleted: true,
      idempotency_key: freshKey(),
    });
    assert.equal(
      (await post(`/v1/handoffs/${unsent.body.id}/send`, activeAction)).status,
      404,
    );
    await refused(client, 'poll_handoffs', agent, 'not_found');

    // An unreachable source cannot starve other confirmed deliveries for
    // this same registered inbox, even if a poll may have offered it before.
    const goodSource = '<p>valid source</p>';
    const good = await ok(client, 'create_artifact', {
      title: 'Other selectable source',
      files: [utf8('index.html', goodSource)],
      idempotency_key: freshKey(),
    });
    const goodProposal = {
      ...currentProposal,
      artifact_id: good.artifact.id,
      file_sha256: createHash('sha256').update(goodSource).digest('hex'),
      start: 0,
      end: 3,
    };
    const following = await post('/v1/handoffs', goodProposal);
    assert.equal(following.status, 201);
    assert.equal(
      (await post(`/v1/handoffs/${following.body.id}/send`, activeAction)).body
        .state,
      'queued',
    );
    const next = (await poll(agent)).deliveries;
    assert.equal(next.length, 1, 'A missing source must not starve the inbox.');
    assert.equal(next[0].id, following.body.id);
    const blockedPath = `/v1/handoffs/${queued.body.id}?workspace_session_id=${active}`;
    assert.equal((await api(blockedPath)).body.state, 'blocked');
    assert.equal(
      (await post(`/v1/handoffs/${queued.body.id}/cancel`, activeAction))
        .status,
      409,
    );
    await ok(client, 'update_artifact', {
      artifact_id: created.artifact.id,
      base_metadata_revision: deleted.artifact.metadata_revision,
      deleted: false,
      idempotency_key: freshKey(),
    });
    assert.deepEqual((await poll(agent)).deliveries, next);
    assert.equal((await api(blockedPath)).body.state, 'blocked');
    const blockedReceipt = await ok(client, 'acknowledge_handoff', {
      ...agent,
      delivery_id: queued.body.id,
    });
    assert.equal(blockedReceipt.state, 'acknowledged');
    await ok(client, 'acknowledge_handoff', {
      ...agent,
      delivery_id: following.body.id,
    });
    assert.deepEqual((await poll(agent)).deliveries, []);

    // A replay can fail after an earlier poll may already have delivered it.
    const previouslyOffered = await post('/v1/handoffs', currentProposal);
    await post(`/v1/handoffs/${previouslyOffered.body.id}/send`, activeAction);
    assert.equal(
      (await poll(agent)).deliveries[0].id,
      previouslyOffered.body.id,
    );
    const current = await ok(client, 'get_artifact', {
      artifact_id: created.artifact.id,
    });
    await ok(client, 'update_artifact', {
      artifact_id: created.artifact.id,
      base_metadata_revision: current.metadata_revision,
      deleted: true,
      idempotency_key: freshKey(),
    });
    await refused(client, 'poll_handoffs', agent, 'not_found');
    const afterOffered = await post('/v1/handoffs', goodProposal);
    await post(`/v1/handoffs/${afterOffered.body.id}/send`, activeAction);
    assert.equal((await poll(agent)).deliveries[0].id, afterOffered.body.id);
    assert.equal(
      (
        await api(
          `/v1/handoffs/${previouslyOffered.body.id}?workspace_session_id=${active}`,
        )
      ).body.state,
      'blocked',
    );
    assert.equal(
      (
        await ok(client, 'acknowledge_handoff', {
          ...agent,
          delivery_id: previouslyOffered.body.id,
        })
      ).state,
      'acknowledged',
    );
    await ok(client, 'acknowledge_handoff', {
      ...agent,
      delivery_id: afterOffered.body.id,
    });
  });
});

test('tools/list serves the generated catalog in both protocol eras', async () => {
  for (const era of Object.keys(ERAS)) {
    await withClient(async (client) => {
      assert.equal(
        client.getNegotiatedProtocolVersion(),
        era === 'modern' ? '2026-07-28' : '2025-11-25',
      );
      assert.equal(client.getServerVersion()?.name, 'artifree');
      assert.equal(client.getInstructions(), catalog.instructions);
      const { tools } = await client.listTools();
      assert.deepEqual(
        tools.map((tool) => tool.name),
        catalog.tools.map((tool) => tool.name),
      );
      for (const [index, tool] of tools.entries()) {
        const expected = catalog.tools[index];
        assert.equal(tool.title, expected.title);
        assert.equal(tool.description, expected.description);
        assert.deepEqual(tool.inputSchema, expected.inputSchema);
        assert.deepEqual(tool.outputSchema, expected.outputSchema);
        assert.deepEqual(tool.annotations, {
          readOnlyHint: expected.effect === 'read',
          destructiveHint: false,
          idempotentHint: !['issue_link', 'issue_session'].includes(
            expected.effect,
          ),
          openWorldHint: false,
        });
      }
    }, era);
  }
});

test('An agent creates, reads, edits, lists history, reads pinned bytes, and previews', async () => {
  const notes = '﻿line one\r\nline two\r\n';
  const png = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0xff];
  await withClient(async (client) => {
    const created = await ok(client, 'create_artifact', {
      title: 'Agent page',
      files: [
        utf8(
          'index.html',
          '<link rel="stylesheet" href="style.css"><h1>v1</h1>',
        ),
        utf8('style.css', 'h1 { color: teal; }'),
        utf8('notes.txt', notes),
        base64('assets/logo.png', png),
      ],
      idempotency_key: freshKey(),
    });
    const id = created.artifact.id;
    assert.equal(created.replayed, false);
    assert.equal(created.revision.revision, 1);
    assert.deepEqual(
      created.revision.files.map((file) => [file.path, file.size]),
      [
        ['assets/logo.png', png.length],
        ['index.html', 51],
        ['notes.txt', Buffer.byteLength(notes)],
        ['style.css', 19],
      ],
    );
    const fetched = await ok(client, 'get_artifact', { artifact_id: id });
    assert.equal(fetched.current_revision, 1);
    const listed = await ok(client, 'list_artifacts', {});
    assert.equal(listed.artifacts[0].id, id);

    const saved = await ok(client, 'save_revision', {
      artifact_id: id,
      base_revision: fetched.current_revision,
      changes: [
        { op: 'put', ...utf8('index.html', '<h1>v2</h1>') },
        { op: 'delete', path: 'style.css' },
      ],
      idempotency_key: freshKey(),
    });
    assert.equal(saved.revision.revision, 2);
    assert.equal(saved.artifact.current_revision, 2);

    const history = await ok(client, 'list_revisions', { artifact_id: id });
    assert.deepEqual(
      history.revisions.map((revision) => revision.revision),
      [2, 1],
    );
    const first = await ok(client, 'get_revision', {
      artifact_id: id,
      revision: 1,
    });
    assert.deepEqual(first, history.revisions[1]);
    assert.deepEqual(
      history.revisions[0].files.map((file) => file.path),
      ['assets/logo.png', 'index.html', 'notes.txt'],
    );

    // Pinned reads return exact bytes: BOM and CRLF in text, binary as base64.
    const old = await ok(client, 'read_file', {
      artifact_id: id,
      revision: 1,
      path: 'index.html',
    });
    assert.equal(
      old.content,
      '<link rel="stylesheet" href="style.css"><h1>v1</h1>',
    );
    assert.equal(old.media_type, 'text/html; charset=utf-8');
    const text = await ok(client, 'read_file', {
      artifact_id: id,
      revision: 2,
      path: 'notes.txt',
    });
    assert.deepEqual(
      [text.encoding, text.content, text.size],
      ['utf8', notes, Buffer.byteLength(notes)],
    );
    const binary = await ok(client, 'read_file', {
      artifact_id: id,
      revision: 2,
      path: 'assets/logo.png',
    });
    assert.equal(binary.encoding, 'base64');
    assert.deepEqual([...Buffer.from(binary.content, 'base64')], png);
    await refused(
      client,
      'read_file',
      { artifact_id: id, revision: 2, path: 'style.css' },
      'not_found',
    );

    // The preview renders the pinned revision in the sandboxed origin.
    const preview = await ok(client, 'create_preview', {
      artifact_id: id,
      revision: 1,
    });
    assert.equal(preview.revision, 1);
    assert.ok(
      preview.url.startsWith(`${server.content_origin}/p/`),
      preview.url,
    );
    assert.ok(preview.expires_at_ms > Date.now());
    const page = await fetch(preview.url);
    assert.equal(page.status, 200);
    assert.match(
      page.headers.get('content-security-policy'),
      /^sandbox allow-scripts /,
    );
    assert.equal(await page.text(), old.content);
    const logo = await fetch(new URL('assets/logo.png', preview.url));
    assert.deepEqual([...new Uint8Array(await logo.arrayBuffer())], png);

    // HTTP sees the same state, so both adapters share one service.
    const viaHttp = await api(`/v1/artifacts/${id}`);
    assert.deepEqual(viaHttp.body, saved.artifact);
  });
});

test('Results conform to the API contract in both eras', async () => {
  const validate = createAjv().compile(apiSchema);
  for (const era of Object.keys(ERAS)) {
    await withClient(async (client) => {
      const created = await ok(client, 'create_artifact', {
        title: `Contract ${era}`,
        files: [utf8('index.html', '<p>c</p>')],
        idempotency_key: freshKey(),
      });
      const id = created.artifact.id;
      const results = [
        created,
        await ok(client, 'list_artifacts', {}),
        await ok(client, 'get_artifact', { artifact_id: id }),
        await ok(client, 'list_revisions', { artifact_id: id }),
        await ok(client, 'get_revision', { artifact_id: id, revision: 1 }),
        await ok(client, 'create_preview', { artifact_id: id, revision: 1 }),
        (
          await client.callTool({
            name: 'get_artifact',
            arguments: { artifact_id: 'art_missing' },
          })
        ).structuredContent,
      ];
      for (const result of results)
        assert.ok(validate(result), JSON.stringify(validate.errors));
    }, era);
  }
});

test('Retries replay, a reused key is refused, and keys are shared with HTTP', async () => {
  await withClient(async (client) => {
    const key = freshKey();
    const request = {
      title: 'Retried',
      files: [utf8('index.html', '<p>once</p>')],
      idempotency_key: key,
    };
    const first = await ok(client, 'create_artifact', request);
    const retry = await ok(client, 'create_artifact', request);
    assert.equal(retry.replayed, true);
    assert.deepEqual({ ...retry, replayed: false }, first);
    const id = first.artifact.id;
    await refused(
      client,
      'create_artifact',
      { ...request, title: 'Something else' },
      'request_key_reused',
    );

    // A lost save response retried after another save still replays the
    // original revision as of its commit.
    const saveKey = freshKey();
    const save = {
      artifact_id: id,
      base_revision: 1,
      changes: [
        { op: 'put', ...utf8('index.html', '<p>twice</p>') },
        { op: 'put', ...utf8('about.html', '<p>about</p>') },
      ],
      idempotency_key: saveKey,
    };
    const saved = await ok(client, 'save_revision', save);
    await ok(client, 'save_revision', {
      ...save,
      base_revision: 2,
      changes: [{ op: 'put', ...utf8('index.html', '<p>three</p>') }],
      idempotency_key: freshKey(),
    });
    const replay = await ok(client, 'save_revision', {
      ...save,
      // The same changes listed in another order are the same request.
      changes: [...save.changes].reverse(),
    });
    assert.equal(replay.replayed, true);
    assert.deepEqual({ ...replay, replayed: false }, saved);
    assert.equal(replay.artifact.current_revision, 2);

    // The HTTP API and MCP share the actor's keys and fingerprints.
    const viaHttp = await api('/v1/artifacts', {
      method: 'POST',
      headers: { 'content-type': 'application/json', 'idempotency-key': key },
      body: JSON.stringify({ title: request.title, files: request.files }),
    });
    assert.equal(viaHttp.status, 201);
    assert.equal(viaHttp.body.replayed, true);
    assert.equal(viaHttp.body.artifact.id, id);
  });
});

test('Tool errors reach 1.x SDK clients as readable results', async () => {
  const client = new V1Client({ name: 'artifree-test-v1', version: '0' });
  await client.connect(
    new V1Transport(new URL(`${server.api_url}/mcp`), {
      requestInit: { headers: { authorization: `Bearer ${server.token}` } },
    }),
  );
  try {
    await client.listTools();
    const created = await ok(client, 'create_artifact', {
      title: 'Old client',
      files: [utf8('index.html', '<p>v1</p>')],
      idempotency_key: freshKey(),
    });
    const id = created.artifact.id;
    await ok(client, 'get_artifact', { artifact_id: id });
    const conflict = await refused(
      client,
      'save_revision',
      {
        artifact_id: id,
        base_revision: 7,
        changes: [{ op: 'delete', path: 'index.html' }],
        idempotency_key: freshKey(),
      },
      'revision_conflict',
    );
    assert.equal(conflict.current_revision, 1);
    await refused(
      client,
      'get_artifact',
      { artifact_id: 'art_missing' },
      'not_found',
    );
  } finally {
    await client.close();
  }
});

test('A stale base is a conflict that writes nothing', async () => {
  await withClient(async (client) => {
    const created = await ok(client, 'create_artifact', {
      title: 'Contended',
      files: [utf8('index.html', '<p>1</p>')],
      idempotency_key: freshKey(),
    });
    const id = created.artifact.id;
    const change = (content) => ({
      artifact_id: id,
      base_revision: 1,
      changes: [{ op: 'put', ...utf8('index.html', content) }],
      idempotency_key: freshKey(),
    });
    await ok(client, 'save_revision', change('<p>winner</p>'));
    for (const base of [1, 0, 99]) {
      const error = await refused(
        client,
        'save_revision',
        { ...change('<p>loser</p>'), base_revision: base },
        'revision_conflict',
      );
      assert.equal(error.current_revision, 2);
    }
    // A conflicting multi-file save leaves no partial revision.
    await refused(
      client,
      'save_revision',
      {
        ...change('<p>loser</p>'),
        changes: [
          { op: 'put', ...utf8('index.html', '<p>loser</p>') },
          { op: 'put', ...utf8('extra.html', '<p>loser</p>') },
        ],
      },
      'revision_conflict',
    );
    const history = await ok(client, 'list_revisions', { artifact_id: id });
    assert.equal(history.revisions.length, 2);
    const current = await ok(client, 'read_file', {
      artifact_id: id,
      revision: 2,
      path: 'index.html',
    });
    assert.equal(current.content, '<p>winner</p>');
  });
});

test('An agent restores an earlier revision byte for byte', async () => {
  await withClient(async (client) => {
    const bytes = Uint8Array.from([0, 0xff, 0xfe, 0x80, 10]);
    const created = await ok(client, 'create_artifact', {
      title: 'Restorable',
      files: [utf8('index.html', '<p>1</p>'), base64('image.png', bytes)],
      idempotency_key: freshKey(),
    });
    const id = created.artifact.id;
    await ok(client, 'save_revision', {
      artifact_id: id,
      base_revision: 1,
      changes: [
        { op: 'put', ...utf8('index.html', '<p>2</p>') },
        { op: 'delete', path: 'image.png' },
      ],
      idempotency_key: freshKey(),
    });
    const restore = {
      artifact_id: id,
      revision: 1,
      base_revision: 2,
      idempotency_key: freshKey(),
    };
    const restored = await ok(client, 'restore_revision', restore);
    assert.equal(restored.revision.revision, 3);
    assert.equal(restored.replayed, false);
    const first = await ok(client, 'get_revision', {
      artifact_id: id,
      revision: 1,
    });
    assert.deepEqual(restored.revision.files, first.files);
    const data = await ok(client, 'read_file', {
      artifact_id: id,
      revision: 3,
      path: 'image.png',
    });
    assert.equal(data.encoding, 'base64');
    assert.deepEqual(Buffer.from(data.content, 'base64'), Buffer.from(bytes));

    // A stale restore writes nothing; the same restore over HTTP replays.
    const stale = await refused(
      client,
      'restore_revision',
      { ...restore, idempotency_key: freshKey() },
      'revision_conflict',
    );
    assert.equal(stale.current_revision, 3);
    const viaHttp = await api(`/v1/artifacts/${id}/revisions/1/restore`, {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        'idempotency-key': restore.idempotency_key,
      },
      body: JSON.stringify({ base_revision: 2 }),
    });
    assert.equal(viaHttp.status, 201);
    assert.equal(viaHttp.body.replayed, true);
    assert.equal(viaHttp.body.revision.revision, 3);
  });
});

test('An agent duplicates a revision into an artifact of its own', async () => {
  await withClient(async (client) => {
    const bytes = Uint8Array.from([0, 0xff, 0xfe, 0x80, 10]);
    const created = await ok(client, 'create_artifact', {
      title: 'Original',
      files: [utf8('index.html', '<p>1</p>'), base64('image.png', bytes)],
      idempotency_key: freshKey(),
    });
    const id = created.artifact.id;
    await ok(client, 'save_revision', {
      artifact_id: id,
      base_revision: 1,
      changes: [{ op: 'delete', path: 'image.png' }],
      idempotency_key: freshKey(),
    });
    const duplicate = {
      artifact_id: id,
      revision: 1,
      idempotency_key: freshKey(),
    };
    const copied = await ok(client, 'duplicate_artifact', duplicate);
    assert.equal(copied.artifact.title, 'Original (Copy)');
    assert.deepEqual(copied.copied, ['files']);
    assert.equal(copied.revision.revision, 1);
    const data = await ok(client, 'read_file', {
      artifact_id: copied.artifact.id,
      revision: 1,
      path: 'image.png',
    });
    assert.deepEqual(Buffer.from(data.content, 'base64'), Buffer.from(bytes));

    // The same duplicate over HTTP replays; a missing revision copies
    // nothing.
    const viaHttp = await api(`/v1/artifacts/${id}/revisions/1/duplicate`, {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        'idempotency-key': duplicate.idempotency_key,
      },
      body: '{}',
    });
    assert.equal(viaHttp.status, 201);
    assert.equal(viaHttp.body.replayed, true);
    assert.equal(viaHttp.body.artifact.id, copied.artifact.id);
    await refused(
      client,
      'duplicate_artifact',
      { ...duplicate, revision: 3, idempotency_key: freshKey() },
      'not_found',
    );
  });
});

test('An agent deletes to the trash, finds nothing there, and recovers', async () => {
  await withClient(async (client) => {
    const title = `Trash ${process.pid}`;
    const created = await ok(client, 'create_artifact', {
      title,
      files: [utf8('index.html', '<p>kept</p>')],
      idempotency_key: freshKey(),
    });
    const id = created.artifact.id;
    const deletion = {
      artifact_id: id,
      base_metadata_revision: 1,
      deleted: true,
      idempotency_key: freshKey(),
    };
    const deleted = await ok(client, 'update_artifact', deletion);
    assert.equal(typeof deleted.artifact.deleted_at_ms, 'number');

    // Every other tool, and HTTP, finds nothing; a retry replays.
    const missing = (await api(`/v1/artifacts/${'art_' + '0'.repeat(32)}`)).body
      .error;
    for (const [name, args] of [
      ['get_artifact', { artifact_id: id }],
      ['list_revisions', { artifact_id: id }],
      ['read_file', { artifact_id: id, revision: 1, path: 'index.html' }],
      [
        'read_file',
        { artifact_id: id, revision: 1, path: 'index.html', offset: 99 },
      ],
      ['create_preview', { artifact_id: id, revision: 1 }],
      [
        'save_revision',
        {
          artifact_id: id,
          base_revision: 1,
          changes: [{ op: 'put', ...utf8('index.html', '<p>lost</p>') }],
          idempotency_key: freshKey(),
        },
      ],
      [
        'update_artifact',
        { ...deletion, base_metadata_revision: 2, idempotency_key: freshKey() },
      ],
    ])
      assert.deepEqual(await refused(client, name, args, 'not_found'), missing);
    assert.equal((await api(`/v1/artifacts/${id}`)).status, 404);
    assert.equal(
      (await ok(client, 'update_artifact', deletion)).replayed,
      true,
    );
    const listed = async (deletedOnly) =>
      (
        await ok(client, 'list_artifacts', {
          q: title,
          ...(deletedOnly ? { deleted: true } : {}),
        })
      ).artifacts.map((artifact) => artifact.id);
    assert.deepEqual(await listed(false), []);
    assert.deepEqual(await listed(true), [id]);

    const recovered = await ok(client, 'update_artifact', {
      artifact_id: id,
      base_metadata_revision: 2,
      deleted: false,
      idempotency_key: freshKey(),
    });
    assert.equal(recovered.artifact.deleted_at_ms, null);
    const file = await ok(client, 'read_file', {
      artifact_id: id,
      revision: 1,
      path: 'index.html',
    });
    assert.equal(file.content, '<p>kept</p>');
    assert.deepEqual(await listed(false), [id]);
  });
});

test('An agent reads a file in parts that join into it', async () => {
  // 1- to 4-byte characters, a byte order mark, and CRLF. Every offset and
  // length is covered by the core's tests; this checks what crosses MCP.
  const text = '\u{FEFF}a\u{e9}\u{d55c}\u{1F600}\r\n'.repeat(2);
  const bytes = Buffer.from(text);
  // Not UTF-8 as a whole, but its first bytes alone are.
  const binary = [0x41, 0x42, 0x43, 0xff, 0xfe, 0x00];
  await withClient(async (client) => {
    const created = await ok(client, 'create_artifact', {
      title: 'Parts',
      files: [
        utf8('index.html', '<p>parts</p>'),
        utf8('notes.txt', text),
        base64('data.png', binary),
      ],
      idempotency_key: freshKey(),
    });
    const file = { artifact_id: created.artifact.id, revision: 1 };
    const notes = { ...file, path: 'notes.txt' };
    const whole = await ok(client, 'read_file', notes);
    const sha256 = createHash('sha256').update(bytes).digest('hex');
    assert.deepEqual(
      [whole.content, whole.size, whole.sha256, whole.offset],
      [text, bytes.length, sha256, 0],
    );
    assert.equal(whole.next_offset, undefined);

    // Each part is text that ends between characters, and they join.
    for (const length of [4, 13]) {
      const parts = [];
      let offset = 0;
      for (;;) {
        const part = await ok(client, 'read_file', {
          ...notes,
          offset,
          length,
        });
        assert.deepEqual(
          [part.encoding, part.offset, part.size, part.sha256],
          ['utf8', offset, bytes.length, sha256],
        );
        assert.ok(part.content.length > 0, `length ${length} made progress`);
        assert.ok(Buffer.byteLength(part.content) <= length);
        parts.push(part.content);
        if (part.next_offset === undefined) break;
        assert.equal(
          part.next_offset,
          offset + Buffer.byteLength(part.content),
        );
        offset = part.next_offset;
      }
      assert.equal(parts.join(''), text, `length ${length}`);
    }
    // A part ends before the character it would split.
    const cut = await ok(client, 'read_file', {
      ...notes,
      offset: 3,
      length: 3,
    });
    assert.deepEqual([cut.content, cut.next_offset], ['a\u{e9}', 6]);
    const end = await ok(client, 'read_file', {
      ...notes,
      offset: bytes.length,
    });
    assert.deepEqual([end.content, end.next_offset], ['', undefined]);

    // A file that is not UTF-8 is cut at bytes and sent as base64, also
    // where the part's bytes alone would be text.
    const head = await ok(client, 'read_file', {
      ...file,
      path: 'data.png',
      offset: 0,
      length: 3,
    });
    assert.deepEqual(
      [
        head.encoding,
        [...Buffer.from(head.content, 'base64')],
        head.next_offset,
      ],
      ['base64', binary.slice(0, 3), 3],
    );

    for (const [args, code, message] of [
      [{ offset: 5 }, 'invalid_request', /inside a UTF-8 character/],
      [{ offset: 9, length: 3 }, 'invalid_request', /at least 4 bytes/],
      [
        { offset: bytes.length + 1 },
        'range_not_satisfiable',
        new RegExp(`${bytes.length} bytes`),
      ],
      [{ length: 0 }, 'invalid_request', /expected a nonzero u64/],
      [{ offset: -1 }, 'invalid_request', /expected u64/],
    ]) {
      const error = await refused(
        client,
        'read_file',
        { ...notes, ...args },
        code,
      );
      assert.match(error.message, message, JSON.stringify(args));
    }
  });
});

test('Invalid input is a tool error the model can read', async () => {
  await withClient(async (client) => {
    const created = await ok(client, 'create_artifact', {
      title: 'Target',
      files: [utf8('index.html', '<p>t</p>')],
      idempotency_key: freshKey(),
    });
    const id = created.artifact.id;
    const create = (fields) => ({
      title: 'Bad',
      files: [utf8('index.html', '<p>x</p>')],
      idempotency_key: freshKey(),
      ...fields,
    });
    const cases = [
      [
        'create_artifact',
        create({ idempotency_key: undefined }),
        'invalid_request',
        /idempotency_key/,
      ],
      [
        'create_artifact',
        create({ idempotency_key: 'short' }),
        'invalid_request',
        /idempotency_key/,
      ],
      [
        'create_artifact',
        create({ owner: 'someone' }),
        'invalid_request',
        /unknown field `owner`/,
      ],
      [
        'create_artifact',
        create({ title: ' padded' }),
        'invalid_request',
        /title/,
      ],
      [
        'create_artifact',
        create({ files: [utf8('page.html', '<p/>')] }),
        'invalid_request',
        /index\.html/,
      ],
      [
        'create_artifact',
        create({
          files: [utf8('index.html', 'x'), utf8('../escape.html', 'x')],
        }),
        'invalid_request',
        /files\[1\]\.path/,
      ],
      [
        'create_artifact',
        create({
          files: [{ path: 'index.html', encoding: 'base64', content: '***' }],
        }),
        'invalid_request',
        /invalid base64/,
      ],
      [
        'create_artifact',
        create({
          files: [{ path: 'index.html', encoding: 'utf16', content: 'x' }],
        }),
        'invalid_request',
        /arguments/,
      ],
      [
        'save_revision',
        {
          artifact_id: id,
          base_revision: -1,
          changes: [],
          idempotency_key: freshKey(),
        },
        'invalid_request',
        /arguments/,
      ],
      [
        'save_revision',
        {
          artifact_id: id,
          base_revision: 1,
          changes: [{ op: 'delete', path: 'index.html' }],
          idempotency_key: freshKey(),
        },
        'invalid_request',
        /index\.html/,
      ],
      [
        'get_artifact',
        { artifact_id: 'art_00000000000000000000000000000000' },
        'not_found',
        /artifact/,
      ],
      [
        'get_artifact',
        { artifact_id: '../../etc/passwd' },
        'not_found',
        /artifact/,
      ],
      [
        'get_revision',
        { artifact_id: id, revision: 0 },
        'not_found',
        /revision/,
      ],
      [
        'get_revision',
        { artifact_id: id, revision: 2 },
        'not_found',
        /revision/,
      ],
      [
        'read_file',
        { artifact_id: id, revision: 1, path: '.hidden' },
        'invalid_request',
        /path/,
      ],
      [
        'create_preview',
        { artifact_id: id, revision: 5 },
        'not_found',
        /revision/,
      ],
      ['list_artifacts', { extra: true }, 'invalid_request', /unknown field/],
    ];
    for (const [name, args, code, message] of cases) {
      const error = await refused(client, name, args, code);
      assert.match(error.message, message, `${name} ${JSON.stringify(args)}`);
    }
    // A rejected request does not reserve its key: the corrected request
    // with the same key commits.
    const key = freshKey();
    await refused(
      client,
      'create_artifact',
      create({ idempotency_key: key, title: '' }),
      'invalid_request',
    );
    const corrected = await ok(
      client,
      'create_artifact',
      create({ idempotency_key: key }),
    );
    assert.equal(corrected.replayed, false);
    const history = await ok(client, 'list_revisions', { artifact_id: id });
    assert.equal(history.revisions.length, 1, 'failed saves must not commit');

    await assert.rejects(
      client.callTool({ name: 'delete_everything', arguments: {} }),
      (error) => error instanceof ProtocolError && error.code === -32602,
    );
  });
});

test('Clients without the token cannot connect, and pages cannot call the endpoint', async () => {
  for (const token of ['', 'f'.repeat(64)])
    await assert.rejects(connect('legacy', token), /unauthenticated/);
  const body = JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'tools/list' });
  const headers = {
    authorization: `Bearer ${server.token}`,
    'content-type': 'application/json',
    accept: 'application/json, text/event-stream',
    'mcp-protocol-version': '2025-11-25',
  };
  const plain = await fetch(`${server.api_url}/mcp`, {
    method: 'POST',
    headers,
    body,
  });
  assert.equal(plain.status, 200);
  const fromPage = await fetch(`${server.api_url}/mcp`, {
    method: 'POST',
    headers: { ...headers, origin: 'http://127.0.0.1:5173' },
    body,
  });
  assert.equal(fromPage.status, 403);
  const unauthenticated = await fetch(`${server.api_url}/mcp`, {
    method: 'POST',
    headers: { ...headers, authorization: 'Bearer nope' },
    body,
  });
  assert.equal(unauthenticated.status, 401);
  assert.equal((await unauthenticated.json()).error.code, 'unauthenticated');
});

test('Escape-heavy text over the request limit is refused; the same bytes fit as base64', async () => {
  // About 6.2 MiB of NUL bytes: within the file limits, but JSON escapes each
  // NUL to six bytes, so the utf8 form exceeds the 36 MiB request limit.
  const nul = new Uint8Array(3.1 * 1024 * 1024);
  const files = (encode) => [
    utf8('index.html', '<p>escaped</p>'),
    encode('a.txt', nul),
    encode('b.txt', nul),
  ];
  const asText = (path, bytes) =>
    utf8(path, Buffer.from(bytes).toString('utf8'));
  await withClient(async (client) => {
    await assert.rejects(
      client.callTool({
        name: 'create_artifact',
        arguments: {
          title: 'Escaped',
          files: files(asText),
          idempotency_key: freshKey(),
        },
      }),
      /413|too large/i,
    );
    const created = await ok(client, 'create_artifact', {
      title: 'Escaped',
      files: files(base64),
      idempotency_key: freshKey(),
    });
    const read = await ok(client, 'read_file', {
      artifact_id: created.artifact.id,
      revision: 1,
      path: 'a.txt',
    });
    // Valid UTF-8 comes back as text, exact.
    assert.equal(read.encoding, 'utf8');
    assert.ok(Buffer.from(read.content, 'utf8').equals(Buffer.from(nul)));
  });
});

test('A largest file fits, and a body over the API limit is refused', async () => {
  // 5 MiB is the per-file limit; its base64 form exceeds the MCP SDK's
  // default 4 MiB body limit, which the endpoint raises to the API's.
  const largest = new Uint8Array(5 * 1024 * 1024).map(
    (_, index) => index % 251,
  );
  await withClient(async (client) => {
    const created = await ok(client, 'create_artifact', {
      title: 'Largest file',
      files: [utf8('index.html', '<p>big</p>'), base64('blob.png', largest)],
      idempotency_key: freshKey(),
    });
    const read = await ok(client, 'read_file', {
      artifact_id: created.artifact.id,
      revision: 1,
      path: 'blob.png',
    });
    assert.ok(Buffer.from(read.content, 'base64').equals(Buffer.from(largest)));
  });

  const response = await fetch(`${server.api_url}/mcp`, {
    method: 'POST',
    headers: {
      authorization: `Bearer ${server.token}`,
      'content-type': 'application/json',
      accept: 'application/json, text/event-stream',
      'mcp-protocol-version': '2025-11-25',
    },
    body: 'x'.repeat(36 * 1024 * 1024 + 1),
  });
  assert.equal(response.status, 413);
});
