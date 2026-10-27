import assert from 'node:assert/strict';
import { resolve } from 'node:path';
import { expect } from '@playwright/test';
import { api } from './smoke-workspace.mjs';

/**
 * Browser checks of decision 0007 against a real server, with a second
 * browser context and API calls standing in for other clients and agents:
 * what follows commits made elsewhere and what stays (a pinned preview, an
 * unsaved draft), when the page reads (subscribe first, never while hidden,
 * one read at a time per view), and what forged streams cannot break
 * (duplicates, resets to older or empty data, resumption from the cursor).
 */
export async function checkLiveUpdates({
  browser,
  page,
  origin,
  server,
  qa,
  checkAccessibility,
}) {
  const workspace = page.locator('#artifacts');
  const nav = workspace.getByRole('navigation', { name: 'Artifacts' });
  const editor = workspace.getByLabel('index.html');
  const [{ id }] = (await api(server, 'GET', '/v1/artifacts')).artifacts;
  const badge = (revision) =>
    workspace.getByText(`Revision ${revision}`, { exact: true });
  const label = page
    .frameLocator('iframe[title^="Preview of"]')
    .locator('#label');
  const reads = countReads(page, id);
  let current = await revisionOf(server, id);

  // Subscribe, then read once: the first read waits for the stream's start
  // instead of reading again after it.
  await page.reload();
  await workspace.scrollIntoViewIfNeeded();
  await expect(badge(current)).toBeVisible();
  await expect(workspace.getByText('Live updates on')).toBeVisible();
  assert.deepEqual(reads.take(), { list: 1, artifact: 1 }, 'Reads on load.');

  const otherContext = await browser.newContext();
  try {
    const { other, errors: otherErrors } = await connectedPage(
      otherContext,
      origin,
      server,
    );
    const elsewhere = other.locator('#artifacts');
    /** Saves `text` as index.html from the second context. */
    async function saveElsewhere(text) {
      await elsewhere.getByRole('tab', { name: 'Source' }).click();
      await elsewhere.getByLabel('index.html').fill(htmlPage(text));
      await elsewhere.getByRole('button', { name: 'Save revision' }).click();
      current += 1;
      await expect(
        elsewhere.getByText(`Revision ${current}`, { exact: true }),
      ).toBeVisible();
    }

    // A preview pinned to revision 1 stays, also across the source tab,
    // while the badge and the list follow; "Latest" follows on its own.
    await workspace.getByRole('tab', { name: 'Preview' }).click();
    const revisionMenu = workspace.getByLabel('Revision', { exact: true });
    await revisionMenu.selectOption('1');
    await expect(label).toHaveText('Revision one');
    await saveElsewhere('Saved elsewhere');
    await expect(badge(current)).toBeVisible();
    await expect(nav.getByText(`r${current}`)).toBeVisible();
    await workspace.getByRole('tab', { name: 'Source' }).click();
    await workspace.getByRole('tab', { name: 'Preview' }).click();
    await expect(revisionMenu).toHaveValue('1');
    await expect(label).toHaveText('Revision one');
    await revisionMenu.selectOption('latest');
    await expect(label).toHaveText('Saved elsewhere');
    await saveElsewhere('Followed');
    await expect(label).toHaveText('Followed');

    // An unsaved draft stays when a newer revision arrives, also when
    // reading that revision fails first; the page says what happened and
    // loads the latest revision only when asked.
    await workspace.getByRole('tab', { name: 'Source' }).click();
    await editor.fill(htmlPage('A draft here'));
    const failing = failNextRead(`/v1/artifacts/${id}`);
    await page.route('**/v1/artifacts/**', failing.handler);
    await saveElsewhere('Saved over the draft');
    await failing.failed;
    await expect(
      workspace.getByText('Could not refresh the artifact'),
    ).toBeVisible();
    await expect(badge(current - 1)).toBeVisible();
    await expect(editor).toHaveValue(/A draft here/);
    await page.unroute('**/v1/artifacts/**', failing.handler);
    await workspace.getByRole('button', { name: 'Try again' }).click();
    await expect(
      workspace.getByText(`Revision ${current} was saved elsewhere`),
    ).toBeVisible();
    await expect(badge(current)).toBeVisible();
    await expect(editor).toHaveValue(/A draft here/);
    await expect(editor).toBeEditable();
    await checkAccessibility('workspace-live-stale');
    await page.screenshot({ path: resolve(qa, 'workspace-live-stale.png') });
    await workspace.getByRole('button', { name: 'Load latest' }).click();
    await expect(editor).toHaveValue(/Saved over the draft/);

    // An agent creates an artifact: both lists show it, and the open one
    // stays open although the list now starts with the new one.
    await api(server, 'POST', '/v1/artifacts', {
      title: 'Made by an agent',
      files: [
        { path: 'index.html', encoding: 'utf8', content: htmlPage('Agent') },
      ],
    });
    const otherNav = elsewhere.getByRole('navigation', { name: 'Artifacts' });
    for (const list of [nav, otherNav])
      await expect(
        list.getByRole('button', { name: /Made by an agent/ }),
      ).toBeVisible();
    await expect(nav.locator('[aria-current="true"]')).toContainText(
      'Smoke artifact',
    );

    // A hidden page gives up its stream and reads nothing; shown again, it
    // resumes from its cursor with the commit made meanwhile.
    const [stream] = reads.streams;
    assert.equal(reads.streams.length, 1, 'One stream per page.');
    const closed = ended(page, stream);
    reads.take();
    await setVisibility(page, 'hidden');
    await expect(
      workspace.getByText('Live updates paused while this tab is hidden.'),
    ).toBeVisible();
    await closed;
    await saveElsewhere('Saved while hidden');
    await expect(badge(current - 1)).toBeVisible();
    assert.deepEqual(reads.take(), { list: 0, artifact: 0 }, 'Hidden reads.');
    await setVisibility(page, 'visible');
    await expect(badge(current)).toBeVisible();
    await expect(workspace.getByText('Live updates on')).toBeVisible();
    assert.equal(reads.streams.length, 2, 'One stream after showing again.');
    assert.match(
      reads.streams[1].headers()['last-event-id'] ?? '',
      /^[0-9a-f]{32}\.\d+$/,
      'Showing the page again must resume from its cursor.',
    );

    // A page opened in the background reads nothing until it is shown.
    await checkBackgroundPage(browser, origin, server);

    // Disconnecting replaces the subscription: the old stream ends.
    const replaced = ended(page, reads.streams[1]);
    await workspace.getByRole('button', { name: 'Disconnect' }).click();
    await replaced;

    await checkForgedStreams({
      page,
      workspace,
      nav,
      editor,
      server,
      id,
      reads,
      badge,
      latest: current,
      saveElsewhere,
    });

    // A burst of commits costs one read at a time per view, and a read
    // refused with 429 is sent again after its Retry-After.
    const paced = paceReads(id);
    await page.route('**/v1/artifacts**', paced.handler);
    for (const text of ['Burst one', 'Burst two', 'Burst three'])
      await commit(server, id, current++, text);
    await expect(badge(current)).toBeVisible();
    await expect(nav.getByText(`r${current}`)).toBeVisible();
    paced.limitNextList();
    await commit(server, id, current++, 'Rate limited');
    await expect(nav.getByText(`r${current}`)).toBeVisible();
    assert.ok(paced.limited, 'No list read was rate limited.');
    assert.deepEqual(paced.most, { list: 1, artifact: 1 }, 'Overlap.');
    await expect(workspace.getByText('Could not list artifacts')).toBeHidden();
    await page.unroute('**/v1/artifacts**', paced.handler);

    reads.stop();
    assert.deepEqual(otherErrors, [], 'Second page JavaScript errors.');
    return { pages: 3, revisions: current };
  } finally {
    await otherContext.close();
  }
}

/**
 * Connects again with every stream forged in turn, then follows the real
 * server: the first stream is held back, so the page reads without it; it
 * then starts at the head, which reads again and marks a draft written
 * meanwhile as unsaveable; the next brings a duplicate
 * and an older change, which read nothing; three resets stand for a
 * server whose data was replaced: by an older copy while a draft is open,
 * by a restored copy with the same revision and other content, and by an
 * empty one while a draft is open; the last reconnection waits while a
 * commit lands elsewhere and, resuming from its cursor, brings it.
 */
async function checkForgedStreams({
  page,
  workspace,
  nav,
  editor,
  server,
  id,
  reads,
  badge,
  latest,
  saveElsewhere,
}) {
  const { feed, head } = await feedHead(server);
  // Clients treat cursors as opaque; this check forges the server's own
  // `<feed>.<sequence>` form.
  const cursor = `${feed}.${head}`;
  const position = (type) =>
    `event: ${type}\nid: ${cursor}\ndata: ${JSON.stringify({ cursor })}\n\n`;
  const streams = scriptStreams(page, [
    position('ready'),
    change(`${feed}.${head - 1}`, id, latest - 1) + change(cursor, id, latest),
    position('reset'),
    position('reset'),
    position('reset'),
    undefined,
  ]);
  await page.route('**/v1/changes', streams.handler);
  await workspace.getByLabel('Access token').fill(server.token);
  await workspace.getByRole('button', { name: 'Connect' }).click();
  await nav.getByRole('button', { name: /Smoke artifact/ }).click();
  await expect(badge(latest)).toBeVisible();
  assert.equal(streams.opened(), 1, 'The first stream was not held back.');
  reads.take();

  // A draft written before the stream started: nothing ties what the page
  // read to the stream's position, so the draft cannot be saved.
  await workspace.getByRole('tab', { name: 'Source' }).click();
  await editor.fill(htmlPage('Draft before the stream'));
  streams.release(0);
  await expect.poll(() => reads.peek()).toEqual({ list: 1, artifact: 1 });
  await expect(
    workspace.getByText("The server's data may have been replaced"),
  ).toBeVisible();
  await workspace.getByRole('button', { name: 'Discard changes' }).click();
  streams.release(1);
  await expect.poll(() => streams.opened()).toBe(3);
  assert.deepEqual(reads.take(), { list: 1, artifact: 1 }, 'Duplicates read.');

  // Reset to an older copy, where the artifact is at revision 1: the draft
  // keeps its revision and files, says that the data was replaced, and
  // cannot be saved.
  await workspace.getByRole('tab', { name: 'Source' }).click();
  await editor.fill(htmlPage('Draft across a reset'));
  const older = replaceData(id, 'older');
  await page.route('**/v1/artifacts**', older);
  streams.release(2);
  await expect.poll(() => reads.peek()).toEqual({ list: 1, artifact: 1 });
  await expect(
    workspace.getByText("The server's data may have been replaced"),
  ).toBeVisible();
  await expect(badge(1)).toBeVisible();
  await expect(editor).toHaveValue(/Draft across a reset/);
  await expect(editor).toBeEditable();
  await expect(
    workspace.getByRole('button', { name: 'Save revision' }),
  ).toBeDisabled();
  await page.unroute('**/v1/artifacts**', older);
  await workspace.getByRole('button', { name: 'Load latest' }).click();
  await expect(badge(latest)).toBeVisible();
  await expect(editor).toHaveValue(/Saved while hidden/);

  // Reset to a restored copy with the same revision and other content:
  // without a draft, the preview and the editor read it again.
  await workspace.getByRole('tab', { name: 'Preview' }).click();
  await expect(
    page.frameLocator('iframe[title^="Preview of"]').locator('#label'),
  ).toBeVisible();
  const previews = countPreviews(page, id);
  const restored = restoredFile(id, latest);
  await page.route('**/v1/artifacts/**', restored);
  streams.release(3);
  await expect.poll(() => previews.count()).toBeGreaterThan(0);
  await workspace.getByRole('tab', { name: 'Source' }).click();
  await expect(editor).toHaveValue(/Restored copy/);
  await page.unroute('**/v1/artifacts/**', restored);
  previews.stop();

  // Reset to empty data: the list is empty and the artifact is gone, yet the
  // open draft stays until the user discards it; then the open artifact
  // follows the list and closes.
  await editor.fill(htmlPage('Draft across an empty reset'));
  const empty = replaceData(id, 'empty');
  await page.route('**/v1/artifacts**', empty);
  streams.release(4);
  await expect(nav.getByText('No artifacts yet')).toBeVisible();
  await expect(
    workspace.getByText('Could not refresh the artifact'),
  ).toBeVisible();
  await expect(editor).toHaveValue(/Draft across an empty reset/);
  await expect(editor).toBeEditable();
  await page.unroute('**/v1/artifacts**', empty);
  await workspace.getByRole('button', { name: 'Discard changes' }).click();
  await expect(editor).toBeHidden();
  await expect(workspace.getByText('No artifacts yet')).toBeVisible();

  // The last reconnection resumes from its cursor with the commit made
  // while it waited, which brings the artifact back.
  await expect.poll(() => streams.opened()).toBe(6);
  await saveElsewhere('Missed while away');
  streams.release(5);
  await expect(badge(latest + 1)).toBeVisible();
  await expect(workspace.getByText('Live updates on')).toBeVisible();
  assert.deepEqual(
    streams.resumedFrom,
    [cursor, cursor, cursor, cursor, cursor],
    'Each reconnection must resume after the last cursor.',
  );
  await page.unroute('**/v1/changes', streams.handler);
}

/**
 * Opens a page in `context`, connects it with the server's token, and
 * collects its JavaScript errors.
 */
async function connectedPage(context, origin, server) {
  const other = await context.newPage();
  other.setDefaultTimeout(10_000);
  const errors = [];
  other.on('pageerror', (error) => errors.push(error.message));
  await other.goto(origin);
  const panel = other.locator('#artifacts');
  await panel.scrollIntoViewIfNeeded();
  await panel.getByLabel('Access token').fill(server.token);
  await panel.getByRole('button', { name: 'Connect' }).click();
  await expect(panel.getByText('Live updates on')).toBeVisible();
  return { other, errors };
}

/**
 * A page connected while hidden opens no stream and reads nothing. It gets
 * its own context: in one Firefox context, a tab that is not selected has
 * its requests held while the selected tab streams, and faking visibility
 * does not select a tab.
 */
async function checkBackgroundPage(browser, origin, server) {
  const context = await browser.newContext();
  const background = await context.newPage();
  try {
    await background.addInitScript(() => {
      Object.defineProperty(document, 'visibilityState', {
        configurable: true,
        get: () => 'hidden',
      });
    });
    const requests = [];
    background.on('request', (request) => {
      const { pathname } = new URL(request.url());
      if (pathname === '/v1/artifacts' || pathname === '/v1/changes')
        requests.push(pathname);
    });
    await background.goto(origin);
    const panel = background.locator('#artifacts');
    await panel.getByLabel('Access token').fill(server.token);
    await panel.getByRole('button', { name: 'Connect' }).click();
    await expect(
      panel.getByText('Live updates paused while this tab is hidden.'),
    ).toBeVisible();
    // Longer than the first read waits for a stream.
    await background.waitForTimeout(2500);
    assert.deepEqual(requests, [], 'A hidden page read or subscribed.');
    await setVisibility(background, 'visible');
    await expect(panel.getByText('Live updates on')).toBeVisible();
    await expect(
      panel.getByRole('navigation', { name: 'Artifacts' }),
    ).toBeVisible();
  } finally {
    await context.close();
  }
}

/**
 * Counts this page's reads of the list and of artifact `id`, and records
 * its change stream requests.
 */
function countReads(page, id) {
  let counts = { list: 0, artifact: 0 };
  const streams = [];
  const listener = (request) => {
    const { pathname } = new URL(request.url());
    if (pathname === '/v1/changes') streams.push(request);
    if (request.method() !== 'GET') return;
    if (pathname === '/v1/artifacts') counts.list += 1;
    if (pathname === `/v1/artifacts/${id}`) counts.artifact += 1;
  };
  page.on('request', listener);
  return {
    streams,
    peek: () => ({ ...counts }),
    /** The counts since the last call. */
    take() {
      const taken = counts;
      counts = { list: 0, artifact: 0 };
      return taken;
    },
    stop: () => page.off('request', listener),
  };
}

/**
 * Answers each change stream request with the next of `bodies` once it is
 * released, and falls back to the server for `undefined` and after the
 * last. Records the cursor each reconnection resumed from.
 */
function scriptStreams(page, bodies) {
  const gates = bodies.map(() => {
    let open;
    const opened = new Promise((resolveOpened) => {
      open = resolveOpened;
    });
    return { open, opened };
  });
  const resumedFrom = [];
  let count = 0;
  return {
    resumedFrom,
    opened: () => count,
    release: (index) => gates[index].open(),
    async handler(route) {
      const index = count;
      count += 1;
      if (index > 0)
        resumedFrom.push(route.request().headers()['last-event-id']);
      if (index >= bodies.length) return route.fallback();
      await gates[index].opened;
      if (bodies[index] === undefined) return route.fallback();
      return route.fulfill({
        status: 200,
        headers: {
          'content-type': 'text/event-stream',
          'cache-control': 'no-store',
        },
        body: bodies[index],
      });
    },
  };
}

/**
 * A route handler that answers reads as a server whose data was replaced:
 * `older` puts artifact `id` at revision 1; `empty` has no artifacts.
 */
function replaceData(id, data) {
  const artifactPath = `/v1/artifacts/${id}`;
  return async (route) => {
    const { pathname } = new URL(route.request().url());
    const paths = ['/v1/artifacts', artifactPath, `${artifactPath}/revisions`];
    if (route.request().method() !== 'GET' || !paths.includes(pathname))
      return route.fallback();
    if (data === 'empty')
      return pathname === '/v1/artifacts'
        ? route.fulfill({ json: { artifacts: [] } })
        : route.fulfill({
            status: 404,
            json: {
              error: { code: 'not_found', message: 'artifact not found' },
            },
          });
    const response = await route.fetch();
    const body = await response.json();
    if (pathname === artifactPath) body.current_revision = 1;
    else if (pathname === '/v1/artifacts') {
      for (const artifact of body.artifacts)
        if (artifact.id === id) artifact.current_revision = 1;
    } else
      body.revisions = body.revisions.filter((item) => item.revision === 1);
    return route.fulfill({ response, json: body });
  };
}

/**
 * A route handler that answers index.html of revision `revision` of
 * artifact `id` with other content, as a restored copy would hold.
 */
function restoredFile(id, revision) {
  const path = `/v1/artifacts/${id}/revisions/${revision}/files/index.html`;
  return (route) =>
    route.request().method() === 'GET' &&
    new URL(route.request().url()).pathname === path
      ? route.fulfill({
          contentType: 'text/html; charset=utf-8',
          body: htmlPage('Restored copy'),
        })
      : route.fallback();
}

/** Counts preview links this page asks for artifact `id`. */
function countPreviews(page, id) {
  let count = 0;
  const pattern = new RegExp(`^/v1/artifacts/${id}/revisions/\\d+/previews$`);
  const listener = (request) => {
    if (
      request.method() === 'POST' &&
      pattern.test(new URL(request.url()).pathname)
    )
      count += 1;
  };
  page.on('request', listener);
  return { count: () => count, stop: () => page.off('request', listener) };
}

/** A route handler that fails the next GET of `path` with 503. */
function failNextRead(path) {
  let fail;
  const failed = new Promise((resolveFailed) => {
    fail = resolveFailed;
  });
  let done = false;
  return {
    failed,
    async handler(route) {
      const { pathname } = new URL(route.request().url());
      if (done || route.request().method() !== 'GET' || pathname !== path)
        return route.fallback();
      done = true;
      await route.fulfill({
        status: 503,
        json: { error: { code: 'unavailable', message: 'Read failed.' } },
      });
      fail();
    },
  };
}

/**
 * A route handler that slows reads of the list and of artifact `id`,
 * recording the most in flight at once, and can refuse the next list read
 * with 429.
 */
function paceReads(id) {
  const inFlight = { list: 0, artifact: 0 };
  const paced = {
    most: { list: 0, artifact: 0 },
    limited: false,
    limitNext: false,
    limitNextList() {
      paced.limitNext = true;
    },
    async handler(route) {
      const { pathname } = new URL(route.request().url());
      const view =
        pathname === '/v1/artifacts'
          ? 'list'
          : pathname === `/v1/artifacts/${id}`
            ? 'artifact'
            : undefined;
      if (view === undefined || route.request().method() !== 'GET')
        return route.fallback();
      if (view === 'list' && paced.limitNext) {
        paced.limitNext = false;
        paced.limited = true;
        return route.fulfill({
          status: 429,
          headers: { 'retry-after': '1' },
          json: { error: { code: 'rate_limited', message: 'Slow down.' } },
        });
      }
      inFlight[view] += 1;
      paced.most[view] = Math.max(paced.most[view], inFlight[view]);
      try {
        await new Promise((resolveLater) => setTimeout(resolveLater, 300));
        return await route.fulfill({ response: await route.fetch() });
      } finally {
        inFlight[view] -= 1;
      }
    },
  };
  return paced;
}

/** Makes `page` report `state` as its visibility, as a tab switch does. */
async function setVisibility(page, state) {
  await page.evaluate((visibility) => {
    Object.defineProperty(document, 'visibilityState', {
      configurable: true,
      get: () => visibility,
    });
    document.dispatchEvent(new Event('visibilitychange'));
  }, state);
}

/** Resolves when `request` ends, or rejects after five seconds. */
function ended(page, request) {
  return new Promise((resolveEnded, rejectEnded) => {
    const timer = setTimeout(
      () => rejectEnded(new Error('The stream stayed open.')),
      5000,
    );
    const done = (finished) => {
      if (finished !== request) return;
      clearTimeout(timer);
      page.off('requestfailed', done);
      page.off('requestfinished', done);
      resolveEnded();
    };
    page.on('requestfailed', done);
    page.on('requestfinished', done);
  });
}

/** Commits `text` as index.html of artifact `id` on top of `base`. */
async function commit(server, id, base, text) {
  await api(server, 'POST', `/v1/artifacts/${id}/revisions`, {
    base_revision: base,
    changes: [
      {
        op: 'put',
        path: 'index.html',
        encoding: 'utf8',
        content: htmlPage(text),
      },
    ],
  });
}

async function revisionOf(server, id) {
  return (await api(server, 'GET', `/v1/artifacts/${id}`)).current_revision;
}

/** A probe-free page whose label is `text`, like the workspace fixtures. */
function htmlPage(text) {
  return `<!doctype html><html lang="en"><head><meta charset="utf-8"><title>Live</title></head><body><h1 id="label">${text}</h1></body></html>\n`;
}

function change(cursor, id, revision) {
  const data = {
    cursor,
    artifact_id: id,
    change: 'revised',
    revision,
    committed_at_ms: 1,
  };
  return `event: change\nid: ${cursor}\ndata: ${JSON.stringify(data)}\n\n`;
}

/** The feed identity and head sequence, read from a `ready` event. */
async function feedHead(server) {
  const controller = new AbortController();
  const response = await fetch(`${server.api_url}/v1/changes`, {
    headers: { authorization: `Bearer ${server.token}` },
    signal: controller.signal,
  });
  const reader = response.body.getReader();
  let text = '';
  while (!text.includes('\n\n')) {
    const { value, done } = await reader.read();
    assert.ok(!done, 'The feed ended before its ready event.');
    text += new TextDecoder().decode(value);
  }
  controller.abort();
  const match = text.match(/^event: ready\nid: ([0-9a-f]{32})\.(\d+)\n/);
  assert.ok(match, `Unexpected first event: ${text}`);
  return { feed: match[1], head: Number(match[2]) };
}
