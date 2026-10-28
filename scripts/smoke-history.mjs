import assert from 'node:assert/strict';
import { expect } from '@playwright/test';
import { forgeReset } from './smoke-live.mjs';
import { api } from './smoke-workspace.mjs';

/**
 * Browser checks for the history tab (decisions 0010 and 0011) against a
 * real artifree-server: files compared by digest, the lines of text files
 * with visible line endings, restores, a retry after a lost answer, a
 * conflict whose latest state arrives late, a forged reset, and the draft
 * lock. Other clients' writes come from the API.
 */
export async function checkHistory({
  browser,
  origin,
  server,
  checkAccessibility,
}) {
  const { artifact } = await api(server, 'POST', '/v1/artifacts', {
    title: 'Revision history',
    files: [
      utf8('index.html', page('Opening', 'Old middle', ...TAIL)),
      utf8('style.css', 'p { margin: 0; }\n'),
      utf8('endings.txt', 'one\r\ntwo\n'),
      bytes('mark.png', [0x89, 0x50, 0x4e, 0x47, 1, 2]),
    ],
  });
  const path = `/v1/artifacts/${artifact.id}`;
  const save = (base, changes) =>
    api(server, 'POST', `${path}/revisions`, { base_revision: base, changes });
  await save(1, [
    {
      op: 'put',
      ...utf8('index.html', page('Opening', 'New middle', ...TAIL)),
    },
    { op: 'put', ...utf8('endings.txt', '\uFEFFone\ntwo') },
    { op: 'put', ...bytes('mark.png', [0x89, 0x50, 0x4e, 0x47, 1, 3]) },
    { op: 'put', ...utf8(LONG_PATH, 'added\n') },
    { op: 'delete', path: 'style.css' },
  ]);
  const digests = async (revision) =>
    (await api(server, 'GET', `${path}/revisions/${revision}`)).files.map(
      (file) => [file.path, file.sha256],
    );
  const latest = async () => (await api(server, 'GET', path)).current_revision;

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
    await workspace.getByLabel('Search titles').fill('Revision history');
    await workspace
      .getByRole('navigation', { name: 'Artifacts' })
      .getByRole('button', { name: /^Revision history/ })
      .click();
    const shows = (revision) =>
      expect(
        workspace.getByText(`Revision ${revision}`, { exact: true }),
      ).toBeVisible();
    await shows(2);
    await workspace.getByRole('tab', { name: 'History' }).click();

    // Revision 1 against the latest: every file compared by digest, the
    // first changed one line by line.
    await expect(
      workspace.getByText(
        '5 files differ from revision 1 to revision 2; 0 files unchanged.',
      ),
    ).toBeVisible();
    const files = workspace.getByRole('navigation', { name: 'Changed files' });
    await expect(files.getByRole('button')).toHaveText([
      'Changedendings.txt',
      'Changedindex.html',
      'Changedmark.png',
      `Added${LONG_PATH}`,
      'Removedstyle.css',
    ]);
    const lines = (name) =>
      workspace.getByRole('table', { name: `Changes to ${name}` });
    // Line endings and the byte order mark are visible.
    await expect(
      lines('endings.txt').locator('[data-kind="removed"] td:last-child'),
    ).toHaveText(['one␍', 'two']);
    await expect(
      lines('endings.txt').locator('[data-kind="added"] td:last-child'),
    ).toHaveText(['⟨BOM⟩one', 'two⟨no line feed⟩']);
    await files.getByRole('button', { name: /index\.html/ }).click();
    const index = lines('index.html');
    await expect(
      index.locator('[data-kind="removed"] td:last-child'),
    ).toHaveText(['<p>Old middle</p>']);
    await expect(index.locator('[data-kind="added"] td:last-child')).toHaveText(
      ['<p>New middle</p>'],
    );
    await expect(
      index.locator('[data-kind="same"]').filter({ hasText: 'Opening' }),
    ).toHaveCount(1);
    // Lines left out after the last change are marked, like those between
    // changes; the file's first line is shown, so nothing precedes it.
    await expect(index.locator('tbody tr')).toHaveText([
      /doctype/,
      /title/,
      /Opening/,
      /Old middle/,
      /New middle/,
      /Tail one/,
      /Tail two/,
      /Tail three/,
      '⋯',
    ]);
    // The longest path fits a narrow window and is shown whole on choice.
    const size = tab.viewportSize();
    await tab.setViewportSize({ width: 360, height: 640 });
    await files.getByRole('button', { name: /added\.txt/ }).click();
    await expect(
      workspace.getByRole('heading', { name: LONG_PATH }),
    ).toBeVisible();
    // Every control and the comparison lie within the window: nothing
    // widens the page or is clipped by a scrolling card.
    for (const box of [
      ...(await files.getByRole('button').all()),
      workspace.getByRole('heading', { name: LONG_PATH }),
    ]) {
      const { x, width } = await box.boundingBox();
      assert.ok(
        x >= 0 && x + width <= 360,
        `Wider than the window: ${x}+${width}.`,
      );
    }
    await tab.setViewportSize(size);
    await checkAccessibility('workspace-history', tab);
    await files.getByRole('button', { name: /mark\.png/ }).click();
    await expect(
      workspace.getByText(/only its size is compared: 6 → 6 bytes/),
    ).toBeVisible();

    // A restore commits revision 1's files as revision 3.
    const restoreButton = (revision) =>
      workspace.getByRole('button', { name: `Restore revision ${revision}` });
    const dialogFor = (revision) =>
      tab.getByRole('dialog', { name: `Restore revision ${revision}` });
    const confirm = (dialog) =>
      dialog.getByRole('button', { name: 'Restore', exact: true });
    await restoreButton(1).click();
    let dialog = dialogFor(1);
    await expect(dialog).toBeVisible();
    await checkAccessibility('workspace-restore-dialog', tab);
    await confirm(dialog).click();
    await expect(dialog).toBeHidden();
    await shows(3);
    assert.deepEqual(await digests(3), await digests(1));

    // A restore whose answer was lost is sent again with its key and
    // applies once.
    let lost = false;
    await tab.route('**/restore', async (route) => {
      if (lost) return route.fallback();
      lost = true;
      await route.fetch();
      return route.fulfill({
        status: 503,
        json: { error: { code: 'unavailable', message: 'Answer lost.' } },
      });
    });
    await workspace.locator('#history-revision').selectOption('2');
    await restoreButton(2).click();
    dialog = dialogFor(2);
    await confirm(dialog).click();
    await expect(dialog.getByText('Outcome unknown')).toBeVisible();
    await confirm(dialog).click();
    await expect(dialog).toBeHidden();
    await shows(4);
    assert.equal(await latest(), 4, 'The retry applied once.');
    assert.deepEqual(await digests(4), await digests(2));
    await tab.unrouteAll({ behavior: 'ignoreErrors' });

    // Another client saves while the dialog is open and before the page
    // has read it. The restore is refused; the next attempt waits until the
    // page has read the save, then restores against it. The dialog keeps
    // the revision it opened on, though the default moves with the latest.
    let release;
    const arrived = new Promise((resolve) => {
      release = resolve;
    });
    await tab.route(`**${path}`, async (route) => {
      if (route.request().method() === 'GET') await arrived;
      return route.fallback();
    });
    // Opened again, the tab offers the revision before the latest, 3.
    await workspace.getByRole('tab', { name: 'Preview' }).click();
    await workspace.getByRole('tab', { name: 'History' }).click();
    await restoreButton(3).click();
    dialog = dialogFor(3);
    await save(4, [{ op: 'put', ...utf8('notes.txt', 'elsewhere\n') }]);
    await confirm(dialog).click();
    await expect(dialog.getByText('Changed elsewhere')).toBeVisible();
    await expect(
      dialog.getByText(/you can restore again once it has/),
    ).toBeVisible();
    await expect(confirm(dialog)).toBeDisabled();
    assert.equal(await latest(), 5, 'Nothing was restored.');
    release();
    await tab.unrouteAll({ behavior: 'ignoreErrors' });
    await expect(confirm(dialog)).toBeEnabled();
    await expect(workspace.locator('#history-revision')).toHaveValue('4');
    await expect(dialog).toHaveAccessibleName('Restore revision 3');
    await confirm(dialog).click();
    await expect(dialog).toBeHidden();
    await shows(6);
    assert.deepEqual(await digests(6), await digests(3));

    // A rename refused the same way: opened again before the page has read
    // the other client's title, the dialog says why it waits, and then
    // offers that title, never the refused one.
    let renamed;
    const learned = new Promise((resolve) => {
      renamed = resolve;
    });
    await tab.route(`**${path}`, async (route) => {
      if (route.request().method() === 'GET') await learned;
      return route.fallback();
    });
    const renameButton = workspace.getByRole('button', {
      name: 'Rename',
      exact: true,
    });
    await renameButton.click();
    const rename = tab.getByRole('dialog', { name: 'Rename artifact' });
    const send = rename.getByRole('button', { name: 'Rename', exact: true });
    const title = rename.getByLabel('Title');
    await title.fill('Mine');
    await api(server, 'PATCH', path, {
      base_metadata_revision: 1,
      title: 'Theirs',
    });
    await send.click();
    await expect(rename.getByText('Changed elsewhere')).toBeVisible();
    await rename.getByRole('button', { name: 'Cancel' }).click();
    await renameButton.click();
    await expect(
      rename.getByText(/you can rename again once it has/),
    ).toBeVisible();
    await expect(send).toBeDisabled();
    renamed();
    await tab.unrouteAll({ behavior: 'ignoreErrors' });
    await expect(title).toHaveValue('Theirs');
    await expect(send).toBeDisabled();
    await rename.getByRole('button', { name: 'Cancel' }).click();

    // While what is shown predates a reset, nothing is prepared from it.
    await workspace.locator('#history-revision').selectOption('5');
    let reread;
    const held = new Promise((resolve) => {
      reread = resolve;
    });
    await tab.route(
      `**${path}`,
      async (route) => {
        await held;
        return route.fallback();
      },
      { times: 1 },
    );
    await forgeReset(tab);
    const writes = [
      workspace.getByRole('button', { name: 'Pin', exact: true }),
      workspace.getByRole('button', { name: 'Rename', exact: true }),
      restoreButton(5),
    ];
    for (const control of writes) await expect(control).toBeDisabled();
    reread();
    for (const control of writes) await expect(control).toBeEnabled();

    // A dialog open across a reset sends nothing; opened again, it is
    // prepared from what the page reads now.
    await restoreButton(5).click();
    dialog = dialogFor(5);
    await forgeReset(tab);
    await expect(
      dialog.getByText("The server's data may have been replaced"),
    ).toBeVisible();
    await expect(confirm(dialog)).toBeDisabled();
    await dialog.getByRole('button', { name: 'Cancel' }).click();
    await workspace
      .getByRole('button', { name: 'Rename', exact: true })
      .click();
    await title.fill('Revision history renamed');
    await forgeReset(tab);
    await expect(
      rename.getByText("The server's data may have been replaced"),
    ).toBeVisible();
    await expect(
      rename.getByRole('button', { name: 'Rename', exact: true }),
    ).toBeDisabled();
    await rename.getByRole('button', { name: 'Cancel' }).click();
    await restoreButton(5).click();
    await expect(
      dialog.getByText("The server's data may have been replaced"),
    ).toBeHidden();
    await confirm(dialog).click();
    await expect(dialog).toBeHidden();
    await shows(7);
    assert.deepEqual(await digests(7), await digests(5));

    // A draft locks restoring: a restore would leave it stale. Opened
    // again, the tab offers the revision before the latest.
    await workspace.getByRole('tab', { name: 'Source' }).click();
    await workspace
      .getByLabel('index.html', { exact: true })
      .fill('<p>draft</p>');
    await workspace.getByRole('tab', { name: 'History' }).click();
    await expect(restoreButton(6)).toBeDisabled();
    await expect(
      workspace.getByText(
        'Save or discard your changes to restore a revision.',
      ),
    ).toBeVisible();

    assert.deepEqual(errors, [], 'Page errors.');
    return { revisions: 7 };
  } finally {
    await context.close();
  }
}

// Unchanged paragraphs after the change: more than three lines of context.
const TAIL = ['Tail one', 'Tail two', 'Tail three', 'Tail four', 'Tail five'];
// A valid path near the 240-byte limit.
const LONG_PATH = `notes/${'a-folder-name-long-enough-to-wrap-badly/'.repeat(5)}added.txt`;

function page(...paragraphs) {
  return `<!doctype html>\n<title>History</title>\n${paragraphs
    .map((text) => `<p>${text}</p>`)
    .join('\n')}\n<p>Closing</p>\n`;
}

function utf8(path, content) {
  return { path, encoding: 'utf8', content };
}

function bytes(path, values) {
  return {
    path,
    encoding: 'base64',
    content: Buffer.from(values).toString('base64'),
  };
}
