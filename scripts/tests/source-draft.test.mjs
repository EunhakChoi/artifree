import assert from 'node:assert/strict';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

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
  plan,
  problems,
  remove,
  rename,
  restore,
} = await import(
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
);
const { packageRules } = await import('@artifree/contracts');

function file(path, size = 10) {
  return { path, size, media_type: 'text/plain', sha256: '0'.repeat(64) };
}

const base = emptyDraft(3, [
  file('index.html'),
  file('assets/logo.png'),
  file('style.css'),
]);
const decoder = new TextDecoder();

function planned(draft) {
  return plan(draft).map((change) =>
    'bytes' in change
      ? { ...change, bytes: decoder.decode(change.bytes) }
      : change,
  );
}

test('A rename moves the base bytes unread and undoes itself when moved back', () => {
  const moved = rename(base, 'assets/logo.png', 'img/logo.png');
  assert.deepEqual(planned(moved), [
    { op: 'delete', path: 'assets/logo.png' },
    { op: 'put', path: 'img/logo.png', copyOf: 'assets/logo.png' },
  ]);
  assert.equal(exists(moved, 'assets/logo.png'), false);
  assert.equal(exists(moved, 'img/logo.png'), true);
  assert.deepEqual(
    entries(moved).find(({ path }) => path === 'img/logo.png'),
    {
      path: 'img/logo.png',
      status: 'added',
      mediaType: 'image/png',
      movedFrom: 'assets/logo.png',
    },
  );
  // Moving it on keeps the original source; moving it back is no change.
  const twice = rename(moved, 'img/logo.png', 'logo.png');
  assert.deepEqual(planned(twice), [
    { op: 'delete', path: 'assets/logo.png' },
    { op: 'put', path: 'logo.png', copyOf: 'assets/logo.png' },
  ]);
  assert.ok(isEmpty(rename(twice, 'logo.png', 'assets/logo.png')));
});

test('A rename carries edited text, and a renamed new file leaves nothing behind', () => {
  const edited = edit(
    base,
    'style.css',
    'a{}\n',
    { text: 'b{}\n', crlf: false },
    false,
  );
  assert.deepEqual(planned(rename(edited, 'style.css', 'css/site.css')), [
    { op: 'put', path: 'css/site.css', bytes: 'a{}\n' },
    { op: 'delete', path: 'style.css' },
  ]);
  const added = rename(add(base, 'notes.txt'), 'notes.txt', 'docs/notes.txt');
  assert.deepEqual(planned(added), [
    { op: 'put', path: 'docs/notes.txt', bytes: '' },
  ]);
});

test('Text back at its original is no change; moved text always is', () => {
  const original = { text: '<p>hi</p>\n', crlf: false };
  const edited = edit(base, 'index.html', '<p>ho</p>\n', original, false);
  assert.equal(entries(edited)[1].status, 'edited');
  assert.ok(
    isEmpty(edit(edited, 'index.html', original.text, original, false)),
  );
  // Text a rename brought has no original at its new path.
  const moved = rename(base, 'style.css', 'site.css');
  const typed = edit(moved, 'site.css', 'b{}\n', undefined, false);
  assert.deepEqual(planned(typed), [
    { op: 'put', path: 'site.css', bytes: 'b{}\n' },
    { op: 'delete', path: 'style.css' },
  ]);
});

test('Removing deletes a base file and forgets a new one; restoring undoes it', () => {
  const removed = remove(add(base, 'new.txt'), 'style.css');
  assert.deepEqual(planned(remove(removed, 'new.txt')), [
    { op: 'delete', path: 'style.css' },
  ]);
  assert.equal(
    entries(removed).find(({ path }) => path === 'style.css').status,
    'deleted',
  );
  assert.deepEqual(planned(restore(removed, 'style.css')), [
    { op: 'put', path: 'new.txt', bytes: '' },
  ]);
});

test('Line breaks and a byte order mark are saved as they were read', () => {
  const draft = edit(base, 'index.html', '﻿a\nb\n', undefined, true);
  const [change] = plan(draft);
  assert.deepEqual(
    [...change.bytes],
    [0xef, 0xbb, 0xbf, 0x61, 0x0d, 0x0a, 0x62, 0x0d, 0x0a],
  );
});

test('A draft the server would refuse names every reason', () => {
  const messages = (draft) => problems(draft).map(({ message }) => message);
  assert.deepEqual(messages(base), []);
  assert.deepEqual(messages(add(base, 'bad path.txt')), [
    'A path may contain only ASCII letters, digits, “-”, “_”, “.”, and “/”.',
  ]);
  assert.deepEqual(messages(add(base, 'Style.css')), [
    'style.css and Style.css differ only by case.',
  ]);
  assert.deepEqual(messages(add(base, 'style.css/a.txt')), [
    'style.css is a file, so style.css/a.txt cannot be inside it.',
  ]);
  assert.deepEqual(messages(remove(base, 'index.html')), [
    'Every artifact needs index.html, the page a preview opens.',
  ]);
  // Once the clash is gone, so is the problem.
  assert.deepEqual(messages(remove(add(base, 'Style.css'), 'style.css')), []);
});

test('Sizes count what each path will hold, moved bytes included', () => {
  const big = packageRules.max_file_bytes + 1;
  const draft = emptyDraft(1, [file('index.html'), file('big.bin.png', big)]);
  assert.deepEqual(problems(remove(draft, 'big.bin.png')), []);
  assert.deepEqual(
    problems(rename(draft, 'big.bin.png', 'moved.png')).map(({ path }) => path),
    ['moved.png'],
  );
  const many = emptyDraft(
    1,
    Array.from({ length: packageRules.max_files }, (_, index) =>
      file(index === 0 ? 'index.html' : `f${index}.txt`, 1),
    ),
  );
  assert.deepEqual(problems(many), []);
  assert.deepEqual(problems(add(many, 'one-more.txt')), [
    { message: `An artifact has at most ${packageRules.max_files} files.` },
  ]);
});
