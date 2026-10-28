import assert from 'node:assert/strict';
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
  server = await startServer(binary, join(scratch, 'data'));
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
          idempotentHint: expected.effect !== 'issue_link',
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

test('An agent deletes an artifact to the trash and recovers it', async () => {
  await withClient(async (client) => {
    const created = await ok(client, 'create_artifact', {
      title: 'Disposable',
      files: [utf8('index.html', '<p>1</p>')],
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
    // Every other tool finds it missing, as HTTP does; the trash lists it.
    for (const [tool, input] of [
      ['get_artifact', { artifact_id: id }],
      ['read_file', { artifact_id: id, revision: 1, path: 'index.html' }],
      ['create_preview', { artifact_id: id, revision: 1 }],
    ])
      await refused(client, tool, input, 'not_found');
    const viaHttp = await api(`/v1/artifacts/${id}`);
    assert.equal(viaHttp.status, 404);
    const trash = await ok(client, 'list_artifacts', { deleted: true });
    assert.ok(trash.artifacts.some((artifact) => artifact.id === id));
    const listed = await ok(client, 'list_artifacts', {});
    assert.ok(!listed.artifacts.some((artifact) => artifact.id === id));
    assert.equal(
      (await ok(client, 'update_artifact', deletion)).replayed,
      true,
    );

    const recovered = await ok(client, 'update_artifact', {
      artifact_id: id,
      base_metadata_revision: 2,
      deleted: false,
      idempotency_key: freshKey(),
    });
    assert.equal(recovered.artifact.deleted_at_ms, null);
    assert.equal(
      (await ok(client, 'get_artifact', { artifact_id: id })).metadata_revision,
      3,
    );
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
