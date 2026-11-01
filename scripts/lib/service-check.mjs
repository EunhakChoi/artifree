import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { CLI_PATH } from './release-layout.mjs';

/**
 * The operator-visible behavior every packaged form of Artifree must show,
 * independent of how the server process was started (unpacked archive in
 * decision 0025, container in decision 0026): health, the packaged web page,
 * create and save with the packaged CLI, a sandboxed preview, and the
 * revision after a restart. Callers own the process lifecycle; this module
 * only talks to a running server from outside.
 */

/** Runs the packaged CLI with `env` and returns its parsed JSON output. */
export function runCli(packageDir, env, args) {
  const result = spawn(
    process.execPath,
    [join(packageDir, ...CLI_PATH.split('/')), ...args],
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
 * Creates an artifact, saves a second revision, and fetches its preview from
 * a server that is already running and empty. `cliEnv` must name the server
 * and token. Returns the artifact id and saved revision (2).
 */
export async function exerciseNewService({
  packageDir,
  cliEnv,
  apiUrl,
  webIndex,
  scratch,
}) {
  const health = await fetch(`${apiUrl}/healthz`);
  assert.equal(health.status, 200);
  assert.equal(await health.text(), 'ok\n');

  const page = Buffer.from(await (await fetch(`${apiUrl}/`)).arrayBuffer());
  assert.deepEqual(page, await readFile(webIndex));

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
  const artifactId = created.artifact.id;
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
  assert.match(frame.headers.get('content-security-policy') ?? '', /sandbox/);
  return {
    artifactId,
    revision: saved.revision.revision,
    previewUrl: preview.url,
  };
}

/** Requires a restarted server to still hold revision 2 of `artifactId`. */
export async function expectSavedRevision({ packageDir, cliEnv, artifactId }) {
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
}
