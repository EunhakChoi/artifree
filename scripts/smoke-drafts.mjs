import assert from 'node:assert/strict';
import { expect } from '@playwright/test';
import { forgeReset } from './smoke-history.mjs';
import { api, bytes, fileButton } from './smoke-workspace.mjs';

const PNG = [137, 80, 78, 71, 13, 10, 26, 10, 0, 255];
const OWNER_KEY = 'artifree.draftOwner';

/**
 * Browser checks for source drafts kept in the browser (decision 0018),
 * against a real artifree-server and the browser's own IndexedDB, in a
 * browser context of their own. A reload keeps a tab and its session
 * storage; a new page is another tab.
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
  const errors = [];
  /**
   * Opens a tab and connects it. `copied` is the owner id a tab duplicated
   * from another finds in its copied session storage.
   */
  const openTab = async (copied) => {
    const tab = await context.newPage();
    tab.setDefaultTimeout(10_000);
    tab.on('pageerror', (error) => errors.push(error.message));
    await tab.addInitScript(
      ([key, owner]) => {
        // The sandboxed preview frames have no storage.
        if (window !== window.top) return;
        if (typeof owner === 'string' && sessionStorage.getItem(key) === null)
          sessionStorage.setItem(key, owner);
        // While the tab's session storage says so, kept drafts cannot be
        // read, as when the browser's storage fails.
        if (sessionStorage.getItem('smoke.refuseReads') !== '1') return;
        const getAll = IDBObjectStore.prototype.getAll;
        IDBObjectStore.prototype.getAll = () => {
          throw new DOMException(
            'The drafts could not be read.',
            'UnknownError',
          );
        };
        globalThis.allowReads = () => {
          sessionStorage.removeItem('smoke.refuseReads');
          IDBObjectStore.prototype.getAll = getAll;
        };
      },
      [OWNER_KEY, copied],
    );
    await tab.goto(origin);
    const workspace = tab.locator('#artifacts');
    const connect = async () => {
      await workspace.getByLabel('Access token').fill(server.token);
      await workspace.getByRole('button', { name: 'Connect' }).click();
    };
    await connect();
    return { tab, workspace, connect, ...ui(workspace) };
  };
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
    discardChanges: area.getByRole('button', { name: 'Discard changes' }),
    save: area.getByRole('button', { name: 'Save revision' }),
    choice: area.getByText('A draft of this artifact was kept in this browser'),
    notKept: area.getByText('This draft is not kept in this browser'),
  });
  /** Reloads `at`, then shows the source of "Kept". */
  const reopen = async (at) => {
    await at.tab.reload();
    await at.open('Kept');
    await at.source();
  };

  // What the store holds, as seen from a page: whose each draft is, the
  // paths it changes, and its index.html text. A write still running when
  // a page closes is lost (decision 0018, item 3), so before a reload or a
  // close each step waits for the state the next page relies on.
  const keptIn = (page) =>
    page.evaluate(
      (key) =>
        new Promise((resolve, reject) => {
          const opening = indexedDB.open('artifree-source-drafts', 1);
          opening.onsuccess = () => {
            const db = opening.result;
            const all = db.transaction('drafts').objectStore('drafts').getAll();
            all.onsuccess = () => {
              const me = sessionStorage.getItem(key);
              db.close();
              resolve(
                all.result
                  .map((record) => ({
                    by: record.owner === me ? 'this tab' : 'another tab',
                    paths: [...record.draft.changes.keys()].sort(),
                    text: record.draft.changes.get('index.html')?.text,
                  }))
                  .sort((left, right) => left.by.localeCompare(right.by)),
              );
            };
            all.onerror = () => reject(all.error);
          };
          opening.onerror = () => reject(opening.error);
        }),
      OWNER_KEY,
    );
  const keeps = (page, expected) =>
    expect.poll(() => keptIn(page)).toEqual(expected);
  const keptField = (page, field) =>
    page.evaluate(
      ([key, fieldName]) =>
        new Promise((resolve, reject) => {
          const opening = indexedDB.open('artifree-source-drafts', 1);
          opening.onerror = () => reject(opening.error);
          opening.onsuccess = () => {
            const db = opening.result;
            const all = db.transaction('drafts').objectStore('drafts').getAll();
            all.onerror = () => reject(all.error);
            all.onsuccess = () => {
              const owner = sessionStorage.getItem(key);
              const record = all.result.find((item) => item.owner === owner);
              db.close();
              resolve(record?.[fieldName]);
            };
          };
        }),
      [OWNER_KEY, field],
    );
  const keptVersion = (page) => keptField(page, 'version');
  const text = (value, by = 'this tab') => ({
    by,
    paths: ['index.html'],
    text: value,
  });
  /**
   * Edits every kept record in `page`'s store, as a closed tab or another
   * server state would leave it: `seed` adds a newer draft of a closed tab
   * with other text, `unseed` removes it, and `tamper` gives every file of
   * the draft's revision another digest.
   */
  const rewrite = (page, edit) =>
    page.evaluate(
      (how) =>
        new Promise((resolve, reject) => {
          const opening = indexedDB.open('artifree-source-drafts', 1);
          opening.onsuccess = () => {
            const db = opening.result;
            const transaction = db.transaction('drafts', 'readwrite');
            const drafts = transaction.objectStore('drafts');
            const all = drafts.getAll();
            all.onsuccess = () => {
              for (const record of all.result) {
                const { draft } = record;
                if (how === 'seed')
                  drafts.put({
                    ...record,
                    owner: 'a closed tab',
                    keptAt: record.keptAt + 60_000,
                    draft: {
                      ...draft,
                      changes: new Map([
                        [
                          'index.html',
                          {
                            ...draft.changes.get('index.html'),
                            text: '<p>other tab</p>',
                          },
                        ],
                      ]),
                    },
                  });
                else if (how === 'unseed') {
                  if (record.owner === 'a closed tab')
                    drafts.delete([record.owner, record.artifactId]);
                } else
                  drafts.put({
                    ...record,
                    draft: {
                      ...draft,
                      files: draft.files.map((file) => ({
                        ...file,
                        sha256: '0'.repeat(64),
                      })),
                    },
                  });
              }
            };
            transaction.oncomplete = () => {
              db.close();
              resolve();
            };
            transaction.onerror = () => reject(transaction.error);
          };
          opening.onerror = () => reject(opening.error);
        }),
      edit,
    );
  // Makes `page`'s IndexedDB writes fail as over the quota, until the
  // returned function allows them again.
  const refuseWrites = async (page) => {
    await page.evaluate(() => {
      const put = IDBObjectStore.prototype.put;
      globalThis.allowWrites = () => {
        IDBObjectStore.prototype.put = put;
      };
      IDBObjectStore.prototype.put = () => {
        throw new DOMException('The quota is exceeded.', 'QuotaExceededError');
      };
    });
    return () => page.evaluate(() => globalThis.allowWrites());
  };
  const refuseDeletes = async (page) => {
    await page.evaluate(() => {
      const remove = IDBObjectStore.prototype.delete;
      globalThis.allowDeletes = () => {
        IDBObjectStore.prototype.delete = remove;
      };
      IDBObjectStore.prototype.delete = () => {
        throw new DOMException(
          'The draft could not be deleted.',
          'UnknownError',
        );
      };
    });
    return () => page.evaluate(() => globalThis.allowDeletes());
  };
  const abortDeletes = async (page) => {
    await page.evaluate(() => {
      const remove = IDBObjectStore.prototype.delete;
      globalThis.allowDeletes = () => {
        IDBObjectStore.prototype.delete = remove;
      };
      IDBObjectStore.prototype.delete = function (...args) {
        const request = remove.apply(this, args);
        request.addEventListener('success', () => this.transaction.abort(), {
          once: true,
        });
        return request;
      };
    });
    return () => page.evaluate(() => globalThis.allowDeletes());
  };
  // Keep a real read-write transaction open so the editor's pre-save write
  // cannot commit until the test releases it.
  const holdWrites = (page) =>
    page.evaluate(
      () =>
        new Promise((resolve, reject) => {
          const opening = indexedDB.open('artifree-source-drafts', 1);
          opening.onerror = () => reject(opening.error);
          opening.onsuccess = () => {
            const db = opening.result;
            const tx = db.transaction('drafts', 'readwrite');
            const store = tx.objectStore('drafts');
            let held = true;
            globalThis.releaseDraftWrites = () => {
              held = false;
            };
            const next = () => {
              if (!held) {
                db.close();
                return;
              }
              const request = store.get(['test-lock', 'test-lock']);
              request.onsuccess = next;
              request.onerror = () => reject(request.error);
            };
            next();
            resolve();
          };
        }),
    );

  try {
    const a = await openTab();

    // A draft outlives a reload, also the bytes a rename moved. After the
    // reload the first listed artifact opens; the kept draft is named and
    // opens, and is recovered by choice.
    await a.open('Kept');
    await a.source();
    await a.text('index.html').fill('<p>kept 2</p>');
    await a.file('logo.png').click();
    await a.workspace.getByRole('button', { name: 'Rename file' }).click();
    await a.tab.getByRole('dialog').getByLabel('Path').fill('img/logo.png');
    await a.tab
      .getByRole('dialog')
      .getByRole('button', { name: 'Rename', exact: true })
      .click();
    await expect(a.file('img/logo.png')).toBeVisible();
    await keeps(a.tab, [
      {
        by: 'this tab',
        paths: ['img/logo.png', 'index.html', 'logo.png'],
        text: '<p>kept 2</p>',
      },
    ]);
    await a.tab.reload();
    await expect(
      a.workspace.getByRole('heading', { name: 'Other', exact: true }),
    ).toBeVisible();
    const notice = a.workspace.getByText('Drafts kept in this browser');
    await expect(notice).toBeVisible();
    await checkAccessibility('drafts-kept-notice', a.tab);
    // A list action cannot open the artifact while its discard is pending;
    // a failed discard leaves the record available to open.
    await holdWrites(a.tab);
    const allowListedDeletes = await abortDeletes(a.tab);
    const discardListed = a.workspace.getByRole('button', {
      name: 'Discard draft of Kept',
    });
    const openListed = a.workspace.getByRole('button', { name: 'Open Kept' });
    await discardListed.click();
    await expect(discardListed).toBeDisabled();
    await expect(openListed).toBeDisabled();
    await a.tab.evaluate(() => globalThis.releaseDraftWrites());
    await expect(
      a.workspace.getByText(/The draft of Kept was not discarded/u),
    ).toBeVisible();
    await allowListedDeletes();
    await expect(openListed).toBeEnabled();
    await a.workspace.getByRole('button', { name: 'Open Kept' }).click();
    await a.source();
    await expect(a.choice).toBeVisible();
    await checkAccessibility('drafts-choice', a.tab);
    await a.recover.click();
    await expect(a.text('index.html')).toHaveValue('<p>kept 2</p>');
    await expect(a.file('img/logo.png')).toBeVisible();
    await expect(a.file('logo.png')).toContainText('deleted');
    await a.save.click();
    await expect(
      a.workspace.getByText('Revision 2', { exact: true }),
    ).toBeVisible();
    assert.deepEqual(
      await bytes(
        server,
        `/v1/artifacts/${kept.id}/revisions/2/files/img/logo.png`,
      ),
      Buffer.from(PNG),
    );
    // A saved draft is no longer kept.
    await keeps(a.tab, []);
    await reopen(a);
    await expect(a.text('index.html')).toHaveValue('<p>kept 2</p>');
    await expect(a.choice).toHaveCount(0);
    await expect(notice).toHaveCount(0);

    // The owner id belongs to the page: after a reconnect, the page's kept
    // draft is still offered, and can be discarded instead.
    await a.text('index.html').fill('<p>to discard</p>');
    await keeps(a.tab, [text('<p>to discard</p>')]);
    await a.tab.reload();
    await a.workspace.getByRole('button', { name: 'Disconnect' }).click();
    await a.connect();
    await a.open('Kept');
    await a.source();
    await a.discardKept.click();
    await expect(a.text('index.html')).toHaveValue('<p>kept 2</p>');
    await keeps(a.tab, []);

    // A page holds its owner id from its start, also while disconnected:
    // another tab leaves its draft alone.
    await a.text('index.html').fill('<p>while away</p>');
    await keeps(a.tab, [text('<p>while away</p>')]);
    await a.tab.reload();
    await a.workspace.getByRole('button', { name: 'Disconnect' }).click();
    // Runtime disconnect is complete only after the server acknowledges it.
    await expect(a.workspace.getByLabel('Access token')).toBeVisible();
    await a.tab.reload();
    await expect(a.workspace.getByLabel('Access token')).toBeVisible();
    const b = await openTab();
    await b.open('Kept');
    await b.source();
    await expect(b.text('index.html')).toHaveValue('<p>kept 2</p>');
    await expect(b.choice).toHaveCount(0);
    await keeps(b.tab, [text('<p>while away</p>', 'another tab')]);
    await b.tab.close();
    await a.connect();
    await a.open('Kept');
    await a.source();
    await a.recover.click();
    await expect(a.text('index.html')).toHaveValue('<p>while away</p>');
    await a.discardChanges.click();
    await keeps(a.tab, []);

    // The page closes while a save is in flight, and the server commits
    // it. The kept draft says so, shows no "saved elsewhere" notice for
    // that revision, and saving it again replays the commit with its key.
    await a.text('index.html').fill('<p>in flight</p>');
    const saveKeys = [];
    a.tab.on('request', (request) => {
      if (
        request.method() === 'POST' &&
        new URL(request.url()).pathname === `/v1/artifacts/${kept.id}/revisions`
      )
        saveKeys.push(request.headers()['idempotency-key']);
    });
    let release;
    const released = new Promise((resolve) => {
      release = resolve;
    });
    let committed;
    const sent = new Promise((resolve) => {
      committed = resolve;
    });
    await a.tab.route('**/v1/artifacts/*/revisions', async (route) => {
      if (route.request().method() !== 'POST') return route.fallback();
      const response = await route.fetch();
      committed();
      await released;
      await route.fulfill({ response }).catch(() => {});
      return undefined;
    });
    await a.save.click();
    await sent;
    await a.tab.reload();
    release();
    await a.tab.unrouteAll({ behavior: 'ignoreErrors' });
    assert.deepEqual(await revisions(), [3, 2, 1]);
    await a.open('Kept');
    await a.source();
    await expect(
      a.workspace.getByText(/answer was not received/u),
    ).toBeVisible();
    await a.recover.click();
    await expect(
      a.workspace.getByText('This draft may already be saved'),
    ).toBeVisible();
    await expect(a.workspace.getByText(/was saved elsewhere/u)).toHaveCount(0);
    // A changed draft may be edited back to the exact request whose answer
    // was lost. Closing and reopening must still reuse that request's key.
    const beforeEdits = await keptVersion(a.tab);
    assert.equal(typeof beforeEdits, 'number');
    await a.text('index.html').fill('<p>temporary edit</p>');
    await a.save.click();
    await expect.poll(() => saveKeys.length).toBe(2);
    assert.notEqual(
      saveKeys[0],
      saveKeys[1],
      'Different requests use different keys.',
    );
    await expect(
      a.workspace.getByText('Not saved: the artifact changed'),
    ).toBeVisible();
    await a.text('index.html').fill('<p>in flight</p>');
    await expect.poll(() => keptVersion(a.tab)).toBe(beforeEdits + 2);
    await a.tab.reload();
    await a.open('Kept');
    await a.source();
    await a.recover.click();
    await expect(
      a.workspace.getByText('This draft may already be saved'),
    ).toBeVisible();
    // The server can replay successfully while local deletion fails. The
    // page keeps the draft and offers to retry removing it without sending
    // another save.
    const allowSavedRemoval = await refuseDeletes(a.tab);
    await a.save.click();
    await expect.poll(() => saveKeys.length).toBe(3);
    assert.equal(
      saveKeys[2],
      saveKeys[0],
      'The earlier unknown request reuses its key.',
    );
    let removal = a.workspace
      .getByRole('alert')
      .filter({ hasText: 'Could not remove the kept draft' });
    await expect(removal).toBeVisible();
    await expect(a.text('index.html')).toHaveValue('<p>in flight</p>');
    await keeps(a.tab, [text('<p>in flight</p>')]);
    await allowSavedRemoval();
    await removal.getByRole('button', { name: 'Try again' }).click();
    await expect(
      a.workspace.getByText('Revision 3', { exact: true }),
    ).toBeVisible();
    await expect(a.save).toBeDisabled();
    assert.equal(saveKeys.length, 3, 'Retrying removal sends no new save.');
    assert.deepEqual(
      await revisions(),
      [3, 2, 1],
      'The save replays; it does not commit twice.',
    );

    // A draft of a tab still open stays with it, also from a duplicate of
    // that tab, which takes an owner id of its own. Once the tab closes,
    // another may continue its draft; while writes are refused, the closed
    // tab's record, the only copy, stays.
    await a.text('index.html').fill('<p>first tab</p>');
    await keeps(a.tab, [text('<p>first tab</p>')]);
    const firstOwner = await a.tab.evaluate(
      (key) => sessionStorage.getItem(key),
      OWNER_KEY,
    );
    assert.notEqual(firstOwner, null);
    const c = await openTab(firstOwner);
    await c.open('Kept');
    await c.source();
    await expect(c.text('index.html')).toHaveValue('<p>in flight</p>');
    await expect(c.choice).toHaveCount(0);
    await keeps(c.tab, [text('<p>first tab</p>', 'another tab')]);
    await a.tab.close();
    await reopen(c);
    await expect(c.choice).toBeVisible();
    // Adoption is one transaction: a refused write must retain the old
    // owner's only copy. Wait for any operation holding that owner's lock
    // before observing the database, so a late delete cannot escape this
    // assertion.
    const allowAdoption = await refuseWrites(c.tab);
    await c.recover.click();
    await expect(c.notKept).toBeVisible();
    await c.tab.evaluate(
      (owner) =>
        navigator.locks.request(`artifree.draft-owner:${owner}`, () => {}),
      firstOwner,
    );
    await keeps(c.tab, [text('<p>first tab</p>', 'another tab')]);
    await allowAdoption();
    await reopen(c);
    await expect(c.choice).toBeVisible();
    // A discard in flight must not race a second choice. If deletion
    // fails, the same choice remains and the original record is intact.
    await holdWrites(c.tab);
    await c.discardKept.click();
    await expect(c.discardKept).toBeDisabled();
    await expect(c.recover).toBeDisabled();
    const allowChoiceDeletes = await refuseDeletes(c.tab);
    await c.tab.evaluate(() => globalThis.releaseDraftWrites());
    await expect(
      c.workspace.getByText(/The draft could not be deleted/u),
    ).toBeVisible();
    await allowChoiceDeletes();
    await expect(c.choice).toBeVisible();
    await keeps(c.tab, [text('<p>first tab</p>', 'another tab')]);
    // A formerly closed owner may return after this choice was listed.
    // Neither a live owner's record nor a newer version of that record may
    // be discarded from the stale choice.
    const observer = await openTab();
    await observer.open('Kept');
    await observer.source();
    await expect(observer.choice).toBeVisible();
    const returned = await openTab(firstOwner);
    await expect
      .poll(() =>
        returned.tab.evaluate(
          async ([key, owner]) => {
            const { held } = await navigator.locks.query();
            return (
              sessionStorage.getItem(key) === owner &&
              held.some((lock) => lock.name === `artifree.draft-owner:${owner}`)
            );
          },
          [OWNER_KEY, firstOwner],
        ),
      )
      .toBe(true);
    await c.discardKept.click();
    await expect(
      c.workspace.getByText(/Another tab is using this draft/u),
    ).toBeVisible();
    await keeps(c.tab, [text('<p>first tab</p>', 'another tab')]);
    await observer.recover.click();
    await expect(observer.notKept).toBeVisible();
    await keeps(observer.tab, [text('<p>first tab</p>', 'another tab')]);
    await returned.open('Kept');
    await returned.source();
    await returned.recover.click();
    await returned.text('index.html').fill('<p>owner returned</p>');
    await keeps(returned.tab, [text('<p>owner returned</p>')]);
    await returned.tab.close();
    await expect
      .poll(() =>
        observer.tab.evaluate(async (owner) => {
          const { held } = await navigator.locks.query();
          return held.some(
            (lock) => lock.name === `artifree.draft-owner:${owner}`,
          );
        }, firstOwner),
      )
      .toBe(false);
    await observer.text('index.html').fill('<p>old choice</p>');
    await expect(
      observer.workspace.getByText(/This draft changed/u),
    ).toBeVisible();
    await keeps(observer.tab, [text('<p>owner returned</p>', 'another tab')]);
    await observer.tab.close();
    await c.discardKept.click();
    await expect(c.workspace.getByText(/This draft changed/u)).toBeVisible();
    await keeps(c.tab, [text('<p>owner returned</p>', 'another tab')]);
    await reopen(c);
    const allowWrites = await refuseWrites(c.tab);
    await c.recover.click();
    await expect(c.text('index.html')).toHaveValue('<p>owner returned</p>');
    await expect(c.notKept).toBeVisible();
    await keeps(c.tab, [text('<p>owner returned</p>', 'another tab')]);
    await allowWrites();
    await c.text('index.html').fill('<p>continued</p>');
    await expect(c.notKept).toBeHidden();
    await keeps(c.tab, [text('<p>continued</p>')]);

    // With a newer draft of a closed tab on the same artifact, the tab's
    // own is offered first, since continuing the other would replace it.
    await rewrite(c.tab, 'seed');
    await reopen(c);
    await c.recover.click();
    await expect(c.text('index.html')).toHaveValue('<p>continued</p>');
    await keeps(c.tab, [
      text('<p>other tab</p>', 'another tab'),
      text('<p>continued</p>'),
    ]);
    await rewrite(c.tab, 'unseed');
    await keeps(c.tab, [text('<p>continued</p>')]);

    // A kept draft whose revision the server no longer lists with the same
    // files is kept as text: nothing of the revision is read or saved.
    await rewrite(c.tab, 'tamper');
    await reopen(c);
    await c.recover.click();
    await expect(
      c.workspace.getByText("The kept draft's revision has changed"),
    ).toBeVisible();
    await expect(c.save).toBeDisabled();
    await expect(c.text('index.html')).toBeEditable();
    await expect(
      c.workspace.getByRole('button', { name: 'Rename file' }),
    ).toBeDisabled();
    // Loading the latest revision ends the draft, also the one continued
    // from the closed tab.
    await c.workspace.getByRole('button', { name: 'Load latest' }).click();
    await expect(c.text('index.html')).toHaveValue('<p>in flight</p>');
    await keeps(c.tab, []);
    await reopen(c);
    await expect(c.text('index.html')).toBeVisible();
    await expect(c.choice).toHaveCount(0);

    // When the browser refuses to keep a draft, the page says so and keeps
    // editing in memory; the next change is kept once it is allowed.
    const allowAgain = await refuseWrites(c.tab);
    await c.text('index.html').fill('<p>over quota</p>');
    await expect(c.notKept).toBeVisible();
    await allowAgain();
    await c.text('index.html').fill('<p>kept again</p>');
    await expect(c.notKept).toBeHidden();
    await keeps(c.tab, [text('<p>kept again</p>')]);

    // When the kept drafts cannot be read, nothing is edited until the
    // user tries again or chooses to edit without them.
    await c.tab.evaluate(() =>
      sessionStorage.setItem('smoke.refuseReads', '1'),
    );
    await reopen(c);
    await expect(
      c.workspace.getByText('Drafts kept in this browser could not be read'),
    ).toBeVisible();
    await expect(c.text('index.html')).toHaveCount(0);
    await c.tab.evaluate(() => globalThis.allowReads());
    await c.workspace.getByRole('button', { name: 'Try again' }).click();
    await c.recover.click();
    await expect(c.text('index.html')).toHaveValue('<p>kept again</p>');
    // Returning the last edit to its original text also ends the draft.
    // A failed delete must not silently unlock the workspace.
    const allowCleanRemoval = await refuseDeletes(c.tab);
    await c.text('index.html').fill('<p>in flight</p>');
    removal = c.workspace
      .getByRole('alert')
      .filter({ hasText: 'Could not remove the kept draft' });
    await expect(removal).toBeVisible();
    await expect(
      c.workspace.getByRole('button', { name: 'Disconnect' }),
    ).toBeDisabled();
    await keeps(c.tab, [text('<p>kept again</p>')]);
    await allowCleanRemoval();
    await removal.getByRole('button', { name: 'Try again' }).click();
    await keeps(c.tab, []);
    await reopen(c);
    await expect(c.text('index.html')).toBeVisible();
    await expect(c.choice).toHaveCount(0);

    // A reset while the pre-save IndexedDB transaction is waiting makes
    // the base unsafe before any HTTP save was sent. Releasing the write
    // must not send it against potentially replaced server data.
    await c.text('index.html').fill('<p>wait before save</p>');
    await keeps(c.tab, [text('<p>wait before save</p>')]);
    await holdWrites(c.tab);
    const saves = [];
    c.tab.on('request', (request) => {
      if (
        request.method() === 'POST' &&
        new URL(request.url()).pathname === `/v1/artifacts/${kept.id}/revisions`
      )
        saves.push(request.url());
    });
    await c.save.click();
    await expect(
      c.workspace.getByRole('button', { name: 'Saving…' }),
    ).toBeVisible();
    await forgeReset(c.tab);
    await expect(
      c.workspace.getByText("The server's data may have been replaced"),
    ).toBeVisible();
    await expect(
      c.workspace.getByRole('button', { name: 'Load latest' }),
    ).toBeDisabled();
    await c.tab.evaluate(() => globalThis.releaseDraftWrites());
    await expect(c.save).toBeDisabled();
    assert.deepEqual(saves, [], 'No save was sent after the reset.');
    assert.deepEqual(await revisions(), [3, 2, 1]);
    await expect(c.text('index.html')).toHaveValue('<p>wait before save</p>');

    // The same preflight window can also cross a move to the trash without
    // changing the feed epoch. The stale save must not be sent then either.
    const allowLatestRemoval = await refuseDeletes(c.tab);
    await c.workspace.getByRole('button', { name: 'Load latest' }).click();
    removal = c.workspace
      .getByRole('alert')
      .filter({ hasText: 'Could not remove the kept draft' });
    await expect(removal).toBeVisible();
    await expect(c.text('index.html')).toHaveValue('<p>wait before save</p>');
    await allowLatestRemoval();
    await removal.getByRole('button', { name: 'Try again' }).click();
    await expect(c.text('index.html')).toHaveValue('<p>in flight</p>');
    await keeps(c.tab, []);
    await c.text('index.html').fill('<p>trash while waiting</p>');
    await keeps(c.tab, [text('<p>trash while waiting</p>')]);
    await holdWrites(c.tab);
    await c.save.click();
    await expect(
      c.workspace.getByRole('button', { name: 'Saving…' }),
    ).toBeVisible();
    const beforeTrash = await api(server, 'GET', `/v1/artifacts/${kept.id}`);
    await api(server, 'PATCH', `/v1/artifacts/${kept.id}`, {
      base_metadata_revision: beforeTrash.metadata_revision,
      deleted: true,
    });
    const inTrash = c.workspace.getByText('Moved to the trash elsewhere');
    await expect(inTrash).toBeVisible();
    await c.tab.evaluate(() => globalThis.releaseDraftWrites());
    await expect(c.save).toBeDisabled();
    assert.deepEqual(saves, [], 'No save was sent after the move to trash.');
    await expect(c.text('index.html')).toHaveValue(
      '<p>trash while waiting</p>',
    );
    const trashed = await api(
      server,
      'GET',
      '/v1/artifacts?deleted=true&q=Kept',
    );
    const inTrashRecord = trashed.artifacts.find((item) => item.id === kept.id);
    assert.ok(inTrashRecord);
    await api(server, 'PATCH', `/v1/artifacts/${kept.id}`, {
      base_metadata_revision: inTrashRecord.metadata_revision,
      deleted: false,
    });
    await expect(inTrash).toBeHidden();
    const allowDiscardRemoval = await refuseDeletes(c.tab);
    await c.discardChanges.click();
    removal = c.workspace
      .getByRole('alert')
      .filter({ hasText: 'Could not remove the kept draft' });
    await expect(removal).toBeVisible();
    await expect(c.text('index.html')).toHaveValue(
      '<p>trash while waiting</p>',
    );
    await allowDiscardRemoval();
    await removal.getByRole('button', { name: 'Try again' }).click();
    await keeps(c.tab, []);

    // The user can explicitly continue when the browser cannot delete the
    // kept copy, knowing that it will still be offered on reopening.
    await c.text('index.html').fill('<p>left in this browser</p>');
    await keeps(c.tab, [text('<p>left in this browser</p>')]);
    const allowExplicitRemoval = await refuseDeletes(c.tab);
    await c.discardChanges.click();
    removal = c.workspace
      .getByRole('alert')
      .filter({ hasText: 'Could not remove the kept draft' });
    await expect(removal).toContainText('may be offered again after a reload');
    await allowExplicitRemoval();
    await removal
      .getByRole('button', { name: 'Continue without removing it' })
      .click();
    await expect(
      c.workspace.getByRole('button', { name: 'Disconnect' }),
    ).toBeEnabled();
    await keeps(c.tab, [text('<p>left in this browser</p>')]);
    await reopen(c);
    await expect(c.choice).toBeVisible();
    await c.discardKept.click();
    await keeps(c.tab, []);

    // A save can complete before a reset while removal of its kept record
    // is still queued. The old answer must not clear the draft after reset.
    const clearing = await create('Clearing', [
      { path: 'index.html', encoding: 'utf8', content: '<p>base</p>' },
    ]);
    await c.open('Clearing');
    await c.source();
    await c.text('index.html').fill('<p>keep across reset</p>');
    await keeps(c.tab, [text('<p>keep across reset</p>')]);
    let receivedSave;
    const saveReceived = new Promise((resolve) => {
      receivedSave = resolve;
    });
    let beforeRemoval;
    await c.tab.route(
      `**/v1/artifacts/${clearing.id}/revisions`,
      async (route) => {
        if (route.request().method() !== 'POST') return route.fallback();
        const response = await route.fetch();
        beforeRemoval = await keptField(c.tab, 'storageVersion');
        await holdWrites(c.tab);
        await route.fulfill({ response });
        receivedSave();
        return undefined;
      },
    );
    await c.save.click();
    await saveReceived;
    assert.equal(typeof beforeRemoval, 'string');
    await forgeReset(c.tab);
    await expect(
      c.workspace.getByText("The server's data may have been replaced"),
    ).toBeVisible();
    await c.tab.evaluate(() => globalThis.releaseDraftWrites());
    await expect
      .poll(() => keptField(c.tab, 'storageVersion'))
      .not.toBe(beforeRemoval);
    await keeps(c.tab, [text('<p>keep across reset</p>')]);
    await expect(c.text('index.html')).toHaveValue('<p>keep across reset</p>');
    await expect(
      c.workspace.getByRole('button', { name: 'Disconnect' }),
    ).toBeDisabled();

    // A transient failure while claiming this page's owner must be
    // retried too; retrying only the database read would stay stuck.
    const retryTab = await context.newPage();
    await retryTab.addInitScript(() => {
      if (window !== window.top) return;
      const locks = navigator.locks;
      const request = locks.request.bind(locks);
      let fail = true;
      locks.request = (...args) =>
        fail
          ? Promise.reject(new Error('The owner could not be claimed.'))
          : request(...args);
      globalThis.allowOwner = () => {
        fail = false;
      };
    });
    await retryTab.goto(origin);
    const retryWorkspace = retryTab.locator('#artifacts');
    await retryWorkspace.getByLabel('Access token').fill(server.token);
    await retryWorkspace.getByRole('button', { name: 'Connect' }).click();
    await retryWorkspace
      .getByRole('navigation', { name: 'Artifacts' })
      .getByRole('button', { name: /^Kept / })
      .click();
    await retryWorkspace.getByRole('tab', { name: 'Source' }).click();
    await expect(
      retryWorkspace.getByText('Drafts kept in this browser could not be read'),
    ).toBeVisible();
    await retryTab.evaluate(() => globalThis.allowOwner());
    await retryWorkspace.getByRole('button', { name: 'Try again' }).click();
    await expect(
      retryWorkspace.getByLabel('index.html', { exact: true }),
    ).toHaveValue('<p>in flight</p>');
    await retryTab.close();

    // A view can be replaced while another tab's draft is being discarded.
    // Hold the owner's lock request before it opens a transaction, so the
    // new view must wait for that page-owned operation rather than reading
    // its old record. A failed discard leaves it available; a successful
    // one removes it before the editor opens.
    await create('Switch', [
      { path: 'index.html', encoding: 'utf8', content: '<p>switch base</p>' },
    ]);
    const ownerTab = await openTab();
    await ownerTab.open('Switch');
    await ownerTab.source();
    await ownerTab.text('index.html').fill('<p>switched draft</p>');
    await expect
      .poll(() => keptIn(ownerTab.tab))
      .toContainEqual(text('<p>switched draft</p>'));
    const oldOwner = await ownerTab.tab.evaluate(
      (key) => sessionStorage.getItem(key),
      OWNER_KEY,
    );
    assert.equal(typeof oldOwner, 'string');
    await ownerTab.tab.close();
    const switched = await openTab();
    await expect
      .poll(() =>
        switched.tab.evaluate(async (owner) => {
          const { held } = await navigator.locks.query();
          return held.some(
            (lock) => lock.name === `artifree.draft-owner:${owner}`,
          );
        }, oldOwner),
      )
      .toBe(false);
    await switched.open('Other');
    await switched.open('Switch');
    await switched.source();
    await expect(switched.choice).toBeVisible();
    const discardAcrossViews = async () => {
      await switched.tab.evaluate((owner) => {
        const locks = navigator.locks;
        const request = locks.request.bind(locks);
        globalThis.heldRemoval = false;
        locks.request = (...args) => {
          if (
            args[0] !== `artifree.draft-owner:${owner}` ||
            args[1]?.ifAvailable !== true
          )
            return request(...args);
          locks.request = request;
          globalThis.heldRemoval = true;
          return new Promise((resolve, reject) => {
            globalThis.endRemoval = (fail) => {
              if (fail) reject(new Error('The discard was refused.'));
              else void request(...args).then(resolve, reject);
            };
          });
        };
      }, oldOwner);
      await switched.discardKept.click();
      await expect
        .poll(() => switched.tab.evaluate(() => globalThis.heldRemoval))
        .toBe(true);
      await switched.open('Other');
      await switched.open('Switch');
      await switched.source();
      await expect(switched.choice).toHaveCount(0);
      await expect(
        switched.workspace.getByText(
          'Looking for a draft kept in this browser…',
        ),
      ).toBeVisible();
    };
    await discardAcrossViews();
    await switched.tab.evaluate(() => globalThis.endRemoval(true));
    await expect(switched.choice).toBeVisible();
    await expect
      .poll(() => keptIn(switched.tab))
      .toContainEqual(text('<p>switched draft</p>', 'another tab'));
    // If adoption failed and the user explicitly leaves the retained copy,
    // a later discard must end that adoption as well. A fresh edit must not
    // keep trying to replace the record the user has since discarded.
    const allowSwitchWrites = await refuseWrites(switched.tab);
    await switched.recover.click();
    await expect(switched.notKept).toBeVisible();
    const allowSwitchDeletes = await refuseDeletes(switched.tab);
    await switched.discardChanges.click();
    const switchRemoval = switched.workspace
      .getByRole('alert')
      .filter({ hasText: 'Could not remove the kept draft' });
    await expect(switchRemoval).toBeVisible();
    await switchRemoval
      .getByRole('button', { name: 'Continue without removing it' })
      .click();
    await allowSwitchDeletes();
    await allowSwitchWrites();
    await switched.open('Other');
    await switched.open('Switch');
    await switched.source();
    await expect(switched.choice).toBeVisible();
    await discardAcrossViews();
    await switched.tab.evaluate(() => globalThis.endRemoval(false));
    await expect(switched.text('index.html')).toHaveValue('<p>switch base</p>');
    await expect(switched.choice).toHaveCount(0);
    await switched.text('index.html').fill('<p>fresh after discard</p>');
    await expect
      .poll(() => keptIn(switched.tab))
      .toContainEqual(text('<p>fresh after discard</p>'));
    await expect(switched.notKept).toHaveCount(0);
    await switched.tab.close();

    // A failure to list other artifacts' drafts must not look like an empty
    // list. Once reads work again, retry reveals the still-kept draft.
    const listReader = await openTab();
    await listReader.tab.evaluate(() =>
      sessionStorage.setItem('smoke.refuseReads', '1'),
    );
    await listReader.tab.reload();
    await listReader.open('Other');
    const listFailure = listReader.workspace.getByText(
      'Drafts kept in this browser could not be listed',
    );
    await expect(listFailure).toBeVisible();
    await checkAccessibility('drafts-list-failure', listReader.tab);
    await listReader.tab.evaluate(() => globalThis.allowReads());
    await listReader.workspace
      .getByRole('button', { name: 'Try listing again' })
      .click();
    await expect(listFailure).toHaveCount(0);
    await expect(
      listReader.workspace.getByRole('button', { name: 'Open Switch' }),
    ).toBeVisible();
    await listReader.tab.close();

    assert.deepEqual(errors, []);
    return { revisions: 3 };
  } finally {
    await context.close();
  }
}
