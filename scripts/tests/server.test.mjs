import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, before, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { createAjv } from '../lib/schema.mjs';
import { buildServer, startServer } from '../lib/server-process.mjs';

const root = fileURLToPath(new URL('../../', import.meta.url));
let binary;
let scratch;

before(async () => {
  binary = buildServer(root);
  scratch = await mkdtemp(join(tmpdir(), 'artifree-server-test-'));
});

after(async () => {
  await rm(scratch, { recursive: true, force: true });
});

async function freshDirectory(name) {
  const directory = join(scratch, name);
  await mkdir(directory, { recursive: true });
  return directory;
}

/** A data directory path that the server creates, owner-only, on first start. */
function dataDirectory(name) {
  return join(scratch, name);
}

function cli(server, args, token = server.token) {
  const env = {
    ...process.env,
    ARTIFREE_URL: server.api_url,
    ARTIFREE_TOKEN: token,
  };
  delete env.ARTIFREE_TOKEN_FILE;
  const result = spawnSync(
    process.execPath,
    ['apps/cli/dist/main.js', 'artifact', ...args],
    {
      cwd: root,
      encoding: 'utf8',
      maxBuffer: 64 * 1024 * 1024,
      timeout: 20_000,
      env,
    },
  );
  if (result.error) throw result.error;
  return result;
}

function json(result) {
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stderr, '');
  return JSON.parse(result.stdout);
}

test('CLI creates, saves, reads pinned revisions, and rejects stale saves', async () => {
  const data = dataDirectory('cli-data');
  const site = await freshDirectory('cli-site');
  await mkdir(join(site, 'assets'));
  await writeFile(join(site, 'index.html'), '<h1>v1</h1>');
  await writeFile(join(site, 'assets/app.css'), 'h1 { color: teal; }');
  await writeFile(join(site, '.DS_Store'), 'ignored');
  await writeFile(join(site, 'v2.html'), '<h1>v2</h1>');
  // Uploads must keep exact bytes, including a byte order mark.
  const notes = '\uFEFFline one\r\nline two\r\n';
  await writeFile(join(site, 'notes.txt'), notes);
  const server = await startServer(binary, data);
  try {
    const created = json(
      cli(server, ['create', '--title', 'Demo', '--dir', site]),
    );
    const id = created.artifact.id;
    assert.deepEqual(
      created.revision.files.map((file) => file.path),
      ['assets/app.css', 'index.html', 'notes.txt', 'v2.html'],
      'Hidden entries are skipped.',
    );
    const saved = json(
      cli(server, [
        'save',
        id,
        '--base',
        '1',
        '--put',
        `index.html=${join(site, 'v2.html')}`,
        '--delete',
        'v2.html',
      ]),
    );
    assert.equal(saved.revision.revision, 2);

    const stale = cli(server, [
      'save',
      id,
      '--base',
      '1',
      '--delete',
      'assets/app.css',
    ]);
    assert.equal(stale.status, 1);
    assert.equal(stale.stdout, '');
    assert.match(stale.stderr, /^error\[revision_conflict\]/);
    assert.match(stale.stderr, /current_revision=2/);

    // A base newer than current is the same conflict for --dir as for
    // --put/--delete, so agents can handle both the same way.
    for (const change of [
      ['--dir', site],
      ['--delete', 'notes.txt'],
    ]) {
      const ahead = cli(server, ['save', id, '--base', '99', ...change]);
      assert.equal(ahead.status, 1, change[0]);
      assert.equal(ahead.stdout, '', change[0]);
      assert.match(ahead.stderr, /^error\[revision_conflict\]/, change[0]);
      assert.match(ahead.stderr, /current_revision=2/, change[0]);
    }

    const first = cli(server, ['cat', id, 'index.html', '--revision', '1']);
    assert.equal(first.stdout, '<h1>v1</h1>');
    assert.equal(cli(server, ['cat', id, 'index.html']).stdout, '<h1>v2</h1>');
    assert.equal(cli(server, ['cat', id, 'notes.txt']).stdout, notes);

    // URL parsing would resolve dot segments to another route, such as the
    // artifact's metadata; the client refuses them instead.
    for (const path of ['../../..', 'assets/./app.css', '%2e%2e']) {
      const dotted = cli(server, ['cat', id, path]);
      assert.equal(dotted.status, 1, path);
      assert.equal(dotted.stdout, '', path);
      assert.match(dotted.stderr, /^error\[invalid_request\]/, path);
    }

    const preview = json(cli(server, ['preview', id, '--revision', '1']));
    assert.ok(preview.url.startsWith(`${server.content_origin}/p/`));
    const response = await fetch(preview.url);
    assert.equal(await response.text(), '<h1>v1</h1>');
    const policy = response.headers.get('content-security-policy') ?? '';
    assert.match(policy, /^sandbox allow-scripts /);
    assert.doesNotMatch(policy, /allow-same-origin|allow-popups/);
  } finally {
    await server.stop();
  }
});

test('Every JSON body the server sends matches the API schema root', async () => {
  // The generated schema's root claims to accept any v1 body; check it
  // against real responses from each JSON route, including errors.
  const schema = JSON.parse(
    await readFile(
      new URL('../../contracts/generated/api-v1.schema.json', import.meta.url),
      'utf8',
    ),
  );
  const validate = createAjv().compile(schema);
  // A root that accepts an arbitrary object would let any body pass below.
  assert.equal(validate({}), false, 'The schema root accepts an empty object.');
  const server = await startServer(binary, dataDirectory('schema-data'));
  try {
    // Each call states the expected status, so an error body (which the
    // schema root also accepts) cannot stand in for the success shape.
    let requests = 0;
    const request = async (status, method, path, body) => {
      const init = {
        method,
        headers: { authorization: `Bearer ${server.token}` },
      };
      requests += 1;
      if (body !== undefined) {
        init.headers['content-type'] = 'application/json';
        init.headers['idempotency-key'] = `schema-key-${requests}`;
        init.body = JSON.stringify(body);
      }
      const response = await fetch(`${server.api_url}${path}`, init);
      const value = await response.json();
      assert.equal(response.status, status, `${method} ${path}`);
      assert.ok(
        validate(value),
        `${method} ${path} (${response.status}) does not match: ${JSON.stringify(validate.errors?.slice(0, 3))}`,
      );
      return value;
    };
    const created = await request(201, 'POST', '/v1/artifacts', {
      title: 'Schema',
      files: [{ path: 'index.html', encoding: 'utf8', content: '<p>1</p>' }],
    });
    const id = created.artifact.id;
    await request(201, 'POST', `/v1/artifacts/${id}/revisions`, {
      base_revision: 1,
      changes: [
        {
          op: 'put',
          path: 'index.html',
          encoding: 'utf8',
          content: '<p>2</p>',
        },
      ],
    });
    await request(200, 'PATCH', `/v1/artifacts/${id}`, {
      base_metadata_revision: 1,
      title: 'Renamed',
      pinned: true,
    });
    await request(409, 'PATCH', `/v1/artifacts/${id}`, {
      base_metadata_revision: 1,
      pinned: false,
    });
    await request(200, 'GET', '/v1/artifacts');
    await request(201, 'POST', '/v1/artifacts', {
      title: 'Second',
      files: [{ path: 'index.html', encoding: 'utf8', content: '<p>1</p>' }],
    });
    const page = await request(200, 'GET', '/v1/artifacts?limit=1');
    assert.ok(page.next_page_token, 'A full page names the next one.');
    await request(400, 'GET', '/v1/artifacts?page_token=x');
    await request(200, 'GET', `/v1/artifacts/${id}`);
    await request(200, 'GET', `/v1/artifacts/${id}/revisions`);
    await request(200, 'GET', `/v1/artifacts/${id}/revisions/1`);
    await request(201, 'POST', `/v1/artifacts/${id}/revisions/1/previews`);
    await request(201, 'POST', `/v1/artifacts/${id}/revisions/1/restore`, {
      base_revision: 2,
    });
    await request(201, 'POST', `/v1/artifacts/${id}/revisions/1/duplicate`, {});
    await request(
      404,
      'GET',
      '/v1/artifacts/art_00000000000000000000000000000000',
    );
    await request(405, 'DELETE', '/v1/artifacts');
  } finally {
    await server.stop();
  }
});

test('CLI renames, pins, and lists by filter and page', async () => {
  const server = await startServer(binary, dataDirectory('cli-metadata'));
  try {
    const site = await freshDirectory('cli-metadata-site');
    await writeFile(join(site, 'index.html'), '<h1>x</h1>');
    const ids = [];
    for (const title of ['Alpha', 'Beta', 'Gamma'])
      ids.push(
        json(cli(server, ['create', '--title', title, '--dir', site])).artifact
          .id,
      );
    const renamed = json(
      cli(server, ['rename', ids[0], 'Alpha plan', '--base-metadata', '1']),
    );
    assert.equal(renamed.artifact.title, 'Alpha plan');
    assert.equal(renamed.artifact.metadata_revision, 2);
    assert.equal(renamed.artifact.current_revision, 1);
    const pinned = json(cli(server, ['pin', ids[0], '--base-metadata', '2']));
    assert.equal(pinned.artifact.pinned, true);
    const stale = cli(server, ['unpin', ids[0], '--base-metadata', '2']);
    assert.equal(stale.status, 1);
    assert.equal(stale.stdout, '');
    assert.match(stale.stderr, /^error\[metadata_conflict\]/);
    assert.match(stale.stderr, /current_metadata_revision=3/);

    const titles = (result) =>
      json(result).artifacts.map((artifact) => artifact.title);
    assert.deepEqual(titles(cli(server, ['list', '--search', 'PLAN'])), [
      'Alpha plan',
    ]);
    assert.deepEqual(titles(cli(server, ['list', '--pinned'])), ['Alpha plan']);
    assert.equal(titles(cli(server, ['list', '--unpinned'])).length, 2);
    const all = titles(cli(server, ['list', '--sort', 'created']));
    assert.deepEqual(all, ['Gamma', 'Beta', 'Alpha plan']);
    const first = json(
      cli(server, ['list', '--sort', 'created', '--limit', '2']),
    );
    assert.equal(first.artifacts.length, 2);
    const rest = json(
      cli(server, [
        'list',
        '--sort',
        'created',
        '--limit',
        '2',
        '--page-token',
        first.next_page_token,
      ]),
    );
    assert.deepEqual(
      [...first.artifacts, ...rest.artifacts].map((artifact) => artifact.title),
      all,
    );
    assert.equal(rest.next_page_token, undefined);
    for (const args of [
      ['list', '--pinned', '--unpinned'],
      ['list', '--sort', 'title'],
      ['list', '--limit', '0'],
      ['rename', ids[1], 'Missing base'],
      ['pin', ids[1], 'extra', '--base-metadata', '1'],
    ]) {
      const refused = cli(server, args);
      assert.equal(refused.status, 2, args.join(' '));
      assert.equal(refused.stdout, '', args.join(' '));
    }
  } finally {
    await server.stop();
  }
});

test('CLI restores and duplicates a revision byte for byte', async () => {
  const server = await startServer(binary, dataDirectory('cli-restore'));
  try {
    const site = await freshDirectory('cli-restore-site');
    const notes = '\uFEFFfirst\r\n';
    await writeFile(join(site, 'index.html'), '<h1>1</h1>');
    await writeFile(join(site, 'notes.txt'), notes);
    const id = json(
      cli(server, ['create', '--title', 'Restore', '--dir', site]),
    ).artifact.id;
    const page = join(site, 'index.html');
    await writeFile(page, '<h1>2</h1>');
    json(
      cli(server, [
        'save',
        id,
        '--base',
        '1',
        '--put',
        `index.html=${page}`,
        '--delete',
        'notes.txt',
      ]),
    );
    const restore = ['restore', id, '1', '--base', '2'];
    const key = ['--idempotency-key', 'cli-restore-key-1'];
    const restored = json(cli(server, [...restore, ...key]));
    assert.equal(restored.revision.revision, 3);
    assert.equal(restored.replayed, false);
    const bytes = cli(server, ['cat', id, 'notes.txt', '--revision', '3']);
    assert.equal(bytes.status, 0, bytes.stderr);
    assert.equal(bytes.stdout, notes);
    assert.equal(json(cli(server, [...restore, ...key])).replayed, true);

    const stale = cli(server, restore);
    assert.equal(stale.status, 1);
    assert.equal(stale.stdout, '');
    assert.match(stale.stderr, /^error\[revision_conflict\]/);
    assert.match(stale.stderr, /current_revision=3/);

    // A duplicate of revision 1 keeps its bytes in a history of its own.
    const duplicate = ['duplicate', id, '1'];
    const copyKey = ['--idempotency-key', 'cli-copy-key-1'];
    const copied = json(cli(server, [...duplicate, ...copyKey]));
    assert.equal(copied.artifact.title, 'Restore (Copy)');
    assert.equal(copied.artifact.current_revision, 1);
    assert.deepEqual(copied.copied, ['files']);
    const copy = copied.artifact.id;
    const copiedBytes = cli(server, ['cat', copy, 'notes.txt']);
    assert.equal(copiedBytes.stdout, notes);
    const again = json(cli(server, [...duplicate, ...copyKey]));
    assert.deepEqual([again.artifact.id, again.replayed], [copy, true]);
    const titled = json(cli(server, [...duplicate, '--title', 'Mine']));
    assert.equal(titled.artifact.title, 'Mine');
    assert.notEqual(titled.artifact.id, copy);
    const missing = cli(server, ['duplicate', id, '9']);
    assert.equal(missing.status, 1);
    assert.match(missing.stderr, /^error\[not_found\]/);

    for (const args of [
      ['restore', id, '1'],
      ['restore', id, 'first', '--base', '3'],
      ['restore', id, '--base', '3'],
      ['duplicate', id],
      ['duplicate', id, '1', '--base', '3'],
    ]) {
      const refused = cli(server, args);
      assert.equal(refused.status, 2, args.join(' '));
      assert.equal(refused.stdout, '', args.join(' '));
    }
  } finally {
    await server.stop();
  }
});

test('CLI uploads text that JSON escaping would inflate within the body limit', async () => {
  // Two 4 MiB files are within the package limits, but escaped as JSON text
  // (NUL becomes \u0000) they would exceed the request body limit.
  const site = await freshDirectory('escaped-site');
  await writeFile(join(site, 'index.html'), '<p>data</p>');
  const heavy = '\0'.repeat(4 * 1024 * 1024);
  await writeFile(join(site, 'a.txt'), heavy);
  await writeFile(join(site, 'b.txt'), heavy);
  const server = await startServer(binary, dataDirectory('escaped-data'));
  try {
    const created = cli(server, [
      'create',
      '--title',
      'Escaped',
      '--dir',
      site,
    ]);
    assert.equal(created.status, 0, created.stderr);
    const id = JSON.parse(created.stdout).artifact.id;
    assert.equal(cli(server, ['cat', id, 'a.txt']).stdout, heavy);
  } finally {
    await server.stop();
  }
});

test('CLI reports authentication and usage failures without stdout', async () => {
  const server = await startServer(binary, dataDirectory('auth-data'));
  try {
    const denied = cli(server, ['list'], 'f'.repeat(64));
    assert.equal(denied.status, 1);
    assert.equal(denied.stdout, '');
    assert.match(denied.stderr, /^error\[unauthenticated\]/);
    for (const args of [
      ['save', 'art_x', '--put', 'index.html=missing.html'],
      ['create', '--title', 'T'],
      ['cat', 'only-id'],
      ['list', '--bogus'],
    ]) {
      const result = cli(server, args);
      assert.equal(result.status, 2, args.join(' '));
      assert.equal(result.stdout, '', args.join(' '));
      assert.ok(result.stderr.trim().length > 0, args.join(' '));
    }
  } finally {
    await server.stop();
  }
});

test('A crash after commit loses nothing and a retry replays the result', async () => {
  const data = dataDirectory('crash-data');
  const site = await freshDirectory('crash-site');
  await writeFile(join(site, 'index.html'), '<p>durable</p>');
  const create = ['create', '--title', 'Durable', '--dir', site];
  const key = ['--idempotency-key', 'crash-test-key-1'];

  const first = await startServer(binary, data);
  let created;
  try {
    created = json(cli(first, [...create, ...key]));
    assert.equal(created.replayed, false);
  } finally {
    // Simulate a client that never saw the response: the process dies abruptly.
    await first.kill();
  }

  const second = await startServer(binary, data);
  try {
    assert.equal(second.token, first.token, 'The access token persists.');
    const retried = json(cli(second, [...create, ...key]));
    assert.equal(retried.replayed, true);
    assert.equal(retried.artifact.id, created.artifact.id);
    const listed = json(cli(second, ['list']));
    assert.equal(listed.artifacts.length, 1, 'The retry must not duplicate.');
    assert.equal(
      cli(second, ['cat', created.artifact.id, 'index.html']).stdout,
      '<p>durable</p>',
    );
  } finally {
    await second.stop();
  }
});
