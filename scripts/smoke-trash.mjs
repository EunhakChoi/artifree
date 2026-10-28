import assert from 'node:assert/strict';
import { expect } from '@playwright/test';
import { forgeReset } from './smoke-history.mjs';
import { setVisible } from './smoke-list.mjs';
import { api, hold } from './smoke-workspace.mjs';

/**
 * Browser checks for the trash (decision 0014) against a real
 * artifree-server: a move to the trash retried after a lost answer, the
 * trash and a recovery from it, deletes made elsewhere with and without a
 * draft, a refused recovery, a recovery across a reset, and the draft
 * locks. The page has its own browser context.
 */
export async function checkTrash({
  browser,
  origin,
  server,
  checkAccessibility,
}) {
  const create = async (title) =>
    (
      await api(server, 'POST', '/v1/artifacts', {
        title,
        files: [
          { path: 'index.html', encoding: 'utf8', content: `<p>${title}</p>` },
        ],
      })
    ).artifact;
  const demo = await create('Trash demo');
  const other = await create('Trash other');
  const path = (artifact) => `/v1/artifacts/${artifact.id}`;
  const metadata = async (artifact) =>
    (
      await api(
        server,
        'GET',
        `/v1/artifacts?deleted=true&q=${encodeURIComponent(artifact.title)}`,
      )
    ).artifacts[0]?.metadata_revision ??
    (await api(server, 'GET', path(artifact))).metadata_revision;
  const setDeleted = async (artifact, deleted) =>
    api(server, 'PATCH', path(artifact), {
      base_metadata_revision: await metadata(artifact),
      deleted,
    });
  const status = async (artifact) =>
    (
      await fetch(`${server.api_url}${path(artifact)}`, {
        headers: { authorization: `Bearer ${server.token}` },
      })
    ).status;

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
    const nav = workspace.getByRole('navigation', { name: 'Artifacts' });
    const trashNav = workspace.getByRole('navigation', { name: 'Trash' });
    const heading = (title) =>
      workspace.getByRole('heading', { name: title, exact: true });
    const show = workspace.getByLabel('Show');
    // List reads only, not reads of one artifact.
    const lists = (url) => url.pathname === '/v1/artifacts';
    const moveButton = workspace.getByRole('button', { name: 'Move to trash' });
    const dialog = tab.getByRole('dialog', {
      name: 'Move Trash demo to the trash',
    });
    await workspace.getByLabel('Search titles').fill('Trash');
    await nav.getByRole('button', { name: /^Trash demo/ }).click();
    await expect(heading('Trash demo')).toBeVisible();

    // A move whose outcome is unknown is resent as the same request. The
    // first attempt fails on the way: had it committed, the feed would
    // close the artifact before the retry (decision 0014, item 6).
    let lost = false;
    await tab.route(`**${path(demo)}`, async (route) => {
      if (lost || route.request().method() !== 'PATCH') return route.fallback();
      lost = true;
      return route.abort('connectionreset');
    });
    await moveButton.click();
    await expect(
      dialog.getByText(/nothing can read, preview, or change it/),
    ).toBeVisible();
    await checkAccessibility('workspace-trash-dialog', tab);
    const confirm = dialog.getByRole('button', { name: 'Move to trash' });
    await confirm.click();
    await expect(dialog.getByText('Outcome unknown')).toBeVisible();
    // The page closes it from its own answer, before any list is read
    // again. (Reads are sent one at a time, so the next artifact opens once
    // the list is.)
    // Without the feed it learns the move from its own answer alone.
    const feed = (url) => url.pathname === '/v1/changes';
    await tab.route(feed, (route) => route.abort());
    await setVisible(tab, false);
    await setVisible(tab, true);
    await expect(
      workspace.getByText(/^Reconnecting live updates/),
    ).toBeVisible();
    const ownMove = await hold(tab, 'GET', lists);
    await confirm.click();
    await expect(dialog).toBeHidden();
    await expect(heading('Trash demo')).toBeHidden();
    await ownMove();
    await expect(heading('Trash other')).toBeVisible();
    await tab.unroute(feed);
    await expect(workspace.getByText('Live updates on.')).toBeVisible({
      timeout: 30_000,
    });
    await tab.unrouteAll({ behavior: 'ignoreErrors' });
    await expect(
      workspace.getByText('Moved “Trash demo” to the trash.'),
    ).toBeVisible();
    await expect(nav.getByRole('button', { name: /^Trash demo/ })).toHaveCount(
      0,
    );
    assert.equal(await status(demo), 404, 'It is in the trash.');
    assert.equal(await metadata(demo), 2, 'The retry moved it once.');

    // The trash lists it; nothing there opens, and a recovery opens it.
    const searched = [];
    const record = (request) => {
      const url = new URL(request.url());
      if (url.pathname === '/v1/artifacts')
        searched.push(url.searchParams.get('q'));
    };
    tab.on('request', record);
    await workspace.getByRole('button', { name: 'Show trash' }).click();
    await expect(show).toHaveValue('trash');
    await expect(
      workspace.getByText(/Artifacts in the trash cannot be opened/),
    ).toBeVisible();
    await checkAccessibility('workspace-trash', tab);
    await trashNav.getByRole('button', { name: 'Recover Trash demo' }).click();
    await expect(show).toHaveValue('all');
    await expect(workspace.getByText('Recovered “Trash demo”.')).toBeVisible();
    // Showing the trash and leaving it keep the search the field shows.
    await expect(workspace.getByLabel('Search titles')).toHaveValue('Trash');
    await expect(nav.getByRole('button')).toHaveText([
      /^Trash other/,
      /^Trash demo/,
    ]);
    tab.off('request', record);
    assert.notDeepEqual(searched, []);
    assert.deepEqual(
      searched.filter((q) => q !== 'Trash'),
      [],
      'Every list read keeps the search.',
    );
    await expect(heading('Trash demo')).toBeVisible();
    assert.equal(await status(demo), 200);

    // Deleted elsewhere while open and listed first: it closes before any
    // list is read again, the page says where it went, and the first one
    // listed outside the trash opens.
    await api(server, 'POST', `${path(demo)}/revisions`, {
      base_revision: 1,
      changes: [
        {
          op: 'put',
          path: 'index.html',
          encoding: 'utf8',
          content: '<p>2</p>',
        },
      ],
    });
    await expect(nav.getByRole('button').first()).toHaveText(/^Trash demo/);
    const elsewhere = await hold(tab, 'GET', lists);
    await setDeleted(demo, true);
    await expect(
      workspace.getByText('The open artifact was moved to the trash.'),
    ).toBeVisible();
    await expect(heading('Trash demo')).toBeHidden();
    await elsewhere();
    await expect(heading('Trash other')).toBeVisible();

    // Deleted elsewhere while the editor holds a draft: it stays open with
    // the draft, says why it cannot be saved, and recovers in place.
    await nav.getByRole('button', { name: /^Trash other/ }).click();
    await expect(heading('Trash other')).toBeVisible();
    await workspace.getByRole('tab', { name: 'Source' }).click();
    const editor = workspace.getByLabel('index.html', { exact: true });
    await editor.fill('<p>draft</p>');
    await expect(moveButton).toBeDisabled();
    await expect(
      workspace.getByText(/Save or discard your changes to move/),
    ).toBeVisible();
    await expect(show.locator('option[value="trash"]')).toBeDisabled();
    await setDeleted(other, true);
    const inTrash = workspace
      .getByRole('alert')
      .filter({ hasText: 'In the trash' });
    await expect(inTrash.getByText(/Your draft is kept here/)).toBeVisible();
    await expect(editor).toHaveValue('<p>draft</p>');
    await inTrash.getByRole('button', { name: 'Recover' }).click();
    await expect(inTrash).toBeHidden();
    await expect(editor).toHaveValue('<p>draft</p>');
    assert.equal(await status(other), 200);
    await workspace.getByRole('button', { name: 'Discard changes' }).click();

    // A recovery from the trash that another change refused waits until
    // the list shows that change; it holds back no other artifact.
    await setDeleted(other, true);
    await show.selectOption('trash');
    const recoverDemo = trashNav.getByRole('button', {
      name: 'Recover Trash demo',
    });
    await expect(recoverDemo).toBeEnabled();
    const base = await metadata(demo);
    await tab.route(
      `**${path(demo)}`,
      (route) =>
        route.request().method() === 'PATCH'
          ? route.fulfill({
              status: 409,
              json: {
                error: {
                  code: 'metadata_conflict',
                  message: 'Changed elsewhere.',
                  current_metadata_revision: base + 2,
                },
              },
            })
          : route.fallback(),
      { times: 1 },
    );
    await recoverDemo.click();
    await expect(
      trashNav.getByText(/changed since this page read it/),
    ).toBeVisible();
    await expect(recoverDemo).toBeDisabled();
    await expect(
      trashNav.getByRole('button', { name: 'Recover Trash other' }),
    ).toBeEnabled();
    await setDeleted(demo, false);
    await setDeleted(demo, true);
    await expect(recoverDemo).toBeEnabled();

    // A trash read before a possible replacement of the server's data
    // recovers nothing until it is read again.
    const release = await hold(tab, 'GET', '**/v1/artifacts?*');
    await forgeReset(tab);
    await expect(trashNav.getByText(/Reading the trash again/)).toBeVisible();
    await expect(recoverDemo).toBeDisabled();
    await release();
    await expect(trashNav.getByText(/Reading the trash again/)).toBeHidden();
    await recoverDemo.click();
    await expect(heading('Trash demo')).toBeVisible();
    assert.equal(await status(demo), 200);

    assert.deepEqual(errors, [], 'Page errors.');
    return { recovered: 3 };
  } finally {
    await context.close();
  }
}
