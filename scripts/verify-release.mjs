import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import {
  mkdtemp,
  mkdir,
  readdir,
  readFile,
  rm,
  stat,
  writeFile,
} from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { basename, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { startServer } from './lib/server-process.mjs';

/**
 * Installs a release archive into an empty directory and exercises it as an
 * operator would (decision 0025). The archive is unpacked with the platform's
 * own extractor; every file is checked against the manifest; then only files
 * from the unpacked tree are run, with an empty PATH and no repository
 * working directory: start, health, the packaged web page, create and save an
 * artifact with the packaged CLI, fetch its preview, stop, restart on the
 * same data, and refuse bad startup configuration. Throws on the first failure.
 */

const windows = process.platform === 'win32';

function sha256(bytes) {
  return createHash('sha256').update(bytes).digest('hex');
}

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

async function listFiles(dir, prefix = '') {
  const found = [];
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    if (entry.isDirectory())
      found.push(
        ...(await listFiles(join(dir, entry.name), `${prefix}${entry.name}/`)),
      );
    else found.push(`${prefix}${entry.name}`);
  }
  return found;
}

/**
 * Checks the unpacked tree against its MANIFEST.json: every listed file has
 * the recorded size and SHA-256, and nothing else is present. Returns the
 * manifest.
 */
export async function verifyManifest(packageDir) {
  const manifest = JSON.parse(
    await readFile(join(packageDir, 'MANIFEST.json'), 'utf8'),
  );
  assert.equal(manifest.format, 1, 'Unsupported manifest format.');
  const listed = new Set();
  for (const file of manifest.files) {
    assert.ok(
      !file.path.startsWith('/') && !file.path.split('/').includes('..'),
      `Unsafe manifest path: ${file.path}`,
    );
    const bytes = await readFile(join(packageDir, ...file.path.split('/')));
    assert.equal(bytes.length, file.size, `Size mismatch: ${file.path}`);
    assert.equal(sha256(bytes), file.sha256, `SHA-256 mismatch: ${file.path}`);
    listed.add(file.path);
  }
  const actual = (await listFiles(packageDir)).filter(
    (p) => p !== 'MANIFEST.json',
  );
  assert.deepEqual(
    actual.sort(),
    [...listed].sort(),
    'Files differ from the manifest.',
  );
  return manifest;
}

/**
 * Requires a server start to fail with a message matching `pattern`. A start
 * that succeeds is stopped first so a broken guard fails here, not by hanging.
 */
async function refuses(starting, pattern) {
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

function runCli(packageDir, env, args) {
  const result = spawn(
    process.execPath,
    [join(packageDir, 'cli', 'artifree.mjs'), ...args],
    {
      env,
      cwd: packageDir,
      stdio: ['ignore', 'pipe', 'pipe'],
    },
  );
  return new Promise((resolveRun, reject) => {
    let stdout = '';
    let stderr = '';
    result.stdout.on('data', (chunk) => (stdout += chunk));
    result.stderr.on('data', (chunk) => (stderr += chunk));
    result.once('error', reject);
    result.once('close', (code) => {
      if (code !== 0)
        reject(new Error(`CLI ${args.join(' ')} exited ${code}: ${stderr}`));
      else resolveRun(JSON.parse(stdout));
    });
  });
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
    try {
      const health = await fetch(`${first.api_url}/healthz`);
      assert.equal(health.status, 200);
      assert.equal(await health.text(), 'ok\n');

      const page = Buffer.from(
        await (await fetch(`${first.web_url}/`)).arrayBuffer(),
      );
      assert.deepEqual(page, await readFile(join(webDir, 'index.html')));

      const cliEnv = {
        ...env,
        ARTIFREE_SERVER: first.api_url,
        ARTIFREE_TOKEN_FILE: first.token_file,
      };
      const siteDir = join(scratch, 'site');
      await mkdir(siteDir);
      await writeFile(
        join(siteDir, 'index.html'),
        '<!doctype html><title>v1</title><p>installed</p>',
      );
      const created = await runCli(packageDir, cliEnv, [
        'artifact',
        'create',
        '--title',
        'Install check',
        '--dir',
        siteDir,
      ]);
      artifactId = created.artifact.id;
      assert.equal(created.revision.revision, 1);
      await writeFile(
        join(siteDir, 'index.html'),
        '<!doctype html><title>v2</title><p>saved</p>',
      );
      const saved = await runCli(packageDir, cliEnv, [
        'artifact',
        'save',
        artifactId,
        '--base',
        '1',
        '--dir',
        siteDir,
      ]);
      assert.equal(saved.revision.revision, 2);

      const preview = await runCli(packageDir, cliEnv, [
        'artifact',
        'preview',
        artifactId,
      ]);
      assert.equal(preview.revision, 2);
      const frame = await fetch(preview.url);
      assert.equal(frame.status, 200);
      assert.match(await frame.text(), /<p>saved<\/p>/);
      assert.match(
        frame.headers.get('content-security-policy') ?? '',
        /sandbox/,
      );
      summary.revision = saved.revision.revision;
    } finally {
      const code = await first.stop();
      // Windows has no graceful signal for a child; there the process is ended.
      if (!windows) assert.equal(code, 0, `Unclean stop: ${first.stderr()}`);
    }

    const second = await serve();
    try {
      const cliEnv = {
        ...env,
        ARTIFREE_SERVER: second.api_url,
        ARTIFREE_TOKEN_FILE: second.token_file,
      };
      const shown = await runCli(packageDir, cliEnv, [
        'artifact',
        'show',
        artifactId,
      ]);
      assert.equal(
        shown.current_revision,
        2,
        'The saved revision must survive a restart.',
      );
    } finally {
      await second.stop();
    }

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
