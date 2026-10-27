import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

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
  assert.equal(descriptor.server_framework, 'axum');
  assert.equal(descriptor.async_runtime, 'tokio');
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
