// Shared harness for tests that run the built CLI against a real
// artifree-server process. The CLI runs from apps/cli/dist/main.js, so run
// `npm run build:packages` before these tests: a stale build tests stale code.
//
//   root             the repository root
//   makeScratch      a new temp directory; `cleanUp` removes it
//   startTestServer  a real server whose data is in <scratch>/data
//   createArtifact   an artifact made by `artifree artifact create`
//   writeTestFile    an input file inside a directory
//   runCli           the CLI against the server, or against a proxy URL
//   parseCliSuccess  asserts exit 0 and empty stderr, then parses stdout
//   cliEnv           the environment runCli uses, for tests that spawn the CLI
//   cleanUp          stops the server, then removes the scratch directory

import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { buildServer, startServer } from './server-process.mjs';

/** The repository root: the working directory for the CLI and the builds. */
export const root = fileURLToPath(new URL('../../', import.meta.url));

const execFileAsync = promisify(execFile);
const cliEntry = join(root, 'apps/cli/dist/main.js');
let serverExecutable;

/** A new, empty directory under the system temp directory. */
export function makeScratch(prefix = 'artifree-cli-') {
  return mkdtemp(join(tmpdir(), prefix));
}

/**
 * Starts a real artifree-server whose data directory is `<scratch>/data`. The
 * binary is built once per test process. The handle has `api_url`, `token`,
 * `stop()` and `kill()`; `cleanUp` stops it.
 */
export async function startTestServer(scratch) {
  serverExecutable ??= buildServer(root);
  return startServer(serverExecutable, join(scratch, 'data'));
}

/** Writes `content` as `name` in `directory` and returns the absolute path. */
export async function writeTestFile(directory, name, content) {
  const location = join(directory, name);
  await writeFile(location, content);
  return location;
}

/**
 * The environment for a CLI process aimed at `url` (default: the server). It
 * sets ARTIFREE_URL and ARTIFREE_TOKEN, and removes ARTIFREE_TOKEN_FILE and
 * ARTIFREE_SERVER, which would otherwise take precedence over the token and URL
 * the test chose.
 */
export function cliEnv(server, url = server.api_url) {
  const env = {
    ...process.env,
    ARTIFREE_URL: url,
    ARTIFREE_TOKEN: server.token,
  };
  delete env.ARTIFREE_TOKEN_FILE;
  delete env.ARTIFREE_SERVER;
  return env;
}

/**
 * Runs `apps/cli/dist/main.js <args>` and resolves with `{ status, stdout,
 * stderr }`. A non-zero exit is a result, not an error. The call rejects only
 * when the CLI cannot start or is killed by a signal, such as a timeout. The
 * `url` option points the CLI at another URL, such as a proxy in front of the
 * server; the token stays the server's.
 */
export async function runCli(server, args, { url } = {}) {
  try {
    const { stdout, stderr } = await execFileAsync(
      process.execPath,
      [cliEntry, ...args],
      {
        cwd: root,
        env: cliEnv(server, url),
        encoding: 'utf8',
        timeout: 60_000,
        maxBuffer: 16 * 1024 * 1024,
      },
    );
    return { status: 0, stdout, stderr };
  } catch (error) {
    // A spawn failure or a signal has a string or null code, not an exit status.
    if (typeof error.code !== 'number') throw error;
    return { status: error.code, stdout: error.stdout, stderr: error.stderr };
  }
}

/**
 * Asserts that a run exited 0 with nothing on stderr, and returns stdout parsed
 * as JSON. `JSON.parse` rejects anything after the first document.
 */
export function parseCliSuccess(result) {
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stderr, '');
  return JSON.parse(result.stdout);
}

/**
 * Creates an artifact through `artifree artifact create` from a one-file site
 * made in `scratch`, and returns its ID.
 */
export async function createArtifact(server, scratch, title = 'Asset test') {
  const site = await mkdtemp(join(scratch, 'site-'));
  await writeTestFile(site, 'index.html', '<h1>Assets</h1>');
  const result = await runCli(server, [
    'artifact',
    'create',
    '--title',
    title,
    '--dir',
    site,
  ]);
  return parseCliSuccess(result).artifact.id;
}

/**
 * Stops `server`, then removes `scratch`. The directory is kept when the stop
 * fails without confirming that the process closed, because a live server may
 * still write to it; the stop error is rethrown either way.
 */
export async function cleanUp({ server, scratch } = {}) {
  let stopError;
  if (server !== undefined) {
    try {
      await server.stop();
    } catch (error) {
      stopError = error;
    }
  }
  // Only ServerClosureUnconfirmedError carries isClosureConfirmed.
  const closed =
    stopError === undefined || stopError?.isClosureConfirmed?.() === true;
  if (scratch !== undefined && closed)
    await rm(scratch, { recursive: true, force: true });
  if (stopError !== undefined) throw stopError;
}
