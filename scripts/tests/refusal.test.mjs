import assert from 'node:assert/strict';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The rule is web code without DOM or React imports; bundle it as it ships.
const bundle = await build({
  entryPoints: [
    fileURLToPath(
      new URL('../../apps/web/src/artifacts/refusal.ts', import.meta.url),
    ),
  ],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const { readPast } = await import(
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
);

test('A refused base holds until the page reads past it and the named version', () => {
  const refusal = { base: 4, current: 6, resets: 2 };
  assert.equal(readPast(undefined, 1, 0), true, 'Nothing was refused.');
  assert.equal(readPast(refusal, 4, 2), false, 'The refused base itself.');
  assert.equal(readPast(refusal, 5, 2), false, 'Older than the named one.');
  assert.equal(readPast(refusal, 6, 2), true);
  assert.equal(readPast(refusal, 7, 2), true);
  // A conflict without a current version needs only a newer one.
  assert.equal(readPast({ ...refusal, current: undefined }, 5, 2), true);
});

test('A refusal from before the data may have been replaced no longer holds', () => {
  // Replaced data can be older than the refused base: the page could
  // never read past it.
  assert.equal(readPast({ base: 7, current: 8, resets: 1 }, 5, 2), true);
});
