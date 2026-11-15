import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { parseArgs } from 'node:util';
import { fileURLToPath } from 'node:url';
import { runCli } from './lib/service-check.mjs';

/**
 * The last step of the supported-host check (decision 0058, R03fb). The host
 * was prepared as docs/execution-host.md describes: the linux-x64 archive is
 * unpacked, its root is packaged with `tools/package-root.sh`, and the server
 * and the execution worker run as the two documented systemd units. This
 * script then talks to the running server from outside, as an operator
 * would: `GET /v1/background` reports an execution worker ready, an
 * artifact is created from the `react` type's starter (decision 0059 rule 4)
 * and its revision 1 is built with `react-v1` through the packaged CLI,
 * and the build preview on the `/b/` route serves the bundle the build
 * published. Node built-ins only: it runs from a sparse checkout without
 * `npm install`. Throws on the first failure.
 */

/** The files a build of the starter publishes (decision 0058 rule 8). */
const PUBLISHED = ['app.css', 'app.js', 'index.html'];

/**
 * Calls `probe` every 500 ms until it returns a value other than undefined.
 * A probe that throws is tried again only with `retryErrors` (a server that
 * is not up yet); otherwise the error ends the check at once.
 */
async function until(what, patienceMs, probe, { retryErrors = false } = {}) {
  const deadline = Date.now() + patienceMs;
  let last;
  for (;;) {
    try {
      const value = await probe();
      if (value !== undefined) return value;
    } catch (error) {
      if (!retryErrors) throw error;
      last = error;
    }
    if (Date.now() > deadline)
      throw new Error(
        `${what} did not happen within ${patienceMs} ms${last ? `: ${last.message}` : ''}.`,
      );
    await new Promise((done) => setTimeout(done, 500));
  }
}

async function main() {
  const { values } = parseArgs({
    options: {
      package: { type: 'string' },
      server: { type: 'string', default: 'http://127.0.0.1:7410' },
      'token-file': { type: 'string' },
    },
    strict: true,
  });
  assert.ok(
    values.package && values['token-file'],
    'Usage: node scripts/verify-supported-host.mjs --package <dir> --token-file <file> [--server <url>]',
  );
  const packageDir = resolve(values.package);
  const server = values.server;
  const tokenFile = resolve(values['token-file']);
  const cliEnv = {
    PATH: '',
    ARTIFREE_SERVER: server,
    ARTIFREE_TOKEN_FILE: tokenFile,
  };

  // The shell script already waited for /healthz; this only fails fast when
  // the server went away meanwhile.
  const health = await fetch(`${server}/healthz`);
  assert.equal(health.status, 200, 'GET /healthz');
  const token = (await readFile(tokenFile, 'utf8')).trim();
  // The execution worker records itself as it looks for work; the server
  // reads that from the database the two units share (decision 0058 rule 20).
  await until('An execution worker being ready', 180_000, async () => {
    const response = await fetch(`${server}/v1/background`, {
      headers: { authorization: `Bearer ${token}` },
    });
    assert.equal(response.status, 200, 'GET /v1/background');
    const background = await response.json();
    return background.execution?.ready === true ? true : undefined;
  });

  // No files: revision 1 is the type's starter (decision 0059 rule 5).
  const created = await runCli(packageDir, cliEnv, [
    'artifact',
    'create',
    '--title',
    'Supported host',
    '--type',
    'react',
  ]);
  assert.deepEqual(created.artifact.type, { id: 'react', release: 1 });
  const artifactId = created.artifact.id;
  const queued = await runCli(packageDir, cliEnv, [
    'job',
    'enqueue',
    artifactId,
    '--revision',
    '1',
    '--build',
    'react-v1',
    '--idempotency-key',
    `supported-host-${artifactId}`,
  ]);
  const job = await until('The build ending', 300_000, async () => {
    const shown = await runCli(packageDir, cliEnv, [
      'job',
      'show',
      queued.job.id,
    ]);
    return ['succeeded', 'failed', 'cancelled'].includes(shown.state)
      ? shown
      : undefined;
  });
  assert.equal(
    job.state,
    'succeeded',
    `The build did not succeed: ${JSON.stringify(job)}`,
  );
  assert.equal(job.build?.file_count, PUBLISHED.length);

  const status = await runCli(packageDir, cliEnv, [
    'artifact',
    'build-status',
    artifactId,
    '--build',
    'react-v1',
  ]);
  assert.equal(status.status, 'built', JSON.stringify(status));
  assert.equal(status.latest_build_revision, 1);

  const preview = await runCli(packageDir, cliEnv, [
    'artifact',
    'preview',
    artifactId,
    '--build',
    'react-v1',
  ]);
  const entry = new URL(preview.url);
  assert.match(entry.pathname, /^\/b\/[^/]+\/$/, preview.url);
  const page = await fetch(entry);
  assert.equal(page.status, 200, 'the build entry');
  assert.match(await page.text(), /<script type="module" src="\.\/app\.js">/);
  const script = await fetch(new URL('app.js', entry));
  assert.equal(script.status, 200, 'the bundle');
  const bundle = await script.text();
  // The template's label and React's own code both reach the bundle.
  assert.match(bundle, /Clicks/);
  assert.match(bundle, /createRoot|react-dom/);
  for (const name of PUBLISHED) {
    const file = await fetch(new URL(name, entry));
    assert.equal(file.status, 200, `the build's ${name}`);
  }
  console.log(
    JSON.stringify({
      artifact: artifactId,
      job: job.id,
      files: job.build.file_count,
      bytes: job.build.byte_count,
      preview: entry.pathname.replace(/[^/]+\/$/, '<ticket>/'),
    }),
  );
}

if (process.argv[1] === fileURLToPath(import.meta.url)) await main();
