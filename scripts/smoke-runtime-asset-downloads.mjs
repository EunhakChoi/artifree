import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { chromium, expect, firefox } from '@playwright/test';
import {
  accessibilityViolations,
  summarizeViolations,
} from './lib/accessibility.mjs';
import { waitForPopupEntrance } from './lib/popup-animation.mjs';
import { buildServer, startServer } from './lib/server-process.mjs';
import { serveStatic } from './lib/static-web.mjs';
import { api, raw } from './smoke-workspace.mjs';

/**
 * Browser checks for the uploaded-file download capability `assets.download`
 * (decision 0045) in the real preview of a real artifree-server. A frame asks
 * to save one upload of its artifact. The server answers the offer first, and
 * the user confirms in the workspace, never in the frame. A refusal is checked
 * twice: the frame says `denied`, no prompt opens, and the server answered 403
 * to the offer. An accepted save is compared with the stored bytes: the browser
 * saves the file under the offered name, and the bytes are equal. A declined
 * save fetches and saves nothing.
 *
 * The suite (scripts/smoke.mjs) and this file run alone both use the engine
 * that ARTIFREE_BROWSER names, Chromium by default. Run this check alone with
 * `npm run build`, then `node scripts/smoke-runtime-asset-downloads.mjs`.
 */

// An independently authored, complete 1x1 RGBA PNG: the server admits it.
const PNG_BASE64 =
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgaPjPAAADgwGAJwGhsAAAAABJRU5ErkJggg==';
const PNG = Buffer.from(PNG_BASE64, 'base64');
// The warning beside the control, as the workspace shows it.
const DOWNLOAD_WARNING =
  'Each download asks you first. Saved files are uploaded content; allowing downloads does not make them safe to open.';
// The frame's button, which asks for one upload, and the prompt's title for it.
const BUTTON = 'Download uploaded file';
const PROMPT = 'Save uploaded file?';
// The artifact declares only this capability, so it has one control.
const manifest = JSON.stringify({
  sdk: 'artifree.app.v1',
  capabilities: ['assets.download'],
});
// The artifact has only this page until the upload exists; it is never shown.
const placeholder =
  '<!doctype html><html lang="en"><head><meta charset="utf-8"><title>Downloads placeholder</title></head><body><p>Waiting for uploads.</p></body></html>';
// The frame's requests as the workspace sends them, with the endpoint captured.
const FRAME_CALL = /\/runtime-views\/[^/]+\/\d+\/assets\/(offer|download)$/u;

const sha256 = (buffer) => createHash('sha256').update(buffer).digest('hex');

/**
 * The page that asks to save the upload. The button makes one request and
 * writes the answer to the output, so the check clears it, presses the button,
 * and waits for the answer. The page names the upload's ID, so it is written
 * after the upload, as the writes check does for its seed.
 */
function probe(assetId) {
  return `<!doctype html><html lang="en"><head><!-- artifree-runtime -->
<meta charset="utf-8"><title>Downloads probe</title></head><body>
<h1>Downloads probe</h1><output id="result">waiting</output>
<div>
<button id="download">${BUTTON}</button>
</div>
<script>
const out = document.querySelector('#result');
const show = (value) => { out.textContent = JSON.stringify(value); };
const failure = (error) => show({ error: error && error.code ? error.code : 'unknown' });
document.querySelector('#download').addEventListener('click', () => {
  window.artifree.assets.requestDownload({ assetId: ${JSON.stringify(assetId)} })
    .then((downloaded) => show({ downloaded }), failure);
});
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
    `smoke-runtime-asset-downloads-${randomUUID()}`,
  );
  assert.equal(status, 201, `Uploading ${contentType} failed: ${status}`);
  return json;
}

/** An artifact with a placeholder page and the manifest that declares the capability. */
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
  // Downloads are accepted, so that the file the browser saves can be read.
  const context = await browser.newContext({ acceptDownloads: true });
  try {
    const page = await context.newPage();
    page.setDefaultTimeout(15_000);
    const errors = [];
    // Each request the frame makes, with the status the server gave it.
    const calls = [];
    // Each file the browser saves from this page.
    const downloads = [];
    page.on('pageerror', (error) => errors.push(error.message));
    page.on('download', (download) => downloads.push(download));
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
      permissions.getByText('Declared: assets.download', { exact: true }),
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
      downloads,
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
    opened.frame.getByRole('button', { name: BUTTON, exact: true }),
  ).toBeVisible();
}

/**
 * Clears the frame's answer and presses its button. The answer is read with
 * `answer`, so an older one cannot pass for this press.
 */
async function press(opened) {
  const result = opened.frame.locator('#result');
  await result.evaluate((element) => {
    element.textContent = 'waiting';
  });
  await opened.frame.getByRole('button', { name: BUTTON, exact: true }).click();
  return result;
}

/** The frame's answer to the press that cleared it, once the frame gives one. */
async function answer(result) {
  await expect
    .poll(async () => result.textContent(), { timeout: 20_000 })
    .not.toBe('waiting');
  return JSON.parse(await result.textContent());
}

/** Waits until the server has answered `count` requests since `seen`, and returns them. */
async function requests(opened, seen, count) {
  await expect.poll(() => opened.calls.length - seen).toBe(count);
  return opened.calls.slice(seen);
}

/**
 * Presses the frame's button and waits for the workspace's prompt. The prompt
 * names the file and its size, and it must not move the preview.
 */
async function waitForPrompt(opened, file, size) {
  // Bring the whole preview into view first: a click that has to scroll the
  // page would move the preview and be mistaken for the prompt moving it.
  await opened.element.scrollIntoViewIfNeeded();
  const box = await opened.element.boundingBox();
  const result = await press(opened);
  const dialog = opened.page.getByRole('dialog', { name: PROMPT });
  await expect(dialog).toBeVisible();
  await waitForPopupEntrance(dialog);
  assert.deepEqual(
    await opened.element.boundingBox(),
    box,
    'The prompt must not move the preview.',
  );
  await expect(dialog.getByText(file, { exact: true })).toBeVisible();
  await expect(
    dialog.getByText(`${size} bytes`, { exact: true }),
  ).toBeVisible();
  return { dialog, result };
}

/**
 * A refused request: the frame says `denied`, the server answered its offer
 * with 403, no prompt opened, and nothing was saved.
 */
async function refused(opened) {
  const seen = opened.calls.length;
  const saved = opened.downloads.length;
  const result = await press(opened);
  assert.deepEqual(
    await answer(result),
    { error: 'denied' },
    'The request must be refused.',
  );
  assert.deepEqual(await requests(opened, seen, 1), [
    { endpoint: 'offer', status: 403 },
  ]);
  await expect(opened.page.getByRole('dialog')).toHaveCount(0);
  assert.equal(
    opened.downloads.length,
    saved,
    'A refused request must not save a file.',
  );
}

/**
 * The checks of decision 0045 for `assets.download` in a real browser: a
 * request is refused without the grant and after a revoke, with no prompt; an
 * accepted request saves the stored bytes under the offered name; a declined
 * request fetches and saves nothing.
 */
export async function checkRuntimeAssetDownloads({
  browser,
  origin,
  server,
  checkAccessibility,
}) {
  const artifact = await createArtifact(
    server,
    `Downloads ${randomUUID().slice(0, 8)}`,
  );
  const uploaded = await upload(server, artifact.id, 0, 'image/png', PNG);
  assert.equal(uploaded.asset_version, 1);
  const { asset } = uploaded;
  assert.deepEqual(
    {
      contentType: asset.content_type,
      size: asset.size_bytes,
      sha256: asset.sha256,
    },
    { contentType: 'image/png', size: PNG.length, sha256: sha256(PNG) },
  );
  const file = `asset-${asset.id}.png`;
  // The page is written after the upload, because it names the upload's ID.
  await api(server, 'POST', `/v1/artifacts/${artifact.id}/revisions`, {
    base_revision: 1,
    changes: [
      {
        op: 'put',
        path: 'index.html',
        encoding: 'utf8',
        content: probe(asset.id),
      },
    ],
  });
  const opened = await open(browser, origin, server, artifact);
  try {
    // The control's warning is its accessible description.
    await expect(
      opened.permissions.getByRole('button', {
        name: 'Allow uploaded file downloads',
        exact: true,
      }),
    ).toHaveAccessibleDescription(DOWNLOAD_WARNING);

    // Without the grant, the request is refused and no prompt opens.
    await refused(opened);

    // The grant lets the frame ask, and the user still confirms each save.
    await change(opened, 'Allow uploaded file downloads', 'assets.download');
    let seen = opened.calls.length;
    let saved = opened.downloads.length;
    const accepting = await waitForPrompt(opened, file, PNG.length);
    // Nothing is saved while the prompt waits for the user.
    assert.equal(opened.downloads.length, saved);
    await checkAccessibility('uploaded-file-download-prompt', opened.page);
    const saving = opened.page.waitForEvent('download');
    await accepting.dialog
      .getByRole('button', { name: 'Save file', exact: true })
      .click();
    await expect(accepting.dialog).toBeHidden();
    const downloaded = await saving;
    assert.equal(downloaded.suggestedFilename(), file);
    assert.equal(opened.downloads.length, saved + 1, 'One file was saved.');
    assert.deepEqual(
      await readFile(await downloaded.path()),
      PNG,
      'The saved file differs from the stored upload.',
    );
    assert.deepEqual(await answer(accepting.result), {
      downloaded: { started: true, assetId: asset.id, bytes: PNG.length },
    });
    assert.deepEqual(await requests(opened, seen, 2), [
      { endpoint: 'offer', status: 200 },
      { endpoint: 'download', status: 200 },
    ]);

    // Declining is an answer: the bytes are never fetched, and nothing is saved.
    seen = opened.calls.length;
    saved = opened.downloads.length;
    const declining = await waitForPrompt(opened, file, PNG.length);
    await declining.dialog
      .getByRole('button', { name: 'Decline', exact: true })
      .click();
    await expect(declining.dialog).toBeHidden();
    assert.deepEqual(await answer(declining.result), { error: 'declined' });
    assert.deepEqual(await requests(opened, seen, 1), [
      { endpoint: 'offer', status: 200 },
    ]);
    assert.equal(
      opened.downloads.length,
      saved,
      'A declined request must not save a file.',
    );

    // Revoking is the user's own act: the next request is refused, with no prompt.
    await change(opened, 'Revoke uploaded file downloads', 'none');
    await refused(opened);
    assert.deepEqual(opened.errors, [], 'Browser JavaScript errors.');
    return {
      refused:
        'without the grant and after revoke: denied, 403 offer, no prompt',
      prompt:
        'names the file and size, does not move the preview, saves nothing while it waits',
      accepted: 'saved as the offered name, bytes equal to the stored PNG',
      declined: 'declined: one offer, no download request, nothing saved',
      warning: 'the control is described by its warning',
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
    join(tmpdir(), 'artifree-runtime-asset-downloads-'),
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
    // The engine the whole suite uses (scripts/smoke.mjs), chosen the same way.
    const engines = { chromium, firefox };
    const name = process.env.ARTIFREE_BROWSER ?? 'chromium';
    assert.ok(Object.hasOwn(engines, name), `Unsupported browser: ${name}`);
    browser = await engines[name].launch();
    const result = await checkRuntimeAssetDownloads({
      browser,
      origin: web.origin,
      server,
      checkAccessibility: runAccessibility,
    });
    console.log(JSON.stringify({ status: 'passed', browser: name, ...result }));
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
