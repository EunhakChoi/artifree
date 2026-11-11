import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, before, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { createAjv } from '../lib/schema.mjs';
import { buildServer, startServer } from '../lib/server-process.mjs';
import { storedZipEntries } from '../lib/zip-entries.mjs';

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

function jobCli(server, args) {
  const env = {
    ...process.env,
    ARTIFREE_URL: server.api_url,
    ARTIFREE_TOKEN: server.token,
  };
  delete env.ARTIFREE_TOKEN_FILE;
  const result = spawnSync(
    process.execPath,
    ['apps/cli/dist/main.js', 'job', ...args],
    { cwd: root, encoding: 'utf8', timeout: 20_000, env },
  );
  if (result.error) throw result.error;
  return result;
}

function dataCli(server, args) {
  const env = {
    ...process.env,
    ARTIFREE_URL: server.api_url,
    ARTIFREE_TOKEN: server.token,
  };
  delete env.ARTIFREE_TOKEN_FILE;
  const result = spawnSync(
    process.execPath,
    ['apps/cli/dist/main.js', 'data', ...args],
    { cwd: root, encoding: 'utf8', timeout: 20_000, env },
  );
  if (result.error) throw result.error;
  return result;
}

/** Runs `artifree-server worker run --once` and returns its JSON lines. */
function drainJobs(data) {
  const result = spawnSync(
    binary,
    ['worker', 'run', '--once', '--poll-ms', '10', '--data-dir', data],
    { encoding: 'utf8', timeout: 60_000 },
  );
  if (result.error) throw result.error;
  assert.equal(result.status, 0, result.stderr);
  return result.stdout
    .trim()
    .split('\n')
    .map((line) => JSON.parse(line));
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
    // cat writes one byte range of the file, counted in bytes.
    const part = (...range) => cli(server, ['cat', id, 'notes.txt', ...range]);
    assert.equal(part('--offset', '3', '--length', '4').stdout, 'line');
    assert.equal(part('--offset', '13').stdout, 'line two\r\n');
    assert.equal(part('--length', '4').stdout, '\uFEFFl');
    const past = part('--offset', String(Buffer.byteLength(notes)));
    assert.equal(past.status, 1);
    assert.equal(past.stdout, '');
    assert.match(past.stderr, /^error\[range_not_satisfiable\]/);
    for (const range of [
      ['--offset=-1'],
      ['--offset=1e3'],
      ['--offset='],
      ['--offset', '1.5'],
      ['--length', '0'],
    ]) {
      const refused = part(...range);
      assert.equal(refused.status, 2, range.join(' '));
      assert.equal(refused.stdout, '', range.join(' '));
    }

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
    const copy = await request(
      201,
      'POST',
      `/v1/artifacts/${id}/revisions/1/duplicate`,
      {},
    );
    // An artifact in the trash carries its time; a write there is missing.
    const trashed = `/v1/artifacts/${copy.artifact.id}`;
    await request(200, 'PATCH', trashed, {
      base_metadata_revision: 1,
      deleted: true,
    });
    await request(200, 'GET', '/v1/artifacts?deleted=true');
    await request(404, 'PATCH', trashed, {
      base_metadata_revision: 2,
      title: 'Lost',
    });
    await request(200, 'PATCH', trashed, {
      base_metadata_revision: 2,
      deleted: false,
    });
    // Purge and its refusals, and both reports of unreferenced content.
    await request(409, 'POST', `${trashed}/purge`, {
      base_metadata_revision: 3,
    });
    await request(200, 'PATCH', trashed, {
      base_metadata_revision: 3,
      deleted: true,
    });
    await request(409, 'POST', `${trashed}/purge`, {
      base_metadata_revision: 3,
    });
    const purged = await request(200, 'POST', `${trashed}/purge`, {
      base_metadata_revision: 4,
    });
    assert.equal(purged.metadata_revision, 5);
    await request(404, 'GET', trashed);
    const unreferenced = await request(200, 'GET', '/v1/content/unreferenced');
    await request(409, 'DELETE', '/v1/content/unreferenced', {
      base_digest: '0'.repeat(64),
    });
    await request(400, 'DELETE', '/v1/content/unreferenced', {
      base_digest: 'not a digest',
    });
    await request(200, 'DELETE', '/v1/content/unreferenced', {
      base_digest: unreferenced.digest,
    });
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

test('CLI deletes to the trash, lists it, and recovers', async () => {
  const server = await startServer(binary, dataDirectory('cli-trash'));
  try {
    const site = await freshDirectory('cli-trash-site');
    await writeFile(join(site, 'index.html'), '<h1>kept</h1>');
    const id = json(cli(server, ['create', '--title', 'Old', '--dir', site]))
      .artifact.id;
    const deletion = [
      'delete',
      id,
      '--base-metadata',
      '1',
      '--idempotency-key',
      'cli-delete-key-1',
    ];
    const deleted = json(cli(server, deletion));
    assert.equal(typeof deleted.artifact.deleted_at_ms, 'number');
    assert.equal(deleted.artifact.metadata_revision, 2);
    assert.equal(json(cli(server, deletion)).replayed, true);
    const missing = cli(server, ['show', id]);
    assert.equal(missing.status, 1);
    assert.equal(missing.stdout, '');
    assert.match(missing.stderr, /^error\[not_found\]/);
    const ids = (args) =>
      json(cli(server, ['list', ...args])).artifacts.map(
        (artifact) => artifact.id,
      );
    assert.deepEqual(ids([]), []);
    assert.deepEqual(ids(['--deleted']), [id]);

    const stale = cli(server, ['recover', id, '--base-metadata', '1']);
    assert.equal(stale.status, 1);
    assert.match(stale.stderr, /^error\[metadata_conflict\]/);
    const recovered = json(
      cli(server, ['recover', id, '--base-metadata', '2']),
    );
    assert.equal(recovered.artifact.deleted_at_ms, null);
    assert.deepEqual(ids([]), [id]);
    assert.equal(
      cli(server, ['cat', id, 'index.html']).stdout,
      '<h1>kept</h1>',
    );
    for (const args of [
      ['delete', id],
      ['recover', id, 'extra', '--base-metadata', '3'],
    ]) {
      const refused = cli(server, args);
      assert.equal(refused.status, 2, args.join(' '));
      assert.equal(refused.stdout, '', args.join(' '));
    }
  } finally {
    await server.stop();
  }
});

test('CLI purges an artifact in the trash only when confirmed, and for good', async () => {
  const data = dataDirectory('cli-purge');
  const site = await freshDirectory('cli-purge-site');
  await writeFile(join(site, 'index.html'), '<h1>doomed</h1>');
  const first = await startServer(binary, data);
  const purge = ['purge', 'ID', '--base-metadata', '2', '--yes'];
  let id;
  let copyId;
  let purgedAt;
  try {
    const create = ['create', '--title', 'Doomed', '--dir', site];
    id = json(cli(first, [...create, '--idempotency-key', 'cli-purge-create']))
      .artifact.id;
    copyId = json(cli(first, ['duplicate', id, '1'])).artifact.id;
    const args = (override = {}) =>
      purge.map((value) => override[value] ?? value);

    // Without --yes nothing is sent: a usage error, the artifact untouched.
    for (const withoutYes of [
      ['purge', id, '--base-metadata', '2'],
      ['purge', id, '--yes'],
      ['purge', '--base-metadata', '2', '--yes'],
    ]) {
      const refused = cli(first, withoutYes);
      assert.equal(refused.status, 2, withoutYes.join(' '));
      assert.equal(refused.stdout, '', withoutYes.join(' '));
    }
    json(cli(first, ['delete', id, '--base-metadata', '1']));
    const unconfirmed = cli(first, ['purge', id, '--base-metadata', '2']);
    assert.equal(unconfirmed.status, 2);
    assert.match(unconfirmed.stderr, /--yes/);
    assert.deepEqual(
      json(cli(first, ['list', '--deleted'])).artifacts.map((a) => a.id),
      [id],
      'the unconfirmed purge removed nothing',
    );

    // Outside the trash, with a stale base, then as it should be.
    const live = cli(first, ['purge', copyId, '--base-metadata', '1', '--yes']);
    assert.equal(live.status, 1);
    assert.match(live.stderr, /^error\[not_in_trash\]/);
    const stale = cli(first, ['purge', id, '--base-metadata', '1', '--yes']);
    assert.equal(stale.status, 1);
    assert.match(stale.stderr, /^error\[metadata_conflict\]/);
    assert.match(stale.stderr, /current_metadata_revision=2/);
    const purged = json(
      cli(first, [...args({ ID: id }), '--idempotency-key', 'cli-purge-key-1']),
    );
    assert.deepEqual(
      [purged.artifact_id, purged.metadata_revision, purged.replayed],
      [id, 3, false],
    );
    assert.equal(typeof purged.purged_at_ms, 'number');
    purgedAt = purged.purged_at_ms;
  } finally {
    await first.stop();
  }

  // The server restarted: the retry still replays, the artifact is gone
  // everywhere, and the copy made before keeps its bytes.
  const second = await startServer(binary, data);
  try {
    const again = json(
      cli(second, [
        ...purge.map((value) => (value === 'ID' ? id : value)),
        '--idempotency-key',
        'cli-purge-key-1',
      ]),
    );
    assert.equal(again.replayed, true);
    assert.equal(again.purged_at_ms, purgedAt);
    assert.equal(again.metadata_revision, 3);
    for (const args of [
      ['show', id],
      ['history', id],
      ['cat', id, 'index.html', '--revision', '1'],
      ['recover', id, '--base-metadata', '3'],
      [
        'create',
        '--title',
        'Doomed',
        '--dir',
        site,
        '--idempotency-key',
        'cli-purge-create',
      ],
    ]) {
      const gone = cli(second, args);
      assert.equal(gone.status, 1, args.join(' '));
      assert.match(gone.stderr, /^error\[not_found\]/, args.join(' '));
    }
    assert.deepEqual(json(cli(second, ['list', '--deleted'])).artifacts, []);
    assert.equal(
      cli(second, ['cat', copyId, 'index.html']).stdout,
      '<h1>doomed</h1>',
    );
    const other = cli(second, ['purge', id, '--base-metadata', '3', '--yes']);
    assert.equal(other.status, 1);
    assert.match(other.stderr, /^error\[not_found\]/);
  } finally {
    await second.stop();
  }
});

test('CLI cleanup of unreferenced content reports first and deletes only with --apply', async () => {
  const server = await startServer(binary, dataDirectory('cli-cleanup'));
  try {
    const run = (args) => {
      const env = {
        ...process.env,
        ARTIFREE_URL: server.api_url,
        ARTIFREE_TOKEN: server.token,
      };
      delete env.ARTIFREE_TOKEN_FILE;
      return spawnSync(
        process.execPath,
        ['apps/cli/dist/main.js', 'content', ...args],
        { cwd: root, encoding: 'utf8', timeout: 20_000, env },
      );
    };
    const dry = json(run(['cleanup']));
    assert.deepEqual(dry, {
      applied: false,
      blobs: 0,
      bytes: 0,
      digest: dry.digest,
    });
    assert.match(dry.digest, /^[0-9a-f]{64}$/);
    // The apply reviews the set itself and sends it with a new key.
    const applied = run(['cleanup', '--apply']);
    assert.equal(applied.status, 0, applied.stderr);
    assert.deepEqual(JSON.parse(applied.stdout), {
      applied: true,
      blobs: 0,
      bytes: 0,
      replayed: false,
    });
    assert.match(applied.stderr, new RegExp(`Reviewed set ${dry.digest}`));
    // A retry with the same key and set replays the first report.
    const first = run([
      'cleanup',
      '--apply',
      '--base-digest',
      dry.digest,
      '--idempotency-key',
      'cli-cleanup-key-1',
    ]);
    const retry = run([
      'cleanup',
      '--apply',
      '--base-digest',
      dry.digest,
      '--idempotency-key',
      'cli-cleanup-key-1',
    ]);
    assert.equal(json(first).replayed, false);
    assert.deepEqual(json(retry), {
      applied: true,
      blobs: 0,
      bytes: 0,
      replayed: true,
    });
    // A set the caller did not review is refused, and nothing is sent twice.
    const stale = run(['cleanup', '--apply', '--base-digest', '0'.repeat(64)]);
    assert.equal(stale.status, 1);
    assert.equal(stale.stdout, '');
    assert.match(stale.stderr, /^error\[content_conflict\]/);
    for (const args of [
      ['cleanup', 'extra'],
      ['unknown'],
      ['cleanup', '--yes'],
      ['cleanup', '--base-digest', 'a'.repeat(64)],
    ]) {
      const refused = run(args);
      assert.equal(refused.status, 2, args.join(' '));
      assert.equal(refused.stdout, '', args.join(' '));
    }
    const unauthorized = spawnSync(
      process.execPath,
      ['apps/cli/dist/main.js', 'content', 'cleanup', '--apply'],
      {
        cwd: root,
        encoding: 'utf8',
        timeout: 20_000,
        env: {
          ...process.env,
          ARTIFREE_URL: server.api_url,
          ARTIFREE_TOKEN: 'wrong',
        },
      },
    );
    assert.equal(unauthorized.status, 1);
    assert.match(unauthorized.stderr, /^error\[unauthenticated\]/);
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

test('CLI exports a revision byte for byte, with a manifest that lists it', async () => {
  const schema = JSON.parse(
    await readFile(
      new URL('../../contracts/generated/api-v1.schema.json', import.meta.url),
      'utf8',
    ),
  );
  const manifestIsValid = createAjv().compile({
    $schema: schema.$schema,
    $defs: schema.$defs,
    $ref: '#/$defs/SourceExportManifest',
  });
  const server = await startServer(binary, dataDirectory('cli-export'));
  try {
    const site = await freshDirectory('cli-export-site');
    const files = {
      'index.html': Buffer.from('\uFEFF<h1>one</h1>\r\n'),
      'assets/logo.png': Buffer.from([0x89, 0x50, 0x4e, 0x47, 0, 0xff, 0x0d]),
      'lib/empty.js': Buffer.alloc(0),
    };
    await mkdir(join(site, 'assets'));
    await mkdir(join(site, 'lib'));
    for (const [path, bytes] of Object.entries(files))
      await writeFile(join(site, path), bytes);
    const id = json(
      cli(server, ['create', '--title', 'Export ✓', '--dir', site]),
    ).artifact.id;
    await writeFile(join(site, 'index.html'), '<h1>two</h1>');
    json(cli(server, ['save', id, '--base', '1', '--dir', site]));
    const [, first] = json(cli(server, ['history', id])).revisions;

    const out = await freshDirectory('cli-export-out');
    const archive = join(out, 'first.zip');
    const exported = json(
      cli(server, ['export', id, '--revision', '1', '--output', archive]),
    );
    const bytes = await readFile(archive);
    assert.deepEqual(exported, {
      artifact_id: id,
      revision: 1,
      output: archive,
      bytes: bytes.length,
      sha256: createHash('sha256').update(bytes).digest('hex'),
    });
    const entries = storedZipEntries(bytes);
    assert.deepEqual(
      entries.map((entry) => entry.name),
      [
        '.artifree-export.json',
        'assets/logo.png',
        'index.html',
        'lib/empty.js',
      ],
    );
    for (const entry of entries.slice(1)) {
      assert.deepEqual(entry.content, files[entry.name], entry.name);
      assert.equal(entry.mode, 0o100644, entry.name);
    }
    const manifest = JSON.parse(entries[0].content.toString('utf8'));
    assert.ok(
      manifestIsValid(manifest),
      JSON.stringify(manifestIsValid.errors?.slice(0, 3)),
    );
    assert.deepEqual(manifest, {
      format: 'artifree.source-export.v1',
      artifact_id: id,
      title: 'Export ✓',
      revision: 1,
      created_at_ms: first.created_at_ms,
      entry: 'index.html',
      runtime: 'none',
      files: first.files,
    });
    for (const file of manifest.files)
      assert.equal(
        createHash('sha256').update(files[file.path]).digest('hex'),
        file.sha256,
        file.path,
      );
    const committed = new Date(first.created_at_ms);
    assert.deepEqual(entries[1].modified, {
      year: committed.getUTCFullYear(),
      month: committed.getUTCMonth() + 1,
      day: committed.getUTCDate(),
      hour: committed.getUTCHours(),
      minute: committed.getUTCMinutes(),
      second: committed.getUTCSeconds() - (committed.getUTCSeconds() % 2),
    });

    // The current revision by default, and the same bytes every time.
    const latest = join(out, 'latest.zip');
    assert.equal(
      json(cli(server, ['export', id, '--output', latest])).revision,
      2,
    );
    const again = join(out, 'again.zip');
    json(cli(server, ['export', id, '--output', again]));
    assert.deepEqual(await readFile(again), await readFile(latest));

    // An existing file is never replaced, and a refusal writes nothing.
    const kept = cli(server, [
      'export',
      id,
      '--revision',
      '2',
      '--output',
      archive,
    ]);
    assert.equal(kept.status, 1);
    assert.equal(kept.stdout, '');
    assert.match(kept.stderr, /already exists/);
    assert.deepEqual(await readFile(archive), bytes);
    const missing = join(out, 'missing.zip');
    const unknown = cli(server, [
      'export',
      id,
      '--revision',
      '3',
      '--output',
      missing,
    ]);
    assert.equal(unknown.status, 1);
    assert.match(unknown.stderr, /^error\[not_found\]/);
    await assert.rejects(readFile(missing), { code: 'ENOENT' });
    const usage = cli(server, ['export', id]);
    assert.equal(usage.status, 2);
    assert.equal(usage.stdout, '');

    // A runtime-enabled entry exports its original bytes and declares that
    // it needs the workspace, using the new manifest format from 0019.
    const runtimeSite = await freshDirectory('cli-export-runtime');
    const runtimeSource =
      '<!doctype html><html><head><!-- artifree-runtime --></head><body>runtime</body></html>';
    await writeFile(join(runtimeSite, 'index.html'), runtimeSource);
    const runtimeId = json(
      cli(server, ['create', '--title', 'Runtime', '--dir', runtimeSite]),
    ).artifact.id;
    const runtimeArchive = join(out, 'runtime.zip');
    json(cli(server, ['export', runtimeId, '--output', runtimeArchive]));
    const runtimeEntries = storedZipEntries(await readFile(runtimeArchive));
    const runtimeManifest = JSON.parse(
      runtimeEntries[0].content.toString('utf8'),
    );
    assert.ok(
      manifestIsValid(runtimeManifest),
      JSON.stringify(manifestIsValid.errors),
    );
    assert.equal(runtimeManifest.format, 'artifree.source-export.v2');
    assert.equal(runtimeManifest.runtime, 'metadata');
    assert.equal(runtimeEntries[1].content.toString('utf8'), runtimeSource);
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

test('CLI queues a verification job that a separate worker process finishes', async () => {
  const data = dataDirectory('cli-jobs');
  const site = await freshDirectory('cli-jobs-site');
  await writeFile(join(site, 'index.html'), '<p>verify me</p>');
  await writeFile(join(site, 'app.js'), 'let a = 1;');
  const server = await startServer(binary, data, ['--no-worker']);
  try {
    const id = json(cli(server, ['create', '--title', 'Jobs', '--dir', site]))
      .artifact.id;
    const enqueue = [
      'enqueue',
      id,
      '--revision',
      '1',
      '--idempotency-key',
      'cli-job-enqueue-1',
    ];
    const queued = json(jobCli(server, enqueue));
    assert.equal(queued.replayed, false);
    assert.equal(queued.job.state, 'queued');
    const jobId = queued.job.id;
    assert.equal(json(jobCli(server, enqueue)).replayed, true);

    // With --no-worker nothing runs the job by itself.
    assert.equal(json(jobCli(server, ['show', jobId])).state, 'queued');

    // A worker process on the same data directory, beside the running server.
    const events = drainJobs(data);
    assert.deepEqual(
      events.map((event) => event.event),
      ['ready', 'claimed', 'job', 'stopped'],
    );
    assert.equal(events[1].job, jobId);
    assert.deepEqual(
      [events[2].job, events[2].state, events[2].verified],
      [jobId, 'succeeded', true],
    );
    assert.equal(events[3].reason, 'drained');

    const shown = json(jobCli(server, ['show', jobId]));
    assert.equal(shown.state, 'succeeded');
    assert.equal(shown.attempt, 1);
    assert.deepEqual(
      [
        shown.result.verdict,
        shown.result.files_checked,
        shown.result.mismatch_count,
      ],
      ['ok', 2, 0],
    );
    assert.deepEqual(
      json(jobCli(server, ['list', '--artifact', id])).jobs.map((j) => j.id),
      [jobId],
    );

    // A job that ended cannot be cancelled; one that waits can, and a worker
    // then finds nothing to do.
    const ended = jobCli(server, ['cancel', jobId]);
    assert.equal(ended.status, 1);
    assert.match(ended.stderr, /^error\[job_ended\]/);
    const second = json(
      jobCli(server, [
        'enqueue',
        id,
        '--revision',
        '1',
        '--idempotency-key',
        'cli-job-enqueue-2',
      ]),
    ).job.id;
    assert.equal(
      json(jobCli(server, ['cancel', second])).job.state,
      'cancelled',
    );
    assert.deepEqual(
      drainJobs(data).map((event) => event.event),
      ['ready', 'stopped'],
    );

    // Usage and not-found errors.
    const unknown = jobCli(server, ['show', `job_${'0'.repeat(32)}`]);
    assert.equal(unknown.status, 1);
    assert.match(unknown.stderr, /^error\[not_found\]/);
    assert.equal(jobCli(server, ['enqueue', id]).status, 2);
    const missing = jobCli(server, ['enqueue', id, '--revision', '9']);
    assert.equal(missing.status, 1);
    assert.match(missing.stderr, /^error\[not_found\]/);
  } finally {
    await server.stop();
  }
});

test('CLI keeps application data per artifact, with versions, across a restart, and copies it only when asked', async () => {
  const data = dataDirectory('cli-app-data');
  const site = await freshDirectory('cli-app-data-site');
  await writeFile(join(site, 'index.html'), '<p>data</p>');
  const write = (id, version, path, value, key) => [
    'set',
    id,
    '--path',
    path,
    '--base-data-version',
    String(version),
    '--value',
    value,
    '--idempotency-key',
    key,
  ];
  const first = await startServer(binary, data);
  let id;
  try {
    id = json(cli(first, ['create', '--title', 'Data', '--dir', site])).artifact
      .id;
    const empty = json(dataCli(first, ['get', id]));
    assert.deepEqual(
      [empty.data_version, empty.exists, empty.value],
      [0, true, {}],
    );
    const set = json(
      dataCli(
        first,
        write(id, 0, 'todos/first', '{"done":false}', 'cli-app-1'),
      ),
    );
    assert.deepEqual(
      [set.data_version, set.changed, set.replayed],
      [1, true, false],
    );
    assert.equal(
      json(
        dataCli(
          first,
          write(id, 0, 'todos/first', '{"done":false}', 'cli-app-1'),
        ),
      ).replayed,
      true,
    );
    const patched = json(
      dataCli(first, [
        'update',
        id,
        '--path',
        'todos/first',
        '--base-data-version',
        '1',
        '--patch',
        '{"done":true,"tags":["a"]}',
      ]),
    );
    assert.equal(patched.data_version, 2);

    // A stale write fails and says which version is current.
    const stale = dataCli(first, write(id, 1, 'x', '1', 'cli-app-2'));
    assert.equal(stale.status, 1);
    assert.match(stale.stderr, /^error\[data_conflict\]/);
    assert.match(stale.stderr, /^current_data_version=2$/m);

    // What JSON.parse would silently change is refused by the server.
    // Numbers beyond 2^53 - 1 are refused in every spelling, and the extremes
    // of the integer types neither crash the server nor the command.
    for (const value of [
      '{"a":1,"a":2}',
      '9007199254740993',
      '-9223372036854775808',
      '9223372036854775807',
      '18446744073709551615',
      '9007199254740993.0',
      '9.007199254740993e15',
      '1e16',
      '1e999',
    ]) {
      // `--value=` so that a negative number is not taken for an option.
      const refused = dataCli(first, [
        'set',
        id,
        '--path',
        'x',
        '--base-data-version',
        '2',
        `--value=${value}`,
        '--idempotency-key',
        'cli-app-3',
      ]);
      assert.equal(refused.status, 1, value);
      assert.match(refused.stderr, /^error\[invalid_request\]/, value);
      assert.doesNotMatch(refused.stderr, /panic/i, value);
    }
    // Without the `=`, a value that starts with a dash is a usage error and
    // nothing is sent.
    const dash = dataCli(first, write(id, 2, 'x', '-1', 'cli-app-dash'));
    assert.equal(dash.status, 2);
    assert.equal(dash.stdout, '');
    assert.equal(
      json(dataCli(first, ['get', id])).data_version,
      2,
      'nothing was written',
    );
    const valueFile = join(site, 'value.json');
    await writeFile(valueFile, '{"from":"file"}');
    const fromFile = json(
      dataCli(first, [
        'set',
        id,
        '--path',
        'file',
        '--base-data-version',
        '2',
        '--value-file',
        valueFile,
      ]),
    );
    assert.equal(fromFile.data_version, 3);

    // A new revision is not a data change.
    await writeFile(join(site, 'index.html'), '<p>data v2</p>');
    json(cli(first, ['save', id, '--base', '1', '--dir', site]));
    assert.equal(json(dataCli(first, ['get', id])).data_version, 3);
  } finally {
    await first.stop();
  }

  const second = await startServer(binary, data);
  try {
    const kept = json(dataCli(second, ['get', id]));
    assert.equal(kept.data_version, 3);
    assert.deepEqual(kept.value, {
      todos: { first: { done: true, tags: ['a'] } },
      file: { from: 'file' },
    });
    assert.equal(
      json(dataCli(second, ['get', id, '--path', 'todos/second'])).exists,
      false,
    );

    const plain = json(cli(second, ['duplicate', id, '1']));
    assert.deepEqual(plain.copied, ['files']);
    assert.equal(
      json(dataCli(second, ['get', plain.artifact.id])).data_version,
      0,
    );
    const copy = json(cli(second, ['duplicate', id, '1', '--copy-data']));
    assert.deepEqual(copy.copied, ['files', 'data']);
    const copied = json(dataCli(second, ['get', copy.artifact.id]));
    assert.deepEqual(copied.value, kept.value);
    assert.equal(copied.data_version, 1);

    // Assets come along only with --copy-assets, under the same IDs.
    const assetsOf = async (artifactId) => {
      const response = await fetch(
        `${second.api_url}/v1/artifacts/${artifactId}/assets`,
        { headers: { authorization: `Bearer ${second.token}` } },
      );
      assert.equal(response.status, 200);
      return response.json();
    };
    const uploaded = await fetch(
      `${second.api_url}/v1/artifacts/${id}/assets`,
      {
        method: 'POST',
        headers: {
          authorization: `Bearer ${second.token}`,
          'content-type': 'application/json',
          'idempotency-key': 'cli-asset-upload-1',
        },
        body: JSON.stringify({
          base_asset_version: 0,
          content_type: 'text/plain',
          content_base64: Buffer.from('asset bytes').toString('base64'),
        }),
      },
    );
    assert.equal(uploaded.status, 201);
    const noAssets = json(cli(second, ['duplicate', id, '1']));
    assert.deepEqual((await assetsOf(noAssets.artifact.id)).assets, []);
    const withAssets = json(
      cli(second, ['duplicate', id, '1', '--copy-data', '--copy-assets']),
    );
    assert.deepEqual(withAssets.copied, ['files', 'data', 'assets']);
    assert.deepEqual(
      (await assetsOf(withAssets.artifact.id)).assets,
      (await assetsOf(id)).assets,
    );

    // The copy is its own space.
    json(
      dataCli(second, [
        'delete',
        copy.artifact.id,
        '--path',
        '',
        '--base-data-version',
        '1',
      ]),
    );
    assert.deepEqual(
      json(dataCli(second, ['get', copy.artifact.id])).value,
      {},
    );
    assert.equal(json(dataCli(second, ['get', id])).data_version, 3);

    // Behind a proxy that strips the body of a DELETE, a value below the root
    // is removed with a null patch; the root is not, because a null patch
    // cannot make the root anything but an object. `set` with `{}` empties it.
    const spaceId = copy.artifact.id;
    json(
      dataCli(second, [
        'set',
        spaceId,
        '--path',
        'k',
        '--base-data-version',
        '2',
        '--value',
        '1',
      ]),
    );
    const rootNull = dataCli(second, [
      'update',
      spaceId,
      '--path',
      '',
      '--base-data-version',
      '3',
      '--patch',
      'null',
    ]);
    assert.equal(rootNull.status, 1);
    assert.match(rootNull.stderr, /^error\[invalid_request\].*root/);
    assert.deepEqual(json(dataCli(second, ['get', spaceId])).value, { k: 1 });
    const below = json(
      dataCli(second, [
        'update',
        spaceId,
        '--path',
        'k',
        '--base-data-version',
        '3',
        '--patch',
        'null',
      ]),
    );
    assert.deepEqual(
      [below.data_version, json(dataCli(second, ['get', spaceId])).value],
      [4, {}],
    );
    json(
      dataCli(second, [
        'set',
        spaceId,
        '--path',
        'k',
        '--base-data-version',
        '4',
        '--value',
        '1',
      ]),
    );
    const cleared = json(
      dataCli(second, [
        'set',
        spaceId,
        '--path',
        '',
        '--base-data-version',
        '5',
        '--value',
        '{}',
      ]),
    );
    assert.deepEqual(
      [cleared.data_version, json(dataCli(second, ['get', spaceId])).value],
      [6, {}],
    );

    // In the trash, nothing about the data is there to read or write.
    json(cli(second, ['delete', id, '--base-metadata', '1']));
    const hidden = dataCli(second, ['get', id]);
    assert.equal(hidden.status, 1);
    assert.match(hidden.stderr, /^error\[not_found\]/);
  } finally {
    await second.stop();
  }
});
