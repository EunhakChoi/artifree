import assert from 'node:assert/strict';
import { expect } from '@playwright/test';
import { setVisible } from './smoke-list.mjs';
import { api, hold } from './smoke-workspace.mjs';

/** Unchanged paragraphs after the changed one: more than three lines. */
const TAIL = ['Tail one', 'Tail two', 'Tail three', 'Tail four', 'Tail five'];
/** A valid path near the 240-byte limit, in at most eight segments. */
const LONG_PATH = `notes/${'a-folder-name-long-enough-to-wrap-badly/'.repeat(5)}added.txt`;
/** A binary file whose name is one 100-byte segment, the longest allowed. */
const LONG_NAME = `${'m'.repeat(96)}.png`;
/** A cursor of a feed other than the server's (decision 0006). */
const FOREIGN_CURSOR = `${'ab'.repeat(16)}.1`;

/**
 * Browser checks for comparing and restoring revisions (decision 0011)
 * against a real artifree-server. The page has its own browser context;
 * other clients' changes come from the API, and the page's reads are held
 * to show what it does before they arrive.
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
      utf8('index.html', page('Old middle')),
      utf8('style.css', 'p { margin: 0; }\n'),
      utf8('endings.txt', 'one\r\ntwo\n'),
      bytes(LONG_NAME, [0x89, 0x50, 0x4e, 0x47, 1, 2]),
    ],
  });
  const path = `/v1/artifacts/${artifact.id}`;
  const save = async (changes) => {
    const { current_revision: base } = await api(server, 'GET', path);
    return api(server, 'POST', `${path}/revisions`, {
      base_revision: base,
      changes,
    });
  };
  await save([
    { op: 'put', ...utf8('index.html', page('New middle')) },
    { op: 'put', ...utf8('endings.txt', '\uFEFFone\ntwo') },
    { op: 'put', ...bytes(LONG_NAME, [0x89, 0x50, 0x4e, 0x47, 1, 3]) },
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
    // Content the page reads: file bytes and preview links.
    const contentReads = [];
    tab.on('request', (request) => {
      const url = new URL(request.url());
      if (/\/revisions\/\d+\/(files\/|previews)/.test(url.pathname))
        contentReads.push(url.pathname);
    });
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
    const viewTab = (name) =>
      workspace.getByRole('tab', { name, exact: true }).click();
    await shows(2);
    await viewTab('History');

    // Revision 1 against the latest: files by digest, in path order, and
    // the first changed one line by line.
    await expect(
      workspace.getByText(
        '5 files differ from revision 1 to revision 2; 0 files unchanged.',
      ),
    ).toBeVisible();
    const files = workspace.getByRole('navigation', { name: 'Changed files' });
    await expect(files.getByRole('button')).toHaveText([
      'Changedendings.txt',
      'Changedindex.html',
      `Changed${LONG_NAME}`,
      `Added${LONG_PATH}`,
      'Removedstyle.css',
    ]);
    const table = (name) =>
      workspace.getByRole('table', { name: `Changes to ${name}` });
    const text = (rows) => rows.locator('td:last-child');
    // Line endings, the byte order mark, and the missing line feed show.
    await expect(
      text(table('endings.txt').locator('[data-kind="removed"]')),
    ).toHaveText(['one␍', 'two']);
    await expect(
      text(table('endings.txt').locator('[data-kind="added"]')),
    ).toHaveText(['⟨BOM⟩one', 'two⟨no line feed⟩']);
    // Three lines of context around the change, and the lines left out
    // after it are marked.
    await files.getByRole('button', { name: /index\.html/ }).click();
    await expect(table('index.html').locator('tbody tr')).toHaveText([
      /doctype/,
      /<title>/,
      /Opening/,
      /−\s*removed<p>Old middle/,
      /\+\s*added<p>New middle/,
      /Tail one/,
      /Tail two/,
      /Tail three/,
      '3 unchanged lines',
    ]);
    await checkAccessibility('workspace-history', tab);
    // Long paths fit a narrow window and are shown whole once chosen.
    const size = tab.viewportSize();
    await tab.setViewportSize({ width: 360, height: 640 });
    const fits = async (box) => {
      const { x, width } = await box.boundingBox();
      assert.ok(x >= 0 && x + width <= 360, `Wider than 360: ${x}+${width}.`);
    };
    await files.getByRole('button', { name: /m\.png/ }).click();
    const sizes = workspace.getByText(/only its size is: 6 → 6 bytes/);
    await expect(sizes).toBeVisible();
    // Text overflowing a paragraph does not widen its box.
    assert.ok(
      await sizes.evaluate(
        (element) => element.scrollWidth <= element.clientWidth,
      ),
      'The sizes text overflows its paragraph.',
    );
    await files.getByRole('button', { name: /added\.txt/ }).click();
    const whole = workspace.getByRole('heading', { name: LONG_PATH });
    await expect(whole).toBeVisible();
    for (const box of [...(await files.getByRole('button').all()), whole])
      await fits(box);
    await tab.setViewportSize(size);

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
    assert.equal(await latest(), 4, 'The lost restore applied once.');
    await tab.unroute('**/restore');

    // A save the page learns of from a notice, before its read arrives:
    // the restore waits for that read, then is prepared against it.
    const history = workspace.locator('#history-revision');
    await history.selectOption('3');
    let release = await hold(tab, 'GET', `**${path}`);
    await restoreButton(3).click();
    dialog = dialogFor(3);
    await save([{ op: 'put', ...utf8('notes.txt', 'elsewhere\n') }]);
    await expect(
      dialog.getByText(/The artifact changed since this was prepared/),
    ).toBeVisible();
    await expect(dialog.getByText(/you can restore once it has/)).toBeVisible();
    await expect(confirm(dialog)).toBeDisabled();
    await release();
    await expect(
      dialog.getByText(/The page shows the latest now/),
    ).toBeVisible();
    await confirm(dialog).click();
    await expect(dialog).toBeHidden();
    await shows(6);
    assert.deepEqual(await digests(6), await digests(3));

    // The page's own restore: until its result is shown, the next one
    // waits, as it would be prepared from the revision before. The feed is
    // stalled, so only the restore's own answer tells the page.
    await history.selectOption('5');
    let resume = await stallFeed(tab);
    release = await hold(tab, 'GET', `**${path}`);
    await restoreButton(5).click();
    dialog = dialogFor(5);
    await confirm(dialog).click();
    await expect(dialog).toBeHidden();
    await restoreButton(5).click();
    await expect(dialog.getByText(/you can restore once it has/)).toBeVisible();
    await expect(confirm(dialog)).toBeDisabled();
    await release();
    await expect(
      dialog.getByText(/The page shows the latest now/),
    ).toBeVisible();
    await expect(confirm(dialog)).toBeEnabled();
    await dialog.getByRole('button', { name: 'Cancel' }).click();
    await shows(7);
    await resume();

    // A conflict that names a revision the page has not read: restoring
    // waits until the page reads it, whatever the dialog does meanwhile.
    await tab.route(
      '**/restore',
      (route) =>
        route.fulfill({
          status: 409,
          json: {
            error: {
              code: 'revision_conflict',
              message: 'Changed elsewhere.',
              current_revision: 8,
            },
          },
        }),
      { times: 1 },
    );
    await history.selectOption('6');
    await restoreButton(6).click();
    dialog = dialogFor(6);
    await confirm(dialog).click();
    await expect(
      dialog.getByText(/so nothing was changed\. The page is reading/),
    ).toBeVisible();
    await expect(confirm(dialog)).toBeDisabled();
    await dialog.getByRole('button', { name: 'Cancel' }).click();
    await restoreButton(6).click();
    await expect(confirm(dialog)).toBeDisabled();
    await save([{ op: 'put', ...utf8('notes.txt', 'later\n') }]);
    await expect(
      dialog.getByText(/The page shows the latest now/),
    ).toBeVisible();
    await expect(confirm(dialog)).toBeEnabled();
    await dialog.getByRole('button', { name: 'Cancel' }).click();
    await shows(8);

    // A save from the editor names the same revision: a restore waits for
    // its result too. The feed is stalled again, here and for the pin.
    resume = await stallFeed(tab);
    await viewTab('Source');
    const editor = workspace.getByLabel('index.html', { exact: true });
    await editor.fill(page('Edited here'));
    release = await hold(tab, 'GET', `**${path}`);
    await workspace.getByRole('button', { name: 'Save revision' }).click();
    await viewTab('History');
    await restoreButton(7).click();
    dialog = dialogFor(7);
    await expect(confirm(dialog)).toBeDisabled();
    await release();
    await expect(confirm(dialog)).toBeEnabled();
    await dialog.getByRole('button', { name: 'Cancel' }).click();
    await shows(9);

    // The page's own pin: until its result is shown, pinning and renaming
    // wait, as they would be prepared from the metadata before.
    const pin = workspace.getByRole('button', { name: 'Pin', exact: true });
    const renameButton = workspace.getByRole('button', {
      name: 'Rename',
      exact: true,
    });
    const rename = tab.getByRole('dialog', { name: 'Rename artifact' });
    const send = rename.getByRole('button', { name: 'Rename', exact: true });
    release = await hold(tab, 'GET', `**${path}`);
    await pin.click();
    await expect(pin).toBeDisabled();
    await renameButton.click();
    await rename.getByLabel('Title').fill('Pinned history');
    await expect(rename.getByText(/you can rename once it has/)).toBeVisible();
    await expect(send).toBeDisabled();
    await release();
    await expect(send).toBeEnabled();
    await rename.getByRole('button', { name: 'Cancel' }).click();
    await expect(pin).toHaveAttribute('aria-pressed', 'true');
    await expect(pin).toBeEnabled();
    await resume();

    // After a forged reset, until the artifact is read again, nothing is
    // read or written for what was read before, and choices made in it,
    // also meanwhile, are forgotten once the new read arrives. The read
    // fails first, so other reads could be sent meanwhile.
    const against = workspace.locator('#history-against');
    await viewTab('Preview');
    const previewRevision = workspace.locator('#preview-revision');
    await previewRevision.selectOption('2');
    await viewTab('History');
    await history.selectOption('2');
    await against.selectOption('3');
    await expect(table('endings.txt')).toBeVisible();
    const before = contentReads.length;
    const failing = (route) =>
      route.request().method() === 'GET'
        ? route.fulfill({
            status: 503,
            json: { error: { code: 'unavailable', message: 'Busy.' } },
          })
        : route.fallback();
    await tab.route(`**${path}`, failing);
    await forgeReset(tab);
    const refreshFailure = workspace
      .getByRole('alert')
      .filter({ hasText: 'Could not refresh the artifact' });
    await expect(refreshFailure).toBeVisible();
    await expect(
      workspace.getByText(/Reading the artifact again/),
    ).toBeVisible();
    for (const control of [pin, renameButton, restoreButton(2)])
      await expect(control).toBeDisabled();
    await expect(
      workspace.getByText(/is compared once the artifact is read again/),
    ).toBeVisible();
    // Choosing other content meanwhile reads nothing either; the pause
    // gives a read that should not start the time to.
    await history.selectOption('4');
    await tab.waitForTimeout(500);
    await viewTab('Preview');
    await expect(
      workspace.getByText(/The preview is prepared once the artifact/),
    ).toBeVisible();
    await previewRevision.selectOption('3');
    await tab.waitForTimeout(500);
    await viewTab('Source');
    await expect(
      workspace.getByText(/The source is read once the artifact/),
    ).toBeVisible();
    await viewTab('History');
    assert.deepEqual(
      contentReads.slice(before),
      [],
      'Nothing is read for data that may have been replaced.',
    );
    await tab.unroute(`**${path}`, failing);
    await refreshFailure.getByRole('button', { name: 'Try again' }).click();
    for (const control of [pin, renameButton, restoreButton(8)])
      await expect(control).toBeEnabled();
    await expect(history).toHaveValue('8');
    await expect(against).toHaveValue('latest');
    await viewTab('Preview');
    await expect(previewRevision).toHaveValue('latest');
    await viewTab('History');

    // A dialog open across a reset sends nothing and offers no retry.
    await restoreButton(8).click();
    dialog = dialogFor(8);
    await forgeReset(tab);
    await expect(
      dialog.getByText("The server's data may have been replaced"),
    ).toBeVisible();
    await expect(confirm(dialog)).toBeDisabled();
    await dialog.getByRole('button', { name: 'Cancel' }).click();
    await renameButton.click();
    await rename.getByLabel('Title').fill('Revision history renamed');
    await forgeReset(tab);
    await expect(
      rename.getByText("The server's data may have been replaced"),
    ).toBeVisible();
    await expect(send).toBeDisabled();
    await rename.getByRole('button', { name: 'Cancel' }).click();
    // Opened again, it is prepared from what the page reads now.
    await restoreButton(8).click();
    await expect(
      dialog.getByText("The server's data may have been replaced"),
    ).toBeHidden();
    await confirm(dialog).click();
    await expect(dialog).toBeHidden();
    await shows(10);
    assert.deepEqual(await digests(10), await digests(8));

    // A restore sent before a reset whose answer was lost: the dialog says
    // it may have been made, and sends nothing again. (The forged answer
    // keeps the request from the server.)
    let answer;
    const answered = new Promise((resolve) => {
      answer = resolve;
    });
    await tab.route(
      '**/restore',
      async (route) => {
        await answered;
        return route.fulfill({
          status: 503,
          json: { error: { code: 'unavailable', message: 'Answer lost.' } },
        });
      },
      { times: 1 },
    );
    await restoreButton(9).click();
    dialog = dialogFor(9);
    await confirm(dialog).click();
    await forgeReset(tab);
    answer();
    await expect(
      dialog.getByText(/Answer lost\. The change may have been made/),
    ).toBeVisible();
    await expect(dialog.getByText(/so it is not sent again/)).toBeVisible();
    await expect(confirm(dialog)).toBeDisabled();
    await dialog.getByRole('button', { name: 'Cancel' }).click();

    // A save the server refused names the revision it found: a restore
    // waits for it, also once the draft is gone.
    await viewTab('Source');
    const source = workspace.getByLabel('index.html', { exact: true });
    const saved = await source.inputValue();
    await source.fill(page('Refused'));
    await tab.route(
      `**${path}/revisions`,
      (route) =>
        route.request().method() === 'POST'
          ? route.fulfill({
              status: 409,
              json: {
                error: {
                  code: 'revision_conflict',
                  message: 'Changed elsewhere.',
                  current_revision: 11,
                },
              },
            })
          : route.fallback(),
      { times: 1 },
    );
    await workspace.getByRole('button', { name: 'Save revision' }).click();
    await expect(
      workspace.getByText('Not saved: the artifact changed'),
    ).toBeVisible();
    await source.fill(saved);
    await viewTab('History');
    await restoreButton(9).click();
    dialog = dialogFor(9);
    await expect(dialog.getByText(/you can restore once it has/)).toBeVisible();
    await expect(confirm(dialog)).toBeDisabled();
    await save([{ op: 'put', ...utf8('notes.txt', 'eleventh\n') }]);
    await expect(confirm(dialog)).toBeEnabled();
    await dialog.getByRole('button', { name: 'Cancel' }).click();
    await shows(11);

    // A draft locks restoring: a restore would leave it stale.
    await viewTab('Source');
    await workspace
      .getByLabel('index.html', { exact: true })
      .fill('<p>draft</p>');
    await viewTab('History');
    await expect(restoreButton(10)).toBeDisabled();
    await expect(
      workspace.getByText(/Restoring waits while the editor holds unsaved/),
    ).toBeVisible();

    assert.deepEqual(errors, [], 'Page errors.');
    return { revisions: 11 };
  } finally {
    await context.close();
  }
}

/**
 * Holds the page's next change stream unanswered, so no notice arrives,
 * until the returned function ends it; the page then resumes from its
 * cursor and receives what it missed.
 */
async function stallFeed(tab) {
  let resume;
  const resumed = new Promise((resolve) => {
    resume = resolve;
  });
  let opened;
  const stalled = new Promise((resolve) => {
    opened = resolve;
  });
  await tab.route(
    (url) => url.pathname === '/v1/changes',
    async (route) => {
      opened();
      await resumed;
      // The page may have ended the stream itself, as when it was hidden.
      await route.abort().catch(() => {});
    },
    { times: 1 },
  );
  await setVisible(tab, false);
  await setVisible(tab, true);
  await stalled;
  return async () => resume();
}

/**
 * Makes the page's change stream resume from a cursor of another feed, so
 * the server answers `reset`, as for a page whose server's data was
 * replaced, and keeps the stream open.
 */
export async function forgeReset(tab) {
  let forged;
  const resumed = new Promise((resolve) => {
    forged = resolve;
  });
  await tab.route(
    (url) => url.pathname === '/v1/changes',
    (route) => {
      const headers = route.request().headers();
      forged();
      return route.continue({
        headers: { ...headers, 'last-event-id': FOREIGN_CURSOR },
      });
    },
    { times: 1 },
  );
  await setVisible(tab, false);
  await setVisible(tab, true);
  await resumed;
}

function page(middle) {
  return `<!doctype html>\n<title>History</title>\n${[
    'Opening',
    middle,
    ...TAIL,
  ]
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
