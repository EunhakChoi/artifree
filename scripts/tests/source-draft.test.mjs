import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The draft and path rules are web code without DOM or React imports;
// bundle them as they ship, with the generated package rules.
const bundle = await build({
  stdin: {
    contents:
      "export * from './source-draft.ts'; export * from './package-path.ts';",
    resolveDir: fileURLToPath(
      new URL('../../apps/web/src/artifacts/', import.meta.url),
    ),
  },
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const draftRules = await import(
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
);
const {
  addFile,
  deleteFile,
  isEmpty,
  listPaths,
  needsBaseBytes,
  pathError,
  plan,
  problems,
  renameFile,
  restoreFile,
  rules,
  setText,
  startDraft,
} = draftRules;

const decoder = new TextDecoder();
const file = (path, size = 10) => ({
  path,
  size,
  media_type: 'text/html; charset=utf-8',
  sha256: '0'.repeat(64),
});
const base = () =>
  startDraft(3, [file('index.html'), file('logo.png', 4), file('a.css')]);
const sent = (draft) =>
  plan(draft).map((change) =>
    change.op === 'delete'
      ? `delete ${change.path}`
      : `put ${change.path} ${decoder.decode(change.bytes)}`,
  );
const messages = (draft) => problems(draft).map((problem) => problem.message);

test('The web judges the shared path fixture as the domain does', async () => {
  const fixture = JSON.parse(
    await readFile(
      new URL('../../contracts/fixtures/package-paths.json', import.meta.url),
      'utf8',
    ),
  );
  assert.ok(fixture.cases.length >= 30);
  for (const { path, error } of fixture.cases)
    assert.equal(pathError(path) ?? null, error, JSON.stringify(path));
  // An inherited object member is not an extension.
  assert.equal(pathError('a.constructor'), 'Extension');
});

test('A rename of an unchanged file carries its base bytes; moving them home is no change', () => {
  let draft = base();
  assert.equal(needsBaseBytes(draft, 'logo.png'), true);
  assert.throws(() => renameFile(draft, 'logo.png', 'img/logo.png', undefined));
  draft = renameFile(
    draft,
    'logo.png',
    'img/logo.png',
    Uint8Array.of(137, 80, 0, 255),
  );
  assert.equal(needsBaseBytes(draft, 'img/logo.png'), false);
  const put = plan(draft).find((change) => change.op === 'put');
  assert.deepEqual([...put.bytes], [137, 80, 0, 255]);
  assert.deepEqual(
    plan(draft).map((change) => `${change.op} ${change.path}`),
    ['put img/logo.png', 'delete logo.png'],
  );
  // Moving on needs no read; moving home undoes the rename.
  draft = renameFile(draft, 'img/logo.png', 'pic.png', undefined);
  draft = renameFile(draft, 'pic.png', 'logo.png', undefined);
  assert.equal(isEmpty(draft), true);
});

test('Moved text keeps its source: it is a change elsewhere, and none back home unedited', () => {
  const css = { path: 'a.css', text: 'a {}' };
  let draft = setText(base(), 'a.css', 'b {}', false, css);
  draft = renameFile(draft, 'a.css', 'b.css', undefined);
  // The old text again is still a change at another path.
  draft = setText(draft, 'b.css', 'a {}', false, undefined);
  assert.deepEqual(sent(draft), ['delete a.css', 'put b.css a {}']);
  // Back home with its source's text, it is no change.
  draft = renameFile(draft, 'b.css', 'a.css', undefined);
  assert.equal(isEmpty(draft), true);
  // Moved home edited, then edited back, it is no change either.
  draft = setText(base(), 'a.css', 'b {}', false, css);
  draft = renameFile(draft, 'a.css', 'c.css', undefined);
  draft = renameFile(draft, 'c.css', 'a.css', undefined);
  assert.deepEqual(sent(draft), ['put a.css b {}']);
  draft = setText(draft, 'a.css', 'a {}', false, undefined);
  assert.equal(isEmpty(draft), true);
  // Moved onto a deleted base path, it replaces that file.
  draft = setText(base(), 'a.css', 'b {}', false, css);
  draft = deleteFile(draft, 'index.html');
  draft = renameFile(draft, 'a.css', 'index.html', undefined);
  draft = setText(draft, 'index.html', 'b {}', false, undefined);
  assert.deepEqual(sent(draft), ['delete a.css', 'put index.html b {}']);
  // Edited bytes keep the path they were read from.
  draft = renameFile(
    base(),
    'logo.png',
    'x.txt',
    new TextEncoder().encode('png'),
  );
  draft = setText(draft, 'x.txt', 'png!', false, {
    path: 'logo.png',
    text: 'png',
  });
  draft = renameFile(draft, 'x.txt', 'logo.png', undefined);
  draft = setText(draft, 'logo.png', 'png', false, undefined);
  assert.equal(isEmpty(draft), true);
});

test('Text equal to its own original is no change, and line breaks are saved as read', () => {
  let draft = setText(base(), 'a.css', 'x\ny', true, {
    path: 'a.css',
    text: '﻿a\nb',
  });
  assert.deepEqual(sent(draft), ['put a.css x\r\ny']);
  draft = setText(draft, 'a.css', '﻿a\nb', true, undefined);
  assert.equal(isEmpty(draft), true);
  draft = setText(draft, 'a.css', '﻿a\nc', true, {
    path: 'a.css',
    text: '﻿a\nb',
  });
  assert.deepEqual(
    Array.from(plan(draft)[0].bytes.subarray(0, 3)),
    [0xef, 0xbb, 0xbf],
    'the byte order mark is kept',
  );
});

test('Adding, deleting, and restoring files', () => {
  let draft = addFile(base(), 'notes.txt');
  draft = setText(draft, 'notes.txt', 'hi', false, undefined);
  draft = deleteFile(draft, 'a.css');
  assert.deepEqual(
    listPaths(draft).map(({ path, state }) => `${path}:${state}`),
    [
      'a.css:deleted',
      'index.html:unchanged',
      'logo.png:unchanged',
      'notes.txt:added',
    ],
  );
  draft = restoreFile(deleteFile(draft, 'notes.txt'), 'a.css');
  assert.equal(isEmpty(draft), true);
});

test('Each reason the server would refuse a draft is named until it is fixed', () => {
  let draft = addFile(base(), 'bad name.html');
  assert.match(
    messages(draft).join(' '),
    /bad name\.html: A path uses only ASCII/,
  );
  draft = deleteFile(draft, 'bad name.html');
  assert.deepEqual(messages(draft), []);

  draft = addFile(draft, 'A.css');
  assert.deepEqual(messages(draft), ['a.css and A.css differ only by case.']);
  draft = deleteFile(draft, 'A.css');

  draft = addFile(draft, 'a.css/x.css');
  assert.deepEqual(messages(draft), ['a.css/x.css is under the file a.css.']);
  draft = deleteFile(draft, 'a.css/x.css');

  draft = renameFile(draft, 'index.html', 'home.html', new Uint8Array(1));
  assert.deepEqual(messages(draft), [`A revision needs ${rules.entry_path}.`]);
  draft = renameFile(draft, 'home.html', 'index.html', undefined);
  assert.equal(isEmpty(draft), true);

  // Sizes count text as saved and bytes a rename moved.
  const big = 'x'.repeat(rules.max_file_bytes);
  draft = setText(base(), 'a.css', big, false, { path: 'a.css', text: 'a' });
  assert.deepEqual(messages(draft), []);
  draft = setText(draft, 'a.css', `${big}\n`, true, undefined);
  assert.match(messages(draft).join(' '), /a\.css is larger than 5 MiB/);
  const full = startDraft(
    1,
    Array.from({ length: 5 }, (_, index) =>
      file(index === 0 ? 'index.html' : `f${index}.css`, rules.max_file_bytes),
    ),
  );
  assert.equal(5 * rules.max_file_bytes, rules.max_package_bytes);
  assert.deepEqual(messages(full), []);
  // A moved file counts its bytes, not the size of the file it replaced.
  draft = deleteFile(
    renameFile(
      full,
      'f1.css',
      'g1.css',
      new Uint8Array(rules.max_file_bytes + 1),
    ),
    'f2.css',
  );
  assert.deepEqual(messages(draft), ['g1.css is larger than 5 MiB.']);
  draft = setText(addFile(full, 'one.css'), 'one.css', 'x', false, undefined);
  assert.deepEqual(messages(draft), [
    'The files together are larger than 25 MiB.',
  ]);
  const crowded = startDraft(
    1,
    Array.from({ length: rules.max_files }, (_, index) =>
      file(index === 0 ? 'index.html' : `f${index}.css`, 1),
    ),
  );
  assert.deepEqual(messages(crowded), []);
  assert.deepEqual(messages(addFile(crowded, 'one-more.css')), [
    `A revision has at most ${rules.max_files} files.`,
  ]);
});
