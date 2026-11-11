import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
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
 * Browser checks for the uploaded-file reading capability `assets.read`
 * (decision 0045) in the real preview of a real artifree-server. A frame lists
 * and reads the uploads of its artifact in parts; the user allows and revokes
 * that reading in the workspace, never in the frame. A refusal is checked
 * twice: the frame says `denied`, and the server answered 403. The bytes the
 * frame read are compared with the bytes uploaded, and the digest the SDK
 * verified is compared with one computed here.
 *
 * Run alone, this file uses Chromium; the suite (scripts/smoke.mjs) runs it in
 * the engine that ARTIFREE_BROWSER names. Run this check alone with
 * `npm run build`, then `node scripts/smoke-runtime-assets.mjs`.
 */

// The SDK asks for assets in parts of this many bytes (decision 0045).
const PART = 262_144;
// 300,000 printable ASCII bytes: a read takes two parts, the second shorter.
const TEXT = Buffer.alloc(300_000);
for (let index = 0; index < TEXT.length; index += 1) {
  TEXT[index] = 32 + (index % 95);
}
// An independently authored, complete 1x1 RGBA PNG: the server admits it.
const PNG = Buffer.from(
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgaPjPAAADgwGAJwGhsAAAAABJRU5ErkJggg==',
  'base64',
);
const manifest = JSON.stringify({
  sdk: 'artifree.app.v1',
  capabilities: ['assets.read'],
});
// The artifact has only this page until the uploads exist; it is never shown.
const placeholder =
  '<!doctype html><html lang="en"><head><meta charset="utf-8"><title>Uploads placeholder</title></head><body><p>Waiting for uploads.</p></body></html>';

const sha256 = (buffer) => createHash('sha256').update(buffer).digest('hex');
const byId = (left, right) => (left.id < right.id ? -1 : 1);

/**
 * The page that drives the runtime. Each button writes one JSON answer to the
 * output, so the smoke clears it, presses a button, and waits for the answer.
 */
function probe(ids) {
  return `<!doctype html><html lang="en"><head><!-- artifree-runtime -->
<meta charset="utf-8"><title>Uploads probe</title></head><body>
<h1>Uploads probe</h1><output id="result">waiting</output>
<div>
<button id="list">List uploads</button>
<button id="read-text">Read text</button>
<button id="read-png">Read PNG</button>
</div>
<script>
const ids = ${JSON.stringify(ids)};
const out = document.querySelector('#result');
const show = (value) => { out.textContent = JSON.stringify(value); };
const failure = (error) => show({ error: error && error.code ? error.code : 'unknown' });
function base64(bytes) {
  let binary = '';
  for (let index = 0; index < bytes.length; index += 8192) {
    binary += String.fromCharCode(...bytes.subarray(index, index + 8192));
  }
  return btoa(binary);
}
async function read(assetId) {
  const content = await window.artifree.assets.read({ assetId });
  return {
    assetId: content.assetId,
    contentType: content.contentType,
    size: content.size,
    sha256: content.sha256,
    length: content.bytes.length,
    base64: base64(content.bytes),
  };
}
const run = (id, action) => document.querySelector(id).addEventListener('click', () => {
  action().then(show, failure);
});
run('#list', async () => window.artifree.assets.list());
run('#read-text', async () => read(ids.text));
run('#read-png', async () => read(ids.png));
</script></body></html>`;
}

/** Uploads one asset through the HTTP API, under its own idempotency key. */
async function upload(server, id, base, contentType, bytes) {
  const { status, json } = await raw(
    server,
    'POST',
    `/v1/artifacts/${id}/assets`,
    {
      base_asset_version: base,
      content_type: contentType,
      content_base64: bytes.toString('base64'),
    },
    `smoke-runtime-assets-${randomUUID()}`,
  );
  assert.equal(status, 201, `Uploading ${contentType} failed: ${status}`);
  return json;
}

/** The asset as the frame lists it, from the server's answer to its upload. */
function listed({ asset }) {
  return {
    id: asset.id,
    contentType: asset.content_type,
    size: asset.size_bytes,
    sha256: asset.sha256,
    createdAtMs: asset.created_at_ms,
  };
}

/** An artifact whose manifest asks for assets.read, with a placeholder page. */
async function createArtifact(server, title) {
  const { artifact } = await api(server, 'POST', '/v1/artifacts', {
    title,
    files: [
      { path: 'index.html', encoding: 'utf8', content: placeholder },
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
    // Each listing and read the frame sends, with the status the server gave.
    const calls = [];
    page.on('pageerror', (error) => errors.push(error.message));
    page.on('response', (response) => {
      const url = response.url();
      const request = response.request();
      if (request.method() !== 'POST') return;
      if (!url.endsWith('/assets/list') && !url.endsWith('/assets/read'))
        return;
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
    await expect(permissions.getByText('Declared: assets.read')).toBeVisible();
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

/** Presses a permission control and waits until the workspace shows the new view. */
async function change(opened, button, granted) {
  const before = await opened.element.getAttribute('src');
  await opened.permissions.getByRole('button', { name: button }).click();
  await expect(
    opened.permissions.getByText(`Granted: ${granted}`),
  ).toBeVisible();
  await expect.poll(() => opened.element.getAttribute('src')).not.toBe(before);
  await expect(
    opened.frame.getByRole('button', { name: 'List uploads' }),
  ).toBeVisible();
}

/**
 * Presses a button and waits for both the answer and the `expected` calls it
 * made. The answer is cleared first, so an older one cannot pass.
 */
async function act(opened, name, expected) {
  const seen = opened.calls.length;
  const result = opened.frame.locator('#result');
  await result.evaluate((element) => {
    element.textContent = 'waiting';
  });
  await opened.frame.getByRole('button', { name }).click();
  await expect
    .poll(async () => result.textContent(), { timeout: 20_000 })
    .not.toBe('waiting');
  const answer = JSON.parse(await result.textContent());
  await expect.poll(() => opened.calls.length - seen).toBe(expected);
  return { answer, calls: opened.calls.slice(seen) };
}

/** A refused read or listing: the frame says `denied`, and the server said 403. */
async function refused(opened, name) {
  const { answer, calls } = await act(opened, name, 1);
  assert.deepEqual(answer, { error: 'denied' }, `${name} must be refused.`);
  assert.deepEqual(
    calls.map(({ status }) => status),
    [403],
  );
}

/** The status, asset, offset and length of each read call, in order. */
const parts = (calls) =>
  calls.map(({ status, request }) => [
    status,
    request.asset_id,
    request.offset,
    request.length,
  ]);

/** The frame's answer for one asset: its type, size and digest, and its bytes. */
function assertRead(answer, asset, bytes) {
  assert.equal(answer.assetId, asset.id);
  assert.equal(answer.contentType, asset.content_type);
  assert.equal(answer.size, bytes.length);
  assert.equal(answer.length, bytes.length);
  assert.equal(answer.sha256, sha256(bytes));
  assert.ok(
    Buffer.from(answer.base64, 'base64').equals(bytes),
    'The bytes read differ from the upload.',
  );
}

/**
 * The checks of decision 0045 for `assets.read` in a real browser: the calls
 * are refused without consent and after a revoke, and allowed in between.
 */
export async function checkRuntimeAssets({
  browser,
  origin,
  server,
  checkAccessibility,
}) {
  const artifact = await createArtifact(
    server,
    `Uploads probe ${randomUUID().slice(0, 8)}`,
  );
  const text = await upload(server, artifact.id, 0, 'text/plain', TEXT);
  const png = await upload(
    server,
    artifact.id,
    text.asset_version,
    'image/png',
    PNG,
  );
  // The uploads need an artifact to belong to, so the page that names them is
  // written as the second revision, and each call asks for a real asset.
  await api(server, 'POST', `/v1/artifacts/${artifact.id}/revisions`, {
    base_revision: 1,
    changes: [
      {
        op: 'put',
        path: 'index.html',
        encoding: 'utf8',
        content: probe({ text: text.asset.id, png: png.asset.id }),
      },
    ],
  });
  const opened = await open(browser, origin, server, artifact);
  try {
    // Nothing is granted: the listing and both reads are refused.
    await refused(opened, 'List uploads');
    await refused(opened, 'Read text');
    await refused(opened, 'Read PNG');

    // The user allows the reading in the workspace; the next view may read.
    await change(opened, 'Allow uploaded file reading', 'assets.read');
    await checkAccessibility('uploaded-file-reading-allowed', opened.page);
    const listing = await act(opened, 'List uploads', 1);
    assert.deepEqual(
      listing.calls.map(({ status }) => status),
      [200],
    );
    assert.equal(listing.answer.assetVersion, png.asset_version);
    assert.deepEqual(
      listing.answer.assets,
      [listed(text), listed(png)].sort(byId),
    );

    // The text is larger than one part: two reads, each for one part.
    const readText = await act(opened, 'Read text', 2);
    assert.deepEqual(parts(readText.calls), [
      [200, text.asset.id, 0, PART],
      [200, text.asset.id, PART, PART],
    ]);
    assertRead(readText.answer, text.asset, TEXT);
    const readPng = await act(opened, 'Read PNG', 1);
    assert.deepEqual(parts(readPng.calls), [[200, png.asset.id, 0, PART]]);
    assertRead(readPng.answer, png.asset, PNG);

    // The user revokes the reading; the next view holds no grant and is refused.
    await change(opened, 'Revoke uploaded file reading', 'none');
    await refused(opened, 'List uploads');
    await refused(opened, 'Read text');
    await refused(opened, 'Read PNG');
    assert.deepEqual(opened.errors, []);
    return {
      listed: 'both uploads: id, type, size, digest and time',
      read: 'text in two parts, PNG in one; bytes and digests compared',
      refused: ['no consent', 'after revoke'],
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
  const directory = await mkdtemp(join(tmpdir(), 'artifree-runtime-assets-'));
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
    const result = await checkRuntimeAssets({
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
