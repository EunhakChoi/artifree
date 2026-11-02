import assert from 'node:assert/strict';
import { mkdtemp, readdir, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, before, test } from 'node:test';
import { buildStaticWeb } from '../lib/static-web.mjs';

// The real Vite build with VITE_ARTIFREE_SERVER (decision 0028). The variable
// is always set explicitly, also to nothing, so that a developer's
// apps/web/.env* files cannot change what these tests build.

let scratch;

before(async () => {
  scratch = await mkdtemp(join(tmpdir(), 'artifree-web-default-'));
});

after(async () => {
  await rm(scratch, { recursive: true, force: true });
});

function build(name, serverValue) {
  const outDir = join(scratch, name);
  return { ...buildStaticWeb(outDir, serverValue), outDir };
}

async function bundleText(outDir) {
  const assets = join(outDir, 'assets');
  const names = (await readdir(assets)).filter((n) => n.endsWith('.js'));
  const texts = await Promise.all(
    names.map((n) => readFile(join(assets, n), 'utf8')),
  );
  return texts.join('\n');
}

test('a valid default server is built into the page', async () => {
  const result = build('with-default', 'https://api.default.example/');
  assert.equal(result.status, 0, result.stderr);
  const text = await bundleText(result.outDir);
  assert.ok(text.includes('https://api.default.example/'));
});

test('an empty default leaves nothing in the page', async () => {
  const result = build('without-default', '');
  assert.equal(result.status, 0, result.stderr);
  assert.ok(!(await bundleText(result.outDir)).includes('default.example'));
});

for (const [name, value] of [
  ['credentials', 'https://user:hunter2@api.default.example'],
  ['a query', 'https://api.default.example?token=hunter2'],
  ['an empty fragment', 'https://api.default.example#'],
  ['a relative address', '/api'],
  ['another scheme', 'ftp://api.default.example'],
]) {
  test(`a default server with ${name} stops the build`, () => {
    const result = build('refused', value);
    assert.notEqual(result.status, 0, 'The build accepted the address.');
    const output = `${result.stdout}${result.stderr}`;
    assert.match(output, /VITE_ARTIFREE_SERVER must be an absolute http/);
    assert.ok(!output.includes('hunter2'), 'The error repeated the value.');
    assert.ok(
      !output.includes('default.example'),
      'The error repeated the value.',
    );
  });
}
