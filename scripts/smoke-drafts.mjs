import assert from 'node:assert/strict';
import { expect } from '@playwright/test';
import { api, bytes, fileButton } from './smoke-workspace.mjs';

const PNG = [137, 80, 78, 71, 13, 10, 26, 10, 0, 255];

/**
 * Browser checks for source drafts kept in the browser (decision 0018)
 * against a real artifree-server, in their own browser context. A reload
 * keeps the tab and its session storage; a new page is another tab.
 */
export async function checkDrafts({
  browser,
  origin,
  server,
  checkAccessibility,
}) {
  const create = async (title, files) =>
    (await api(server, 'POST', '/v1/artifacts', { title, files })).artifact;
  const kept = await create('Kept', [
    { path: 'index.html', encoding: 'utf8', content: '<p>kept</p>' },
    {
      path: 'logo.png',
      encoding: 'base64',
      content: Buffer.from(PNG).toString('base64'),
    },
  ]);
  // Created later, so listed first: a reload opens it, not the draft's.
  await create('Other', [
    { path: 'index.html', encoding: 'utf8', content: '<p>other</p>' },
  ]);
  const revisions = async () =>
    (
      await api(server, 'GET', `/v1/artifacts/${kept.id}/revisions`)
    ).revisions.map((item) => item.revision);

  const context = await browser.newContext();
  try {
    const errors = [];
    // `copied` stands for session storage a duplicated tab copies: the
    // owner id of the tab it duplicates.
    const openTab = async (copied) => {
      const tab = await context.newPage();
      tab.setDefaultTimeout(10_000);
      tab.on('pageerror', (error) => errors.push(error.message));
      if (copied !== undefined)
        await tab.addInitScript((owner) => {
          // Not in the sandboxed preview frames, which have no storage.
          if (window !== window.top) return;
          if (sessionStorage.getItem('artifree.draftOwner') === null)
            sessionStorage.setItem('artifree.draftOwner', owner);
        }, copied);
      await tab.goto(origin);
      const workspace = tab.locator('#artifacts');
      await workspace.getByLabel('Access token').fill(server.token);
      await workspace.getByRole('button', { name: 'Connect' }).click();
      return { tab, workspace };
    };
    const { tab, workspace } = await openTab();
    const ui = (area) => ({
      text: (path) => area.getByLabel(path, { exact: true }),
      file: (path) => fileButton(area, path),
      source: () => area.getByRole('tab', { name: 'Source' }).click(),
      open: async (title) => {
        await area
          .getByRole('navigation', { name: 'Artifacts' })
          .getByRole('button', { name: new RegExp(`^${title} `) })
          .click();
        await expect(
          area.getByRole('heading', { name: title, exact: true }),
        ).toBeVisible();
      },
      recover: area.getByRole('button', { name: 'Recover draft' }),
      discardKept: area.getByRole('button', { name: 'Discard kept draft' }),
      save: area.getByRole('button', { name: 'Save revision' }),
      choice: area.getByText(
        'A draft of this artifact was kept in this browser',
      ),
    });
    const a = ui(workspace);
    // What the store keeps: whose each draft is, the paths it changes, and
    // its index.html text. A write still running when a page closes is lost
    // (decision 0018), so a step waits for the state the next page relies
    // on before it reloads or closes the tab.
    const keptIn = (page) =>
      page.evaluate(
        () =>
          new Promise((resolve, reject) => {
            const opening = indexedDB.open('artifree-source-drafts', 1);
            opening.onsuccess = () => {
              const db = opening.result;
              const all = db
                .transaction('drafts')
                .objectStore('drafts')
                .getAll();
              all.onsuccess = () => {
                const me = sessionStorage.getItem('artifree.draftOwner');
                resolve(
                  all.result.map((record) => ({
                    by: record.owner === me ? 'this tab' : 'another tab',
                    paths: [...record.draft.changes.keys()].sort(),
                    text: record.draft.changes.get('index.html')?.text,
                  })),
                );
                db.close();
              };
              all.onerror = () => reject(all.error);
            };
            opening.onerror = () => reject(opening.error);
          }),
      );
    const keeps = (page, expected) =>
      expect.poll(() => keptIn(page)).toEqual(expected);
    const text = (value) => [
      { by: 'this tab', paths: ['index.html'], text: value },
    ];

    // A draft outlives a reload, also its moved bytes. After the reload the
    // first listed artifact opens; the kept draft is named and opens.
    await a.open('Kept');
    await a.source();
    await a.text('index.html').fill('<p>kept 2</p>');
    await a.file('logo.png').click();
    await workspace.getByRole('button', { name: 'Rename file' }).click();
    await tab.getByRole('dialog').getByLabel('Path').fill('img/logo.png');
    await tab
      .getByRole('dialog')
      .getByRole('button', { name: 'Rename', exact: true })
      .click();
    await expect(a.file('img/logo.png')).toBeVisible();
    await keeps(tab, [
      {
        by: 'this tab',
        paths: ['img/logo.png', 'index.html', 'logo.png'],
        text: '<p>kept 2</p>',
      },
    ]);
    await tab.reload();
    await expect(
      workspace.getByRole('heading', { name: 'Other', exact: true }),
    ).toBeVisible();
    const notice = workspace.getByText('Drafts kept in this browser');
    await expect(notice).toBeVisible();
    await checkAccessibility('drafts-kept-notice', tab);
    await workspace.getByRole('button', { name: 'Open Kept' }).click();
    await a.source();
    await expect(a.choice).toBeVisible();
    await checkAccessibility('drafts-choice', tab);
    await a.recover.click();
    await expect(a.text('index.html')).toHaveValue('<p>kept 2</p>');
    await expect(a.file('img/logo.png')).toBeVisible();
    await expect(a.file('logo.png')).toContainText('deleted');
    await a.save.click();
    await expect(
      workspace.getByText('Revision 2', { exact: true }),
    ).toBeVisible();
    assert.deepEqual(
      await bytes(
        server,
        `/v1/artifacts/${kept.id}/revisions/2/files/img/logo.png`,
      ),
      Buffer.from(PNG),
    );
    // A saved draft is no longer kept.
    await keeps(tab, []);
    await tab.reload();
    await a.open('Kept');
    await a.source();
    await expect(a.text('index.html')).toHaveValue('<p>kept 2</p>');
    await expect(a.choice).toHaveCount(0);
    await expect(notice).toHaveCount(0);

    // A kept draft can be discarded instead.
    await a.text('index.html').fill('<p>to discard</p>');
    await keeps(tab, text('<p>to discard</p>'));
    await tab.reload();
    await a.open('Kept');
    await a.source();
    await a.discardKept.click();
    await expect(a.text('index.html')).toHaveValue('<p>kept 2</p>');
    await tab.reload();
    await a.open('Kept');
    await a.source();
    await expect(a.text('index.html')).toBeVisible();
    await expect(a.choice).toHaveCount(0);

    // The page closes while a save is in flight; the server commits it.
    // The kept draft says so, shows no "saved elsewhere" notice for that
    // revision, and saving it again replays the commit with its key.
    await a.text('index.html').fill('<p>in flight</p>');
    let release;
    const released = new Promise((resolve) => {
      release = resolve;
    });
    let committed;
    const sent = new Promise((resolve) => {
      committed = resolve;
    });
    await tab.route('**/v1/artifacts/*/revisions', async (route) => {
      if (route.request().method() !== 'POST') return route.fallback();
      const response = await route.fetch();
      committed();
      await released;
      await route.fulfill({ response }).catch(() => {});
      return undefined;
    });
    await a.save.click();
    await sent;
    await tab.reload();
    release();
    await tab.unrouteAll({ behavior: 'ignoreErrors' });
    assert.deepEqual(await revisions(), [3, 2, 1]);
    await a.open('Kept');
    await a.source();
    await expect(workspace.getByText(/answer was not received/u)).toBeVisible();
    await a.recover.click();
    await expect(
      workspace.getByText('This draft may already be saved'),
    ).toBeVisible();
    await expect(workspace.getByText(/was saved elsewhere/u)).toHaveCount(0);
    await a.save.click();
    await expect(
      workspace.getByText('Revision 3', { exact: true }),
    ).toBeVisible();
    await expect(a.save).toBeDisabled();
    assert.deepEqual(
      await revisions(),
      [3, 2, 1],
      'The save replays; it does not commit twice.',
    );

    // A draft of a tab that is still open stays with it, also from a
    // duplicate of that tab, which takes an owner id of its own. Once that
    // tab closes, another tab may continue it.
    await a.text('index.html').fill('<p>first tab</p>');
    await keeps(tab, text('<p>first tab</p>'));
    const second = await openTab(
      await tab.evaluate(() => sessionStorage.getItem('artifree.draftOwner')),
    );
    const b = ui(second.workspace);
    await b.open('Kept');
    await b.source();
    await expect(b.text('index.html')).toHaveValue('<p>in flight</p>');
    await expect(b.choice).toHaveCount(0);
    await keeps(second.tab, [
      { by: 'another tab', paths: ['index.html'], text: '<p>first tab</p>' },
    ]);
    await tab.close();
    await second.tab.reload();
    await b.open('Kept');
    await b.source();
    await b.recover.click();
    await expect(b.text('index.html')).toHaveValue('<p>first tab</p>');
    await keeps(second.tab, text('<p>first tab</p>'));

    // A kept draft whose revision the server no longer lists with the same
    // files is kept as text: nothing is read or saved for it.
    await second.tab.evaluate(
      () =>
        new Promise((resolve, reject) => {
          const opening = indexedDB.open('artifree-source-drafts', 1);
          opening.onsuccess = () => {
            const db = opening.result;
            const transaction = db.transaction('drafts', 'readwrite');
            const drafts = transaction.objectStore('drafts');
            const all = drafts.getAll();
            all.onsuccess = () => {
              for (const record of all.result)
                drafts.put({
                  ...record,
                  draft: {
                    ...record.draft,
                    files: record.draft.files.map((file) => ({
                      ...file,
                      sha256: '0'.repeat(64),
                    })),
                  },
                });
            };
            transaction.oncomplete = () => {
              db.close();
              resolve();
            };
            transaction.onerror = () => reject(transaction.error);
          };
          opening.onerror = () => reject(opening.error);
        }),
    );
    await second.tab.reload();
    await b.open('Kept');
    await b.source();
    await b.recover.click();
    await expect(
      second.workspace.getByText("The kept draft's revision has changed"),
    ).toBeVisible();
    await expect(b.save).toBeDisabled();
    await expect(b.text('index.html')).toBeEditable();
    await expect(
      second.workspace.getByRole('button', { name: 'Rename file' }),
    ).toBeDisabled();
    await second.workspace.getByRole('button', { name: 'Load latest' }).click();
    await expect(b.text('index.html')).toHaveValue('<p>in flight</p>');
    // Loading the latest revision ends the draft, also the one adopted
    // from the closed tab.
    await keeps(second.tab, []);
    await second.tab.reload();
    await b.open('Kept');
    await b.source();
    await expect(b.text('index.html')).toBeVisible();
    await expect(b.choice).toHaveCount(0);

    // When the browser refuses to keep a draft, the page says so and keeps
    // editing in memory; the next change is kept once it is allowed.
    await second.tab.evaluate(() => {
      const put = IDBObjectStore.prototype.put;
      globalThis.restorePut = () => {
        IDBObjectStore.prototype.put = put;
      };
      IDBObjectStore.prototype.put = () => {
        throw new DOMException('The quota is exceeded.', 'QuotaExceededError');
      };
    });
    await b.text('index.html').fill('<p>over quota</p>');
    const notKept = second.workspace.getByText(
      'This draft is not kept in this browser',
    );
    await expect(notKept).toBeVisible();
    await second.tab.evaluate(() => globalThis.restorePut());
    await b.text('index.html').fill('<p>kept again</p>');
    await expect(notKept).toBeHidden();
    await keeps(second.tab, text('<p>kept again</p>'));
    await second.tab.reload();
    await b.open('Kept');
    await b.source();
    await b.recover.click();
    await expect(b.text('index.html')).toHaveValue('<p>kept again</p>');
    await second.workspace
      .getByRole('button', { name: 'Discard changes' })
      .click();
    await keeps(second.tab, []);
    await second.tab.reload();
    await b.open('Kept');
    await b.source();
    await expect(b.text('index.html')).toBeVisible();
    await expect(b.choice).toHaveCount(0);

    assert.deepEqual(errors, []);
    return { revisions: 3 };
  } finally {
    await context.close();
  }
}
