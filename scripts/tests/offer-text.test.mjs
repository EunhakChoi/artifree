import assert from 'node:assert/strict';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The offer text is web code without DOM or React imports; bundle it as it ships.
const bundle = await build({
  entryPoints: [
    fileURLToPath(
      new URL('../../apps/web/src/artifacts/offer-text.ts', import.meta.url),
    ),
  ],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const { offerText } = await import(
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
);

// Every expected string is written out literally; none is derived from the code.
const offerRows = [
  {
    name: 'a file offer shows its recorded strings',
    offer: {
      kind: 'file',
      path: 'src/index.html',
      fileName: 'index.html',
      size: 42,
    },
    expected: {
      title: 'Save a file from this preview?',
      description:
        'The preview asked to save a source file of this revision. Nothing is saved unless you accept.',
      fileName: 'index.html',
      sourcePath: 'src/index.html',
      size: '42 bytes',
    },
  },
  {
    name: 'an archive offer shows its recorded strings',
    offer: {
      kind: 'archive',
      path: null,
      fileName: 'revision-3.zip',
      size: null,
    },
    expected: {
      title: 'Save a file from this preview?',
      description:
        'The preview asked to save the source files of this revision as a ZIP archive. Nothing is saved unless you accept.',
      fileName: 'revision-3.zip',
      sourcePath: undefined,
      size: undefined,
    },
  },
  {
    name: 'an asset offer shows its recorded strings',
    offer: { kind: 'asset', fileName: 'photo.png', bytes: 1024 },
    expected: {
      title: 'Save uploaded file?',
      description:
        'The preview asked to save a file uploaded to this artifact. Saving it does not make it safe to open. Nothing is saved unless you accept.',
      fileName: 'photo.png',
      sourcePath: undefined,
      size: '1024 bytes',
    },
  },
];

test('offerText gives the recorded strings for each offer kind', async (t) => {
  for (const row of offerRows) {
    await t.test(row.name, () => {
      assert.deepEqual(offerText(row.offer), row.expected);
    });
  }
});

test('without an offer the text is the source-file wording with no details', () => {
  assert.deepEqual(offerText(undefined), {
    title: 'Save a file from this preview?',
    description:
      'The preview asked to save a source file of this revision. Nothing is saved unless you accept.',
    fileName: undefined,
    sourcePath: undefined,
    size: undefined,
  });
});

test('a null path yields no source path', () => {
  const text = offerText({
    kind: 'file',
    path: null,
    fileName: 'index.html',
    size: 42,
  });
  assert.equal(text.sourcePath, undefined);
});

test('a null size yields no size', () => {
  const text = offerText({
    kind: 'file',
    path: 'src/index.html',
    fileName: 'index.html',
    size: null,
  });
  assert.equal(text.size, undefined);
});

test('a size of 0 is shown as 0 bytes, not hidden', () => {
  const text = offerText({ kind: 'file', path: 'x', fileName: 'x', size: 0 });
  assert.equal(text.size, '0 bytes');
});

test('a source path of exactly 80 characters is shown whole', () => {
  const path = 'a'.repeat(80);
  const text = offerText({ kind: 'file', path, fileName: 'x', size: 1 });
  assert.equal(text.sourcePath, 'a'.repeat(80));
});

test('a source path of 81 characters is shown as 80 characters and an ellipsis', () => {
  const path = `${'a'.repeat(79)}bc`;
  const text = offerText({ kind: 'file', path, fileName: 'x', size: 1 });
  assert.equal(text.sourcePath, `${'a'.repeat(79)}b…`);
});
