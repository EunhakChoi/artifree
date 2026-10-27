import assert from 'node:assert/strict';
import { execFile, spawnSync } from 'node:child_process';
import { once } from 'node:events';
import { readFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

const root = fileURLToPath(new URL('../../', import.meta.url));
const manifest = JSON.parse(
  await readFile(new URL('../../package.json', import.meta.url), 'utf8'),
);
function cli(args) {
  const result = spawnSync(
    process.execPath,
    ['apps/cli/dist/main.js', ...args],
    { cwd: root, encoding: 'utf8', timeout: 10_000 },
  );
  if (result.error) throw result.error;
  return result;
}

test('CLI version emits only the requested version', () => {
  const result = cli(['--version']);
  assert.equal(result.status, 0);
  assert.equal(result.stdout, `${manifest.version}\n`);
  assert.equal(result.stderr, '');
});

test('CLI JSON matches the live Rust descriptor', () => {
  const rust = spawnSync(
    'cargo',
    ['run', '--quiet', '--locked', '-p', 'artifree-server', '--', '--describe'],
    { cwd: root, encoding: 'utf8', timeout: 120_000 },
  );
  if (rust.error) throw rust.error;
  assert.equal(rust.status, 0, rust.stderr);
  const result = cli(['describe', '--json']);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stderr, '');
  const descriptor = JSON.parse(result.stdout);
  assert.deepEqual(descriptor, JSON.parse(rust.stdout));
  assert.equal(descriptor.http_api, true);
  assert.equal(descriptor.mcp_api, true);
  assert.equal(descriptor.server_framework, 'axum');
  assert.equal(descriptor.async_runtime, 'tokio');
});

test('CLI list without paging options prints every page', async () => {
  // A stand-in server with two pages: a real one pages only after 500.
  const artifact = (title, index) => ({
    id: `art_${String(index).repeat(32)}`,
    kind: 'app',
    title,
    pinned: false,
    current_revision: 1,
    metadata_revision: 1,
    created_at_ms: index,
    updated_at_ms: index,
  });
  const pages = new Map([
    [null, { artifacts: [artifact('One', 1)], next_page_token: 'second' }],
    ['second', { artifacts: [artifact('Two', 2)], next_page_token: null }],
  ]);
  const queries = [];
  const server = createServer((request, response) => {
    const { searchParams } = new URL(request.url ?? '/', 'http://stub');
    queries.push(Object.fromEntries(searchParams));
    response.setHeader('content-type', 'application/json');
    response.end(JSON.stringify(pages.get(searchParams.get('page_token'))));
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  try {
    const address = server.address();
    assert.ok(address !== null && typeof address === 'object');
    const env = {
      ...process.env,
      ARTIFREE_URL: `http://127.0.0.1:${address.port}`,
      ARTIFREE_TOKEN: 'f'.repeat(64),
    };
    delete env.ARTIFREE_TOKEN_FILE;
    const { stdout } = await promisify(execFile)(
      process.execPath,
      ['apps/cli/dist/main.js', 'artifact', 'list', '--search', 'o'],
      { cwd: root, env, timeout: 10_000 },
    );
    assert.deepEqual(
      JSON.parse(stdout).artifacts.map(({ title }) => title),
      ['One', 'Two'],
    );
    assert.deepEqual(queries, [{ q: 'o' }, { q: 'o', page_token: 'second' }]);
  } finally {
    server.close();
  }
});

test('CLI help says the inventory does not probe a server', () => {
  const result = cli(['--help']);
  assert.equal(result.status, 0);
  assert.match(result.stdout, /Usage:/);
  assert.match(result.stdout, /does not probe a server/);
  assert.equal(result.stderr, '');
});

for (const args of [
  ['serve'],
  ['describe', 'extra'],
  ['--json'],
  ['--unknown'],
  ['--help', '--version'],
  ['--help', '--json'],
  ['describe', '--version'],
  ['artifact', 'unknown'],
  ['artifact', 'list', 'extra'],
  ['artifact', '--help', 'list'],
]) {
  test(`CLI rejects ${args.join(' ')} without success output`, () => {
    const result = cli(args);
    assert.equal(result.status, 2);
    assert.equal(result.stdout, '');
    assert.ok(
      result.stderr.trim().length > 0,
      'Invalid input needs an actionable diagnostic.',
    );
  });
}
