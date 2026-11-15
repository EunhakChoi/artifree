import assert from 'node:assert/strict';
import { cp, mkdtemp, readFile, rm, writeFile, mkdir } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import {
  LOCK_FILE,
  addRelease,
  checkReleases,
} from '../check-type-releases.mjs';

const typesDir = fileURLToPath(new URL('../../types/', import.meta.url));

/** A copy of the repository's types directory to change. */
async function copyOfTypes() {
  const scratch = await mkdtemp(join(tmpdir(), 'artifree-types-'));
  await cp(typesDir, scratch, { recursive: true });
  return scratch;
}

test('the repository types match their lock', async () => {
  assert.deepEqual(await checkReleases(typesDir), []);
});

test('a changed, missing, added or removed file of a locked release is refused', async () => {
  const cases = [
    [
      'changed',
      (dir) => writeFile(join(dir, 'react/1/guide.md'), '# changed\n'),
      'react/1/guide.md differs from the locked release.',
    ],
    [
      'removed',
      (dir) => rm(join(dir, 'react/1/starter/src/app.css')),
      'react/1/starter/src/app.css is locked but missing.',
    ],
    [
      'added',
      (dir) => writeFile(join(dir, 'react/1/starter/extra.txt'), 'x'),
      'react/1/starter/extra.txt is not in the locked release.',
    ],
    [
      'release removed',
      (dir) => rm(join(dir, 'react/1'), { recursive: true }),
      'react/1 is locked but its directory is gone.',
    ],
  ];
  for (const [name, change, problem] of cases) {
    const dir = await copyOfTypes();
    try {
      await change(dir);
      const problems = await checkReleases(dir);
      assert.ok(problems.includes(problem), `${name}: ${problems.join(' ')}`);
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  }
});

test('an unlisted release directory is refused until it is added, and a locked one cannot be added again', async () => {
  const dir = await copyOfTypes();
  try {
    await mkdir(join(dir, 'react/2'));
    await writeFile(join(dir, 'react/2/type.txt'), 'id: react\nrelease: 2\n');
    assert.ok(
      (await checkReleases(dir)).some((problem) =>
        problem.startsWith('react/2 is not in releases.lock.json'),
      ),
    );
    await addRelease(dir, 'react/2');
    assert.deepEqual(await checkReleases(dir), []);
    const lock = JSON.parse(await readFile(join(dir, LOCK_FILE), 'utf8'));
    assert.deepEqual(Object.keys(lock.releases), ['react/1', 'react/2']);
    await assert.rejects(addRelease(dir, 'react/1'), /already locked/);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});
