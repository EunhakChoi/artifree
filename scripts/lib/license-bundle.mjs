import assert from 'node:assert/strict';
import { readdir, readFile, lstat } from 'node:fs/promises';
import { join } from 'node:path';
import { sha256 } from './release-layout.mjs';

/**
 * License texts of the third-party code inside a release package (decision
 * 0037). The packager collects the license, copying, and notice files of every
 * Rust crate linked into the server and every npm package that may be bundled
 * into `web/` and `cli/`, stores each distinct text once as
 * `LICENSES/texts/<sha256 prefix>.txt`, and maps components to texts in
 * `LICENSES/INDEX.json`. A component without any text fails packaging unless a
 * reviewed exception in `scripts/license-exceptions.json` names it. The
 * verifier uses `verifyLicenseBundle` on the unpacked tree. Only Node
 * built-ins are imported, because the verifier runs without `npm install`.
 */

/** Package-relative paths of the bundle. */
export const LICENSE_INDEX_PATH = 'LICENSES/INDEX.json';
export const LICENSE_TEXT_DIR = 'LICENSES/texts';

// LICENSE, LICENSE-MIT, LICENCE.md, COPYING, NOTICE, UNLICENSE, COPYRIGHT.
const LICENSE_FILE_NAME =
  /^(?:licen[sc]e|copying|notice|unlicense|copyright)(?:[-_][\w.-]*?)?(?:\.(?:txt|md|markdown|rst))?$/i;

/**
 * Package-relative path of the text with SHA-256 `hash`. The file name is the
 * first 32 hex digits (128 bits): the archive writer limits paths to 100 bytes,
 * and the index and verifier use the full hash.
 */
export function licenseTextPath(hash) {
  return `${LICENSE_TEXT_DIR}/${hash.slice(0, 32)}.txt`;
}

const byCodePoint = (a, b) => (a < b ? -1 : a > b ? 1 : 0);

/**
 * Reads the license files at the top of `dir`: names matching the license
 * pattern plus `extra` (a manifest-declared relative path such as Cargo's
 * `license_file`). A matching name that is not a regular file is an error,
 * because a link could point outside the component.
 */
export async function findLicenseFiles(dir, extra = []) {
  const names = new Set(extra);
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    if (!LICENSE_FILE_NAME.test(entry.name)) continue;
    if (entry.isDirectory()) continue;
    names.add(entry.name);
  }
  const found = [];
  for (const name of [...names].sort(byCodePoint)) {
    const path = join(dir, ...name.split('/'));
    const info = await lstat(path).catch(() => undefined);
    assert.ok(
      info?.isFile(),
      `Declared license file is missing or not a regular file: ${path}`,
    );
    const data = await readFile(path);
    assert.ok(data.length > 0, `License file is empty: ${path}`);
    found.push({ name, data });
  }
  return found;
}

/**
 * Builds the license files of a package. `components` are
 * `{ ecosystem, name, version, license, dir, licenseFile? }`; `exceptions`
 * maps `ecosystem:name@version` to a reason. Returns the package files
 * (`LICENSES/INDEX.json` and the texts) in path order. Throws, naming every
 * offending component, when one has no text and no exception, when an exception
 * names a component that has a text or is not part of this package.
 */
export async function bundleLicenses(components, exceptions) {
  const texts = new Map();
  const entries = [];
  const missing = [];
  const keys = new Set();
  for (const component of components) {
    const key = `${component.ecosystem}:${component.name}@${component.version}`;
    // The same package can be installed at two paths; list it once.
    if (keys.has(key)) continue;
    keys.add(key);
    const files = await findLicenseFiles(
      component.dir,
      component.licenseFile ? [component.licenseFile] : [],
    );
    const entry = {
      ecosystem: component.ecosystem,
      name: component.name,
      version: component.version,
      license: component.license ?? null,
      files: files.map((file) => {
        const hash = sha256(file.data);
        texts.set(hash, file.data);
        return { name: file.name, sha256: hash };
      }),
    };
    const reason = Object.hasOwn(exceptions, key) ? exceptions[key] : undefined;
    if (files.length === 0) {
      if (reason === undefined) missing.push(key);
      else entry.exception = reason;
    } else {
      assert.ok(
        reason === undefined,
        `License exception is stale; ${key} has a license text.`,
      );
    }
    entries.push(entry);
  }
  assert.equal(
    missing.length,
    0,
    `No license text and no recorded exception for: ${missing.join(', ')}`,
  );
  for (const key of Object.keys(exceptions))
    assert.ok(keys.has(key), `License exception names no component: ${key}`);

  entries.sort(
    (a, b) =>
      byCodePoint(a.ecosystem, b.ecosystem) ||
      byCodePoint(a.name, b.name) ||
      byCodePoint(a.version, b.version),
  );
  const hashes = [...texts.keys()].sort(byCodePoint);
  const index = { format: 1, components: entries, texts: hashes };
  return [
    {
      path: LICENSE_INDEX_PATH,
      data: Buffer.from(`${JSON.stringify(index, null, 2)}\n`),
    },
    ...hashes.map((hash) => ({
      path: licenseTextPath(hash),
      data: texts.get(hash),
    })),
  ];
}

/**
 * Checks an unpacked package's license bundle: the index parses, every
 * component has texts or a recorded exception, every referenced text exists and
 * is named after its own SHA-256, and no text is unreferenced. The manifest
 * check (which covers these files' hashes) is separate.
 */
export async function verifyLicenseBundle(packageDir) {
  const index = JSON.parse(
    await readFile(join(packageDir, ...LICENSE_INDEX_PATH.split('/')), 'utf8'),
  );
  assert.equal(index.format, 1, 'Unsupported license index format.');
  assert.ok(
    index.components.some((c) => c.ecosystem === 'cargo') &&
      index.components.some((c) => c.ecosystem === 'npm'),
    'The license index lists no Rust crate or no npm package.',
  );
  const expected = index.texts.map((hash) =>
    licenseTextPath(hash).split('/').pop(),
  );
  const present = (
    await readdir(join(packageDir, ...LICENSE_TEXT_DIR.split('/')))
  ).sort(byCodePoint);
  assert.deepEqual(present, expected, 'License texts differ from the index.');
  for (const hash of index.texts) {
    const bytes = await readFile(
      join(packageDir, ...licenseTextPath(hash).split('/')),
    );
    assert.equal(
      sha256(bytes),
      hash,
      `License text does not match its hash: ${hash}`,
    );
  }
  const referenced = new Set();
  for (const component of index.components) {
    const label = `${component.ecosystem}:${component.name}@${component.version}`;
    if (component.files.length === 0)
      assert.ok(
        typeof component.exception === 'string' && component.exception !== '',
        `No license text and no exception for ${label}.`,
      );
    for (const file of component.files) {
      assert.ok(
        index.texts.includes(file.sha256),
        `Unknown license text for ${label}.`,
      );
      referenced.add(file.sha256);
    }
  }
  assert.equal(
    referenced.size,
    index.texts.length,
    'A license text is unreferenced.',
  );
  return { components: index.components.length, texts: index.texts.length };
}

/**
 * The third-party npm packages reachable from workspace `startKey` (a key of
 * `lock.packages`, e.g. `apps/web`) through dependencies, optional
 * dependencies, and peer dependencies, resolved the way Node resolves them in
 * the lockfile tree. This over-approximates what a bundler includes (it cannot
 * miss a bundled package, and may list one that was tree-shaken away). Links to
 * workspace packages are followed but are not listed: that code is Artifree's.
 * Returns lockfile keys such as `node_modules/react`, sorted.
 */
export function npmClosure(lock, startKey) {
  const packages = lock.packages;
  assert.ok(packages[startKey], `Not in package-lock: ${startKey}`);
  const resolve = (from, name) => {
    let base = from;
    for (;;) {
      const key =
        base === '' ? `node_modules/${name}` : `${base}/node_modules/${name}`;
      if (packages[key]) return key;
      if (base === '') return undefined;
      const cut = base.lastIndexOf('/node_modules/');
      base = cut === -1 ? '' : base.slice(0, cut);
    }
  };
  const seen = new Set();
  const found = new Set();
  const queue = [startKey];
  while (queue.length > 0) {
    const key = queue.pop();
    if (seen.has(key)) continue;
    seen.add(key);
    const entry = packages[key];
    if (entry.link) {
      queue.push(entry.resolved);
      continue;
    }
    if (key.includes('node_modules/')) found.add(key);
    const wanted = {
      ...entry.peerDependencies,
      ...entry.optionalDependencies,
      ...entry.dependencies,
    };
    for (const name of Object.keys(wanted)) {
      const next = resolve(key, name);
      // An optional dependency for another platform is absent from the lock.
      if (next === undefined) {
        assert.ok(
          entry.optionalDependencies?.[name] !== undefined ||
            entry.peerDependenciesMeta?.[name]?.optional === true,
          `Dependency ${name} of ${key} is not in package-lock.`,
        );
      } else queue.push(next);
    }
  }
  return [...found].sort(byCodePoint);
}

/**
 * npm components for `keys` (lockfile keys), read from the installed copies
 * under `root`. The installed version must equal the lockfile's, so a stale
 * `node_modules` cannot supply different license texts.
 */
export async function npmComponents(root, lock, keys) {
  const components = [];
  for (const key of keys) {
    const dir = join(root, ...key.split('/'));
    const manifest = JSON.parse(
      await readFile(join(dir, 'package.json'), 'utf8'),
    );
    const locked = lock.packages[key];
    assert.equal(
      manifest.version,
      locked.version,
      `Installed ${key} is ${manifest.version}, but package-lock says ${locked.version}; run "npm ci".`,
    );
    components.push({
      ecosystem: 'npm',
      name: key.slice(
        key.lastIndexOf('node_modules/') + 'node_modules/'.length,
      ),
      version: locked.version,
      license:
        typeof manifest.license === 'string'
          ? manifest.license
          : (locked.license ?? null),
      dir,
    });
  }
  return components;
}

/**
 * Requires every npm package among the bundler's `inputs` (paths relative to
 * the repository root, such as `node_modules/a/node_modules/b/index.js`) to be
 * one of `allowedPackages` (lockfile keys such as `node_modules/a`), so a
 * package that went into a bundle cannot lack a collected license text.
 */
export function assertCollectedInputs(inputs, allowedPackages) {
  for (const input of inputs) {
    const at = input.lastIndexOf('node_modules/');
    if (at === -1) continue;
    const name = /^(@[^/]+\/[^/]+|[^/]+)/.exec(
      input.slice(at + 'node_modules/'.length),
    )[0];
    const dir = `${input.slice(0, at)}node_modules/${name}`;
    assert.ok(
      allowedPackages.has(dir),
      `The bundle includes ${dir}, which has no collected license text.`,
    );
  }
}
