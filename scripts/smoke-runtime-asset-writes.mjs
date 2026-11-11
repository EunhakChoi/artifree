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
import { api, bytes, raw } from './smoke-workspace.mjs';

/**
 * Browser checks for the uploaded-file writing capability `assets.write`
 * (decision 0045) in the real preview of a real artifree-server. A frame adds
 * and deletes the uploads of its artifact; the user allows and revokes that in
 * the workspace, never in the frame. A refusal is checked twice: the frame says
 * `denied` and the server answered 403, and the uploads did not change. Each
 * stored upload is read back through the management API and compared with the
 * bytes the frame sent. A call based on an old version gets `conflict`, with
 * the version the server holds.
 *
 * Run alone, this file uses Chromium; the suite (scripts/smoke.mjs) runs it in
 * the engine that ARTIFREE_BROWSER names. Run this check alone with
 * `npm run build`, then `node scripts/smoke-runtime-asset-writes.mjs`.
 */

// The frame's text has a two-byte character: the SDK saves a string as UTF-8.
const TEXT = 'Frame upload, café.\nline two\n';
const TEXT_BYTES = Buffer.from(TEXT, 'utf8');
// Uploaded through the management API before the page exists.
const SEED = Buffer.from('Seed upload, stored before the page.\n');
// An independently authored, complete 1x1 RGBA PNG: the server admits it.
const PNG_BASE64 =
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgaPjPAAADgwGAJwGhsAAAAABJRU5ErkJggg==';
const PNG = Buffer.from(PNG_BASE64, 'base64');
// The warnings beside the two controls, as the workspace shows them.
const READ_WARNING =
  "Allowing this gives the app the bytes of this artifact's private uploaded files. The preview is not isolated from the network, so the app could send those bytes elsewhere. Allowing access does not make the files safe to run.";
const WRITE_WARNING =
  "Allowing this lets the app add files to this artifact's private uploaded files and delete any of them, including files you uploaded, without asking you again until you revoke it or the artifact's source changes. Deleted files cannot be restored. Added files count toward this artifact's upload limits and are checked by type, which does not make them safe to run.";
// Both capabilities are declared, so each one has its own control.
const manifest = JSON.stringify({
  sdk: 'artifree.app.v1',
  capabilities: ['assets.read', 'assets.write'],
});
// The artifact has only this page until the seed exists; it is never shown.
const placeholder =
  '<!doctype html><html lang="en"><head><meta charset="utf-8"><title>Uploads placeholder</title></head><body><p>Waiting for uploads.</p></body></html>';
// The frame's calls as the workspace sends them, with the endpoint captured.
const FRAME_CALL =
  /\/runtime-views\/[^/]+\/\d+\/assets\/(list|read|upload|delete)$/u;

const sha256 = (buffer) => createHash('sha256').update(buffer).digest('hex');
const byId = (left, right) => (left.id < right.id ? -1 : 1);

/** A management asset, with the names the frame gives its fields. */
const camel = (asset) => ({
  id: asset.id,
  contentType: asset.content_type,
  size: asset.size_bytes,
  sha256: asset.sha256,
  createdAtMs: asset.created_at_ms,
});

/**
 * The page that drives the writes. Each button writes one JSON answer to the
 * output, so the check clears it, presses a button, and waits for the answer.
 * The page starts from the seed's ID and version, and then follows the
 * versions its own receipts name, as an app would.
 */
function probe(seed) {
  return `<!doctype html><html lang="en"><head><!-- artifree-runtime -->
<meta charset="utf-8"><title>Uploads writer probe</title></head><body>
<h1>Uploads writer probe</h1><output id="result">waiting</output>
<div>
<button id="list">List uploads</button>
<button id="upload-text">Upload text</button>
<button id="upload-png">Upload PNG</button>
<button id="upload-stale">Upload with stale base</button>
<button id="delete">Delete last upload</button>
</div>
<script>
const out = document.querySelector('#result');
const show = (value) => { out.textContent = JSON.stringify(value); };
const failure = (error) => show({
  error: error && error.code ? error.code : 'unknown',
  currentAssetVersion: error && error.currentAssetVersion,
});
let version = ${JSON.stringify(seed.version)};
let last = ${JSON.stringify(seed.id)};
let calls = 0;
const retryId = () => { calls += 1; return 'smoke-write-' + calls; };
const TEXT = ${JSON.stringify(TEXT)};
const PNG_BASE64 = ${JSON.stringify(PNG_BASE64)};
const png = () => Uint8Array.from(atob(PNG_BASE64), (character) => character.charCodeAt(0));
const stored = (receipt) => {
  version = receipt.assetVersion;
  last = receipt.asset.id;
  return { uploaded: receipt };
};
const run = (id, action) => document.querySelector(id).addEventListener('click', () => {
  action().then(show, failure);
});
run('#list', () => window.artifree.assets.list());
run('#upload-text', () => window.artifree.assets.upload({
  contentType: 'text/plain', content: TEXT, baseAssetVersion: version, retryId: retryId(),
}).then(stored));
run('#upload-png', () => window.artifree.assets.upload({
  contentType: 'image/png', content: png(), baseAssetVersion: version, retryId: retryId(),
}).then(stored));
run('#upload-stale', () => window.artifree.assets.upload({
  contentType: 'text/plain', content: 'stale', baseAssetVersion: 0, retryId: retryId(),
}));
run('#delete', () => window.artifree.assets.delete({
  assetId: last, baseAssetVersion: version, retryId: retryId(),
}).then((receipt) => {
  version = receipt.assetVersion;
  return { removed: receipt };
}));
</script></body></html>`;
}

/** Uploads one asset through the management API, under its own idempotency key. */
async function upload(server, id, base, contentType, content) {
  const { status, json } = await raw(
    server,
    'POST',
    `/v1/artifacts/${id}/assets`,
    {
      base_asset_version: base,
      content_type: contentType,
      content_base64: content.toString('base64'),
    },
    `smoke-runtime-asset-writes-${randomUUID()}`,
  );
  assert.equal(status, 201, `Uploading ${contentType} failed: ${status}`);
  return json;
}

/** The status of one management GET; the body is not needed. */
async function httpStatus(server, path) {
  const response = await fetch(`${server.api_url}${path}`, {
    headers: { authorization: `Bearer ${server.token}` },
  });
  await response.arrayBuffer();
  return response.status;
}

/** An artifact with a placeholder page and the manifest that declares both capabilities. */
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
    // Each write and listing the frame makes, with the status the server gave.
    const calls = [];
    page.on('pageerror', (error) => errors.push(error.message));
    page.on('response', (response) => {
      if (response.request().method() !== 'POST') return;
      const match = FRAME_CALL.exec(new URL(response.url()).pathname);
      if (match !== null)
        calls.push({ endpoint: match[1], status: response.status() });
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
    await expect(
      permissions.getByText('Declared: assets.read, assets.write', {
        exact: true,
      }),
    ).toBeVisible();
    await expect(
      permissions.getByText('Granted: none', { exact: true }),
    ).toBeVisible();
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

/**
 * Presses a permission control, and waits until the workspace shows the new
 * grant and the preview has reloaded with it.
 */
async function change(opened, button, granted) {
  const before = await opened.element.getAttribute('src');
  await opened.permissions
    .getByRole('button', { name: button, exact: true })
    .click();
  await expect(
    opened.permissions.getByText(`Granted: ${granted}`, { exact: true }),
  ).toBeVisible();
  await expect.poll(() => opened.element.getAttribute('src')).not.toBe(before);
  await expect(
    opened.frame.getByRole('button', { name: 'List uploads', exact: true }),
  ).toBeVisible();
}

/**
 * Presses a probe button and waits for both the answer and the calls it made.
 * The answer is cleared first, so an older one cannot pass.
 */
async function act(opened, name, expected) {
  const seen = opened.calls.length;
  const result = opened.frame.locator('#result');
  await result.evaluate((element) => {
    element.textContent = 'waiting';
  });
  await opened.frame.getByRole('button', { name, exact: true }).click();
  await expect
    .poll(async () => result.textContent(), { timeout: 20_000 })
    .not.toBe('waiting');
  const answer = JSON.parse(await result.textContent());
  await expect.poll(() => opened.calls.length - seen).toBe(expected);
  return { answer, calls: opened.calls.slice(seen) };
}

/** A refused call: the frame says `denied`, and the server answered 403 to that endpoint. */
async function refused(opened, name, endpoint) {
  const { answer, calls } = await act(opened, name, 1);
  assert.deepEqual(answer, { error: 'denied' }, `${name} must be refused.`);
  assert.deepEqual(calls, [{ endpoint, status: 403 }]);
}

/**
 * A stored upload: the receipt names the type, size and digest of the bytes
 * the frame sent, and the version the upload produced.
 */
function assertStored(answer, expected, contentType, version) {
  assert.ok(
    answer.uploaded !== undefined,
    `The upload was not stored: ${JSON.stringify(answer)}`,
  );
  const { asset, assetVersion } = answer.uploaded;
  assert.equal(assetVersion, version);
  assert.deepEqual(
    { contentType: asset.contentType, size: asset.size, sha256: asset.sha256 },
    { contentType, size: expected.length, sha256: sha256(expected) },
  );
  return asset;
}

/** The management API's view of the uploads: the asset version and the assets, by ID. */
async function assertServer(server, id, version, assets) {
  const listing = await api(server, 'GET', `/v1/artifacts/${id}/assets`);
  assert.equal(listing.asset_version, version, 'The asset version differs.');
  assert.deepEqual(
    listing.assets.map(camel).sort(byId),
    [...assets].sort(byId),
  );
}

/** The bytes the management API reads for one asset equal the bytes the frame sent. */
async function assertContent(server, id, assetId, expected) {
  const actual = await bytes(
    server,
    `/v1/artifacts/${id}/assets/${assetId}/content`,
  );
  assert.ok(
    actual.equals(expected),
    'The management API read differs from the bytes the frame sent.',
  );
}

/** A deleted asset has no content any more. */
async function assertGone(server, id, assetId) {
  assert.equal(
    await httpStatus(server, `/v1/artifacts/${id}/assets/${assetId}/content`),
    404,
  );
}

/**
 * The checks of decision 0045 for `assets.write` in a real browser: the writes
 * are refused without consent and after a revoke, and they are stored, with
 * the bytes sent, while consent lasts.
 */
export async function checkRuntimeAssetWrites({
  browser,
  origin,
  server,
  checkAccessibility,
}) {
  const artifact = await createArtifact(
    server,
    `Uploads writer ${randomUUID().slice(0, 8)}`,
  );
  const seed = await upload(server, artifact.id, 0, 'text/plain', SEED);
  assert.equal(seed.asset_version, 1);
  const seeded = camel(seed.asset);
  // The page is written after the seed, because it names the seed and version.
  await api(server, 'POST', `/v1/artifacts/${artifact.id}/revisions`, {
    base_revision: 1,
    changes: [
      {
        op: 'put',
        path: 'index.html',
        encoding: 'utf8',
        content: probe({ id: seed.asset.id, version: seed.asset_version }),
      },
    ],
  });
  const opened = await open(browser, origin, server, artifact);
  try {
    const { permissions } = opened;
    // Each control's warning is its accessible description.
    await expect(
      permissions.getByRole('button', {
        name: 'Allow uploaded file changes',
        exact: true,
      }),
    ).toHaveAccessibleDescription(WRITE_WARNING);
    await expect(
      permissions.getByRole('button', {
        name: 'Allow uploaded file reading',
        exact: true,
      }),
    ).toHaveAccessibleDescription(READ_WARNING);

    // Without the grant, the upload and the deletion are refused, and nothing changes.
    await refused(opened, 'Upload text', 'upload');
    await refused(opened, 'Delete last upload', 'delete');
    await assertServer(server, artifact.id, 1, [seeded]);

    // Allowing changes does not allow reading: the listing stays refused.
    await change(opened, 'Allow uploaded file changes', 'assets.write');
    await expect(
      permissions.getByRole('button', {
        name: 'Revoke uploaded file changes',
        exact: true,
      }),
    ).toHaveAccessibleDescription(WRITE_WARNING);
    await expect(
      permissions.getByRole('button', {
        name: 'Allow uploaded file reading',
        exact: true,
      }),
    ).toBeVisible();
    await checkAccessibility('uploaded-file-changes-allowed', opened.page);
    await refused(opened, 'List uploads', 'list');

    // The text is stored as version 2, and the stored bytes are the bytes sent.
    const text = await act(opened, 'Upload text', 1);
    assert.deepEqual(text.calls, [{ endpoint: 'upload', status: 200 }]);
    const textAsset = assertStored(text.answer, TEXT_BYTES, 'text/plain', 2);
    await assertContent(server, artifact.id, textAsset.id, TEXT_BYTES);
    await assertServer(server, artifact.id, 2, [seeded, textAsset]);

    // The PNG is binary: it is stored as version 3 with its exact bytes.
    const png = await act(opened, 'Upload PNG', 1);
    assert.deepEqual(png.calls, [{ endpoint: 'upload', status: 200 }]);
    const pngAsset = assertStored(png.answer, PNG, 'image/png', 3);
    await assertContent(server, artifact.id, pngAsset.id, PNG);
    await assertServer(server, artifact.id, 3, [seeded, textAsset, pngAsset]);

    // A call based on version 0 is a conflict that names version 3, and stores nothing.
    const stale = await act(opened, 'Upload with stale base', 1);
    assert.deepEqual(stale.answer, {
      error: 'conflict',
      currentAssetVersion: 3,
    });
    assert.deepEqual(stale.calls, [{ endpoint: 'upload', status: 409 }]);
    await assertServer(server, artifact.id, 3, [seeded, textAsset, pngAsset]);

    // The deletion removes the PNG: its receipt, its content and the listing agree.
    const deleted = await act(opened, 'Delete last upload', 1);
    assert.deepEqual(deleted.answer, {
      removed: { assetId: pngAsset.id, assetVersion: 4, deleted: true },
    });
    assert.deepEqual(deleted.calls, [{ endpoint: 'delete', status: 200 }]);
    await assertGone(server, artifact.id, pngAsset.id);
    await assertServer(server, artifact.id, 4, [seeded, textAsset]);

    // After the revoke the next call is refused again, and nothing changes.
    await change(opened, 'Revoke uploaded file changes', 'none');
    await refused(opened, 'Upload text', 'upload');
    await assertServer(server, artifact.id, 4, [seeded, textAsset]);
    assert.deepEqual(opened.errors, []);
    return {
      refused: [
        'upload and delete without consent',
        'listing while only changes are allowed',
        'upload after revoke',
      ],
      stored: 'text (UTF-8) and PNG (binary): receipts and bytes compared',
      conflict: 'stale base answers conflict and names version 3',
      deleted: 'PNG: receipt, content 404, listing',
      warnings: 'each control is described by its own warning',
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
  const directory = await mkdtemp(
    join(tmpdir(), 'artifree-runtime-asset-writes-'),
  );
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
    const result = await checkRuntimeAssetWrites({
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
