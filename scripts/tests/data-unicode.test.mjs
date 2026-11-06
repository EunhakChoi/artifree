import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import {
  copyFile,
  mkdir,
  mkdtemp,
  readFile,
  rm,
  writeFile,
} from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

const generator = new URL('../generate-data-unicode.mjs', import.meta.url);
const table = new URL(
  '../../crates/artifree-domain/src/data_unicode.rs',
  import.meta.url,
);

test('offline integrity check rejects an edited Unicode table', async () => {
  const directory = await mkdtemp(
    join(tmpdir(), 'artifree-unicode-integrity-'),
  );
  try {
    await mkdir(join(directory, 'scripts'));
    await mkdir(join(directory, 'crates/artifree-domain/src'), {
      recursive: true,
    });
    await copyFile(
      generator,
      join(directory, 'scripts/generate-data-unicode.mjs'),
    );
    const text = await readFile(table, 'utf8');
    const target = join(
      directory,
      'crates/artifree-domain/src/data_unicode.rs',
    );
    await writeFile(target, text);
    const run = () =>
      spawnSync(
        process.execPath,
        [join(directory, 'scripts/generate-data-unicode.mjs'), '--check'],
        { encoding: 'utf8' },
      );
    assert.equal(run().status, 0);
    // A one-scalar allowlist change must fail without needing downloaded UCD.
    assert.ok(text.includes('(0xA2, 0xA6)'));
    await writeFile(target, text.replace('(0xA2, 0xA6)', '(0xA1, 0xA6)'));
    const refused = run();
    assert.notEqual(refused.status, 0);
    assert.match(refused.stderr, /differs from the pinned generation/);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test('offline generator rejects a different UCD release before changing output', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'artifree-unicode-version-'));
  try {
    await writeFile(
      join(directory, 'UnicodeData.txt'),
      '0041;LATIN CAPITAL LETTER A;Lu;0;L;;;;;N;;;;0061;\n',
    );
    const before = await readFile(table);
    const refused = spawnSync(
      process.execPath,
      [fileURLToPath(generator), directory],
      { encoding: 'utf8' },
    );
    assert.notEqual(refused.status, 0);
    assert.match(refused.stderr, /UnicodeData.txt/);
    assert.deepEqual(await readFile(table), before);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
