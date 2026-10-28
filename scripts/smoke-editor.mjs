import assert from 'node:assert/strict';
import { expect } from '@playwright/test';
import { forgeReset } from './smoke-history.mjs';
import {
  api,
  bytes,
  fileButton,
  hold,
  loseResponse,
} from './smoke-workspace.mjs';

/**
 * Browser checks for the code workspace (decision 0017) against a real
 * artifree-server, in their own browser context:
 * - a draft spans files and is saved as one revision;
 * - a rename carries the moved bytes;
 * - paths the server would refuse are refused before anything is sent;
 * - a save that may have committed stays so until it is settled;
 * - a revision saved elsewhere leaves the draft;
 * - after a possible replacement of the server's data, nothing is read for
 *   the draft or sent.
 */
export async function checkEditor({
  browser,
  origin,
  server,
  checkAccessibility,
}) {
  const logo = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0, 0xff, 0x0d, 0x0a]);
  const { artifact } = await api(server, 'POST', '/v1/artifacts', {
    title: 'Editor demo',
    files: [
      { path: 'index.html', encoding: 'utf8', content: '<p>one</p>\r\n' },
      { path: 'style.css', encoding: 'utf8', content: 'p { color: red }\n' },
      {
        path: 'img/logo.png',
        encoding: 'base64',
        content: logo.toString('base64'),
      },
    ],
  });
  const source = `/v1/artifacts/${artifact.id}`;
  const history = async () =>
    (await api(server, 'GET', `${source}/revisions`)).revisions.map(
      ({ revision }) => revision,
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
    await workspace.getByLabel('Search titles').fill('Editor demo');
    await workspace
      .getByRole('navigation', { name: 'Artifacts' })
      .getByRole('button', { name: /^Editor demo/ })
      .click();
    await expect(
      workspace.getByRole('heading', { name: 'Editor demo', exact: true }),
    ).toBeVisible();
    await workspace.getByRole('tab', { name: 'Source' }).click();
    const panel = workspace.getByRole('tabpanel', { name: 'Source' });
    const files = panel.getByRole('navigation', { name: 'Files' });
    const open = (path) => fileButton(files, path).click();
    const save = workspace.getByRole('button', { name: 'Save revision' });
    const notSaved = workspace
      .getByRole('alert')
      .filter({ hasText: /^Not saved/ });
    const savedElsewhere = workspace.getByText(/was saved elsewhere/);
    async function renameTo(from, to) {
      await open(from);
      await panel.getByRole('button', { name: 'Rename file' }).click();
      const dialog = tab.getByRole('dialog', { name: `Rename ${from}` });
      await dialog.getByLabel('Path').fill(to);
      await dialog.getByRole('button', { name: 'Rename' }).click();
      return dialog;
    }

    // A draft spans files: switching files keeps every edit.
    const index = workspace.getByLabel('index.html');
    await expect(index).toHaveValue('<p>one</p>\n');
    await index.fill('<p>two</p>\n');
    await open('style.css');
    await workspace.getByLabel('style.css').fill('p { color: blue }\n');
    await open('index.html');
    await expect(index).toHaveValue('<p>two</p>\n');
    await expect(fileButton(files, 'style.css')).toContainText('Edited');
    await expect(
      workspace.getByRole('button', { name: 'Disconnect' }),
    ).toBeDisabled();

    // The dialog refuses a path the server would refuse.
    await open('img/logo.png');
    await expect(
      workspace.getByText('img/logo.png is not a text file.'),
    ).toBeVisible();
    const dialog = await renameTo('img/logo.png', 'assets/logo.exe');
    const pathField = dialog.getByLabel('Path');
    const confirm = dialog.getByRole('button', { name: 'Rename' });
    await expect(
      dialog.getByText('Artifree does not serve files with this extension.'),
    ).toBeVisible();
    await expect(pathField).toHaveAttribute('aria-invalid', 'true');
    await checkAccessibility('editor-invalid-path', tab);
    await pathField.fill('Index.html');
    await expect(
      dialog.getByText('index.html differs from it only by case.'),
    ).toBeVisible();
    // Moving the file reads its bytes; a failed read moves nothing.
    await tab.route(
      `**${source}/revisions/1/files/img/logo.png`,
      (route) =>
        route.fulfill({
          status: 503,
          json: { error: { code: 'unavailable', message: 'Busy.' } },
        }),
      { times: 1 },
    );
    await pathField.fill('assets/logo.png');
    await confirm.click();
    await expect(dialog.getByText('img/logo.png was not moved.')).toBeVisible();
    await confirm.click();
    await expect(dialog).toBeHidden();
    await expect(fileButton(files, 'assets/logo.png')).toContainText('Added');
    await expect(fileButton(files, 'img/logo.png')).toContainText('Deleted');

    // A new file, a deleted one, and a draft the server would refuse.
    await files.getByRole('button', { name: 'New file' }).click();
    const added = tab.getByRole('dialog', { name: 'New file' });
    await added.getByLabel('Path').fill('pages/about.html');
    await added.getByRole('button', { name: 'Add' }).click();
    await workspace.getByLabel('pages/about.html').fill('<p>about</p>');
    await open('style.css');
    await panel.getByRole('button', { name: 'Delete file' }).click();
    await expect(fileButton(files, 'style.css')).toContainText('Deleted');
    await renameTo('index.html', 'home.html');
    await expect(
      workspace.getByText('Every artifact needs index.html'),
    ).toBeVisible();
    await expect(save).toBeDisabled();
    await checkAccessibility('editor-draft-problems', tab);
    await renameTo('home.html', 'index.html');
    await expect(
      workspace.getByText('Every artifact needs index.html'),
    ).toBeHidden();
    await expect(index).toHaveValue('<p>two</p>\n');

    // Saved, every change is one revision, and the moved file keeps its
    // bytes. Nothing is read between the click and the request.
    const reads = [];
    let sent = false;
    const recordRead = (request) => {
      if (request.method() === 'POST' && request.url().endsWith('/revisions'))
        sent = true;
      else if (!sent && request.url().includes('/files/'))
        reads.push(request.url());
    };
    tab.on('request', recordRead);
    await save.click();
    await expect(
      workspace.getByText('Revision 2', { exact: true }),
    ).toBeVisible();
    tab.off('request', recordRead);
    assert.ok(sent, 'The save was sent.');
    assert.deepEqual(reads, [], 'Nothing is read before the save is sent.');
    const second = await api(server, 'GET', `${source}/revisions/2`);
    assert.deepEqual(
      second.files.map(({ path }) => path),
      ['assets/logo.png', 'index.html', 'pages/about.html'],
    );
    assert.deepEqual(
      await bytes(server, `${source}/revisions/2/files/assets/logo.png`),
      logo,
      'A moved file keeps its bytes.',
    );
    assert.deepEqual(
      await bytes(server, `${source}/revisions/2/files/index.html`),
      Buffer.from('<p>two</p>\r\n'),
      'An edit keeps CRLF line breaks.',
    );
    assert.deepEqual(await history(), [2, 1], 'One revision per save.');
    await expect(
      workspace.getByRole('button', { name: 'Disconnect' }),
    ).toBeEnabled();

    // A save whose answer was lost may have committed. It stays so after a
    // later attempt fails for another reason: the revision it may have
    // made is not reported as saved elsewhere, and the same request is
    // sent again with its key and replays.
    await index.fill('<p>three</p>\n');
    const restoreSave = await loseResponse(tab, `**${source}/revisions`);
    await save.click();
    await expect(notSaved).toBeVisible();
    await restoreSave();
    await expect(
      workspace.getByText('Revision 3', { exact: true }),
    ).toBeVisible();
    await expect(savedElsewhere).toBeHidden();
    let refused = false;
    const refuseOnce = (route) => {
      if (refused || route.request().method() !== 'POST')
        return route.fallback();
      refused = true;
      return route.fulfill({
        status: 400,
        json: { error: { code: 'invalid_request', message: 'Refused.' } },
      });
    };
    await tab.route(`**${source}/revisions`, refuseOnce);
    await save.click();
    await expect(notSaved).toContainText('An earlier attempt may have been');
    await expect(savedElsewhere).toBeHidden();
    await tab.unroute(`**${source}/revisions`, refuseOnce);
    await save.click();
    await expect(notSaved).toBeHidden();
    await expect(index).toHaveValue('<p>three</p>\n');
    assert.deepEqual(await history(), [3, 2, 1], 'The retry replayed.');

    // A revision saved elsewhere leaves the draft as it is; saving it is
    // refused, and loading the latest revision replaces it.
    await index.fill('<p>mine</p>\n');
    await api(server, 'POST', `${source}/revisions`, {
      base_revision: 3,
      changes: [
        { op: 'put', path: 'index.html', encoding: 'utf8', content: 'agent' },
      ],
    });
    await expect(
      workspace.getByText('Revision 4 was saved elsewhere'),
    ).toBeVisible();
    await expect(index).toHaveValue('<p>mine</p>\n');
    await save.click();
    await expect(
      workspace.getByText('Not saved: the artifact changed'),
    ).toBeVisible();
    await expect(index).toHaveValue('<p>mine</p>\n');
    await workspace.getByRole('button', { name: 'Load latest' }).click();
    await expect(index).toHaveValue('agent');

    // Text moved onto a deleted file's path is edited there at once, while
    // the replaced file's read is held.
    const releaseAbout = await hold(
      tab,
      'GET',
      `**${source}/revisions/4/files/pages/about.html`,
    );
    await open('pages/about.html');
    await panel.getByRole('button', { name: 'Delete file' }).click();
    await files.getByRole('button', { name: 'New file' }).click();
    const fresh = tab.getByRole('dialog', { name: 'New file' });
    await fresh.getByLabel('Path').fill('draft.html');
    await fresh.getByRole('button', { name: 'Add' }).click();
    await workspace.getByLabel('draft.html').fill('<p>moved</p>');
    await renameTo('draft.html', 'pages/about.html');
    const about = workspace.getByLabel('pages/about.html');
    await expect(about).toHaveValue('<p>moved</p>');
    await about.fill('<p>moved and edited</p>');
    await expect(about).toHaveValue('<p>moved and edited</p>');
    await releaseAbout();
    await workspace.getByRole('button', { name: 'Discard changes' }).click();
    await expect(fileButton(files, 'draft.html')).toBeHidden();

    // The saved revision can be previewed beside the editor.
    await workspace.getByRole('button', { name: 'Show saved preview' }).click();
    await expect(
      workspace.locator('iframe[title="Saved revision 4 of Editor demo"]'),
    ).toBeVisible();
    await checkAccessibility('editor-saved-preview', tab);
    await workspace.getByRole('button', { name: 'Hide saved preview' }).click();

    // After a possible replacement of the server's data, revision 4 may
    // name other content: the draft stays, but nothing of revision 4 is
    // read, moved, or saved.
    await open('index.html');
    await index.fill('<p>kept</p>');
    const later = [];
    const recordLater = (request) => {
      const url = new URL(request.url());
      if (
        url.pathname.startsWith(`${source}/revisions/4/files/`) ||
        (request.method() === 'POST' && url.pathname.endsWith('/revisions'))
      )
        later.push(`${request.method()} ${url.pathname}`);
    };
    tab.on('request', recordLater);
    await forgeReset(tab);
    await expect(
      workspace.getByText("The server's data may have been replaced"),
    ).toBeVisible();
    await expect(index).toHaveValue('<p>kept</p>');
    await expect(save).toBeDisabled();
    await open('pages/about.html');
    await expect(
      workspace.getByText('pages/about.html is not read here now.'),
    ).toBeVisible();
    const late = await renameTo('assets/logo.png', 'logo.png');
    await expect(
      late.getByText(/assets\/logo\.png was not moved/),
    ).toBeVisible();
    await late.getByRole('button', { name: 'Cancel' }).click();
    tab.off('request', recordLater);
    assert.deepEqual(later, [], 'Nothing is read or sent for revision 4.');
    assert.deepEqual(await history(), [4, 3, 2, 1]);

    assert.deepEqual(errors, [], 'Page errors.');
    return { revisions: 4, moved: 'bytes kept', replayed: 1 };
  } finally {
    await context.close();
  }
}
