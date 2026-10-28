import assert from 'node:assert/strict';
import { expect } from '@playwright/test';
import { forgeReset } from './smoke-history.mjs';
import { api } from './smoke-workspace.mjs';

/**
 * Browser checks for duplicates (decision 0012) against a real
 * artifree-server: a copy of the revision the preview shows, a retry after
 * a lost answer that creates one copy, the source left as it was, and the
 * draft lock. The page has its own browser context.
 */
export async function checkDuplicate({
  browser,
  origin,
  server,
  checkAccessibility,
}) {
  const { artifact } = await api(server, 'POST', '/v1/artifacts', {
    title: 'Duplicate demo',
    files: [
      { path: 'index.html', encoding: 'utf8', content: '<p>first</p>\r\n' },
      {
        path: 'logo.png',
        encoding: 'base64',
        content: Buffer.from([0x89, 0x50, 0x4e, 0x47, 0, 0xff]).toString(
          'base64',
        ),
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
  await api(server, 'PATCH', source, {
    base_metadata_revision: 1,
    pinned: true,
  });
  const manifest = async (path) =>
    (await api(server, 'GET', path)).files.map(({ path, sha256 }) => [
      path,
      sha256,
    ]);

  const context = await browser.newContext();
  try {
    const tab = await context.newPage();
    tab.setDefaultTimeout(10_000);
    const errors = [];
    tab.on('pageerror', (error) => errors.push(error.message));
    await tab.goto(origin);
    const workspace = tab.locator('#artifacts');
    await workspace.getByLabel('Access token').fill(server.token);
    await workspace.getByRole('button', { name: 'Connect' }).click();
    await workspace.getByLabel('Search titles').fill('Duplicate demo');
    const nav = workspace.getByRole('navigation', { name: 'Artifacts' });
    await nav.getByRole('button', { name: /^Duplicate demo/ }).click();
    await expect(
      workspace.getByRole('heading', { name: 'Duplicate demo', exact: true }),
    ).toBeVisible();

    // The dialog offers the revision the preview shows.
    await workspace.locator('#preview-revision').selectOption('1');
    await workspace
      .getByRole('button', { name: 'Duplicate', exact: true })
      .click();
    const dialog = tab.getByRole('dialog', {
      name: 'Duplicate Duplicate demo',
    });
    await expect(dialog.getByLabel('Revision')).toHaveValue('1');
    await checkAccessibility('workspace-duplicate-dialog', tab);

    // A dialog opened before the server's data may have been replaced
    // sends nothing; opened again, it offers what the page reads now. The
    // preview's choice belonged to the earlier read, so the latest is
    // offered (0011 decision 8).
    await forgeReset(tab);
    await expect(
      dialog.getByText("The server's data may have been replaced"),
    ).toBeVisible();
    await expect(
      dialog.getByRole('button', { name: 'Duplicate', exact: true }),
    ).toBeDisabled();
    await dialog.getByRole('button', { name: 'Cancel' }).click();
    await workspace
      .getByRole('button', { name: 'Duplicate', exact: true })
      .click();
    await expect(dialog.getByLabel('Revision')).toHaveValue('2');
    await dialog.getByLabel('Revision').selectOption('1');

    // The first answer is lost; sending again creates no second copy.
    let lost = false;
    await tab.route('**/duplicate', async (route) => {
      if (lost) return route.fallback();
      lost = true;
      await route.fetch();
      return route.fulfill({
        status: 503,
        json: { error: { code: 'unavailable', message: 'Response lost.' } },
      });
    });
    const send = dialog.getByRole('button', { name: 'Duplicate', exact: true });
    await send.click();
    await expect(dialog.getByText('Outcome unknown')).toBeVisible();
    await send.click();
    await expect(dialog).toBeHidden();
    await tab.unrouteAll({ behavior: 'ignoreErrors' });

    // The copy opens: revision 1 with the files of the source's revision 1.
    await expect(
      workspace.getByRole('heading', { name: 'Duplicate demo (Copy)' }),
    ).toBeVisible();
    await expect(
      workspace.getByText('Revision 1', { exact: true }),
    ).toBeVisible();
    const copies = await api(
      server,
      'GET',
      `/v1/artifacts?q=${encodeURIComponent('Duplicate demo (Copy)')}`,
    );
    assert.equal(copies.artifacts.length, 1, 'The retry created one copy.');
    const [copy] = copies.artifacts;
    assert.equal(copy.pinned, false);
    assert.equal(copy.current_revision, 1);
    assert.deepEqual(
      await manifest(`/v1/artifacts/${copy.id}/revisions/1`),
      await manifest(`${source}/revisions/1`),
    );
    const left = await api(server, 'GET', source);
    assert.deepEqual(
      [left.current_revision, left.pinned, left.title],
      [2, true, 'Duplicate demo'],
      'The source stays as it was.',
    );

    // A draft locks duplicating, since the copy would open in its place.
    await workspace.getByRole('tab', { name: 'Source' }).click();
    await workspace
      .getByLabel('index.html', { exact: true })
      .fill('<p>draft</p>');
    await expect(
      workspace.getByRole('button', { name: 'Duplicate', exact: true }),
    ).toBeDisabled();
    await expect(
      workspace.getByText(/Save or discard your changes to duplicate/),
    ).toBeVisible();

    assert.deepEqual(errors, [], 'Page errors.');
    return { copies: 1 };
  } finally {
    await context.close();
  }
}
