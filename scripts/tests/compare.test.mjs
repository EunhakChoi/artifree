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
const { MAX_EDITS, MAX_LINES, changedFiles, compareLines, excerpt, toLines } =
  await import(
    `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
  );

/** A small deterministic generator, so a failure can be replayed. */
function random(seed) {
  let state = seed >>> 0;
  return (limit) => {
    state = (Math.imul(state, 1_664_525) + 1_013_904_223) >>> 0;
    return state % limit;
  };
}

/** The length of a longest common subsequence, by dynamic programming. */
function commonLength(a, b) {
  let row = Array.from({ length: b.length + 1 }, () => 0);
  for (const line of a) {
    const next = [0];
    for (let j = 0; j < b.length; j += 1)
      next.push(line === b[j] ? row[j] + 1 : Math.max(row[j + 1], next[j]));
    row = next;
  }
  return row[b.length];
}

function lines(next, count, alphabet) {
  return Array.from({ length: count }, () => `${next(alphabet)}\n`);
}

test('A comparison changes the fewest lines and rebuilds both texts', () => {
  const next = random(20_261_028);
  for (let round = 0; round < 3_000; round += 1) {
    const before = lines(next, next(30), 1 + next(6));
    const after = lines(next, next(30), 1 + next(6));
    const result = compareLines(before, after);
    const context = `Round ${round}: ${JSON.stringify([before, after])}`;
    assert.ok(result !== undefined, context);
    assert.deepEqual(
      result.filter((line) => line.kind !== 'added').map((line) => line.text),
      before,
      context,
    );
    assert.deepEqual(
      result.filter((line) => line.kind !== 'removed').map((line) => line.text),
      after,
      context,
    );
    assert.equal(
      result.filter((line) => line.kind === 'same').length,
      commonLength(before, after),
      `${context}: not the fewest changes.`,
    );
    const numbers = (side) =>
      result.map((line) => line[side]).filter((n) => n !== undefined);
    const counting = (length) => Array.from({ length }, (_, i) => i + 1);
    assert.deepEqual(numbers('old'), counting(before.length), context);
    assert.deepEqual(numbers('new'), counting(after.length), context);
  }
});

test('Texts past the line or change limits are not compared', () => {
  const many = Array.from({ length: MAX_LINES + 1 }, (_, i) => `${i}\n`);
  assert.equal(compareLines(many, ['a\n']), undefined);
  assert.equal(compareLines(['a\n'], many), undefined);
  assert.ok(compareLines(many.slice(1), many.slice(1)) !== undefined);
  // Each line differs, so every line is a change.
  const changes = (count, prefix) =>
    Array.from({ length: count }, (_, i) => `${prefix}${i}\n`);
  const half = MAX_EDITS / 2;
  assert.equal(
    compareLines(changes(half, 'a'), changes(half, 'b')).length,
    MAX_EDITS,
    'Exactly the limit is compared.',
  );
  assert.equal(
    compareLines(changes(half + 1, 'a'), changes(half, 'b')),
    undefined,
  );
});

test('Line endings, a byte order mark, and a missing final line feed differ', () => {
  assert.deepEqual(toLines(''), []);
  assert.deepEqual(toLines('one\r\ntwo'), ['one\r\n', 'two']);
  assert.deepEqual(toLines('\uFEFFone\n\n'), ['\uFEFFone\n', '\n']);
  const compared = compareLines(
    toLines('one\r\ntwo\n'),
    toLines('\uFEFFone\ntwo'),
  ).map((line) => [line.kind, line.text]);
  assert.deepEqual(compared, [
    ['removed', 'one\r\n'],
    ['removed', 'two\n'],
    ['added', '\uFEFFone\n'],
    ['added', 'two'],
  ]);
});

test('An excerpt marks every left-out run: before, between, and after', () => {
  const before = Array.from({ length: 30 }, (_, i) => `${i}\n`);
  const after = [...before];
  after[10] = 'ten\n';
  after[20] = 'twenty\n';
  const shown = excerpt(compareLines(before, after));
  const summary = shown.map((item) =>
    item.kind === 'gap' ? `gap ${item.count}` : item.line.kind[0],
  );
  assert.deepEqual(summary, [
    'gap 7',
    ...'sssrasss'.split(''),
    'gap 3',
    ...'sssrasss'.split(''),
    'gap 6',
  ]);
  // Context that touches joins two changes into one run.
  after[14] = 'fourteen\n';
  const joined = excerpt(compareLines(before, after)).filter(
    (item) => item.kind === 'gap',
  );
  assert.deepEqual(
    joined.map((item) => item.count),
    [7, 6],
  );
  // A change at either end leaves nothing out there.
  const ends = excerpt(compareLines(['a\n', 'b\n'], ['x\n', 'b\n']));
  assert.ok(ends.every((item) => item.kind === 'line'));
});

test('Files are compared by digest, in path order', () => {
  const file = (path, sha256, size = 1) => ({
    path,
    sha256,
    size,
    media_type: 'text/plain',
  });
  const { changes, unchanged } = changedFiles(
    [file('b.txt', 'b1'), file('a.txt', 'a1'), file('same.txt', 's', 4)],
    [file('c.txt', 'c1'), file('b.txt', 'b2'), file('same.txt', 's', 4)],
  );
  assert.deepEqual(
    changes.map((change) => `${change.kind} ${change.path}`),
    ['removed a.txt', 'changed b.txt', 'added c.txt'],
  );
  assert.equal(unchanged, 1);
  // Equal sizes do not make equal files.
  assert.equal(
    changedFiles([file('x', 'one', 6)], [file('x', 'two', 6)]).changes.length,
    1,
  );
});
