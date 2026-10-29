import assert from 'node:assert/strict';
import { expect } from '@playwright/test';
import { forgeReset } from './smoke-history.mjs';
import { api, bytes, fileButton, loseResponse } from './smoke-workspace.mjs';

const PNG = [137, 80, 78, 71, 13, 10, 26, 10, 0, 255, 254, 1];

/**
 * Browser checks for editing an artifact's files (decision 0017) against a
 * real artifree-server, in the page's own browser context. Each step makes
 * one moment, such as a read held while the artifact moves to the trash,
 * and checks what the draft does then: every change passes one write
 * point, which decides with the state the change lands in.
 */
export async function checkEditor({
  browser,
  origin,
  server,
  checkAccessibility,
}) {
  const { artifact } = await api(server, 'POST', '/v1/artifacts', {
    title: 'Files',
    files: [
      {
        path: 'index.html',
        encoding: 'utf8',
        content: '<p>home</p>\r\n<p>end</p>\r\n',
      },
      { path: 'pages/about.html', encoding: 'utf8', content: '<p>about</p>' },
      { path: 'style.css', encoding: 'utf8', content: 'p {}' },
      {
        path: 'assets/logo.png',
        encoding: 'base64',
        content: Buffer.from(PNG).toString('base64'),
      },
    ],
  });
  const id = artifact.id;
  const filePath = (revision, path) =>
    `/v1/artifacts/${id}/revisions/${revision}/files/${path}`;
  // The metadata revision, in the trash or not.
  const metadata = async () => {
    const { artifacts } = await api(
      server,
      'GET',
      '/v1/artifacts?deleted=true&q=Files',
    );
    return (
      artifacts.find((item) => item.id === id) ??
      (await api(server, 'GET', `/v1/artifacts/${id}`))
    ).metadata_revision;
  };
  const setDeleted = async (deleted) =>
    api(server, 'PATCH', `/v1/artifacts/${id}`, {
      base_metadata_revision: await metadata(),
      deleted,
    });

  const context = await browser.newContext();
  try {
    const tab = await context.newPage();
    tab.setDefaultTimeout(10_000);
    const errors = [];
    tab.on('pageerror', (error) => errors.push(error.message));
    // Every request for a file of a revision, in order, with the saves.
    const requests = [];
    tab.on('request', (request) => {
      const { pathname } = new URL(request.url());
      if (
        pathname.includes('/files/') ||
        (request.method() === 'POST' && pathname.endsWith('/revisions'))
      )
        requests.push(`${request.method()} ${pathname}`);
    });
    await tab.goto(origin);
    const workspace = tab.locator('#artifacts');
    await workspace.getByLabel('Access token').fill(server.token);
    await workspace.getByRole('button', { name: 'Connect' }).click();
    await workspace
      .getByRole('navigation', { name: 'Artifacts' })
      .getByRole('button', { name: /^Files / })
      .click();
    await workspace.getByRole('tab', { name: 'Source' }).click();
    const file = (path) => fileButton(workspace, path);
    const text = (path) => workspace.getByLabel(path, { exact: true });
    const saveButton = workspace.getByRole('button', { name: 'Save revision' });
    const newFile = workspace.getByRole('button', { name: 'New file' });
    const renameButton = workspace.getByRole('button', { name: 'Rename file' });
    const deleteButton = workspace.getByRole('button', { name: 'Delete file' });
    const disconnect = workspace.getByRole('button', { name: 'Disconnect' });
    const dialog = tab.getByRole('dialog');
    const pathInput = dialog.getByLabel('Path');
    const rename = async (from, to) => {
      await file(from).click();
      await renameButton.click();
      await pathInput.fill(to);
      await dialog.getByRole('button', { name: 'Rename', exact: true }).click();
    };
    /** Holds the answer to the next request `match` accepts until released. */
    const holdAnswer = async (method, match) => {
      let release;
      const released = new Promise((resolve) => {
        release = resolve;
      });
      let asked;
      const seen = new Promise((resolve) => {
        asked = resolve;
      });
      let taken = false;
      const matches = (url) => match(url.pathname);
      const handler = async (route) => {
        if (taken || route.request().method() !== method)
          return route.fallback();
        taken = true;
        asked();
        // The server answers now; the page sees the answer only later.
        const response = await route.fetch();
        await released;
        await route.fulfill({ response }).catch(() => {});
        return undefined;
      };
      // Kept until the context closes: once taken, it lets requests pass.
      await tab.route(matches, handler);
      let timer;
      const late = new Promise((_, reject) => {
        timer = setTimeout(
          () => reject(new Error(`No ${method} request was held within 10 s.`)),
          10_000,
        );
      });
      return {
        seen: Promise.race([seen, late]).finally(() => clearTimeout(timer)),
        release,
      };
    };

    // A draft spans files: each keeps its text while others are opened.
    // Choosing another artifact and disconnecting stay locked (0007).
    await expect(text('index.html')).toHaveValue('<p>home</p>\n<p>end</p>\n');
    await text('index.html').fill('<p>home 2</p>\n<p>end</p>\n');
    await file('style.css').click();
    await text('style.css').fill('p { color: red }');
    await file('index.html').click();
    await expect(text('index.html')).toHaveValue('<p>home 2</p>\n<p>end</p>\n');
    await expect(file('style.css')).toContainText('edited');
    await expect(disconnect).toBeDisabled();

    // The dialog refuses what the server would refuse, and case-only twins.
    await newFile.click();
    await pathInput.fill('notes.ts');
    await expect(
      dialog.getByText('Artifree does not serve files with this extension.'),
    ).toBeVisible();
    await pathInput.fill('Style.CSS');
    await expect(
      dialog.getByText('Style.CSS differs from style.css only by case.'),
    ).toBeVisible();
    await expect(
      dialog.getByRole('button', { name: 'Add file' }),
    ).toBeDisabled();
    await pathInput.fill('notes.txt');
    await dialog.getByRole('button', { name: 'Add file' }).click();
    await expect(dialog).toBeHidden();
    await text('notes.txt').fill('new notes');
    await checkAccessibility('editor-draft', tab);

    // A failed read of the file to move moves nothing, and says why.
    await tab.route(
      (url) => url.pathname === filePath(1, 'assets/logo.png'),
      (route) =>
        route.fulfill({
          status: 503,
          json: { error: { code: 'unavailable', message: 'Storage is busy.' } },
        }),
      { times: 1 },
    );
    await rename('assets/logo.png', 'img/logo.png');
    await expect(
      dialog.getByText('assets/logo.png was not moved. Storage is busy.'),
    ).toBeVisible();
    await dialog.getByRole('button', { name: 'Cancel' }).click();
    await expect(file('img/logo.png')).toHaveCount(0);
    await expect(file('assets/logo.png')).not.toContainText(
      /edited|added|deleted/u,
    );

    // A binary file moves with its bytes; index.html moved away disables
    // saving until it is back; a deleted file can be restored.
    await rename('assets/logo.png', 'img/logo.png');
    await expect(dialog).toBeHidden();
    await expect(
      workspace.getByText('img/logo.png is not shown here'),
    ).toBeVisible();
    await rename('index.html', 'home.html');
    await expect(
      workspace.getByText('A revision needs index.html.'),
    ).toBeVisible();
    await expect(saveButton).toBeDisabled();
    await rename('home.html', 'index.html');
    await file('pages/about.html').click();
    await deleteButton.click();
    await workspace
      .getByRole('button', { name: 'Restore pages/about.html' })
      .click();
    await expect(text('pages/about.html')).toHaveValue('<p>about</p>');

    // One save sends every change as one revision, with nothing read
    // between the click and the request.
    requests.length = 0;
    await saveButton.click();
    await expect(
      workspace.getByText('Revision 2', { exact: true }),
    ).toBeVisible();
    assert.deepEqual(
      requests.slice(
        0,
        requests.findIndex((request) => request.startsWith('POST')) + 1,
      ),
      [`POST /v1/artifacts/${id}/revisions`],
      'Nothing is read between the click and the request.',
    );
    const two = await api(server, 'GET', `/v1/artifacts/${id}/revisions`);
    assert.deepEqual(
      two.revisions
        .find((item) => item.revision === 2)
        .files.map((item) => item.path),
      [
        'img/logo.png',
        'index.html',
        'notes.txt',
        'pages/about.html',
        'style.css',
      ],
    );
    assert.deepEqual(
      await bytes(server, filePath(2, 'img/logo.png')),
      Buffer.from(PNG),
    );
    assert.deepEqual(
      await bytes(server, filePath(2, 'index.html')),
      Buffer.from('<p>home 2</p>\r\n<p>end</p>\r\n'),
      'An edit keeps CRLF line breaks.',
    );

    // A save whose answer was lost may have committed. A refused retry
    // does not settle that, so no "saved elsewhere" notice shows for the
    // revision it may have made, and the next retry replays it.
    await file('style.css').click();
    await text('style.css').fill('p { color: blue }');
    const restoreLost = await loseResponse(tab, '**/v1/artifacts/*/revisions');
    await saveButton.click();
    await expect(
      workspace.getByText(/The save may have been committed/u),
    ).toBeVisible();
    await restoreLost();
    await tab.route(
      '**/v1/artifacts/*/revisions',
      (route) =>
        route.request().method() === 'POST'
          ? route.fulfill({
              status: 429,
              headers: { 'retry-after': '0' },
              json: { error: { code: 'rate_limited', message: 'Slow down.' } },
            })
          : route.fallback(),
      { times: 1 },
    );
    await saveButton.click();
    await expect(
      workspace.getByText(/Slow down\..*may have been committed/u),
    ).toBeVisible();
    await expect(workspace.getByText(/was saved elsewhere/u)).toHaveCount(0);
    await saveButton.click();
    await expect(
      workspace.getByText('Revision 3', { exact: true }),
    ).toBeVisible();
    const three = await api(server, 'GET', `/v1/artifacts/${id}`);
    assert.equal(three.current_revision, 3, 'The retry replays the lost save.');

    // A save elsewhere leaves the draft; saving it is refused, and "Load
    // latest" replaces it.
    await text('style.css').fill('p { color: green }');
    await api(server, 'POST', `/v1/artifacts/${id}/revisions`, {
      base_revision: 3,
      changes: [
        {
          op: 'put',
          path: 'notes.txt',
          encoding: 'utf8',
          content: 'elsewhere',
        },
      ],
    });
    await expect(
      workspace.getByText('Revision 4 was saved elsewhere'),
    ).toBeVisible();
    await expect(text('style.css')).toHaveValue('p { color: green }');
    await saveButton.click();
    await expect(
      workspace.getByText('Not saved: the artifact changed'),
    ).toBeVisible();
    await workspace.getByRole('button', { name: 'Load latest' }).click();
    await expect(
      workspace.getByText('Revision 4', { exact: true }),
    ).toBeVisible();
    await expect(text('style.css')).toHaveValue('p { color: blue }');
    await expect(disconnect).toBeEnabled();

    const loadLatest = workspace.getByRole('button', { name: 'Load latest' });
    const replacedNotice = workspace.getByText(
      "The server's data may have been replaced",
    );
    const inTrash = workspace.getByText('Moved to the trash elsewhere');
    /** Opens a rename of `from` whose read of the file is held. */
    const renameHeld = async (revision, from, to) => {
      const held = await holdAnswer(
        'GET',
        (pathname) => pathname === filePath(revision, from),
      );
      await renameButton.click();
      await pathInput.fill(to);
      await dialog.getByRole('button', { name: 'Rename', exact: true }).click();
      await held.seen;
      return held;
    };

    // A newer revision saved elsewhere while there is no draft is followed.
    await api(server, 'POST', `/v1/artifacts/${id}/revisions`, {
      base_revision: 4,
      changes: [
        { op: 'put', path: 'notes.txt', encoding: 'utf8', content: 'five' },
      ],
    });
    await expect(
      workspace.getByText('Revision 5', { exact: true }),
    ).toBeVisible();

    // The trash. A rename read the server answered before the move, and
    // delivered after the page shows the trash, moves nothing, and an open
    // dialog cannot submit there. The open file's text stays editable, but
    // nothing is read, opened, added, moved, or deleted, and no preview is
    // offered.
    await file('index.html').click();
    await text('index.html').fill('<p>kept</p>\n');
    await file('img/logo.png').click();
    const trashRead = await renameHeld(5, 'img/logo.png', 'logo.png');
    await setDeleted(true);
    await expect(inTrash).toBeVisible();
    await expect(
      dialog.getByText(
        'Paths cannot change now. The artifact is in the trash.',
      ),
    ).toBeVisible();
    trashRead.release();
    await expect(
      dialog.getByText(
        'img/logo.png was not moved. The artifact is in the trash.',
      ),
    ).toBeVisible();
    await dialog.getByRole('button', { name: 'Cancel' }).click();
    await expect(file('logo.png')).toHaveCount(0);
    await setDeleted(false);
    await expect(inTrash).toBeHidden();
    await file('style.css').click();
    await expect(text('style.css')).toHaveValue('p { color: blue }');
    await renameButton.click();
    await pathInput.fill('main.css');
    requests.length = 0;
    await setDeleted(true);
    await expect(inTrash).toBeVisible();
    await expect(
      dialog.getByRole('button', { name: 'Rename', exact: true }),
    ).toBeDisabled();
    await dialog.getByRole('button', { name: 'Cancel' }).click();
    await expect(file('main.css')).toHaveCount(0);
    await expect(file('notes.txt')).toBeDisabled();
    await expect(newFile).toBeDisabled();
    await expect(renameButton).toBeDisabled();
    await expect(deleteButton).toBeDisabled();
    await expect(
      workspace.getByRole('button', { name: 'Show saved preview' }),
    ).toHaveCount(0);
    // Edits that return to the read text end its change without a read.
    await text('style.css').fill('p { color: blue }');
    await expect(file('style.css')).not.toContainText('edited');
    await expect(text('style.css')).toBeEditable();
    await text('style.css').fill('p { color: trash }');
    // Files the draft holds still open without a read, so their edits
    // stay reachable; the others would need one.
    await file('index.html').click();
    await expect(text('index.html')).toHaveValue('<p>kept</p>\n');
    await expect(text('index.html')).toBeEditable();
    await file('style.css').click();
    await expect(saveButton).toBeDisabled();
    assert.deepEqual(requests, [], 'Nothing is read in the trash.');
    await setDeleted(false);
    await expect(inTrash).toBeHidden();

    // A save held across a forged reset: its answer belongs to data the
    // server may no longer hold, so the draft stays and saving stays off.
    // Paths cannot change then either; text still can.
    const save = await holdAnswer('POST', (pathname) =>
      pathname.endsWith('/revisions'),
    );
    await saveButton.click();
    await save.seen;
    await forgeReset(tab);
    // Released once the page shows the reset, so the answer lands after it.
    await expect(replacedNotice).toBeVisible();
    save.release();
    // The answer is in once the save no longer shows as in flight.
    await expect(saveButton).toHaveText('Save revision');
    await expect(replacedNotice).toBeVisible();
    await expect(saveButton).toBeDisabled();
    await expect(disconnect).toBeDisabled();
    await expect(text('style.css')).toHaveValue('p { color: trash }');
    await expect(renameButton).toBeDisabled();
    await expect(deleteButton).toBeDisabled();
    await expect(newFile).toBeDisabled();
    await expect(file('notes.txt')).toBeDisabled();
    await file('index.html').click();
    await expect(text('index.html')).toHaveValue('<p>kept</p>\n');
    await file('style.css').click();
    await text('style.css').fill('p { color: after }');
    await loadLatest.click();
    await expect(
      workspace.getByText('Revision 6', { exact: true }),
    ).toBeVisible();
    await expect(text('style.css')).toHaveValue('p { color: trash }');

    // A read held across a forged reset is not shown, and is not offered
    // again: the number may now name other content.
    await text('style.css').fill('p { color: draft }');
    const shownRead = await holdAnswer(
      'GET',
      (pathname) => pathname === filePath(6, 'pages/about.html'),
    );
    await file('pages/about.html').click();
    await shownRead.seen;
    await forgeReset(tab);
    // Released once the page shows the reset, so the answer lands after it.
    await expect(replacedNotice).toBeVisible();
    shownRead.release();
    const notRead = workspace
      .getByRole('alert')
      .filter({ hasText: 'Could not read pages/about.html' });
    await expect(notRead).toContainText(
      "Nothing was read. The server's data may have been replaced",
    );
    await expect(
      notRead.getByRole('button', { name: 'Try again' }),
    ).toHaveCount(0);
    await loadLatest.click();
    await expect(replacedNotice).toBeHidden();

    // So is a rename's read: nothing moves.
    await text('pages/about.html').fill('<p>draft</p>');
    await file('style.css').click();
    await expect(text('style.css')).toHaveValue('p { color: trash }');
    const replacedRead = await renameHeld(6, 'style.css', 'main.css');
    await forgeReset(tab);
    // Released once the page shows the reset, so the answer lands after it.
    await expect(replacedNotice).toBeVisible();
    replacedRead.release();
    await expect(
      dialog.getByText(
        "style.css was not moved. Nothing was read. The server's data may have been replaced since this draft's revision was read.",
      ),
    ).toBeVisible();
    await dialog.getByRole('button', { name: 'Cancel' }).click();
    await expect(file('main.css')).toHaveCount(0);
    await checkAccessibility('editor-replaced', tab);
    assert.deepEqual(
      (await api(server, 'GET', `/v1/artifacts/${id}/revisions`)).revisions.map(
        (item) => item.revision,
      ),
      [6, 5, 4, 3, 2, 1],
    );
    assert.deepEqual(errors, []);
    return { revisions: 6 };
  } finally {
    await context.close();
  }
}
