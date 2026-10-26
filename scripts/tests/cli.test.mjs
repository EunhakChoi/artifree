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

test('CLI help identifies its scaffold limitations', () => {
  const result = cli(['--help']);
  assert.equal(result.status, 0);
  assert.match(result.stdout, /Usage:/);
  assert.match(result.stdout, /not implemented/);
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
