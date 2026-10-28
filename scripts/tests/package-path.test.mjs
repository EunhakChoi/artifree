import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The web's copy of the path rules, bundled as it ships.
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
const fixture = JSON.parse(
  await readFile(
    new URL('../../contracts/fixtures/package-paths.json', import.meta.url),
    'utf8',
  ),
);

test('The web judges every shared path case as the server does', () => {
  assert.ok(fixture.cases.length > 30, 'the fixture lost its cases');
  for (const { path, error } of fixture.cases)
    assert.equal(pathError(path) ?? null, error, JSON.stringify(path));
});

test('A media type follows the extension without regard to case', () => {
  assert.equal(mediaTypeOf('a/b/Page.HTM'), 'text/html; charset=utf-8');
  assert.equal(mediaTypeOf('font.woff2'), 'font/woff2');
  assert.equal(mediaTypeOf('dir.css/file'), undefined);
  assert.equal(mediaTypeOf('toString'), undefined);
  assert.equal(mediaTypeOf('a.constructor'), undefined);
});
