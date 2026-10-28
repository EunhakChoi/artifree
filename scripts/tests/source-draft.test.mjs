import assert from 'node:assert/strict';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';
import { packageRules } from '@artifree/contracts';

// The draft rules are web code without DOM or React imports; bundle them as
// they ship.
const bundle = await build({
  entryPoints: [
    fileURLToPath(
      new URL('../../apps/web/src/artifacts/source-draft.ts', import.meta.url),
    ),
  ],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  mainFields: ['module', 'main'],
  write: false,
});
const {
  add,
  edit,
  emptyDraft,
  entries,
  exists,
  isEmpty,
  movingReads,
  plan,
  problems,
  remove,
  rename,
  restore,
} = await import(
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
);

function file(path, size = 12) {
  return { path, size, media_type: 'text/plain', sha256: '0'.repeat(64) };
}

const base = emptyDraft(4, [
  file('index.html'),
  file('style.css'),
  file('img/logo.png', 3),
]);
const logo = new Uint8Array([0x89, 0, 0xff]);
const decoder = new TextDecoder();

/** The changes a save sends, with text decoded for comparison. */
function sent(draft) {
  return plan(draft).map((change) =>
    change.op === 'put'
      ? { ...change, bytes: decoder.decode(change.bytes) }
      : change,
  );
}

test('Moving an unchanged file carries its base bytes, and moving it home undoes it', () => {
  assert.equal(movingReads(base, 'img/logo.png'), true);
  assert.throws(() => rename(base, 'img/logo.png', 'logo.png', undefined));
  const moved = rename(base, 'img/logo.png', 'a/logo.png', logo);
  assert.deepEqual(plan(moved), [
    { op: 'put', path: 'a/logo.png', bytes: logo },
    { op: 'delete', path: 'img/logo.png' },
  ]);
  assert.deepEqual(entries(moved)[0], {
    path: 'a/logo.png',
    status: 'added',
    mediaType: 'image/png',
    movedFrom: 'img/logo.png',
  });
  // The draft holds the bytes now: moving on reads nothing.
  assert.equal(movingReads(moved, 'a/logo.png'), false);
  const again = rename(moved, 'a/logo.png', 'b/logo.png', undefined);
  assert.equal(exists(again, 'a/logo.png'), false);
  assert.ok(isEmpty(rename(again, 'b/logo.png', 'img/logo.png', undefined)));
});

test('Moved text loses its original, and a moved new file leaves nothing behind', () => {
  const edited = edit(base, 'style.css', 'p{}\n', false, 'a{}\n');
  assert.equal(movingReads(edited, 'style.css'), false);
  const moved = rename(edited, 'style.css', 'css/site.css', undefined);
  assert.deepEqual(moved.changes.get('css/site.css'), {
    kind: 'put',
    content: { kind: 'text', text: 'p{}\n', crlf: false },
  });
  // Moved onto a deleted base path, the text is not that file's either.
  const over = rename(
    remove(edited, 'index.html'),
    'style.css',
    'index.html',
    undefined,
  );
  assert.deepEqual(over.changes.get('index.html'), {
    kind: 'put',
    content: { kind: 'text', text: 'p{}\n', crlf: false },
  });
  // Moved home, it is still changed text.
  const home = rename(moved, 'css/site.css', 'style.css', undefined);
  assert.deepEqual(sent(home), [
    { op: 'put', path: 'style.css', bytes: 'p{}\n' },
  ]);
  const fresh = rename(add(base, 'a.txt'), 'a.txt', 'b/a.txt', undefined);
  assert.deepEqual(sent(fresh), [{ op: 'put', path: 'b/a.txt', bytes: '' }]);
});

test('Text equal to its own original is no change; text without one is', () => {
  const edited = edit(base, 'index.html', '<p>b</p>', false, '<p>a</p>');
  assert.equal(
    entries(edited).find(({ path }) => path === 'index.html').status,
    'edited',
  );
  assert.ok(isEmpty(edit(edited, 'index.html', '<p>a</p>', false, '<p>a</p>')));
  const typed = edit(add(base, 'a.txt'), 'a.txt', '', false, undefined);
  assert.deepEqual(sent(typed), [{ op: 'put', path: 'a.txt', bytes: '' }]);
});

test('Removing deletes a base file and forgets a new one; restoring undoes either', () => {
  const removed = remove(add(base, 'new.txt'), 'style.css');
  assert.deepEqual(sent(remove(removed, 'new.txt')), [
    { op: 'delete', path: 'style.css' },
  ]);
  assert.equal(entries(removed).at(-1).status, 'deleted');
  assert.equal(exists(removed, 'style.css'), false);
  assert.deepEqual(sent(restore(removed, 'style.css')), [
    { op: 'put', path: 'new.txt', bytes: '' },
  ]);
});

test('Saved text keeps CRLF line breaks and a byte order mark', () => {
  const draft = edit(base, 'index.html', '﻿a\nb\n', true, undefined);
  assert.deepEqual(
    [...plan(draft)[0].bytes],
    [0xef, 0xbb, 0xbf, 0x61, 0x0d, 0x0a, 0x62, 0x0d, 0x0a],
  );
});

test('Each reason the server would refuse a draft is named until it is fixed', () => {
  const reasons = (draft) => problems(draft).map(({ message }) => message);
  assert.deepEqual(reasons(base), []);
  assert.deepEqual(reasons(add(base, 'a b.txt')), [
    'a b.txt: A path may use only ASCII letters, digits, “-”, “_”, “.”, and “/”.',
  ]);
  assert.deepEqual(reasons(add(base, 'Style.css')), [
    'Style.css and style.css differ only by case.',
  ]);
  assert.deepEqual(reasons(remove(add(base, 'Style.css'), 'style.css')), []);
  assert.deepEqual(reasons(add(base, 'style.css/x.txt')), [
    'style.css is a file, so style.css/x.txt cannot be inside it.',
  ]);
  assert.deepEqual(reasons(remove(base, 'index.html')), [
    'Every artifact needs index.html, the page its preview opens.',
  ]);
});

test('Sizes count what each path will hold, moved bytes included', () => {
  const large = packageRules.max_file_bytes + 1;
  const draft = emptyDraft(1, [file('index.html'), file('big.png', large)]);
  const tooLarge = `big.png is larger than ${packageRules.max_file_bytes} bytes.`;
  assert.deepEqual(
    problems(draft).map(({ message }) => message),
    [tooLarge],
  );
  const big = new Uint8Array(large);
  assert.deepEqual(
    problems(rename(draft, 'big.png', 'b.png', big)).map(({ path }) => path),
    ['b.png'],
  );
  assert.deepEqual(problems(remove(draft, 'big.png')), []);
  const full = emptyDraft(
    1,
    Array.from({ length: packageRules.max_files }, (_, index) =>
      file(index === 0 ? 'index.html' : `f${index}.txt`, 1),
    ),
  );
  assert.deepEqual(problems(full), []);
  assert.deepEqual(problems(add(full, 'one-more.txt')), [
    { message: `An artifact has at most ${packageRules.max_files} files.` },
  ]);
});
