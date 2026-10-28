import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { expect } from '@playwright/test';
import { storedZipEntries } from './lib/zip-entries.mjs';
import { forgeReset } from './smoke-history.mjs';
import { api } from './smoke-workspace.mjs';

/**
 * Browser checks for source exports (decision 0015) against a real
 * artifree-server: the file the browser saves for the revision the preview
 * shows holds that revision's bytes and manifest, a failed download says
 * nothing changed and can be tried again, and a possible replacement of the
 * server's data stops it. The page has its own browser context.
 */
export async function checkExport({
  browser,
  origin,
  server,
  checkAccessibility,
}) {
  const logo = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0, 0xff, 0x0d, 0x0a]);
  const { artifact } = await api(server, 'POST', '/v1/artifacts', {
    title: 'Export demo',
    files: [
      { path: 'index.html', encoding: 'utf8', content: '<p>first</p>\r\n' },
      {
        path: 'img/logo.png',
        encoding: 'base64',
        content: logo.toString('base64'),
      },
    ],
  });
  const source = `/v1/artifacts/${artifact.id}`;
  await api(server, 'POST', `${source}/revisions`, {
    base_revision: 1,
    changes: [
      { op: 'put', path: 'index.html', encoding: 'utf8', content: '<p>2</p>' },
    ],
  });
  const first = await api(server, 'GET', `${source}/revisions/1`);

  const context = await browser.newContext({ acceptDownloads: true });
  try {
    const tab = await context.newPage();
    tab.setDefaultTimeout(10_000);
    const errors = [];
    tab.on('pageerror', (error) => errors.push(error.message));
    await tab.goto(origin);
    const workspace = tab.locator('#artifacts');
    await workspace.getByLabel('Access token').fill(server.token);
    await workspace.getByRole('button', { name: 'Connect' }).click();
    await workspace.getByLabel('Search titles').fill('Export demo');
    const nav = workspace.getByRole('navigation', { name: 'Artifacts' });
    await nav.getByRole('button', { name: /^Export demo/ }).click();
    await expect(
      workspace.getByRole('heading', { name: 'Export demo', exact: true }),
    ).toBeVisible();

    // A failed download changed nothing and says so.
    await workspace.locator('#preview-revision').selectOption('1');
    const button = workspace.getByRole('button', {
      name: 'Download revision 1',
    });
    await tab.route(
      '**/export',
      (route) =>
        route.fulfill({
          status: 503,
          json: { error: { code: 'unavailable', message: 'Busy.' } },
        }),
      { times: 1 },
    );
    await button.click();
    const failed = workspace
      .getByRole('alert')
      .filter({ hasText: 'Not downloaded' });
    await expect(failed).toContainText('Nothing changed; try again.');
    await checkAccessibility('workspace-export-failed', tab);

    // Tried again, the browser saves revision 1 as the server stores it.
    const saving = tab.waitForEvent('download');
    await button.click();
    const download = await saving;
    assert.equal(download.suggestedFilename(), `${artifact.id}-r1.zip`);
    await expect(failed).toBeHidden();
    const entries = storedZipEntries(await readFile(await download.path()));
    assert.deepEqual(
      entries.map(({ name }) => name),
      ['.artifree-export.json', 'img/logo.png', 'index.html'],
    );
    const manifest = JSON.parse(entries[0].content.toString('utf8'));
    assert.deepEqual(
      [manifest.revision, manifest.files, manifest.runtime],
      [1, first.files, 'none'],
    );
    assert.deepEqual(entries[1].content, logo);
    assert.equal(entries[2].content.toString('utf8'), '<p>first</p>\r\n');

    // After a possible replacement the same number may name other files,
    // so it waits until the artifact is read again.
    let held;
    const reread = new Promise((resolve) => {
      held = resolve;
    });
    await tab.route(
      (url) => url.pathname === `${source}/revisions`,
      async (route) => {
        await reread;
        await route.fallback();
      },
    );
    await forgeReset(tab);
    await expect(
      workspace.getByRole('button', { name: /^Download revision/ }),
    ).toBeDisabled();
    held();
    await tab.unrouteAll({ behavior: 'ignoreErrors' });
    await expect(
      workspace.getByRole('button', { name: 'Download revision 2' }),
    ).toBeEnabled();

    assert.deepEqual(errors, [], 'Page errors.');
    return { downloads: 1, failed: 1 };
  } finally {
    await context.close();
  }
}
