import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, before, test } from 'node:test';
import { fileURLToPath } from 'node:url';
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

    // A missing base revision is server state: an API error, not usage.
    const missing = cli(server, ['save', id, '--base', '99', '--dir', site]);
    assert.equal(missing.status, 1);
    assert.equal(missing.stdout, '');
    assert.match(missing.stderr, /^error\[not_found\]/);

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
