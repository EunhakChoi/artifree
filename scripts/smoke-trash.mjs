import assert from 'node:assert/strict';
import { expect } from '@playwright/test';
import { forgeReset } from './smoke-history.mjs';
import { setVisible } from './smoke-list.mjs';
import { api, hold } from './smoke-workspace.mjs';

/**
 * Browser checks for the trash in the web workspace (decision 0014) against
 * a real artifree-server. The page has its own browser context; moves and
 * recoveries made elsewhere come from the API. Each step creates one
 * moment, such as a list read held, the change feed cut, or an answer held
 * across a reset, and checks what the page does then.
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
  // Created in this order, so the last is listed first.
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
    const recoverIn = (artifact) =>
      trashNav.getByRole('button', { name: `Recover ${artifact.title}` });
    const search = workspace.getByLabel('Search titles');
    const show = workspace.getByLabel('Show');
    const order = workspace.getByLabel('Order');
    const moveButton = workspace.getByRole('button', {
      name: 'Move to trash',
    });
    const editor = workspace.getByLabel('index.html');
    const saveButton = workspace.getByRole('button', {
      name: 'Save revision',
    });
    const discard = workspace.getByRole('button', { name: 'Discard changes' });
    const lists = (url) => url.pathname === '/v1/artifacts';
    const feed = (url) => url.pathname === '/v1/changes';
    const live = workspace.getByText('Live updates on.');
    // The change feed is refused until restored; the page reconnects.
    const cutFeed = async () => {
      await tab.route(feed, (route) => route.abort());
      await setVisible(tab, false);
      await setVisible(tab, true);
      await expect(
        workspace.getByText(/^Reconnecting live updates/),
      ).toBeVisible();
    };
    const restoreFeed = async () => {
      await tab.unroute(feed);
      await setVisible(tab, false);
      await setVisible(tab, true);
      await expect(live).toBeVisible({ timeout: 30_000 });
    };
    const draft = async (artifact, text) => {
      await item(artifact).click();
      await expect(heading(artifact)).toBeVisible();
      await workspace.getByRole('tab', { name: 'Source' }).click();
      await editor.fill(text);
    };
    const patched = (artifact) =>
      tab.waitForResponse(
        (response) =>
          response.request().method() === 'PATCH' &&
          new URL(response.url()).pathname === path(artifact),
      );
    await search.fill('Bin');
    await expect(item(three)).toBeVisible();

    // A draft locks the move and the trash, which would close it.
    await draft(one, '<p>draft</p>');
    await expect(moveButton).toBeDisabled();
    await expect(
      workspace.getByText(/to move this artifact to the trash/),
    ).toBeVisible();
    await expect(show.locator('option[value="trash"]')).toBeDisabled();
    await discard.click();
    await expect(moveButton).toBeEnabled();

    // With the feed cut, a move commits but its answer is lost. The retry
    // is the same request under the same key, so the server moves it once.
    // From that answer alone the page closes the artifact and leaves it out
    // of the list, before the list is read again.
    await cutFeed();
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
    const movedHeld = await hold(tab, 'GET', lists);
    await confirm.click();
    await expect(dialog).toBeHidden();
    await expect(heading(one)).toBeHidden();
    await expect(item(one)).toHaveCount(0);
    await expect(
      workspace.getByText('Moved “Bin one” to the trash.'),
    ).toBeVisible();
    await movedHeld();
    await tab.unroute(`**${path(one)}`);
    await restoreFeed();
    assert.deepEqual(
      [keys.length, keys[0] === keys[1], await metadata(one)],
      [2, true, 2],
      'The retry reused the key and moved it once.',
    );

    // The trash lists it with the search kept; nothing there opens.
    await workspace.getByRole('button', { name: 'Show trash' }).click();
    await expect(show).toHaveValue('trash');
    await expect(recoverIn(one)).toBeVisible();
    await expect(search).toHaveValue('Bin');
    await expect(
      workspace.getByText(/cannot be opened, previewed, or changed/),
    ).toBeVisible();
    await checkAccessibility('workspace-trash', tab);
    // While another search's list loads, the fields stay, with the text
    // being typed and the way back.
    const searchHeld = await hold(tab, 'GET', lists);
    await search.fill('Bin o');
    await tab.waitForRequest(
      (request) => new URL(request.url()).searchParams.get('q') === 'Bin o',
    );
    await expect(search).toBeFocused();
    await expect(show).toHaveValue('trash');
    await searchHeld();
    await expect(recoverIn(one)).toBeVisible();
    const searchedBack = tab.waitForResponse((response) => {
      const url = new URL(response.url());
      return lists(url) && url.searchParams.get('q') === 'Bin';
    });
    await search.fill('Bin');
    await searchedBack;

    // A recovery answered late opens the artifact with the search and order
    // the page shows then, not those it was sent with.
    listReads.length = 0;
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

    // Moved elsewhere while open and listed first: before the list is read
    // again, it closes, says why, leaves the list, and the next one outside
    // the trash is chosen. (Reads are sent one at a time, so that one is
    // read once the list is.)
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

    // With a draft it stays open and keeps the draft, which cannot be
    // saved. Nothing is read for it: the preview and the history say why.
    await draft(two, '<p>kept</p>');
    const readsInTrash = [];
    const recordRead = (request) => {
      const url = new URL(request.url());
      if (
        url.pathname.startsWith(path(two)) &&
        (request.method() === 'GET' || url.pathname.endsWith('/previews'))
      )
        readsInTrash.push(`${request.method()} ${url.pathname}`);
    };
    tab.on('request', recordRead);
    await setDeleted(two, true);
    const inTrash = workspace
      .getByRole('alert')
      .filter({ hasText: 'Moved to the trash elsewhere' });
    const recoverHere = inTrash.getByRole('button', {
      name: 'Recover',
      exact: true,
    });
    await expect(inTrash).toBeVisible();
    await expect(editor).toHaveValue('<p>kept</p>');
    await expect(saveButton).toBeDisabled();
    await checkAccessibility('workspace-trash-draft', tab);
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
    await workspace.getByRole('tab', { name: 'Source' }).click();
    tab.off('request', recordRead);
    assert.deepEqual(readsInTrash, [], 'Nothing is read for it in the trash.');

    // Without the feed, it was recovered and moved again elsewhere. The
    // recovery is refused; one read finds it still in the trash, at the
    // revision the refusal named, and the next recovery is sent from there.
    // The draft stays editable throughout.
    await cutFeed();
    await setDeleted(two, false);
    await setDeleted(two, true);
    const refused = patched(two);
    await recoverHere.click();
    expect((await refused).status()).toBe(409);
    await expect(
      inTrash.getByText(/changed since this page read it/),
    ).toBeVisible();
    await expect(editor).toBeEditable();
    await expect(discard).toBeVisible();
    await expect(recoverHere).toBeEnabled();
    await recoverHere.click();
    await expect(inTrash).toBeHidden();
    await expect(workspace.getByText('Recovered “Bin two”.')).toBeVisible();
    await expect(editor).toHaveValue('<p>kept</p>');
    await expect(saveButton).toBeEnabled();
    await restoreFeed();
    // Recovered, it can be previewed again.
    await workspace.getByRole('tab', { name: 'Preview' }).click();
    await expect(
      workspace.locator('iframe[title^="Preview of Bin two"]'),
    ).toBeVisible();
    await workspace.getByRole('tab', { name: 'Source' }).click();
    await expect(editor).toHaveValue('<p>kept</p>');

    // Without the feed, it was recovered elsewhere. The recovery is
    // refused; one read finds it outside the trash, so the draft can be
    // saved again.
    await setDeleted(two, true);
    await expect(inTrash).toBeVisible();
    await cutFeed();
    await setDeleted(two, false);
    await recoverHere.click();
    await expect(inTrash).toBeHidden();
    await expect(saveButton).toBeEnabled();
    await expect(editor).toHaveValue('<p>kept</p>');
    await restoreFeed();

    // What the page learned of the trash ends with a possible replacement
    // of the server's data, before anything is read again: the record
    // could name revisions the server no longer has. Recovered while the
    // feed was away, it keeps the draft, which waits as any draft does
    // after a replacement.
    await setDeleted(two, true);
    await expect(inTrash).toBeVisible();
    await cutFeed();
    await setDeleted(two, false);
    await tab.unroute(feed);
    const replacedListHeld = await hold(tab, 'GET', lists);
    await forgeReset(tab);
    await expect(live).toBeVisible({ timeout: 30_000 });
    await expect(inTrash).toBeHidden();
    await replacedListHeld();
    await expect(
      workspace.getByText("The server's data may have been replaced"),
    ).toBeVisible();
    await expect(editor).toHaveValue('<p>kept</p>');
    await workspace.getByRole('button', { name: 'Load latest' }).click();
    await expect(editor).toHaveValue('<p>Bin two</p>');

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

    // A recovery answered after a possible replacement does not move the
    // page: the trash it reads again says what is there.
    const staleListHeld = await hold(tab, 'GET', lists);
    const staleAnswerHeld = await hold(tab, 'PATCH', `**${path(one)}`);
    await recoverIn(one).click();
    await forgeReset(tab);
    await expect(recoverIn(three)).toBeDisabled();
    const lateRecovery = patched(one);
    await staleAnswerHeld();
    expect((await lateRecovery).status()).toBe(200);
    await staleListHeld();
    await expect(recoverIn(three)).toBeEnabled();
    await expect(recoverIn(one)).toHaveCount(0);
    await expect(show).toHaveValue('trash');
    await expect(workspace.getByText('Recovered “Bin one”.')).toHaveCount(0);

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
    await discard.click();
    await expect(item(three)).toBeVisible();

    // A move answered after a possible replacement says nothing of the
    // move: the list read again says what is there.
    await item(three).click();
    await expect(heading(three)).toBeVisible();
    const moveHeld = await hold(tab, 'PATCH', `**${path(three)}`);
    const relistHeld = await hold(tab, 'GET', lists);
    await moveButton.click();
    await tab
      .getByRole('dialog')
      .getByRole('button', { name: 'Move to trash' })
      .click();
    // The page lists again only once it has taken the reset.
    const relisted = tab.waitForRequest(
      (request) => request.method() === 'GET' && lists(new URL(request.url())),
    );
    await forgeReset(tab);
    await relisted;
    const lateMove = patched(three);
    await moveHeld();
    expect((await lateMove).status()).toBe(200);
    await relistHeld();
    await expect(item(three)).toHaveCount(0);
    await expect(
      workspace.getByText('Moved “Bin three” to the trash.'),
    ).toHaveCount(0);

    assert.deepEqual(errors, [], 'Page errors.');
    return { moved: 2, recovered: 4 };
  } finally {
    await context.close();
  }
}
