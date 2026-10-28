import assert from 'node:assert/strict';
import { expect } from '@playwright/test';
import { api } from './smoke-workspace.mjs';

/**
 * Browser checks for the web list and artifact metadata (decision 0009)
 * against a real artifree-server: search, order, pin filter, more pages,
 * renaming and pinning, and their failures. The page has its own browser
 * context; other clients' changes come from the API.
 */
export async function checkListAndMetadata({
  browser,
  origin,
  server,
  checkAccessibility,
}) {
  const created = async (title) =>
    (
      await api(server, 'POST', '/v1/artifacts', {
        title,
        files: [file(title)],
      })
    ).artifact;
  const alpha = await created('Alpha notes');
  const beta = await created('Beta notes');
  // More than one page, all newer than the artifacts above.
  for (let index = 1; index <= 51; index += 1)
    await created(`Bulk ${String(index).padStart(2, '0')}`);

  const context = await browser.newContext();
  try {
    const page = await context.newPage();
    page.setDefaultTimeout(10_000);
    const errors = [];
    page.on('pageerror', (error) => errors.push(error.message));
    const lists = [];
    const reads = [];
    page.on('request', (request) => {
      const url = new URL(request.url());
      if (request.method() !== 'GET' || !url.pathname.startsWith('/v1/'))
        return;
      const parts = url.pathname.split('/').filter(Boolean);
      if (parts[1] !== 'artifacts') return;
      if (parts.length === 2) lists.push(Object.fromEntries(url.searchParams));
      reads.push(parts.length === 2 ? 'list' : parts.slice(3).join('/'));
    });
    await page.goto(origin);
    const workspace = page.locator('#artifacts');
    await workspace.getByLabel('Access token').fill(server.token);
    await workspace.getByRole('button', { name: 'Connect' }).click();
    const nav = workspace.getByRole('navigation', { name: 'Artifacts' });
    const items = nav.getByRole('button', { name: / r\d+$/ });
    const heading = (name) => workspace.getByRole('heading', { name });
    const quiet = () => page.waitForTimeout(800);

    // A first page of 50, then more on request.
    await expect(items).toHaveCount(50);
    await expect(heading('Bulk 51')).toBeVisible();
    await nav.getByRole('button', { name: 'Show more' }).click();
    await expect(
      nav.getByRole('button', { name: /Alpha notes/ }),
    ).toBeVisible();
    await expect(nav.getByRole('button', { name: 'Show more' })).toBeHidden();
    const shown = await items.count();
    assert.ok(shown > 53, `Every artifact is shown: ${shown}.`);

    // A commit elsewhere reads the list again, as long as it was.
    await quiet();
    let mark = lists.length;
    await api(server, 'POST', `/v1/artifacts/${alpha.id}/revisions`, {
      base_revision: 1,
      changes: [{ op: 'put', ...file('Alpha 2') }],
    });
    await expect(items.first()).toHaveAccessibleName(/^Alpha notes/);
    await expect(items).toHaveCount(shown);
    await quiet();
    assert.equal(lists.length - mark, 1, 'One list read for a commit.');

    // Searching waits for typing to pause; the open artifact stays open.
    mark = lists.length;
    await workspace
      .getByLabel('Search titles')
      .pressSequentially('notes', { delay: 40 });
    await expect(items).toHaveCount(2);
    await quiet();
    assert.deepEqual(
      lists.slice(mark).map(({ q }) => q),
      ['notes'],
      'One search for the whole text.',
    );
    await expect(heading('Bulk 51')).toBeVisible();
    await expect(nav.locator('[aria-current="true"]')).toHaveCount(0);

    // Pinning changes no revision; the pin filter lists it. The page shows
    // its own update without waiting for the notice, held here.
    await nav.getByRole('button', { name: /Beta notes/ }).click();
    await expect(heading('Beta notes')).toBeVisible();
    let releaseFeed;
    const feedHeld = new Promise((resolve) => {
      releaseFeed = resolve;
    });
    let streamHeld;
    const streamArrived = new Promise((resolve) => {
      streamHeld = resolve;
    });
    await page.route('**/v1/changes*', (route) => {
      streamHeld();
      // The page may have cancelled the request by then.
      return feedHeld.then(() => route.abort().catch(() => undefined));
    });
    await setVisible(page, false);
    await setVisible(page, true);
    await streamArrived;
    // The list read that follows fails too, so only this page's own answer
    // can tell it about the pin.
    await page.route(
      (url) => url.pathname === '/v1/artifacts',
      (route) =>
        route.fulfill({
          status: 503,
          json: { error: { code: 'unavailable', message: 'List is busy.' } },
        }),
      { times: 1 },
    );
    const pin = workspace.getByRole('button', { name: 'Pin', exact: true });
    await pin.click();
    await expect(pin).toHaveAttribute('aria-pressed', 'true');
    await expect(workspace.getByText('Could not list artifacts')).toBeVisible();
    releaseFeed();
    await page.unroute('**/v1/changes*');
    await nav.getByRole('button', { name: 'Try again' }).click();
    await expect(
      nav.getByRole('button', { name: /Beta notes Pinned, r1/ }),
    ).toBeVisible();
    await workspace.getByLabel('Search titles').fill('');
    await expect(items).toHaveCount(50);
    // A failed read for another filter shows no artifacts of the last one.
    await page.route(
      (url) =>
        url.pathname === '/v1/artifacts' &&
        url.searchParams.get('pinned') === 'true',
      (route) =>
        route.fulfill({
          status: 503,
          json: { error: { code: 'unavailable', message: 'List is busy.' } },
        }),
      { times: 1 },
    );
    await workspace.getByLabel('Show').selectOption('pinned');
    await expect(workspace.getByText('Could not list artifacts')).toBeVisible();
    await expect(items).toHaveCount(0);
    await expect(nav.getByRole('button', { name: 'Show more' })).toBeHidden();
    await nav.getByRole('button', { name: 'Try again' }).click();
    await expect(items).toHaveCount(1);
    await expect(items.first()).toHaveAccessibleName(/^Beta notes/);
    await workspace.getByLabel('Show').selectOption('all');

    // Order by creation keeps the newest created first.
    await workspace.getByLabel('Order').selectOption('created');
    await expect(items.first()).toHaveAccessibleName(/^Bulk 51/);
    await workspace.getByLabel('Order').selectOption('updated');
    await expect(items.first()).toHaveAccessibleName(/^Alpha notes/);

    // Renaming keeps a draft and reads the list and the open artifact once.
    await nav.getByRole('button', { name: /Alpha notes/ }).click();
    await expect(heading('Alpha notes')).toBeVisible();
    await workspace.getByRole('tab', { name: 'Source' }).click();
    const editor = workspace.getByLabel('index.html');
    await editor.fill(html('Draft'));
    await quiet();
    mark = reads.length;
    const renamed = await rename(
      page,
      workspace,
      'Alpha renamed',
      checkAccessibility,
    );
    await expect(renamed).toBeHidden();
    await expect(heading('Alpha renamed')).toBeVisible();
    await expect(
      nav.getByRole('button', { name: /Alpha renamed/ }),
    ).toBeVisible();
    await expect(editor).toHaveValue(html('Draft'));
    await quiet();
    assert.deepEqual(
      reads.slice(mark).sort(),
      ['', 'list', 'revisions'],
      'A rename reads the list, the artifact, and its history once.',
    );
    let current = await api(server, 'GET', `/v1/artifacts/${alpha.id}`);
    assert.equal(current.current_revision, 2, 'A rename adds no revision.');
    assert.equal(current.metadata_revision, 2);

    // A rename whose answer was lost is sent again with its key: it
    // applies once.
    let lost = false;
    await page.route(`**/v1/artifacts/${alpha.id}`, async (route) => {
      if (lost || route.request().method() !== 'PATCH') return route.fallback();
      lost = true;
      await route.fetch();
      return route.fulfill({
        status: 503,
        json: { error: { code: 'unavailable', message: 'Response lost.' } },
      });
    });
    const dialog = await rename(page, workspace, 'Alpha final');
    await expect(dialog.getByText('Outcome unknown')).toBeVisible();
    await expect(dialog.getByText(/Response lost\./)).toBeVisible();
    await dialog.getByRole('button', { name: 'Rename' }).click();
    await expect(dialog).toBeHidden();
    await expect(heading('Alpha final')).toBeVisible();
    current = await api(server, 'GET', `/v1/artifacts/${alpha.id}`);
    assert.equal(current.metadata_revision, 3, 'The retry applied once.');

    // An update another client won changes nothing; the latest is read.
    await page.route(`**/v1/artifacts/${alpha.id}`, (route) =>
      route.request().method() === 'PATCH'
        ? route.fulfill({
            status: 409,
            json: {
              error: {
                code: 'metadata_conflict',
                message: 'Changed elsewhere.',
                current_metadata_revision: 4,
              },
            },
          })
        : route.fallback(),
    );
    await quiet();
    mark = reads.length;
    await pin.click();
    await expect(workspace.getByText('Changed elsewhere')).toBeVisible();
    await expect(pin).toHaveAttribute('aria-pressed', 'false');
    await quiet();
    assert.deepEqual(reads.slice(mark).sort(), ['', 'revisions']);
    await page.unrouteAll({ behavior: 'ignoreErrors' });
    await pin.click();
    await expect(pin).toHaveAttribute('aria-pressed', 'true');
    await expect(workspace.getByText('Changed elsewhere')).toBeHidden();
    await expect(editor).toHaveValue(html('Draft'));

    // A pin whose answer was lost: once the page reads what it did, no
    // unknown outcome is left on screen.
    let lostPin = false;
    await page.route(`**/v1/artifacts/${alpha.id}`, async (route) => {
      if (lostPin || route.request().method() !== 'PATCH')
        return route.fallback();
      lostPin = true;
      await route.fetch();
      return route.fulfill({
        status: 503,
        json: { error: { code: 'unavailable', message: 'Response lost.' } },
      });
    });
    await pin.click();
    await expect(pin).toHaveAttribute('aria-pressed', 'false');
    await expect(workspace.getByText('Outcome unknown')).toBeHidden();
    current = await api(server, 'GET', `/v1/artifacts/${alpha.id}`);
    assert.equal(current.pinned, false, 'The lost unpin was applied.');
    await page.unrouteAll({ behavior: 'ignoreErrors' });
    current = await api(server, 'GET', `/v1/artifacts/${beta.id}`);
    assert.equal(current.pinned, true, 'Beta stayed pinned.');

    assert.deepEqual(errors, [], 'Page errors.');
    return { artifacts: shown, lists: lists.length };
  } finally {
    await context.close();
  }
}

/** Shows or hides the page as a browser does when its tab changes. */
function setVisible(page, visible) {
  return page.evaluate(
    (state) => {
      Object.defineProperty(document, 'visibilityState', {
        configurable: true,
        get: () => state,
      });
      document.dispatchEvent(new Event('visibilitychange'));
    },
    visible ? 'visible' : 'hidden',
  );
}

/** Renames the open artifact through its dialog; returns the dialog. */
async function rename(page, workspace, title, checkAccessibility) {
  await workspace.getByRole('button', { name: 'Rename', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'Rename artifact' });
  await expect(dialog).toBeVisible();
  if (checkAccessibility !== undefined)
    await checkAccessibility('workspace-rename-dialog', page);
  await dialog.getByLabel('Title').fill(title);
  await dialog.getByRole('button', { name: 'Rename' }).click();
  return dialog;
}

function file(text) {
  return { path: 'index.html', encoding: 'utf8', content: html(text) };
}

function html(text) {
  return `<!doctype html><title>${text}</title><p id="label">${text}</p>\n`;
}
