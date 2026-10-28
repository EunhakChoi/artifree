import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The web's copy of the domain's path rules, bundled as it ships.
const bundle = await build({
  entryPoints: [
    fileURLToPath(
      new URL('../../apps/web/src/artifacts/package-path.ts', import.meta.url),
    ),
  ],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  mainFields: ['module', 'main'],
  write: false,
});
const { mediaTypeOf, pathError } = await import(
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
);
const { cases } = JSON.parse(
  await readFile(
    new URL('../../contracts/fixtures/package-paths.json', import.meta.url),
    'utf8',
  ),
);

test('The web judges every shared path case as the domain does', () => {
  assert.ok(cases.length >= 40, `The fixture lost cases: ${cases.length}`);
  for (const { path, error } of cases)
    assert.equal(pathError(path) ?? null, error, JSON.stringify(path));
});

test('A media type follows the last extension of the file name, in any case', () => {
  assert.equal(mediaTypeOf('a/b/Page.HTM'), 'text/html; charset=utf-8');
  assert.equal(mediaTypeOf('x.tar.css'), 'text/css; charset=utf-8');
  assert.equal(mediaTypeOf('page.css/index'), undefined);
  assert.equal(mediaTypeOf('toString'), undefined);
  assert.equal(mediaTypeOf('a.constructor'), undefined);
  assert.equal(mediaTypeOf('a.__proto__'), undefined);
});
