import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, before, test } from 'node:test';
import {
  assertCollectedInputs,
  bundleLicenses,
  findLicenseFiles,
  licenseTextPath,
  npmClosure,
  npmComponents,
  verifyLicenseBundle,
} from '../lib/license-bundle.mjs';
import { sha256 } from '../lib/release-layout.mjs';

let scratch;
before(async () => {
  scratch = await mkdtemp(join(tmpdir(), 'artifree-license-test-'));
});
after(async () => {
  await rm(scratch, { recursive: true, force: true });
});

let counter = 0;
/** Creates a directory holding `files` ({ name: text }) and returns it. */
async function component(files) {
  counter += 1;
  const dir = join(scratch, `c${counter}`);
  await mkdir(dir);
  for (const [name, text] of Object.entries(files))
    await writeFile(join(dir, name), text);
  return dir;
}

const MIT = 'MIT License text\n';
const APACHE = 'Apache License text\n';

async function sample() {
  return [
    {
      ecosystem: 'npm',
      name: 'zeta',
      version: '2.0.0',
      license: 'MIT',
      dir: await component({ LICENSE: MIT, 'index.js': 'x' }),
    },
    {
      ecosystem: 'cargo',
      name: 'beta',
      version: '1.0.0',
      license: 'MIT OR Apache-2.0',
      dir: await component({ 'LICENSE-MIT': MIT, 'LICENSE-APACHE': APACHE }),
    },
    {
      ecosystem: 'cargo',
      name: 'alpha',
      version: '1.0.0',
      license: 'MIT',
      dir: await component({ 'license.md': MIT }),
    },
  ];
}

test('license texts are deduplicated by content and listed in sorted order', async () => {
  const files = await bundleLicenses(await sample(), {});
  const index = JSON.parse(files[0].data);
  assert.deepEqual(
    index.components.map((c) => `${c.ecosystem}:${c.name}`),
    ['cargo:alpha', 'cargo:beta', 'npm:zeta'],
  );
  assert.deepEqual(
    index.texts,
    [sha256(Buffer.from(MIT)), sha256(Buffer.from(APACHE))].sort(),
  );
  assert.equal(files.length, 1 + 2, 'the MIT text is stored once');
  assert.deepEqual(index.components[1].files, [
    { name: 'LICENSE-APACHE', sha256: sha256(Buffer.from(APACHE)) },
    { name: 'LICENSE-MIT', sha256: sha256(Buffer.from(MIT)) },
  ]);
  assert.deepEqual(
    files.map((f) => f.path),
    [
      'LICENSES/INDEX.json',
      ...index.texts.map((hash) => licenseTextPath(hash)),
    ],
  );
});

test('the same inputs give the same bundle bytes', async () => {
  const components = await sample();
  const first = await bundleLicenses(components, {});
  const second = await bundleLicenses([...components].reverse(), {});
  assert.deepEqual(first, second);
});

test('a component without a license text fails packaging and is named', async () => {
  const components = [
    ...(await sample()),
    {
      ecosystem: 'npm',
      name: 'bare',
      version: '0.1.0',
      dir: await component({ 'README.md': 'r' }),
    },
  ];
  await assert.rejects(
    bundleLicenses(components, {}),
    /No license text and no recorded exception for: npm:bare@0\.1\.0/,
  );
});

test('a recorded exception lets a component without text through, with its reason', async () => {
  const bare = {
    ecosystem: 'npm',
    name: 'bare',
    version: '0.1.0',
    license: 'MIT',
    dir: await component({}),
  };
  const files = await bundleLicenses([bare], {
    'npm:bare@0.1.0': 'Upstream ships no text; see issue 1.',
  });
  const index = JSON.parse(files[0].data);
  assert.equal(
    index.components[0].exception,
    'Upstream ships no text; see issue 1.',
  );
  assert.deepEqual(index.components[0].files, []);
});

test('an exception for a component that has a text, or none, is refused', async () => {
  const components = await sample();
  await assert.rejects(
    bundleLicenses(components, { 'cargo:alpha@1.0.0': 'stale' }),
    /exception is stale; cargo:alpha@1\.0\.0/,
  );
  await assert.rejects(
    bundleLicenses(components, { 'cargo:gone@9.9.9': 'old' }),
    /names no component: cargo:gone@9\.9\.9/,
  );
});

test('an empty license file is refused', async () => {
  const dir = await component({ LICENSE: '' });
  await assert.rejects(findLicenseFiles(dir), /License file is empty/);
});

test('a declared license file outside the usual names is collected', async () => {
  const dir = await component({ 'terms.txt': 'custom terms\n' });
  const found = await findLicenseFiles(dir, ['terms.txt']);
  assert.deepEqual(
    found.map((f) => f.name),
    ['terms.txt'],
  );
  await assert.rejects(
    findLicenseFiles(dir, ['missing.txt']),
    /missing or not a regular file/,
  );
});

test('license files with dotted or dashed suffixes are all collected', async () => {
  const dir = await component({
    'LICENSE.MIT': MIT,
    LICENSE: APACHE,
    'COPYING.LESSER': 'lesser\n',
    LICENCE: 'licence\n',
    UNLICENSE: 'unlicense\n',
    'NOTICE.txt': 'notice\n',
    'PATENTS-GRANT': 'patents\n',
    COPYRIGHT: 'copyright\n',
    'license-checker.js': 'module.exports = 1;',
  });
  assert.deepEqual(
    (await findLicenseFiles(dir)).map((f) => f.name),
    [
      'COPYING.LESSER',
      'COPYRIGHT',
      'LICENCE',
      'LICENSE',
      'LICENSE.MIT',
      'NOTICE.txt',
      'PATENTS-GRANT',
      'UNLICENSE',
    ],
  );
});

test('LICENSE.MIT alone is collected', async () => {
  const dir = await component({ 'LICENSE.MIT': MIT, 'README.md': 'r' });
  assert.deepEqual(
    (await findLicenseFiles(dir)).map((f) => f.name),
    ['LICENSE.MIT'],
  );
});

test('names that only look like license files are ignored', async () => {
  const dir = await component({
    'LICENSEE.txt': 'x',
    'license.rs': 'fn main() {}',
    'licensing.md': 'x',
    'license-checker.js': 'x',
    'notices.json': '{}',
  });
  assert.deepEqual(await findLicenseFiles(dir), []);
});

test('a license file that is a link is refused', async (t) => {
  const dir = await component({ real: MIT });
  try {
    await symlink(join(dir, 'real'), join(dir, 'LICENSE'));
  } catch (error) {
    if (error.code === 'EPERM' || error.code === 'EACCES') {
      t.skip('this account may not create symlinks');
      return;
    }
    throw error;
  }
  await assert.rejects(findLicenseFiles(dir), /not a regular file/);
});

/** Writes `files` under a fresh package directory and returns it. */
async function writeBundle(files) {
  counter += 1;
  const dir = join(scratch, `pkg${counter}`);
  for (const file of files) {
    const path = join(dir, ...file.path.split('/'));
    await mkdir(join(path, '..'), { recursive: true });
    await writeFile(path, file.data);
  }
  return dir;
}

test('a consistent bundle verifies', async () => {
  const dir = await writeBundle(await bundleLicenses(await sample(), {}));
  assert.deepEqual(await verifyLicenseBundle(dir), { components: 3, texts: 2 });
});

test('verification rejects an altered, extra, or missing text and an unexplained component', async () => {
  const files = await bundleLicenses(await sample(), {});
  const text = files[1];

  let dir = await writeBundle(
    files.map((f) => (f === text ? { ...f, data: Buffer.from('changed') } : f)),
  );
  await assert.rejects(verifyLicenseBundle(dir), /does not match its hash/);

  dir = await writeBundle([
    ...files,
    { path: 'LICENSES/texts/extra.txt', data: Buffer.from('x') },
  ]);
  await assert.rejects(verifyLicenseBundle(dir), /differ from the index/);

  dir = await writeBundle(files.filter((f) => f !== text));
  await assert.rejects(verifyLicenseBundle(dir), /differ from the index/);

  const index = JSON.parse(files[0].data);
  const extra = Buffer.from('an unreferenced text\n');
  const extraHash = sha256(extra);
  dir = await writeBundle([
    {
      path: 'LICENSES/INDEX.json',
      data: Buffer.from(
        JSON.stringify({ ...index, texts: [...index.texts, extraHash].sort() }),
      ),
    },
    ...files.slice(1),
    { path: licenseTextPath(extraHash), data: extra },
  ]);
  await assert.rejects(verifyLicenseBundle(dir), /text is unreferenced/);

  index.components[0].files = [];
  dir = await writeBundle([
    { path: 'LICENSES/INDEX.json', data: Buffer.from(JSON.stringify(index)) },
    ...files.slice(1),
  ]);
  await assert.rejects(
    verifyLicenseBundle(dir),
    /No license text and no exception for cargo:alpha/,
  );
});

test('verification rejects an index of another format', async () => {
  const files = await bundleLicenses(await sample(), {});
  const index = { ...JSON.parse(files[0].data), format: 2 };
  const dir = await writeBundle([
    { path: 'LICENSES/INDEX.json', data: Buffer.from(JSON.stringify(index)) },
    ...files.slice(1),
  ]);
  await assert.rejects(verifyLicenseBundle(dir), /Unsupported license index/);
});

test('verification rejects a bundle that lists no npm package', async () => {
  const cargoOnly = (await sample()).filter((c) => c.ecosystem === 'cargo');
  const dir = await writeBundle(await bundleLicenses(cargoOnly, {}));
  await assert.rejects(
    verifyLicenseBundle(dir),
    /no Rust crate or no npm package/,
  );
});

const lock = {
  packages: {
    '': { name: 'root' },
    'apps/web': {
      version: '0.1.0',
      dependencies: {
        '@artifree/client': '0.1.0',
        react: '1',
        'needs-old': '1',
      },
      devDependencies: { vite: '1' },
    },
    'packages/client': { version: '0.1.0', dependencies: { zod: '1' } },
    'node_modules/@artifree/client': {
      resolved: 'packages/client',
      link: true,
    },
    'node_modules/react': {
      version: '1.0.0',
      dependencies: { scheduler: '1' },
    },
    'node_modules/scheduler': { version: '1.0.0' },
    'node_modules/zod': { version: '1.0.0' },
    'node_modules/vite': { version: '1.0.0' },
    'node_modules/needs-old': {
      version: '1.0.0',
      dependencies: { scheduler: '0.5' },
    },
    'node_modules/needs-old/node_modules/scheduler': { version: '0.5.0' },
  },
};

test('the npm closure follows links and nested versions, and leaves out dev dependencies and own code', () => {
  assert.deepEqual(npmClosure(lock, 'apps/web'), [
    'node_modules/needs-old',
    'node_modules/needs-old/node_modules/scheduler',
    'node_modules/react',
    'node_modules/scheduler',
    'node_modules/zod',
  ]);
});

test('a dependency missing from the lock fails unless it is optional', () => {
  const broken = structuredClone(lock);
  broken.packages['node_modules/react'].dependencies.ghost = '1';
  assert.throws(
    () => npmClosure(broken, 'apps/web'),
    /Dependency ghost of node_modules\/react is not in package-lock/,
  );
  broken.packages['node_modules/react'].optionalDependencies = { ghost: '1' };
  assert.deepEqual(
    npmClosure(broken, 'apps/web').includes('node_modules/react'),
    true,
  );
});

test('an installed package whose version differs from the lock is refused', async () => {
  const root = await component({});
  await mkdir(join(root, 'node_modules', 'react'), { recursive: true });
  await writeFile(
    join(root, 'node_modules', 'react', 'package.json'),
    JSON.stringify({ name: 'react', version: '1.0.0', license: 'MIT' }),
  );
  const [only] = await npmComponents(root, lock, ['node_modules/react']);
  assert.equal(only.name, 'react');
  assert.equal(only.license, 'MIT');
  await writeFile(
    join(root, 'node_modules', 'react', 'package.json'),
    JSON.stringify({ name: 'react', version: '2.0.0' }),
  );
  await assert.rejects(
    npmComponents(root, lock, ['node_modules/react']),
    /is 2\.0\.0, but package-lock says 1\.0\.0/,
  );
});

test('a bundled npm package outside the collected set is refused', () => {
  const allowed = new Set(['node_modules/react', 'node_modules/@scope/pkg']);
  assertCollectedInputs(
    [
      'apps/cli/dist/main.js',
      'node_modules/react/index.js',
      'node_modules/@scope/pkg/lib/a.js',
    ],
    allowed,
  );
  assert.throws(
    () => assertCollectedInputs(['node_modules/left-pad/index.js'], allowed),
    /includes node_modules\/left-pad, which has no collected license text/,
  );
  assert.throws(
    () =>
      assertCollectedInputs(
        ['node_modules/react/node_modules/inner/x.js'],
        allowed,
      ),
    /node_modules\/react\/node_modules\/inner/,
  );
});
