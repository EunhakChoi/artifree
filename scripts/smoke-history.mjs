import assert from 'node:assert/strict';
import { expect } from '@playwright/test';
import { api } from './smoke-workspace.mjs';

/**
 * Browser checks for comparing and restoring revisions (decisions 0010 and
 * 0011) against a real artifree-server: a comparison by digest, a text
 * diff, a restore, a retry after a lost answer, a conflict, and the draft
 * lock. The page has its own browser context; other clients' saves come
 * from the API.
 */
export async function checkHistory({
  browser,
  origin,
  server,
  checkAccessibility,
}) {
  const { artifact } = await api(server, 'POST', '/v1/artifacts', {
    title: 'History demo',
    files: [
      text('index.html', page('first line', 'old middle')),
      text('style.css', 'p { color: green; }\n'),
      binary('logo.png', [0x89, 0x50, 0x4e, 0x47, 0, 1]),
    ],
  });
  const files = `/v1/artifacts/${artifact.id}/revisions`;
  const save = (base, changes) =>
    api(server, 'POST', files, { base_revision: base, changes });
  await save(1, [
    { op: 'put', ...text('index.html', page('first line', 'new middle')) },
    { op: 'put', ...text('notes.txt', 'added\n') },
    { op: 'put', ...binary('logo.png', [0x89, 0x50, 0x4e, 0x47, 0, 2]) },
    { op: 'delete', path: 'style.css' },
  ]);
  const manifest = async (revision) =>
    (await api(server, 'GET', `${files}/${revision}`)).files.map(
      ({ path, sha256 }) => [path, sha256],
    );

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
    await workspace.getByLabel('Search titles').fill('History demo');
    const nav = workspace.getByRole('navigation', { name: 'Artifacts' });
    await nav.getByRole('button', { name: /^History demo/ }).click();
    await expect(
      workspace.getByRole('heading', { name: 'History demo' }),
    ).toBeVisible();
    const current = (revision) =>
      expect(
        workspace.getByText(`Revision ${revision}`, { exact: true }),
      ).toBeVisible();
    await workspace.getByRole('tab', { name: 'History' }).click();

    // Revision 1 against the latest: files compared by digest, one text
    // file shown line by line.
    await expect(
      workspace.getByText(
        '4 files differ from revision 1 to revision 2; 0 files unchanged.',
      ),
    ).toBeVisible();
    const changed = workspace.getByRole('navigation', {
      name: 'Changed files',
    });
    await expect(changed.getByRole('button')).toHaveText([
      'Changed index.html',
      'Changed logo.png',
      'Added notes.txt',
      'Removed style.css',
    ]);
    const lines = workspace.getByRole('region', {
      name: 'Changes to index.html',
    });
    await expect(lines.locator('[data-kind="removed"]')).toHaveText([
      /old middle/,
    ]);
    await expect(lines.locator('[data-kind="added"]')).toHaveText([
      /new middle/,
    ]);
    await expect(
      lines.locator('[data-kind="same"]').filter({ hasText: 'first line' }),
    ).toHaveCount(1);
    await checkAccessibility('workspace-history', tab);
    await changed.getByRole('button', { name: /logo\.png/ }).click();
    await expect(
      workspace.getByText(/only its size is compared: 6 → 6 bytes/),
    ).toBeVisible();

    // Restoring revision 1 commits its files as revision 3.
    await workspace.getByRole('button', { name: 'Restore revision 1' }).click();
    let dialog = tab.getByRole('dialog', { name: 'Restore revision 1' });
    await expect(dialog).toBeVisible();
    await checkAccessibility('workspace-restore-dialog', tab);
    await dialog.getByRole('button', { name: 'Restore', exact: true }).click();
    await expect(dialog).toBeHidden();
    await current(3);
    assert.deepEqual(await manifest(3), await manifest(1));

    // A restore whose answer was lost is sent again with its key: it
    // applies once.
    let lost = false;
    await tab.route('**/restore', async (route) => {
      if (lost) return route.fallback();
      lost = true;
      await route.fetch();
      return route.fulfill({
        status: 503,
        json: { error: { code: 'unavailable', message: 'Response lost.' } },
      });
    });
    await workspace.locator('#history-revision').selectOption('2');
    await workspace.getByRole('button', { name: 'Restore revision 2' }).click();
    dialog = tab.getByRole('dialog', { name: 'Restore revision 2' });
    await dialog.getByRole('button', { name: 'Restore', exact: true }).click();
    await expect(dialog.getByText('Outcome unknown')).toBeVisible();
    await dialog.getByRole('button', { name: 'Restore', exact: true }).click();
    await expect(dialog).toBeHidden();
    await current(4);
    let read = await api(server, 'GET', `/v1/artifacts/${artifact.id}`);
    assert.equal(read.current_revision, 4, 'The retry applied once.');
    assert.deepEqual(await manifest(4), await manifest(2));
    await tab.unrouteAll({ behavior: 'ignoreErrors' });

    // A save elsewhere while the dialog is open: the dialog keeps the
    // revision it opened on, though the default choice moves with the
    // latest; nothing is restored, and the next attempt is prepared against
    // the latest revision. Opening the tab again chooses revision 3.
    await workspace.getByRole('tab', { name: 'Preview' }).click();
    await workspace.getByRole('tab', { name: 'History' }).click();
    await workspace.getByRole('button', { name: 'Restore revision 3' }).click();
    dialog = tab.getByRole('dialog', { name: 'Restore revision 3' });
    await expect(dialog).toBeVisible();
    await save(4, [{ op: 'put', ...text('notes.txt', 'elsewhere\n') }]);
    await current(5);
    await expect(dialog).toBeVisible();
    await dialog.getByRole('button', { name: 'Restore', exact: true }).click();
    await expect(dialog.getByText('Changed elsewhere')).toBeVisible();
    read = await api(server, 'GET', `/v1/artifacts/${artifact.id}`);
    assert.equal(read.current_revision, 5, 'Nothing was restored.');
    await dialog.getByRole('button', { name: 'Restore', exact: true }).click();
    await expect(dialog).toBeHidden();
    await current(6);
    assert.deepEqual(await manifest(6), await manifest(3));

    // A draft locks restoring, since a restore would leave it stale. The
    // tab opens again on the revision before the latest.
    await workspace.getByRole('tab', { name: 'Source' }).click();
    await workspace
      .getByLabel('index.html', { exact: true })
      .fill('<p>draft</p>');
    await workspace.getByRole('tab', { name: 'History' }).click();
    await expect(
      workspace.getByRole('button', { name: 'Restore revision 5' }),
    ).toBeDisabled();
    await expect(
      workspace.getByText(
        'Save or discard your changes to restore a revision.',
      ),
    ).toBeVisible();

    assert.deepEqual(errors, [], 'Page errors.');
    return { revisions: 6 };
  } finally {
    await context.close();
  }
}

function page(...lines) {
  return `<!doctype html>\n<title>History</title>\n${lines.map((line) => `<p>${line}</p>`).join('\n')}\n<p>last line</p>\n`;
}

function text(path, content) {
  return { path, encoding: 'utf8', content };
}

function binary(path, bytes) {
  return {
    path,
    encoding: 'base64',
    content: Buffer.from(bytes).toString('base64'),
  };
}
