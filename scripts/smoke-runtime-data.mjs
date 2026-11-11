import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { chromium, expect } from '@playwright/test';
import {
  accessibilityViolations,
  summarizeViolations,
} from './lib/accessibility.mjs';
import { buildServer, startServer } from './lib/server-process.mjs';
import { serveStatic } from './lib/static-web.mjs';
import { api, raw } from './smoke-workspace.mjs';

/**
 * Browser checks for the application data reading capability `data.read`
 * (decision 0053) in the real preview of a real artifree-server. The owner
 * saves application data through the HTTP API; a frame reads it with
 * `window.artifree.data.get`, but only after the user allows that reading in
 * the workspace, never in the frame. A refusal is checked twice: the frame says
 * `denied`, and the server answered 403. The values the frame reads are
 * compared with what the owner wrote, and its version with the one the owner's
 * own read reports.
 *
 * Run alone, this file uses Chromium; the suite (scripts/smoke.mjs) runs it in
 * the engine that ARTIFREE_BROWSER names. Run this check alone with
 * `npm run build`, then `node scripts/smoke-runtime-data.mjs`.
 */

const ALLOW = 'Allow app data reading';
// The warning the workspace shows beside the control (decision 0053, rule 3).
const WARNING =
  'Allowing this lets the app read the application data saved for this artifact. The preview is not isolated from the network, so the app could send what it reads elsewhere.';
// What the owner stores at `notes`, written out here rather than read back
// from the code under test.
const NOTES = { greeting: 'hello', count: 2 };
// The reads the frame makes, one button each. Once reading is allowed, `exists`
// and `value` say what the frame must answer; `value` is absent when nothing is
// at the path, because the SDK hands that answer over without a value.
const READS = [
  { label: 'Read notes', path: 'notes', exists: true, value: NOTES },
  { label: 'Read missing', path: 'missing', exists: false },
  { label: 'Read root', path: '', exists: true, value: { notes: NOTES } },
];
const manifest = JSON.stringify({
  sdk: 'artifree.app.v1',
  capabilities: ['data.read'],
});

/**
 * The page that drives the runtime. Each button asks for one path and writes
 * the answer, or the refusal's code, to the output, so the smoke clears the
 * output, presses a button, and waits for the answer.
 */
function probe() {
  const buttons = READS.map(
    ({ label }, index) =>
      `<button id="read-${index}" type="button">${label}</button>`,
  ).join('');
  const paths = JSON.stringify(READS.map(({ path }) => path));
  return `<!doctype html><html lang="en"><head><!-- artifree-runtime -->
<meta charset="utf-8"><title>Application data probe</title></head><body>
<h1>Application data probe</h1><output id="result">waiting</output>
<div>${buttons}</div>
<script>
const paths = ${paths};
const out = document.querySelector('#result');
const show = (value) => { out.textContent = JSON.stringify(value); };
const failure = (error) => show({ error: error && error.code ? error.code : 'unknown' });
paths.forEach((path, index) => {
  document.querySelector('#read-' + index).addEventListener('click', () => {
    // A synchronous throw, such as a missing runtime, is answered like a refusal.
    Promise.resolve().then(() => window.artifree.data.get({ path })).then(show, failure);
  });
});
</script></body></html>`;
}

/** The owner writes `notes` through the HTTP API, under its own idempotency key. */
async function writeNotes(server, id) {
  const { status } = await raw(
    server,
    'PUT',
    `/v1/artifacts/${id}/data`,
    { path: 'notes', base_data_version: 0, value: NOTES },
    `smoke-runtime-data-${randomUUID()}`,
  );
  assert.equal(status, 200, `Writing the owner's data failed: ${status}`);
}

/** The owner's own read of `notes`, which must find what it wrote; returns the space's version. */
async function ownerDataVersion(server, id) {
  const owner = await api(server, 'GET', `/v1/artifacts/${id}/data?path=notes`);
  assert.equal(owner.exists, true, 'The owner must find the data it wrote.');
  assert.deepEqual(owner.value, NOTES);
  return owner.data_version;
}

/** An artifact whose manifest asks for data.read, with the probe as its page. */
async function createArtifact(server, title) {
  const { artifact } = await api(server, 'POST', '/v1/artifacts', {
    title,
    files: [
      { path: 'index.html', encoding: 'utf8', content: probe() },
      { path: 'artifree-runtime.json', encoding: 'utf8', content: manifest },
    ],
  });
  return artifact;
}

/** Opens the artifact's preview in its own browser context, as a user would. */
async function open(browser, origin, server, artifact) {
  const context = await browser.newContext();
  try {
    const page = await context.newPage();
    page.setDefaultTimeout(15_000);
    const errors = [];
    // Each read the frame sends, with the status the server gave.
    const calls = [];
    page.on('pageerror', (error) => errors.push(error.message));
    page.on('response', (response) => {
      const request = response.request();
      if (request.method() !== 'POST') return;
      if (!new URL(response.url()).pathname.endsWith('/data/read')) return;
      calls.push({
        status: response.status(),
        request: request.postDataJSON(),
      });
    });
    await page.goto(origin);
    const workspace = page.locator('#artifacts');
    await workspace.getByLabel('Access token').fill(server.token);
    await workspace.getByRole('button', { name: 'Connect' }).click();
    await workspace.getByLabel('Search titles').fill(artifact.title);
    await workspace
      .getByRole('navigation', { name: 'Artifacts' })
      .getByRole('button', { name: new RegExp(`^${artifact.title}`) })
      .click();
    const permissions = workspace.getByRole('region', {
      name: 'Runtime permissions',
    });
    await expect(permissions.getByText('Declared: data.read')).toBeVisible();
    await expect(permissions.getByText('Granted: none')).toBeVisible();
    return {
      context,
      page,
      permissions,
      errors,
      calls,
      frame: workspace.frameLocator('iframe[title^="Preview of"]'),
      element: workspace.locator('iframe[title^="Preview of"]'),
    };
  } catch (error) {
    await context.close();
    throw error;
  }
}

/** Presses a permission control and waits until the workspace and the preview show the new grant. */
async function change(opened, button, granted) {
  const before = await opened.element.getAttribute('src');
  await opened.permissions.getByRole('button', { name: button }).click();
  await expect(
    opened.permissions.getByText(`Granted: ${granted}`),
  ).toBeVisible();
  await expect.poll(() => opened.element.getAttribute('src')).not.toBe(before);
  await expect(
    opened.frame.getByRole('heading', { name: 'Application data probe' }),
  ).toBeVisible();
}

/**
 * Presses one read's button and waits for its answer and for the one call it
 * made. The answer is cleared first, so an older one cannot pass.
 */
async function act(opened, label) {
  const seen = opened.calls.length;
  const result = opened.frame.locator('#result');
  await result.evaluate((element) => {
    element.textContent = 'waiting';
  });
  await opened.frame.getByRole('button', { name: label }).click();
  await expect
    .poll(async () => result.textContent(), { timeout: 20_000 })
    .not.toBe('waiting');
  const answer = JSON.parse(await result.textContent());
  await expect.poll(() => opened.calls.length - seen).toBe(1);
  return { answer, calls: opened.calls.slice(seen) };
}

/** A read before the grant: the frame says `denied`, and the server said 403 for that path. */
async function refusedRead(opened, read) {
  const { answer, calls } = await act(opened, read.label);
  assert.deepEqual(
    answer,
    { error: 'denied' },
    `${read.label} must be refused without the grant.`,
  );
  assert.deepEqual(
    calls.map(({ status, request }) => [status, request.path]),
    [[403, read.path]],
  );
}

/** What the frame must answer for a read once reading is allowed, at the owner's version. */
function expectedAnswer({ path, exists, value }, dataVersion) {
  const answer = { path, dataVersion, exists };
  return exists ? { ...answer, value } : answer;
}

/** A read after the grant: the answer the owner's data gives, and a 200 for that path. */
async function allowedRead(opened, read, dataVersion) {
  const { answer, calls } = await act(opened, read.label);
  assert.deepEqual(
    answer,
    expectedAnswer(read, dataVersion),
    `${read.label} must answer what the owner stored.`,
  );
  assert.deepEqual(
    calls.map(({ status, request }) => [status, request.path]),
    [[200, read.path]],
  );
}

/**
 * The checks of decision 0053 for `data.read` in a real browser: the reads are
 * refused without consent, and allowed after it with the owner's values.
 */
export async function checkRuntimeData({
  browser,
  origin,
  server,
  checkAccessibility,
}) {
  const artifact = await createArtifact(
    server,
    `Application data probe ${randomUUID().slice(0, 8)}`,
  );
  await writeNotes(server, artifact.id);
  const dataVersion = await ownerDataVersion(server, artifact.id);
  const opened = await open(browser, origin, server, artifact);
  try {
    // Nothing is granted: every read is refused, and the server said 403.
    for (const read of READS) {
      await refusedRead(opened, read);
    }

    // The control and its warning are visible before the user presses it.
    await expect(
      opened.permissions.getByText(WARNING, { exact: true }),
    ).toBeVisible();
    await expect(
      opened.permissions.getByRole('button', { name: ALLOW }),
    ).toHaveAccessibleDescription(WARNING);
    await checkAccessibility('app-data-reading-consent', opened.page);

    // The user allows the reading in the workspace; the next view may read.
    await change(opened, ALLOW, 'data.read');
    await checkAccessibility('app-data-reading-allowed', opened.page);
    for (const read of READS) {
      await allowedRead(opened, read, dataVersion);
    }
    assert.deepEqual(opened.errors, []);
    return {
      read: 'notes, a missing path and the root, as the owner stored them',
      refused: 'every read before the user allowed app data reading',
    };
  } finally {
    await opened.context.close();
  }
}

async function runAccessibility(label, target) {
  const violations = await accessibilityViolations(target);
  assert.deepEqual(
    summarizeViolations(violations),
    [],
    `Accessibility failures: ${label}`,
  );
}

/** Runs this check alone against a fresh server and the built web app. */
async function runAlone() {
  const root = fileURLToPath(new URL('../', import.meta.url));
  const binary =
    process.env.ARTIFREE_SERVER_BIN === undefined
      ? buildServer(root)
      : resolve(process.env.ARTIFREE_SERVER_BIN);
  const directory = await mkdtemp(join(tmpdir(), 'artifree-runtime-data-'));
  let server;
  let web;
  let browser;
  try {
    server = await startServer(binary, directory, [
      '--frame-ancestors',
      'http://127.0.0.1:*',
    ]);
    web = await serveStatic(resolve(root, 'apps/web/dist'), 0, {
      apiUrl: server.api_url,
    });
    browser = await chromium.launch();
    const result = await checkRuntimeData({
      browser,
      origin: web.origin,
      server,
      checkAccessibility: runAccessibility,
    });
    console.log(
      JSON.stringify({ status: 'passed', browser: 'chromium', ...result }),
    );
  } finally {
    await browser?.close();
    await web?.close();
    await server?.stop();
    await rm(directory, { recursive: true, force: true });
  }
}

const isEntry =
  process.argv[1] !== undefined &&
  import.meta.url === pathToFileURL(resolve(process.argv[1])).href;
if (isEntry) await runAlone();
