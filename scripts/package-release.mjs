import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdir, lstat, readdir, readFile, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { parseArgs } from 'node:util';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';
import { tarGz, zip } from './lib/archive.mjs';
import {
  CLI_PATH,
  TARGET_LAYOUT,
  entrypoints,
  sha256,
} from './lib/release-layout.mjs';
import { buildServer } from './lib/server-process.mjs';

/**
 * Release packaging (decision 0025). `buildReleasePackage` turns an already
 * built server binary, the built web assets, and the built CLI into one
 * archive for the host platform, with a manifest listing every file's size and
 * SHA-256, and a `.sha256` file for the archive itself. It compiles nothing
 * except the CLI bundle; `main` builds the server first when no binary is
 * named.
 */

/** Supported release targets. Each is built natively on its own platform. */
export const TARGETS = {
  'linux-x64': {
    ...TARGET_LAYOUT['linux-x64'],
    triple: 'x86_64-unknown-linux-gnu',
    extension: 'tar.gz',
    pack: tarGz,
  },
  'windows-x64': {
    ...TARGET_LAYOUT['windows-x64'],
    triple: 'x86_64-pc-windows-msvc',
    extension: 'zip',
    pack: zip,
  },
};

/** The target name of the machine running this script, or undefined. */
export function hostTarget() {
  return Object.entries(TARGETS).find(
    ([, t]) => t.platform === process.platform && t.arch === process.arch,
  )?.[0];
}

/**
 * Refuses a binary that is not an x86-64 ELF (Linux) or PE (Windows) image,
 * so a wrongly built or wrongly named file cannot ship under a platform name.
 */
export function assertBinaryFormat(target, bytes) {
  if (target === 'linux-x64') {
    assert.ok(
      bytes.length > 20 &&
        bytes.readUInt32BE(0) === 0x7f454c46 &&
        bytes.readUInt16LE(18) === 0x3e,
      'The server binary is not an x86-64 ELF executable.',
    );
  } else {
    const peOffset = bytes.length > 0x40 ? bytes.readUInt32LE(0x3c) : 0;
    assert.ok(
      bytes.length > 0x40 &&
        bytes.readUInt16BE(0) === 0x4d5a &&
        peOffset + 6 <= bytes.length &&
        bytes.readUInt32BE(peOffset) === 0x50450000 &&
        bytes.readUInt16LE(peOffset + 4) === 0x8664,
      'The server binary is not an x86-64 PE executable.',
    );
  }
}

async function webFiles(webDir) {
  const found = [];
  async function walk(dir, prefix) {
    for (const name of (await readdir(dir)).sort()) {
      const path = join(dir, name);
      const info = await lstat(path);
      assert.ok(
        !info.isSymbolicLink(),
        `Web assets contain a symlink: ${path}`,
      );
      if (info.isDirectory()) await walk(path, `${prefix}${name}/`);
      else if (info.isFile())
        found.push({
          path: `web/${prefix}${name}`,
          data: await readFile(path),
        });
      else assert.fail(`Web assets contain a special file: ${path}`);
    }
  }
  await walk(webDir, '');
  assert.ok(
    found.some((file) => file.path === 'web/index.html'),
    `No index.html in ${webDir}; run "npm run build:web" first.`,
  );
  return found;
}

async function cliBundle(root) {
  const entry = join(root, 'apps/cli/dist/main.js');
  await lstat(entry).catch(() => {
    assert.fail(
      'apps/cli/dist/main.js is missing; run "npm run build:packages".',
    );
  });
  const result = await build({
    entryPoints: [entry],
    absWorkingDir: root,
    bundle: true,
    platform: 'node',
    format: 'esm',
    target: 'node22',
    write: false,
    legalComments: 'none',
    logLevel: 'silent',
  });
  assert.equal(result.outputFiles.length, 1);
  return Buffer.from(result.outputFiles[0].contents);
}

/**
 * The third-party Rust crates linked into the server for `triple`: name,
 * version, and declared license expression (not the license texts).
 * Only registry/git crates reachable through normal dependencies count:
 * workspace crates (`source === null`) are Artifree's own code under the
 * repository LICENSE, and build-only or dev-only crates are not linked into
 * the binary, so listing them would claim more than ships.
 */
function rustDependencies(root, triple) {
  const metadata = JSON.parse(
    execFileSync(
      'cargo',
      [
        'metadata',
        '--locked',
        '--format-version=1',
        `--filter-platform=${triple}`,
      ],
      { cwd: root, encoding: 'utf8', maxBuffer: 256 * 1024 * 1024 },
    ),
  );
  const nodes = new Map(metadata.resolve.nodes.map((node) => [node.id, node]));
  const rootId = metadata.packages.find(
    (pkg) => pkg.name === 'artifree-server',
  ).id;
  const reached = new Set([rootId]);
  const queue = [rootId];
  while (queue.length > 0) {
    for (const dep of nodes.get(queue.pop()).deps) {
      const normal = dep.dep_kinds.some((kind) => kind.kind === null);
      if (normal && !reached.has(dep.pkg)) {
        reached.add(dep.pkg);
        queue.push(dep.pkg);
      }
    }
  }
  return metadata.packages
    .filter((pkg) => reached.has(pkg.id) && pkg.source !== null)
    .map((pkg) => ({
      name: pkg.name,
      version: pkg.version,
      license: pkg.license ?? null,
    }))
    .sort(
      (a, b) =>
        a.name.localeCompare(b.name, 'en') ||
        a.version.localeCompare(b.version, 'en'),
    );
}

/**
 * Builds `artifree-<version>-<target>.<ext>` and its `.sha256` in `outDir`
 * and returns `{ archive, checksum, name, manifest }`. The binary must have
 * been built for `target` and report the repository's version.
 */
export async function buildReleasePackage({
  root,
  target,
  binary,
  webDir,
  outDir,
}) {
  const spec = TARGETS[target];
  assert.ok(
    spec,
    `Unknown target ${target}; use ${Object.keys(TARGETS).join(' or ')}.`,
  );
  const version = JSON.parse(
    await readFile(join(root, 'package.json'), 'utf8'),
  ).version;
  const binaryBytes = await readFile(binary);
  assertBinaryFormat(target, binaryBytes);
  // The binary can only be run to read its version on its own platform; a
  // different target is checked for format only.
  if (hostTarget() === target) {
    const reported = execFileSync(binary, ['--version'], { encoding: 'utf8' });
    assert.equal(
      reported.trim(),
      `artifree-server ${version}`,
      'The server binary does not report the repository version.',
    );
  }

  const name = `artifree-${version}-${target}`;
  const paths = entrypoints(target);
  const text = async (path) => readFile(join(root, path));
  const files = [
    { path: 'LICENSE', data: await text('LICENSE') },
    {
      path: 'THIRD_PARTY_NOTICES.md',
      data: await text('THIRD_PARTY_NOTICES.md'),
    },
    {
      path: 'RUST_DEPENDENCIES.json',
      data: Buffer.from(
        `${JSON.stringify(rustDependencies(root, spec.triple), null, 2)}\n`,
      ),
    },
    {
      path: 'docs/release-packages.md',
      data: await text('docs/release-packages.md'),
    },
    {
      path: 'docs/backup-restore.md',
      data: await text('docs/backup-restore.md'),
    },
    { path: paths.server, data: binaryBytes, executable: true },
    { path: CLI_PATH, data: await cliBundle(root), executable: true },
    ...(await webFiles(webDir)),
  ].sort((a, b) => (a.path < b.path ? -1 : 1));

  const manifest = {
    format: 1,
    name: 'artifree',
    version,
    target,
    entrypoints: paths,
    files: files.map((file) => ({
      path: file.path,
      size: file.data.length,
      sha256: sha256(file.data),
    })),
  };
  const entries = [
    {
      path: 'MANIFEST.json',
      data: Buffer.from(`${JSON.stringify(manifest, null, 2)}\n`),
    },
    ...files,
  ].map((file) => ({ ...file, path: `${name}/${file.path}` }));

  const archive = join(outDir, `${name}.${spec.extension}`);
  const bytes = spec.pack(entries);
  await mkdir(outDir, { recursive: true });
  await writeFile(archive, bytes);
  const checksum = `${archive}.sha256`;
  await writeFile(checksum, `${sha256(bytes)}  ${name}.${spec.extension}\n`);
  return { archive, checksum, name, manifest };
}

async function main() {
  const root = fileURLToPath(new URL('../', import.meta.url));
  const { values } = parseArgs({
    options: {
      target: { type: 'string' },
      binary: { type: 'string' },
      'web-dir': { type: 'string' },
      out: { type: 'string' },
    },
    strict: true,
  });
  const target = values.target ?? hostTarget();
  assert.ok(target, 'No release target for this platform; pass --target.');
  assert.ok(
    TARGETS[target]?.platform === process.platform,
    `Release packages are built natively; ${target} cannot be built on ${process.platform}.`,
  );
  const binary =
    values.binary === undefined
      ? buildServer(root, { release: true })
      : resolve(values.binary);
  const result = await buildReleasePackage({
    root,
    target,
    binary,
    webDir: resolve(values['web-dir'] ?? join(root, 'apps/web/dist')),
    outDir: resolve(values.out ?? join(root, '.artifree/release')),
  });
  console.log(
    JSON.stringify({ archive: result.archive, checksum: result.checksum }),
  );
}

if (process.argv[1] === fileURLToPath(import.meta.url)) await main();
