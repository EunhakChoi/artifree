import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { expect } from '@playwright/test';
import { api, raw } from './smoke-workspace.mjs';

const MIB = 1024 * 1024;
const FILE_LIMIT = 5 * MIB;
// An independently authored, complete 1x1 RGBA PNG: the server admits it.
const PNG = Buffer.from(
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgaPjPAAADgwGAJwGhsAAAAABJRU5ErkJggg==',
  'base64',
);
// An independently authored (Pillow 11.3.0, libjpeg-turbo), complete 8x8 gray
// baseline JPEG: the server admits it.
const JPEG = Buffer.from(
  '/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAgGBgcGBQgHBwcJCQgKDBQNDAsLDBkSEw8UHRofHh0aHBwgJC4nICIsIxwcKDcpLDAxNDQ0Hyc5PTgyPC4zNDL/wAALCAAIAAgBAREA/8QAHwAAAQUBAQEBAQEAAAAAAAAAAAECAwQFBgcICQoL/8QAtRAAAgEDAwIEAwUFBAQAAAF9AQIDAAQRBRIhMUEGE1FhByJxFDKBkaEII0KxwRVS0fAkM2JyggkKFhcYGRolJicoKSo0NTY3ODk6Q0RFRkdISUpTVFVWV1hZWmNkZWZnaGlqc3R1dnd4eXqDhIWGh4iJipKTlJWWl5iZmqKjpKWmp6ipqrKztLW2t7i5usLDxMXGx8jJytLT1NXW19jZ2uHi4+Tl5ufo6erx8vP09fb3+Pn6/9oACAEBAAA/ADQVt/A3hJtWli3XDfubOMxlhJOVJQNgjC8EnkcA45wD/9k=',
  'base64',
);
// A type the server does not admit; its bytes are never read.
const GIF = Buffer.from('GIF89a');

/**
 * Browser checks for an artifact's assets (decision 0045, rule 11) against a
 * real artifree-server: uploads and downloads as attachments without the
 * management page showing or running the bytes, the server's type, quota and
 * version refusals, the local size refusal, a lost answer retried as the same
 * request under the same key, and deletes behind a confirmation. The page has
 * its own browser context.
 */
export async function checkAssets({
  browser,
  origin,
  server,
  checkAccessibility,
}) {
  let rawRequests = 0;
  async function list(id) {
    rawRequests += 1;
    const { status, json } = await raw(
      server,
      'GET',
      `/v1/artifacts/${id}/assets`,
    );
    assert.equal(status, 200, `Listing assets failed: ${status}`);
    return json;
  }
  async function seedUpload(id, base, content, key) {
    rawRequests += 1;
    const { status, json } = await raw(
      server,
      'POST',
      `/v1/artifacts/${id}/assets`,
      {
        base_asset_version: base,
        content_type: 'text/plain',
        content_base64: content.toString('base64'),
      },
      key,
    );
    assert.equal(status, 201, `Seeding an asset failed: ${status}`);
    return json;
  }
  const seed = (title) =>
    api(server, 'POST', '/v1/artifacts', {
      title,
      files: [{ path: 'index.html', encoding: 'utf8', content: '<p>a</p>' }],
    });
  const { artifact } = await seed('Assets demo');
  const { artifact: full } = await seed('Assets full');
  rawRequests += 2;
  // Five files of 5 MiB fill the artifact's 25 MiB exactly.
  const filler = Buffer.alloc(FILE_LIMIT, 'a');
  for (let index = 0; index < 5; index += 1)
    await seedUpload(full.id, index, filler, `smoke-assets-fill-${index}`);

  const context = await browser.newContext({ acceptDownloads: true });
  try {
    const tab = await context.newPage();
    tab.setDefaultTimeout(10_000);
    const errors = [];
    const refusals = [];
    const resets = [];
    const unexpectedConsole = [];
    const opened = [];
    const navigations = [];
    const uploads = [];
    const deletes = [];
    let browserRequests = 0;
    tab.on('pageerror', (error) => errors.push(error.message));
    tab.on('console', (message) => {
      if (message.type() !== 'error' && message.type() !== 'warning') return;
      const text = message.text();
      const fromApi = message.location().url.startsWith(`${origin}/v1/`);
      // The refusals this check provokes, and the answers it cuts.
      if (fromApi && /status of (409|422) /.test(text))
        refusals.push(text.match(/status of (\d+)/)[1]);
      else if (fromApi && text.includes('ERR_CONNECTION_RESET'))
        resets.push(text);
      else unexpectedConsole.push(text);
    });
    tab.on('request', (request) => {
      const url = request.url();
      if (url.startsWith(`${origin}/v1/`)) browserRequests += 1;
      if (request.isNavigationRequest() && url.includes('/assets'))
        navigations.push(url);
      if (!url.startsWith(`${origin}/v1/artifacts/${artifact.id}/assets`))
        return;
      const sent = {
        key: request.headers()['idempotency-key'],
        body: request.postData(),
      };
      if (request.method() === 'POST') uploads.push(sent);
      if (request.method() === 'DELETE') deletes.push(sent);
    });

    await tab.goto(origin);
    const workspace = tab.locator('#artifacts');
    await workspace.getByLabel('Access token').fill(server.token);
    await workspace.getByRole('button', { name: 'Connect' }).click();
    async function open(title) {
      await workspace.getByLabel('Search titles').fill(title);
      await workspace
        .getByRole('navigation', { name: 'Artifacts' })
        .getByRole('button', { name: new RegExp(`^${title}`) })
        .click();
      await expect(
        workspace.getByRole('heading', { name: title, exact: true }),
      ).toBeVisible();
      await assetsTab.click();
    }
    const views = workspace.getByRole('tablist', { name: 'Artifact views' });
    const assetsTab = views.getByRole('tab', { name: 'Assets', exact: true });
    const panel = workspace.getByRole('tabpanel', { name: 'Assets' });
    const version = (n) => panel.getByText(`Asset version ${n}`);
    const storage = panel.getByRole('meter', { name: 'Storage' });
    const files = panel.getByRole('meter', { name: 'Files' });
    const fileInput = panel.getByLabel('File', { exact: true });
    const declared = panel.getByLabel('Declared type');
    const uploadButton = panel.getByRole('button', {
      name: 'Upload',
      exact: true,
    });
    const rows = panel
      .getByRole('list', { name: 'Assets' })
      .getByRole('listitem');
    const alert = (title) =>
      panel.getByRole('alert').filter({ hasText: title });
    async function upload(name, mimeType, buffer) {
      await fileInput.setInputFiles({ name, mimeType, buffer });
      await uploadButton.click();
    }

    await open('Assets demo');
    await expect(panel).toContainText('downloaded as attachments');
    await expect(version(0)).toBeVisible();
    await expect(storage).toHaveAttribute('aria-valuetext', '0 B of 25 MiB');
    await expect(files).toHaveAttribute('aria-valuetext', '0 of 64 files');
    await checkAccessibility('workspace-assets', tab);
    // Watched from here: the accessibility check opens a blank page of its own.
    tab.on('popup', (popup) => opened.push(popup.url()));
    context.on('page', (page) => opened.push(page.url()));

    // A file over 5 MiB is refused before it is read; nothing is sent.
    await upload('big.txt', 'text/plain', Buffer.alloc(FILE_LIMIT + 1, 'b'));
    await expect(alert('Not changed')).toContainText(
      'The file is larger than 5 MiB, so it was not read or sent.',
    );
    assert.equal(uploads.length, 0, 'An oversize file was sent.');

    // An upload is listed, and its download is an attachment with the same
    // bytes; the page neither shows nor opens them.
    const marker = `asset-marker-${crypto.randomUUID()}`;
    const text = Buffer.from(`${marker}\n`);
    await upload('notes.txt', 'text/plain', text);
    await expect(alert('Uploaded')).toBeVisible();
    await expect(version(1)).toBeVisible();
    await expect(rows).toHaveCount(1);
    await expect(storage).toHaveAttribute(
      'aria-valuetext',
      `${text.length} B of 25 MiB`,
    );
    await expect(files).toHaveAttribute('aria-valuetext', '1 of 64 files');
    const [notes] = (await list(artifact.id)).assets;
    assert.equal(notes.content_type, 'text/plain');
    const prefix = (asset) => asset.id.slice(0, 12);
    const pageUrl = tab.url();
    const saving = tab.waitForEvent('download');
    await panel
      .getByRole('button', { name: `Download asset ${prefix(notes)}` })
      .click();
    const download = await saving;
    assert.equal(download.suggestedFilename(), `asset-${notes.id}`);
    assert.deepEqual(await readFile(await download.path()), text);
    assert.equal(tab.url(), pageUrl, 'The download navigated the page.');
    assert.equal(
      (await tab.content()).includes(marker),
      false,
      'The page shows the asset content.',
    );
    assert.equal(
      await panel
        .locator(
          'iframe[src^="blob:"], iframe[src*="/assets/"], img, object, embed',
        )
        .count(),
      0,
      'The assets panel embeds content.',
    );

    // A type the server does not admit is sent as declared and refused.
    await fileInput.setInputFiles({
      name: 'pixel.gif',
      mimeType: 'image/gif',
      buffer: GIF,
    });
    await expect(declared).toHaveValue('image/gif');
    await uploadButton.click();
    await expect(alert('Not changed')).toContainText(
      'This file type is not accepted. Uploads accept plain text, CSV, PNG and JPEG.',
    );
    let space = await list(artifact.id);
    assert.deepEqual(
      [space.asset_version, space.assets.length],
      [1, 1],
      'A refused type changed the assets.',
    );

    // Against a stale list the upload is refused with the current version,
    // the list is read again, and a new upload succeeds under a new key.
    await expect(version(1)).toBeVisible();
    await seedUpload(
      artifact.id,
      1,
      Buffer.from('elsewhere\n'),
      'smoke-assets-elsewhere',
    );
    const conflictAt = uploads.length;
    await upload('again.txt', 'text/plain', Buffer.from('again\n'));
    await expect(alert('The assets changed elsewhere')).toContainText(
      'Current version 2',
    );
    await expect(version(2)).toBeVisible();
    await expect(rows).toHaveCount(2);
    await upload('again.txt', 'text/plain', Buffer.from('again\n'));
    await expect(alert('Uploaded')).toBeVisible();
    await expect(version(3)).toBeVisible();
    assert.equal(uploads.length, conflictAt + 2);
    assert.notEqual(
      uploads[conflictAt].key,
      uploads[conflictAt + 1].key,
      'The upload after a conflict reused the refused key.',
    );

    // A lost answer is held through a tab switch; the retry is the same
    // request under the same key, and the server makes it once.
    const assetsPath = `**/v1/artifacts/${artifact.id}/assets`;
    let cut = 0;
    await tab.route(assetsPath, async (route) => {
      if (route.request().method() !== 'POST' || cut > 0)
        return route.fallback();
      cut += 1;
      await route.fetch();
      return route.abort('connectionreset');
    });
    const lostAt = uploads.length;
    await upload('lost.txt', 'text/plain', Buffer.from('lost\n'));
    const unknown = alert('Outcome unknown');
    await expect(
      unknown.getByRole('button', { name: 'Retry upload' }),
    ).toBeVisible();
    space = await list(artifact.id);
    assert.equal(space.asset_version, 4, 'The lost upload did not commit.');
    await views.getByRole('tab', { name: 'Data', exact: true }).click();
    await assetsTab.click();
    // The held attempt locks the trash, like an unsaved draft, and a file
    // chosen again cannot be sent beside it.
    await expect(
      workspace.getByRole('button', { name: 'Move to trash' }),
    ).toBeDisabled();
    await fileInput.setInputFiles({
      name: 'lost.txt',
      mimeType: 'text/plain',
      buffer: Buffer.from('lost\n'),
    });
    await expect(uploadButton).toBeDisabled();
    await unknown.getByRole('button', { name: 'Retry upload' }).click();
    await expect.poll(() => uploads.length).toBe(lostAt + 2);
    assert.equal(
      uploads[lostAt + 1].key,
      uploads[lostAt].key,
      'The retry used a new key.',
    );
    assert.equal(
      uploads[lostAt + 1].body,
      uploads[lostAt].body,
      'The retry sent a different request.',
    );
    await expect(alert('Uploaded')).toContainText(
      'Uploaded earlier; this is the original result.',
    );
    await expect(unknown).toBeHidden();
    // A success by retry clears the form, so the same file is not sent
    // again under a new key.
    await expect(fileInput).toHaveValue('');
    await expect(declared).toHaveValue('');
    await expect(uploadButton).toBeDisabled();
    await expect(
      workspace.getByRole('button', { name: 'Move to trash' }),
    ).toBeEnabled();
    await tab.unroute(assetsPath);
    space = await list(artifact.id);
    assert.deepEqual(
      [space.asset_version, space.assets.length],
      [4, 4],
      'The retried upload was made twice.',
    );

    // Cancelling the confirmation sends nothing; confirming deletes.
    const deleteButton = (asset) =>
      panel.getByRole('button', { name: `Delete asset ${prefix(asset)}` });
    await deleteButton(notes).click();
    let dialog = tab.getByRole('dialog', {
      name: `Delete asset ${prefix(notes)}`,
    });
    await expect(dialog.getByText('Removes this file')).toBeVisible();
    assert.equal(deletes.length, 0, 'Asking to delete sent a delete.');
    await dialog.getByRole('button', { name: 'Cancel' }).click();
    await expect(dialog).toBeHidden();
    assert.equal(deletes.length, 0, 'Cancelling sent a delete.');
    await deleteButton(notes).click();
    await dialog.getByRole('button', { name: 'Delete' }).click();
    await expect(alert('Deleted')).toBeVisible();
    await expect(deleteButton(notes)).toHaveCount(0);
    await expect(version(5)).toBeVisible();

    // A lost delete answer, retried under the same key, deletes once.
    const other = space.assets.find((asset) => asset.id !== notes.id);
    const assetPath = `**/v1/artifacts/${artifact.id}/assets/*`;
    cut = 0;
    await tab.route(assetPath, async (route) => {
      if (route.request().method() !== 'DELETE' || cut > 0)
        return route.fallback();
      cut += 1;
      await route.fetch();
      return route.abort('connectionreset');
    });
    await deleteButton(other).click();
    dialog = tab.getByRole('dialog', { name: `Delete asset ${prefix(other)}` });
    await dialog.getByRole('button', { name: 'Delete' }).click();
    await unknown.getByRole('button', { name: 'Retry delete' }).click();
    await expect.poll(() => deletes.length).toBe(3);
    assert.equal(deletes[2].key, deletes[1].key, 'The retry used a new key.');
    await expect(alert('Deleted')).toContainText(
      'Deleted earlier; this is the original result.',
    );
    await tab.unroute(assetPath);
    space = await list(artifact.id);
    assert.deepEqual(
      [space.asset_version, space.assets.length],
      [6, 2],
      'The retried delete was made twice.',
    );

    // A complete PNG is listed and downloaded like any other asset: the page
    // keeps its bytes and never renders them.
    await upload('pixel.png', 'image/png', PNG);
    await expect(alert('Uploaded')).toBeVisible();
    await expect(version(7)).toBeVisible();
    space = await list(artifact.id);
    const pixel = space.assets.find(
      (asset) => asset.content_type === 'image/png',
    );
    assert.ok(pixel, 'The PNG is not listed.');
    const savingPixel = tab.waitForEvent('download');
    await panel
      .getByRole('button', { name: `Download asset ${prefix(pixel)}` })
      .click();
    assert.deepEqual(await readFile(await (await savingPixel).path()), PNG);
    assert.equal(
      await panel.locator('img, picture, canvas').count(),
      0,
      'The assets panel renders the image.',
    );
    assert.deepEqual(
      [space.asset_version, space.assets.length],
      [7, 3],
      'The PNG upload changed more than one asset.',
    );

    // A complete JPEG is listed and downloaded the same way.
    await upload('photo.jpg', 'image/jpeg', JPEG);
    await expect(alert('Uploaded')).toBeVisible();
    await expect(version(8)).toBeVisible();
    space = await list(artifact.id);
    const photo = space.assets.find(
      (asset) => asset.content_type === 'image/jpeg',
    );
    assert.ok(photo, 'The JPEG is not listed.');
    const savingPhoto = tab.waitForEvent('download');
    await panel
      .getByRole('button', { name: `Download asset ${prefix(photo)}` })
      .click();
    assert.deepEqual(await readFile(await (await savingPhoto).path()), JPEG);
    assert.deepEqual(
      [space.asset_version, space.assets.length],
      [8, 4],
      'The JPEG upload changed more than one asset.',
    );

    // A full artifact refuses one more byte, and nothing is added.
    await open('Assets full');
    await expect(storage).toHaveAttribute('aria-valuetext', '25 MiB of 25 MiB');
    await expect(files).toHaveAttribute('aria-valuetext', '5 of 64 files');
    await upload('one.txt', 'text/plain', Buffer.from('x'));
    await expect(alert('Not changed')).toContainText(
      'Over the asset limit: 5 MiB per file, 64 files and 25 MiB per artifact',
    );
    space = await list(full.id);
    assert.deepEqual(
      [space.asset_version, space.assets.length],
      [5, 5],
      'An upload over the limit changed the assets.',
    );

    assert.deepEqual(opened, [], 'Assets opened a page.');
    assert.deepEqual(navigations, [], 'Assets navigated to their content.');
    assert.deepEqual(errors, [], 'Browser JavaScript errors.');
    assert.deepEqual(unexpectedConsole, [], 'Unexpected console messages.');
    // Firefox does not log failed fetch responses; Chromium logs each one.
    if (browser.browserType().name() === 'chromium') {
      assert.deepEqual(
        refusals.sort(),
        ['409', '422', '422'],
        'Only the stale upload and the type and quota refusals may fail.',
      );
      assert.equal(resets.length, 2, 'Only the two cut answers may fail.');
    }
    return {
      cases: [
        'local-oversize',
        'upload-download-no-inline',
        'type-refused',
        'stale-base',
        'lost-upload-retry',
        'delete-cancel-confirm',
        'lost-delete-retry',
        'png-upload-download-no-inline',
        'jpeg-upload-download',
        'quota-refused',
      ],
      apiRequests: { browser: browserRequests, raw: rawRequests },
    };
  } finally {
    await context.close();
  }
}
