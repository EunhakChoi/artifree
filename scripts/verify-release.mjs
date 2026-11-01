import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtemp, mkdir, readdir, readFile, rm, stat } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { basename, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { TARGET_LAYOUT, entrypoints, sha256 } from './lib/release-layout.mjs';
import { startServer } from './lib/server-process.mjs';
import {
  exerciseNewService,
  expectSavedRevision,
} from './lib/service-check.mjs';

/**
 * Installs a release archive into an empty directory and exercises it as an
 * operator would (decision 0025). The archive is unpacked with the platform's
 * own extractor; every file is checked against the manifest; then only files
 * from the unpacked tree are run, with an empty PATH and no repository
 * working directory: start, health, the packaged web page, create and save an
 * artifact with the packaged CLI, fetch its preview, stop, restart on the
 * same data, back up the running server and restore into a new directory that
 * serves the same revision, and refuse bad startup configuration. Throws on the first failure.
 */

const windows = process.platform === 'win32';

/** Unpacks `archive` into the empty directory `into` with the OS extractor. */
export function extract(archive, into) {
  if (archive.endsWith('.zip')) {
    assert.ok(windows, 'ZIP release archives are unpacked on Windows.');
    // Paths travel in the environment: `-Command` does not bind trailing
    // arguments to $args.
    execFileSync(
      'powershell.exe',
      [
        '-NoProfile',
        '-NonInteractive',
        '-Command',
        'Expand-Archive -LiteralPath $env:ARTIFREE_ARCHIVE -DestinationPath $env:ARTIFREE_DEST',
      ],
      {
        stdio: ['ignore', 'inherit', 'inherit'],
        env: {
          ...process.env,
          ARTIFREE_ARCHIVE: archive,
          ARTIFREE_DEST: into,
        },
      },
    );
  } else {
    execFileSync('tar', ['-xzf', archive, '-C', into], { stdio: 'inherit' });
  }
}

/**
 * Walks the unpacked tree without following links. Returns the package-
 * relative paths of regular files and of directories; anything else
 * (symlink, device, socket) is refused, because a link named like a listed
 * file could pass a hash check while pointing outside the package.
 */
async function walk(dir, prefix = '', found = { files: [], dirs: [] }) {
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    const path = `${prefix}${entry.name}`;
    if (entry.isDirectory()) {
      found.dirs.push(path);
      await walk(join(dir, entry.name), `${path}/`, found);
    } else if (entry.isFile()) {
      found.files.push(path);
    } else {
      assert.fail(`Not a regular file or directory: ${path}`);
    }
  }
  return found;
}

/**
 * Checks the unpacked tree against its MANIFEST.json: the entrypoints are
 * exactly those of the manifest's target and are listed files; the tree has
 * only regular files and directories; every listed file has the recorded size
 * and SHA-256; and nothing else is present, not even an empty directory.
 * Returns the manifest.
 */
export async function verifyManifest(packageDir) {
  const manifest = JSON.parse(
    await readFile(join(packageDir, 'MANIFEST.json'), 'utf8'),
  );
  assert.equal(manifest.format, 1, 'Unsupported manifest format.');
  assert.ok(
    Object.hasOwn(TARGET_LAYOUT, manifest.target),
    `Unknown manifest target: ${manifest.target}`,
  );
  const expected = entrypoints(manifest.target);
  assert.deepEqual(manifest.entrypoints, expected, 'Unexpected entrypoints.');
  const listed = new Set();
  for (const file of manifest.files) {
    assert.ok(
      file.path !== '' &&
        !file.path.startsWith('/') &&
        !file.path.includes('\\') &&
        !file.path.split('/').some((part) => part === '' || part === '..'),
      `Unsafe manifest path: ${file.path}`,
    );
    listed.add(file.path);
  }
  for (const required of [
    expected.server,
    expected.cli,
    `${expected.web}/index.html`,
  ]) {
    assert.ok(listed.has(required), `The manifest does not list ${required}.`);
  }

  const found = await walk(packageDir);
  const files = found.files.filter((path) => path !== 'MANIFEST.json');
  assert.ok(
    found.files.includes('MANIFEST.json'),
    'MANIFEST.json is not a regular file.',
  );
  assert.deepEqual(
    files.sort(),
    [...listed].sort(),
    'Files differ from the manifest.',
  );
  const parents = new Set();
  for (const path of listed) {
    const parts = path.split('/');
    for (let n = 1; n < parts.length; n += 1)
      parents.add(parts.slice(0, n).join('/'));
  }
  assert.deepEqual(
    found.dirs.sort(),
    [...parents].sort(),
    'Directories differ from the manifest.',
  );
  for (const file of manifest.files) {
    const bytes = await readFile(join(packageDir, ...file.path.split('/')));
    assert.equal(bytes.length, file.size, `Size mismatch: ${file.path}`);
    assert.equal(sha256(bytes), file.sha256, `SHA-256 mismatch: ${file.path}`);
  }
  return manifest;
}

/**
 * Runs `body` with a started server, then stops it. A failing body keeps its
 * own error: stopping is best effort then. After a successful body the stop
 * must be clean (exit status 0; Windows has no graceful signal for a child,
 * so there the process is ended and only the restart proves the data).
 */
async function withServer(server, body) {
  let result;
  try {
    result = await body();
  } catch (error) {
    await server.stop().catch(() => undefined);
    throw error;
  }
  const code = await server.stop();
  if (!windows) assert.equal(code, 0, `Unclean stop: ${server.stderr()}`);
  return result;
}

/**
 * Requires a server start to fail with a message matching `pattern`. A start
 * that succeeds is stopped first so a broken guard fails here, not by hanging.
 */
export async function refuses(starting, pattern) {
  let server;
  try {
    server = await starting;
  } catch (error) {
    assert.match(String(error.message), pattern);
    return;
  }
  await server.kill();
  assert.fail(
    `The server started although it should have refused (${pattern}).`,
  );
}

/**
 * Runs the whole installation check for `archive` (a path to a `.tar.gz` or
 * `.zip` built by package-release.mjs). Returns a short summary object.
 */
export async function verifyRelease(archive) {
  const scratch = await mkdtemp(join(tmpdir(), 'artifree-install-'));
  try {
    const installRoot = join(scratch, 'install');
    await mkdir(installRoot);
    extract(resolve(archive), installRoot);
    const [top, ...others] = await readdir(installRoot);
    assert.equal(
      others.length,
      0,
      'The archive must have one top-level directory.',
    );
    const packageDir = join(installRoot, top);
    const manifest = await verifyManifest(packageDir);

    const layout = TARGET_LAYOUT[manifest.target];
    assert.ok(
      layout.platform === process.platform && layout.arch === process.arch,
      `${manifest.target} cannot run on ${process.platform}-${process.arch}.`,
    );
    const expectedName = `artifree-${manifest.version}-${manifest.target}`;
    assert.ok(
      basename(archive).startsWith(`${expectedName}.`) && top === expectedName,
      `The manifest (${expectedName}) does not match the archive name or its directory (${basename(archive)}, ${top}).`,
    );
    const binary = join(packageDir, ...manifest.entrypoints.server.split('/'));
    const webDir = join(packageDir, manifest.entrypoints.web);
    const dataDir = join(scratch, 'data');
    const workDir = join(scratch, 'cwd');
    await mkdir(workDir);
    // No PATH, no HOME, no repository: only the unpacked files run.
    const env = windows
      ? { SystemRoot: process.env.SystemRoot, PATH: '' }
      : { PATH: '' };
    const serve = (extra = []) =>
      startServer(binary, dataDir, ['--web-dir', webDir, ...extra], {
        env,
        cwd: workDir,
      });

    const summary = {};
    const first = await serve();
    let artifactId;
    await withServer(first, async () => {
      const cliEnv = {
        ...env,
        ARTIFREE_SERVER: first.api_url,
        ARTIFREE_TOKEN_FILE: first.token_file,
      };
      const siteScratch = join(scratch, 'check');
      await mkdir(siteScratch);
      const exercised = await exerciseNewService({
        packageDir,
        cliEnv,
        apiUrl: first.api_url,
        webIndex: join(webDir, 'index.html'),
        scratch: siteScratch,
      });
      artifactId = exercised.artifactId;
      summary.revision = exercised.revision;
    });

    const snapshot = join(scratch, 'snapshot.sqlite3');
    const restoredDir = join(scratch, 'restored');
    const second = await serve();
    await withServer(second, async () => {
      const cliEnv = {
        ...env,
        ARTIFREE_SERVER: second.api_url,
        ARTIFREE_TOKEN_FILE: second.token_file,
      };
      await expectSavedRevision({ packageDir, cliEnv, artifactId });
      // An online backup of the running server (decision 0027).
      const backedUp = JSON.parse(
        execFileSync(
          binary,
          ['backup', '--data-dir', dataDir, '--output', snapshot],
          {
            env,
            cwd: workDir,
            encoding: 'utf8',
          },
        ),
      );
      assert.equal(backedUp.event, 'backup');
    });

    // The backup restores into a new directory that serves the same revision.
    const restored = JSON.parse(
      execFileSync(
        binary,
        ['restore', '--input', snapshot, '--data-dir', restoredDir],
        {
          env,
          cwd: workDir,
          encoding: 'utf8',
        },
      ),
    );
    assert.equal(restored.event, 'restore');
    const third = await startServer(
      binary,
      restoredDir,
      ['--web-dir', webDir],
      {
        env,
        cwd: workDir,
      },
    );
    await withServer(third, async () => {
      const cliEnv = {
        ...env,
        ARTIFREE_SERVER: third.api_url,
        ARTIFREE_TOKEN_FILE: third.token_file,
      };
      await expectSavedRevision({ packageDir, cliEnv, artifactId });
    });

    await refuses(
      startServer(
        binary,
        join(scratch, 'data2'),
        ['--web-dir', join(scratch, 'missing')],
        { env, cwd: workDir },
      ),
      /web directory/,
    );
    await refuses(
      serve([
        '--api-origin',
        'https://same.example',
        '--content-origin',
        'https://same.example',
      ]),
      /must differ/,
    );
    return summary;
  } finally {
    await rm(scratch, { recursive: true, force: true });
  }
}

/**
 * Checks that the `.sha256` file beside `archive` names it and matches its
 * bytes, as a downloader's `sha256sum -c` would.
 */
export async function verifyChecksum(archive) {
  const [expected, name] = (await readFile(`${archive}.sha256`, 'utf8'))
    .trim()
    .split(/\s+/);
  assert.equal(name, basename(archive), 'The checksum names another file.');
  assert.equal(
    sha256(await readFile(archive)),
    expected,
    'Archive checksum mismatch.',
  );
}

async function main() {
  const [given] = process.argv.slice(2);
  assert.ok(
    given,
    'Usage: node scripts/verify-release.mjs <archive-or-directory>',
  );
  let archive = resolve(given);
  if ((await stat(archive)).isDirectory()) {
    const found = (await readdir(archive)).filter((n) =>
      /\.(tar\.gz|zip)$/.test(n),
    );
    assert.equal(found.length, 1, `Expected one archive in ${archive}.`);
    archive = join(archive, found[0]);
  }
  await verifyChecksum(archive);
  console.log(JSON.stringify(await verifyRelease(archive)));
}

if (process.argv[1] === fileURLToPath(import.meta.url)) await main();
