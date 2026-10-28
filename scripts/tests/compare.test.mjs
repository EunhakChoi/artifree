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

/** A small seeded generator, so a failure names a reproducible case. */
function random(seed) {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let value = Math.imul(state ^ (state >>> 15), 1 | state);
    value ^= value + Math.imul(value ^ (value >>> 7), 61 | value);
    return ((value ^ (value >>> 14)) >>> 0) / 4294967296;
  };
}

function longestCommon(a, b) {
  const row = Array.from({ length: b.length + 1 }, () => 0);
  for (const line of a) {
    let diagonal = 0;
    for (let j = 1; j <= b.length; j += 1) {
      const above = row[j];
      row[j] = line === b[j - 1] ? diagonal + 1 : Math.max(row[j], row[j - 1]);
      diagonal = above;
    }
  }
  return row[b.length];
}

test('A line comparison is a shortest edit script that rebuilds both sides', () => {
  const next = random(20261028);
  for (let round = 0; round < 3000; round += 1) {
    const lines = (length) =>
      Array.from({ length }, () => 'abcd'[Math.floor(next() * 4)]);
    const before = lines(Math.floor(next() * 30));
    const after = lines(Math.floor(next() * 30));
    const result = diffLines(before, after);
    const label = JSON.stringify({ before, after });
    assert.deepEqual(
      result.filter((line) => line.kind !== 'added').map((line) => line.text),
      before,
      label,
    );
    assert.deepEqual(
      result.filter((line) => line.kind !== 'removed').map((line) => line.text),
      after,
      label,
    );
    const edits = result.filter((line) => line.kind !== 'same').length;
    assert.equal(
      edits,
      before.length + after.length - 2 * longestCommon(before, after),
      label,
    );
    // Numbers count each side's lines from 1.
    assert.deepEqual(
      result.flatMap((line) => line.before ?? []),
      before.map((_, index) => index + 1),
      label,
    );
    assert.deepEqual(
      result.flatMap((line) => line.after ?? []),
      after.map((_, index) => index + 1),
      label,
    );
  }
});

test('Past the line or edit limit only sizes are compared', () => {
  const numbered = (prefix, count) =>
    Array.from({ length: count }, (_, index) => `${prefix}${index}\n`);
  assert.equal(diffLines(numbered('a', MAX_LINES + 1), []), undefined);
  const half = MAX_EDITS / 2;
  assert.equal(
    diffLines(numbered('a', half), numbered('b', half)).length,
    MAX_EDITS,
  );
  assert.equal(
    diffLines(numbered('a', half + 1), numbered('b', half)),
    undefined,
  );
  // Common lines at either end cost nothing.
  const same = numbered('s', MAX_LINES - 2);
  assert.equal(
    diffLines([...same, 'x\n'], [...same, 'y\n']).filter(
      (line) => line.kind !== 'same',
    ).length,
    2,
  );
});

test('Lines keep their carriage returns and line feeds', () => {
  assert.deepEqual(splitLines(''), []);
  assert.deepEqual(splitLines('a\r\nb'), ['a\r\n', 'b']);
  assert.deepEqual(splitLines('a\n\n'), ['a\n', '\n']);
  // A changed line ending or a missing final line feed is a change.
  for (const [before, after] of [
    ['a\n', 'a\r\n'],
    ['a\n', 'a'],
    ['\uFEFFa\n', 'a\n'],
  ]) {
    const kinds = diffLines(splitLines(before), splitLines(after)).map(
      (line) => line.kind,
    );
    assert.deepEqual(kinds, ['removed', 'added'], JSON.stringify(before));
  }
});

test('Hunks keep three lines of context and join when they touch', () => {
  const lines = (count) =>
    Array.from({ length: count }, (_, index) => `${index + 1}\n`);
  const before = lines(30);
  const after = [...before];
  after[9] = 'ten\n';
  after[24] = 'twenty-five\n';
  const groups = hunks(diffLines(before, after));
  assert.deepEqual(
    groups.map((group) => [group[0].before, group.at(-1).before]),
    [
      [7, 13],
      [22, 28],
    ],
  );
  // Line 17's context touches line 10's; one line, 21, separates 25.
  after[16] = 'seventeen\n';
  assert.deepEqual(
    hunks(diffLines(before, after)).map((group) => [
      group[0].before,
      group.at(-1).before,
    ]),
    [
      [7, 20],
      [22, 28],
    ],
  );
  assert.deepEqual(hunks(diffLines(before, before)), []);
});

test('Manifests are compared by digest in path order', () => {
  const file = (path, digit, size = 3) => ({
    path,
    size,
    sha256: digit.repeat(64),
    media_type: 'text/plain; charset=utf-8',
  });
  const { differences, unchanged } = compareManifests(
    [
      file('b.txt', 'a'),
      file('same.txt', 'c'),
      file('gone.txt', 'd'),
      file('a.png', 'e'),
    ],
    [
      file('new.txt', 'f'),
      file('b.txt', 'b'),
      file('same.txt', 'c'),
      file('a.png', 'f'),
    ],
  );
  assert.equal(unchanged, 1);
  assert.deepEqual(
    differences.map(({ path, kind }) => [path, kind]),
    [
      ['a.png', 'changed'],
      ['b.txt', 'changed'],
      ['gone.txt', 'removed'],
      ['new.txt', 'added'],
    ],
  );
});
