import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtemp, readdir, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, before, test } from 'node:test';
import { fileURLToPath } from 'node:url';

// Decision 0028: a static build can carry a default API server in
// VITE_ARTIFREE_SERVER. These tests run the real Vite build: a good address is
// compiled into the page, none leaves the page as it was, and a bad address
// stops the build instead of producing a page that cannot connect.

const root = fileURLToPath(new URL('../../', import.meta.url));
const marker = 'https://default-marker.example.test';
let scratch;

before(async () => {
  scratch = await mkdtemp(join(tmpdir(), 'artifree-web-default-test-'));
});

after(async () => {
  await rm(scratch, { recursive: true, force: true });
});

function build(name, server) {
  const env = { ...process.env };
  delete env.VITE_ARTIFREE_SERVER;
  if (server !== undefined) env.VITE_ARTIFREE_SERVER = server;
  const outDir = join(scratch, name);
  const result = spawnSync(
    process.execPath,
    [
      join(root, 'node_modules/vite/bin/vite.js'),
      'build',
      '--outDir',
      outDir,
      '--emptyOutDir',
    ],
    { cwd: join(root, 'apps/web'), encoding: 'utf8', env, timeout: 170_000 },
  );
  return { ...result, outDir };
}

async function bundleText(outDir) {
  const files = await readdir(join(outDir, 'assets'));
  const scripts = files.filter((file) => file.endsWith('.js'));
  assert.ok(scripts.length > 0, 'the build must write a script');
  const texts = await Promise.all(
    scripts.map((file) => readFile(join(outDir, 'assets', file), 'utf8')),
  );
  return texts.join('\n');
}

test('a build with an empty variable has no default server, whatever an env file says', async () => {
  // A process variable beats every `apps/web/.env*` file in Vite, so the empty
  // value keeps this case independent of a developer's ignored local files.
  const result = build('none', '');
  assert.equal(result.status, 0, result.stderr + result.stdout);
  assert.ok(!(await bundleText(result.outDir)).includes(marker));
});

test('a build with the variable compiles the address into the page', async () => {
  const result = build('set', marker);
  assert.equal(result.status, 0, result.stderr + result.stdout);
  assert.ok((await bundleText(result.outDir)).includes(marker));
});

const refused = [
  ['credentials', 'https://user:secret@api.example.test', /credentials/],
  ['another scheme', 'ftp://api.example.test', /http:\/\/ or https:\/\//],
  ['a relative address', '/v1', /absolute/],
  ['a query string', 'https://api.example.test/?x=1', /query string/],
];
for (const [name, value, pattern] of refused) {
  test(`a build with ${name} in the variable fails`, () => {
    const result = build(`refused-${name.replaceAll(' ', '-')}`, value);
    assert.notEqual(result.status, 0, 'the build must not succeed');
    const output = result.stderr + result.stdout;
    assert.match(output, /VITE_ARTIFREE_SERVER is not usable\./);
    assert.match(output, pattern);
    assert.doesNotMatch(output, /secret/, 'a secret is never echoed');
  });
}
