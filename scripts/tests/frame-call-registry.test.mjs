import assert from 'node:assert/strict';
import { readdir, readFile } from 'node:fs/promises';
import { join, relative, sep } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The workspace's frame-call registry, bundled as it ships (decision 0052,
// rules 6, 9 and 12).
const bundle = await build({
  entryPoints: [
    fileURLToPath(
      new URL('../../apps/web/src/artifacts/frame-rpc.ts', import.meta.url),
    ),
  ],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const { FRAME_CALL_TYPES, callId, parseCall } = await import(
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
);

// The ten calls rule 6 of decision 0052 names. They are written out here, not
// read from the registry, so that the registry is checked against the record.
const RECORDED_CALLS = [
  'metadata',
  'source.list',
  'source.read',
  'source.download',
  'source.write',
  'assets.list',
  'assets.read',
  'assets.upload',
  'assets.delete',
  'assets.download',
];

// Names that are not calls: near misses of the recorded ones, the offer steps
// of a download, and names the object prototype has. A plain object lookup
// answers for the last ones, so the registry must not use one.
const NOT_CALLS = [
  'source.offer',
  'assets.offer',
  'assets.rename',
  'source.delete',
  'Metadata',
  'metadata ',
  'toString',
  '__proto__',
  'constructor',
  'hasOwnProperty',
  '',
];

// A module that imports from `frame-calls/`, by a static import, a re-export
// or a dynamic import.
const FRAME_CALL_IMPORT =
  /(?:from\s+|import\s*\(\s*|import\s+)['"][^'"]*frame-calls\//;
// The host envelope, the one module outside `frame-calls/` that may import it.
const ENVELOPE = 'artifacts/frame-rpc.ts';
const FRAME_CALLS_DIR = 'artifacts/frame-calls/';

/** Every `.ts` and `.tsx` file under a directory, at any depth. */
async function typeScriptFiles(directory) {
  const files = [];
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) {
      files.push(...(await typeScriptFiles(path)));
    } else if (/\.tsx?$/.test(entry.name)) {
      files.push(path);
    }
  }
  return files;
}

test('The registry serves exactly the ten recorded call types', () => {
  assert.equal(FRAME_CALL_TYPES.length, RECORDED_CALLS.length);
  assert.deepEqual([...FRAME_CALL_TYPES].sort(), [...RECORDED_CALLS].sort());
});

test('Each recorded type and cancel has an answerable id, and near misses do not', () => {
  for (const type of [...RECORDED_CALLS, 'cancel']) {
    assert.equal(callId({ type, id: 7 }), 7, type);
  }
  for (const type of NOT_CALLS) {
    assert.equal(callId({ type, id: 7 }), undefined, type);
    assert.equal(parseCall({ type, id: 7 }), undefined, type);
  }
});

test("A cancel is the envelope's, with exactly its type and id", () => {
  assert.deepEqual(parseCall({ type: 'cancel', id: 4 }), {
    type: 'cancel',
    id: 4,
  });
  assert.equal(parseCall({ type: 'cancel', id: 4, path: 'a.txt' }), undefined);
  assert.equal(parseCall({ type: 'cancel', id: 0 }), undefined);
});

// The request gate is in the entry bundle and the host registry loads on
// demand, so only the host envelope may import the call modules (decision 0052,
// rule 9).
test('Only the host envelope imports the frame call modules', async () => {
  const sourceRoot = fileURLToPath(
    new URL('../../apps/web/src/', import.meta.url),
  );
  const importers = [];
  for (const file of await typeScriptFiles(sourceRoot)) {
    const text = await readFile(file, 'utf8');
    if (FRAME_CALL_IMPORT.test(text)) {
      importers.push(relative(sourceRoot, file).split(sep).join('/'));
    }
  }
  importers.sort();
  assert.deepEqual(
    importers.filter(
      (path) => path !== ENVELOPE && !path.startsWith(FRAME_CALLS_DIR),
    ),
    [],
  );
  assert.ok(importers.includes(ENVELOPE), 'the envelope imports the calls');
});
