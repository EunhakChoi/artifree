import assert from 'node:assert/strict';
import { expect } from '@playwright/test';
import { forgeReset } from './smoke-history.mjs';
import { setVisible } from './smoke-list.mjs';
import { api, hold } from './smoke-workspace.mjs';

/**
 * Browser checks for the trash in the web workspace (decision 0014) against
 * a real artifree-server. The page has its own browser context; deletes
 * and recoveries made elsewhere come from the API. Each step names the
 * moment it creates: a list read held, the feed cut, or a write answered
 * late, so that the page's behavior then is what is checked.
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
  // Created in this order, so the newest is listed first by creation.
  const one = await create('Bin one');
  const two = await create('Bin two');
  const three = await create('Bin three');
  const path = (artifact) => `/v1/artifacts/${artifact.id}`;
  // The metadata revision, in the trash or not.
  const metadata = async (artifact) => {
    const { artifacts } = await api(
      server,
      'GET',
      `/v1/artifacts?deleted=true&q=${encodeURIComponent(artifact.title)}`,
    );
    const trashed = artifacts.find(({ id }) => id === artifact.id);
    return (trashed ?? (await api(server, 'GET', path(artifact))))
      .metadata_revision;
  };
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
    // Every list read, with its search and order.
    const listReads = [];
    tab.on('request', (request) => {
      const url = new URL(request.url());
      if (request.method() === 'GET' && url.pathname === '/v1/artifacts')
        listReads.push(Object.fromEntries(url.searchParams));
    });
    await tab.goto(origin);
    const workspace = tab.locator('#artifacts');
    await workspace.getByLabel('Access token').fill(server.token);
    await workspace.getByRole('button', { name: 'Connect' }).click();
    const nav = workspace.getByRole('navigation', { name: 'Artifacts' });
    const trashNav = workspace.getByRole('navigation', { name: 'Trash' });
    const item = (artifact) =>
      nav.getByRole('button', { name: new RegExp(`^${artifact.title} `) });
    const heading = (artifact) =>
      workspace.getByRole('heading', { name: artifact.title, exact: true });
    const show = workspace.getByLabel('Show');
    const order = workspace.getByLabel('Order');
    const moveButton = workspace.getByRole('button', {
      name: 'Move to trash',
    });
    const recoverIn = (artifact) =>
      trashNav.getByRole('button', { name: `Recover ${artifact.title}` });
    const lists = (url) => url.pathname === '/v1/artifacts';
    const editor = workspace.getByLabel('index.html');
    const draft = async (artifact, text) => {
      await item(artifact).click();
      await expect(heading(artifact)).toBeVisible();
      await workspace.getByRole('tab', { name: 'Source' }).click();
      await editor.fill(text);
    };
    await workspace.getByLabel('Search titles').fill('Bin');
    await expect(item(three)).toBeVisible();

    // A draft locks the move and the trash, which would close it.
    await draft(one, '<p>draft</p>');
    await expect(moveButton).toBeDisabled();
    await expect(
      workspace.getByText(/to move this artifact to the trash/),
    ).toBeVisible();
    await expect(show.locator('option[value="trash"]')).toBeDisabled();
    await workspace.getByRole('button', { name: 'Discard changes' }).click();
    await expect(moveButton).toBeEnabled();

    // With the feed cut, a move commits but its answer is lost. The retry
    // is the same request under the same key, so the server moves it once.
    // The page learns the move from that answer alone: it closes the
    // artifact and leaves it out of the list before the list is read again.
    const feed = (url) => url.pathname === '/v1/changes';
    await tab.route(feed, (route) => route.abort());
    await setVisible(tab, false);
    await setVisible(tab, true);
    await expect(
      workspace.getByText(/^Reconnecting live updates/),
    ).toBeVisible();
    const keys = [];
    await tab.route(`**${path(one)}`, async (route) => {
      if (route.request().method() !== 'PATCH') return route.fallback();
      keys.push(route.request().headers()['idempotency-key']);
      if (keys.length > 1) return route.fallback();
      await route.fetch();
      return route.abort('connectionreset');
    });
    await moveButton.click();
    const dialog = tab.getByRole('dialog', {
      name: 'Move Bin one to the trash',
    });
    await expect(
      dialog.getByText(/nothing can read, preview, or change it/),
    ).toBeVisible();
    await checkAccessibility('workspace-trash-dialog', tab);
    const confirm = dialog.getByRole('button', { name: 'Move to trash' });
    await confirm.click();
    await expect(dialog.getByText('Outcome unknown')).toBeVisible();
    assert.equal(await status(one), 404, 'The lost attempt committed.');
    const listsHeld = await hold(tab, 'GET', lists);
    await confirm.click();
    await expect(dialog).toBeHidden();
    await expect(heading(one)).toBeHidden();
    await expect(item(one)).toHaveCount(0);
    await expect(
      workspace.getByText('Moved “Bin one” to the trash.'),
    ).toBeVisible();
    await listsHeld();
    await tab.unroute(feed);
    await tab.unroute(`**${path(one)}`);
    await expect(workspace.getByText('Live updates on.')).toBeVisible({
      timeout: 30_000,
    });
    assert.deepEqual(
      [keys.length, keys[0] === keys[1], await metadata(one)],
      [2, true, 2],
      'The retry reused the key and moved it once.',
    );

    // The trash lists it with the search kept; nothing there opens.
    listReads.length = 0;
    await workspace.getByRole('button', { name: 'Show trash' }).click();
    await expect(show).toHaveValue('trash');
    await expect(recoverIn(one)).toBeVisible();
    await expect(workspace.getByLabel('Search titles')).toHaveValue('Bin');
    await expect(
      workspace.getByText(/cannot be opened, previewed, or changed/),
    ).toBeVisible();
    await checkAccessibility('workspace-trash', tab);

    // A recovery answered late opens the artifact with the search and order
    // the page shows then, not those it was sent with.
    const recoveryHeld = await hold(tab, 'PATCH', `**${path(one)}`);
    await recoverIn(one).click();
    await order.selectOption('created');
    await expect(order).toHaveValue('created');
    await recoveryHeld();
    await expect(workspace.getByText('Recovered “Bin one”.')).toBeVisible();
    await expect(show).toHaveValue('all');
    await expect(order).toHaveValue('created');
    await expect(heading(one)).toBeVisible();
    assert.ok(
      listReads.length > 0 &&
        listReads.every((read) => read.q === 'Bin') &&
        listReads.at(-1).sort === 'created',
      `Every list read kept the search: ${JSON.stringify(listReads)}`,
    );

    // Deleted elsewhere while open and listed first: before the list is
    // read again, it closes, says why, leaves the list, and the next one
    // outside the trash is chosen. (Reads are sent one at a time, so that
    // one is read once the list is.)
    await item(three).click();
    await expect(heading(three)).toBeVisible();
    const elsewhereHeld = await hold(tab, 'GET', lists);
    await setDeleted(three, true);
    await expect(heading(three)).toBeHidden();
    await expect(
      workspace.getByText('The open artifact was moved to the trash.'),
    ).toBeVisible();
    await expect(item(three)).toHaveCount(0);
    await expect(item(two)).toHaveAttribute('aria-current', 'true');
    await elsewhereHeld();
    await expect(heading(two)).toBeVisible();

    // With a draft it stays open, keeps the draft, and recovers in place.
    await draft(two, '<p>kept</p>');
    await setDeleted(two, true);
    const inTrash = workspace
      .getByRole('alert')
      .filter({ hasText: 'Moved to the trash elsewhere' });
    await expect(inTrash).toBeVisible();
    await expect(editor).toHaveValue('<p>kept</p>');
    await expect(
      workspace.getByRole('button', { name: 'Save revision' }),
    ).toBeDisabled();
    await checkAccessibility('workspace-trash-draft', tab);
    // Nothing is read for it: the preview and the history say why instead.
    const previews = [];
    const recordPreview = (request) => {
      if (
        request.method() === 'POST' &&
        new URL(request.url()).pathname.endsWith('/previews')
      )
        previews.push(request.url());
    };
    tab.on('request', recordPreview);
    await workspace.getByRole('tab', { name: 'Preview' }).click();
    await expect(
      workspace.getByText(
        'This artifact is in the trash. Recover it to preview it.',
      ),
    ).toBeVisible();
    await workspace.getByRole('tab', { name: 'History' }).click();
    await expect(
      workspace.getByText(/Recover it to compare or restore its revisions/),
    ).toBeVisible();
    tab.off('request', recordPreview);
    assert.deepEqual(previews, [], 'Nothing is previewed in the trash.');
    await workspace.getByRole('tab', { name: 'Source' }).click();
    await inTrash.getByRole('button', { name: 'Recover', exact: true }).click();
    await expect(inTrash).toBeHidden();
    await expect(workspace.getByText('Recovered “Bin two”.')).toBeVisible();
    await expect(editor).toHaveValue('<p>kept</p>');
    await expect(
      workspace.getByRole('button', { name: 'Save revision' }),
    ).toBeEnabled();
    // Recovered, it can be previewed again.
    await workspace.getByRole('tab', { name: 'Preview' }).click();
    await expect(
      workspace.locator('iframe[title^="Preview of Bin two"]'),
    ).toBeVisible();
    await workspace.getByRole('tab', { name: 'Source' }).click();
    await expect(editor).toHaveValue('<p>kept</p>');
    await workspace.getByRole('button', { name: 'Discard changes' }).click();

    // A recovery another change refused waits for that change, and holds
    // back no other artifact in the trash.
    await setDeleted(one, true);
    await show.selectOption('trash');
    await expect(recoverIn(three)).toBeVisible();
    await tab.route(
      `**${path(three)}`,
      async (route) =>
        route.request().method() === 'PATCH'
          ? route.fulfill({
              status: 409,
              json: {
                error: {
                  code: 'metadata_conflict',
                  message: 'Changed elsewhere.',
                  current_metadata_revision: (await metadata(three)) + 1,
                },
              },
            })
          : route.fallback(),
      { times: 1 },
    );
    await recoverIn(three).click();
    await expect(
      trashNav.getByText(/changed since this page read it/),
    ).toBeVisible();
    await expect(recoverIn(three)).toBeDisabled();
    await expect(recoverIn(one)).toBeEnabled();

    // A trash read before a possible replacement of the server's data
    // recovers nothing until the trash is read again; what a conflict
    // proved before it no longer holds.
    const resetHeld = await hold(tab, 'GET', lists);
    await forgeReset(tab);
    await expect(recoverIn(one)).toBeDisabled();
    await resetHeld();
    await expect(recoverIn(one)).toBeEnabled();
    await expect(recoverIn(three)).toBeEnabled();

    // A recovery answered after the page left the trash keeps what the page
    // shows, a draft included.
    const lateHeld = await hold(tab, 'PATCH', `**${path(three)}`);
    await recoverIn(three).click();
    await show.selectOption('all');
    await draft(two, '<p>meanwhile</p>');
    await lateHeld();
    await expect(workspace.getByText('Recovered “Bin three”.')).toBeVisible();
    await expect(heading(two)).toBeVisible();
    await expect(editor).toHaveValue('<p>meanwhile</p>');
    await workspace.getByRole('button', { name: 'Discard changes' }).click();
    await expect(item(three)).toBeVisible();

    assert.deepEqual(errors, [], 'Page errors.');
    return { moved: 2, recovered: 3 };
  } finally {
    await context.close();
  }
}
