import assert from 'node:assert/strict';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The comparison is web code without DOM or React imports; bundle it as it
// ships.
const bundle = await build({
  entryPoints: [
    fileURLToPath(
      new URL('../../apps/web/src/artifacts/compare.ts', import.meta.url),
    ),
  ],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const { MAX_EDITS, MAX_LINES, compareManifests, diffLines, hunks, splitLines } =
  await import(
    `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
  );

/** The number of lines two sequences share at most (dynamic programming). */
function commonLength(a, b) {
  const table = Array.from({ length: a.length + 1 }, () =>
    Array.from({ length: b.length + 1 }, () => 0),
  );
  for (let i = a.length - 1; i >= 0; i -= 1)
    for (let j = b.length - 1; j >= 0; j -= 1)
      table[i][j] =
        a[i] === b[j]
          ? table[i + 1][j + 1] + 1
          : Math.max(table[i + 1][j], table[i][j + 1]);
  return table[0][0];
}

test('Comparisons rebuild both sides with the fewest changed lines', () => {
  // A fixed generator, so a failure reproduces.
  let seed = 7;
  const next = () => (seed = (seed * 1103515245 + 12345) % 2 ** 31) / 2 ** 31;
  const sample = () =>
    Array.from(
      { length: Math.floor(next() * 12) },
      () => 'abc'[Math.floor(next() * 3)],
    );
  for (let round = 0; round < 3000; round += 1) {
    const before = sample();
    const after = sample();
    const lines = diffLines(before, after);
    const label = JSON.stringify([before, after]);
    assert.deepEqual(
      lines.filter((line) => line.kind !== 'added').map((line) => line.text),
      before,
      label,
    );
    assert.deepEqual(
      lines.filter((line) => line.kind !== 'removed').map((line) => line.text),
      after,
      label,
    );
    assert.equal(
      lines.filter((line) => line.kind === 'same').length,
      commonLength(before, after),
      label,
    );
    let earlier = 0;
    let later = 0;
    for (const line of lines) {
      if (line.kind !== 'added') assert.equal(line.before, (earlier += 1));
      if (line.kind !== 'removed') assert.equal(line.after, (later += 1));
    }
  }
});

test('Too many lines or changes give no comparison', () => {
  assert.equal(
    diffLines(
      Array.from({ length: MAX_LINES + 1 }, () => 'x'),
      [],
    ),
    undefined,
  );
  const lines = Array.from({ length: MAX_EDITS + 1 }, (_, index) => `${index}`);
  assert.equal(diffLines(lines, []), undefined);
  assert.equal(diffLines(lines.slice(1), []).length, MAX_EDITS);
});

test('Lines end at line feeds and keep carriage returns', () => {
  assert.deepEqual(splitLines('a\r\nb\n'), ['a\r', 'b']);
  assert.deepEqual(splitLines('a\n\n'), ['a', '']);
  assert.deepEqual(splitLines('a'), ['a']);
  assert.deepEqual(splitLines(''), []);
});

test('Hunks keep three lines of context and join nearby changes', () => {
  const before = Array.from({ length: 30 }, (_, index) => `line ${index}`);
  const after = [...before];
  after[5] = 'five';
  after[10] = 'ten';
  after[25] = 'twenty-five';
  const groups = hunks(diffLines(before, after));
  assert.deepEqual(
    groups.map(({ lines }) => [lines[0].text, lines.at(-1).text]),
    [
      ['line 2', 'line 13'],
      ['line 22', 'line 28'],
    ],
  );
  assert.deepEqual(hunks(diffLines(before, before)), []);
});

test('Manifests are compared by digest, without reading files', () => {
  const file = (path, sha256) => ({
    path,
    sha256: sha256.repeat(64),
    size: 1,
    media_type: 'text/plain; charset=utf-8',
  });
  const revision = (files) => ({
    revision: 1,
    created_by: 'owner',
    created_at_ms: 0,
    files,
  });
  const { differences, unchanged } = compareManifests(
    revision([file('a.txt', 'a'), file('b.txt', 'b'), file('c.txt', 'c')]),
    revision([file('b.txt', 'b'), file('c.txt', 'd'), file('d.txt', 'e')]),
  );
  assert.deepEqual(
    differences.map(({ path, kind }) => [path, kind]),
    [
      ['a.txt', 'removed'],
      ['c.txt', 'changed'],
      ['d.txt', 'added'],
    ],
  );
  assert.equal(unchanged, 1);
});
