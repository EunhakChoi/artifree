import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import {
  chmod,
  mkdir,
  mkdtemp,
  readdir,
  readFile,
  rm,
  stat,
  writeFile,
} from 'node:fs/promises';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { checkDockerfile } from './lib/dockerfile-policy.mjs';
import {
  exerciseNewService,
  expectSavedRevision,
} from './lib/service-check.mjs';
import { extract, verifyChecksum, verifyManifest } from './verify-release.mjs';

/**
 * Builds the container image from an unpacked linux-x64 release package and
 * exercises it like the clean-install check does (decisions 0025 and 0026):
 * image metadata, health, the packaged web page, create/save/preview with the
 * packaged CLI against the published ports, a read-only root filesystem,
 * graceful stop, restart on the same named volume, MCP staying loopback-only
 * through a published port, and two refused configurations. Requires Docker
 * and Node; throws on the first failure and removes what it created.
 */

const here = fileURLToPath(new URL('.', import.meta.url));
const dockerfilePath = join(here, '..', 'deploy', 'container', 'Dockerfile');
const NON_ROOT_UID = '10001';

/** Runs `docker` and returns `{ status, stdout, stderr }` without throwing. */
function docker(args, { input } = {}) {
  const result = spawnSync('docker', args, {
    encoding: 'utf8',
    input,
    timeout: 300_000,
  });
  if (result.error) throw result.error;
  return {
    status: result.status,
    stdout: result.stdout.trim(),
    stderr: result.stderr.trim(),
  };
}

/** Runs `docker` and requires success. */
function dockerOk(args) {
  const result = docker(args);
  assert.equal(
    result.status,
    0,
    `docker ${args.join(' ')} failed: ${result.stderr}`,
  );
  return result.stdout;
}

async function freePort() {
  const probe = createServer();
  await new Promise((done) => probe.listen(0, '127.0.0.1', done));
  const { port } = probe.address();
  await new Promise((done) => probe.close(done));
  return port;
}

async function until(what, attempt, seconds = 60) {
  const deadline = Date.now() + seconds * 1000;
  let last;
  while (Date.now() < deadline) {
    try {
      const value = await attempt();
      if (value) return value;
    } catch (error) {
      last = error;
    }
    await new Promise((done) => setTimeout(done, 500));
  }
  throw new Error(`Timed out waiting for ${what}.`, { cause: last });
}

/**
 * Checks the built image's metadata against the contract in decision 0026.
 * Exported so a test can feed it inspect output.
 */
export function checkImageConfig(config) {
  assert.equal(config.User, `${NON_ROOT_UID}:${NON_ROOT_UID}`);
  assert.deepEqual(Object.keys(config.ExposedPorts ?? {}).sort(), [
    '7410/tcp',
    '7411/tcp',
  ]);
  assert.deepEqual(Object.keys(config.Volumes ?? {}), ['/data']);
  assert.ok(
    config.Healthcheck?.Test?.[0] === 'CMD',
    'The image needs a health check.',
  );
  assert.equal(config.Entrypoint?.[0], '/app/bin/artifree-server');
  assert.equal(config.Cmd ?? null, null);
}

/** Starts a detached, locked-down container and returns its handle. */
function startContainer({ image, name, volume, apiPort, contentPort, extra }) {
  dockerOk([
    'run',
    '--detach',
    '--name',
    name,
    '--read-only',
    '--cap-drop',
    'ALL',
    '--security-opt',
    'no-new-privileges',
    '--health-interval',
    '2s',
    '--volume',
    `${volume}:/data`,
    '--publish',
    `127.0.0.1:${apiPort}:7410`,
    '--publish',
    `127.0.0.1:${contentPort}:7411`,
    image,
    '--content-origin',
    `http://127.0.0.1:${contentPort}`,
    '--allowed-host',
    `127.0.0.1:${apiPort}`,
    ...(extra ?? []),
  ]);
}

async function waitHealthy(name, apiUrl) {
  await until('the API', async () => (await fetch(`${apiUrl}/healthz`)).ok);
  await until(
    'the container to report healthy',
    () =>
      dockerOk(['inspect', '--format', '{{.State.Health.Status}}', name]) ===
      'healthy',
  );
}

function stopCleanly(name) {
  dockerOk(['stop', '--time', '10', name]);
  assert.equal(
    dockerOk(['inspect', '--format', '{{.State.ExitCode}}', name]),
    '0',
    'SIGTERM must stop the server with status 0 (not 137).',
  );
}

/** Requires `docker run` with `args` to exit nonzero with stderr matching. */
export function refusedRun(args, pattern) {
  const result = docker(['run', '--rm', ...args]);
  assert.notEqual(result.status, 0, `The container started: ${args.join(' ')}`);
  assert.match(result.stderr, pattern);
}

/** Runs the whole container check for the release archive at `archive`. */
export async function verifyContainer(archive) {
  const scratch = await mkdtemp(join(tmpdir(), 'artifree-container-'));
  const suffix = randomBytes(4).toString('hex');
  const image = `artifree-verify:${suffix}`;
  const volume = `artifree-verify-${suffix}`;
  const names = [`artifree-a-${suffix}`, `artifree-b-${suffix}`];
  try {
    const installRoot = join(scratch, 'install');
    await mkdir(installRoot);
    extract(resolve(archive), installRoot);
    const [top] = await readdir(installRoot);
    const packageDir = join(installRoot, top);
    const manifest = await verifyManifest(packageDir);
    assert.equal(manifest.target, 'linux-x64', 'The image is linux-x64 only.');

    checkDockerfile(await readFile(dockerfilePath, 'utf8'));
    dockerOk(['build', '--tag', image, '--file', dockerfilePath, packageDir]);
    const inspected = JSON.parse(
      dockerOk(['image', 'inspect', '--format', '{{json .Config}}', image]),
    );
    checkImageConfig(inspected);

    dockerOk(['volume', 'create', volume]);
    const apiPort = await freePort();
    const contentPort = await freePort();
    const apiUrl = `http://127.0.0.1:${apiPort}`;
    const tokenFile = join(scratch, 'access-token');
    const cliEnv = {
      PATH: '',
      ARTIFREE_SERVER: apiUrl,
      ARTIFREE_TOKEN_FILE: tokenFile,
    };
    const common = { image, volume, apiPort, contentPort };

    startContainer({ ...common, name: names[0] });
    const summary = {};
    let artifactId;
    try {
      await waitHealthy(names[0], apiUrl);
      assert.equal(dockerOk(['exec', names[0], 'id', '-u']), NON_ROOT_UID);
      const token = dockerOk(['exec', names[0], 'cat', '/data/access-token']);
      await writeFile(tokenFile, `${token}\n`, { mode: 0o600 });
      const siteScratch = join(scratch, 'check');
      await mkdir(siteScratch);
      const exercised = await exerciseNewService({
        packageDir,
        cliEnv,
        apiUrl,
        webIndex: join(packageDir, 'web', 'index.html'),
        scratch: siteScratch,
      });
      artifactId = exercised.artifactId;
      summary.revision = exercised.revision;
      assert.ok(
        exercised.previewUrl.startsWith(`http://127.0.0.1:${contentPort}/`),
        `The preview must be on the separate origin: ${exercised.previewUrl}`,
      );

      // Docker publishes through a bridge, so the server sees a non-loopback
      // peer: MCP stays refused unless --allow-remote-mcp is set (0024).
      const mcp = await fetch(`${apiUrl}/mcp`, {
        method: 'POST',
        headers: {
          authorization: `Bearer ${token}`,
          'content-type': 'application/json',
          accept: 'application/json, text/event-stream',
        },
        body: JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'ping' }),
      });
      assert.equal(
        mcp.status,
        403,
        'MCP must be refused from a published port.',
      );
      stopCleanly(names[0]);
    } catch (error) {
      console.error(docker(['logs', names[0]]).stderr);
      throw error;
    }

    // Same volume, new container.
    startContainer({ ...common, name: names[1] });
    try {
      await waitHealthy(names[1], apiUrl);
      await expectSavedRevision({ packageDir, cliEnv, artifactId });
      stopCleanly(names[1]);
    } catch (error) {
      console.error(docker(['logs', names[1]]).stderr);
      throw error;
    }

    refusedRun(
      [
        image,
        '--api-origin',
        'https://same.example',
        '--content-origin',
        'https://same.example',
      ],
      /must differ/,
    );
    // A bind mount other users can enter is refused before any data is read.
    const open = join(scratch, 'open-data');
    await mkdir(open);
    await chmod(open, 0o755);
    refusedRun(
      ['--volume', `${open}:/data`, image],
      /accessible to other users/,
    );
    return summary;
  } finally {
    for (const name of names) docker(['rm', '--force', name]);
    docker(['volume', 'rm', '--force', volume]);
    docker(['image', 'rm', '--force', image]);
    await rm(scratch, { recursive: true, force: true });
  }
}

async function main() {
  const [given] = process.argv.slice(2);
  assert.ok(
    given,
    'Usage: node scripts/verify-container.mjs <archive-or-directory>',
  );
  let archive = resolve(given);
  if ((await stat(archive)).isDirectory()) {
    const found = (await readdir(archive)).filter((n) =>
      n.endsWith('-linux-x64.tar.gz'),
    );
    assert.equal(
      found.length,
      1,
      `Expected one linux-x64 archive in ${archive}.`,
    );
    archive = join(archive, found[0]);
  }
  await verifyChecksum(archive);
  console.log(JSON.stringify(await verifyContainer(archive)));
}

if (process.argv[1] === fileURLToPath(import.meta.url)) await main();
