import assert from 'node:assert/strict';
import { resolve } from 'node:path';
import { expect } from '@playwright/test';
import { api } from './smoke-workspace.mjs';

/**
 * Browser checks for commits made elsewhere, with a second page in its own
 * browser context: the open artifact, the list, and the latest preview
 * follow; a pinned preview and an unsaved draft stay, also when reading the
 * newer revision fails; a hidden page gives up its stream and catches up
 * when shown; a replaced subscription ends; duplicate and older events
 * reload nothing; and a reconnection resumes from its cursor with the
 * commit it missed.
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
  const [{ id }] = (await api(server, 'GET', '/v1/artifacts')).artifacts;
  const label = page
    .frameLocator('iframe[title^="Preview of"]')
    .locator('#label');
  const badge = (revision) =>
    workspace.getByText(`Revision ${revision}`, { exact: true });

  // Record this page's feed requests and reads from a fresh load on.
  const streamRequests = [];
  const reads = { list: 0, artifact: 0 };
  const recordStream = (request) => {
    const { pathname } = new URL(request.url());
    if (pathname === '/v1/changes') streamRequests.push(request);
    if (request.method() !== 'GET') return;
    if (pathname === '/v1/artifacts') reads.list += 1;
    if (pathname === `/v1/artifacts/${id}`) reads.artifact += 1;
  };
  page.on('request', recordStream);
  const current = (await api(server, 'GET', `/v1/artifacts/${id}`))
    .current_revision;
  await page.reload();
  await workspace.scrollIntoViewIfNeeded();
  await expect(badge(current)).toBeVisible();

  const otherContext = await browser.newContext();
  const other = await otherContext.newPage();
  other.setDefaultTimeout(10_000);
  const otherErrors = [];
  other.on('pageerror', (error) => otherErrors.push(error.message));
  try {
    await other.goto(origin);
    const elsewhere = other.locator('#artifacts');
    await elsewhere.scrollIntoViewIfNeeded();
    await elsewhere.getByLabel('Access token').fill(server.token);
    await elsewhere.getByRole('button', { name: 'Connect' }).click();
    await expect(elsewhere.getByText('Live updates on')).toBeVisible();
    await expect(workspace.getByText('Live updates on')).toBeVisible();
    // The page subscribes, then reads once: it waits for the stream's start
    // instead of reading again after it.
    assert.deepEqual(reads, { list: 1, artifact: 1 }, 'Reads after a reload.');

    /** Saves `text` as index.html from the other page. */
    async function saveElsewhere(text, revision) {
      await elsewhere.getByRole('tab', { name: 'Source' }).click();
      await elsewhere.getByLabel('index.html').fill(page_(text));
      await elsewhere.getByRole('button', { name: 'Save revision' }).click();
      await expect(
        elsewhere.getByText(`Revision ${revision}`, { exact: true }),
      ).toBeVisible();
    }

    // This page pins revision 1 in its preview; a save elsewhere updates
    // the revision and the list, but the pinned preview stays.
    await workspace.getByRole('tab', { name: 'Preview' }).click();
    const revisionSelect = workspace.getByLabel('Revision', { exact: true });
    await revisionSelect.selectOption('1');
    await expect(label).toHaveText('Revision one');
    await saveElsewhere('Saved elsewhere', current + 1);
    await expect(badge(current + 1)).toBeVisible();
    await expect(
      workspace
        .getByRole('navigation', { name: 'Artifacts' })
        .getByText(`r${current + 1}`),
    ).toBeVisible();
    await expect(revisionSelect).toHaveValue('1');
    await expect(label).toHaveText('Revision one');
    // The pin survives a visit to the source tab.
    await workspace.getByRole('tab', { name: 'Source' }).click();
    await workspace.getByRole('tab', { name: 'Preview' }).click();
    await expect(revisionSelect).toHaveValue('1');

    // The latest preview follows the next save on its own.
    await revisionSelect.selectOption('latest');
    await expect(label).toHaveText('Saved elsewhere');
    await saveElsewhere('Followed', current + 2);
    await expect(label).toHaveText('Followed');
    await expect(revisionSelect).toHaveValue('latest');

    // An unsaved draft stays when a newer revision arrives; the page says
    // so and loads the latest revision only when asked.
    await workspace.getByRole('tab', { name: 'Source' }).click();
    const editor = workspace.getByLabel('index.html');
    await editor.fill(page_('A draft here'));
    // Reading the newer revision fails first: the draft and the revision it
    // was written against stay, and trying again reads it.
    const failRead = failNextRead();
    await page.route(`**/v1/artifacts/${id}`, failRead.handler);
    await saveElsewhere('Saved over the draft', current + 3);
    await failRead.failed;
    await expect(
      workspace.getByText('Could not refresh the artifact'),
    ).toBeVisible();
    await expect(editor).toHaveValue(/A draft here/);
    await expect(badge(current + 2)).toBeVisible();
    await page.unroute(`**/v1/artifacts/${id}`, failRead.handler);
    await workspace.getByRole('button', { name: 'Try again' }).click();
    await expect(
      workspace.getByText(`Revision ${current + 3} was saved elsewhere`),
    ).toBeVisible();
    await expect(
      workspace.getByText('Could not refresh the artifact'),
    ).toBeHidden();
    await expect(badge(current + 3)).toBeVisible();
    await expect(editor).toHaveValue(/A draft here/);
    await expect(editor).toBeEditable();
    await checkAccessibility('workspace-live-stale');
    await page.screenshot({ path: resolve(qa, 'workspace-live-stale.png') });
    await workspace.getByRole('button', { name: 'Load latest' }).click();
    await expect(editor).toHaveValue(/Saved over the draft/);
    await expect(
      workspace.getByText(`Revision ${current + 3} was saved elsewhere`),
    ).toBeHidden();

    // An agent creates another artifact: both lists show it, and the open
    // artifact stays open although the list now starts with the new one.
    await api(server, 'POST', '/v1/artifacts', {
      title: 'Made by an agent',
      files: [
        { path: 'index.html', encoding: 'utf8', content: page_('Agent') },
      ],
    });
    const nav = workspace.getByRole('navigation', { name: 'Artifacts' });
    await expect(
      nav.getByRole('button', { name: /Made by an agent/ }),
    ).toBeVisible();
    await expect(
      elsewhere
        .getByRole('navigation', { name: 'Artifacts' })
        .getByRole('button', { name: /Made by an agent/ }),
    ).toBeVisible();
    await expect(nav.locator('[aria-current="true"]')).toContainText(
      'Smoke artifact',
    );

    // A hidden page gives up its stream; shown again, it resumes from its
    // cursor and reads the commit made meanwhile.
    assert.equal(streamRequests.length, 1, 'One stream per connected page.');
    const paused = settled(page, streamRequests[0]);
    await setVisibility(page, 'hidden');
    await expect(
      workspace.getByText('Live updates paused while this tab is hidden.'),
    ).toBeVisible();
    await paused;
    await saveElsewhere('Saved while hidden', current + 4);
    await expect(badge(current + 3)).toBeVisible();
    await setVisibility(page, 'visible');
    await expect(badge(current + 4)).toBeVisible();
    await expect(workspace.getByText('Live updates on')).toBeVisible();
    assert.equal(streamRequests.length, 2, 'One stream after showing again.');
    assert.match(
      streamRequests[1].headers()['last-event-id'] ?? '',
      /^[0-9a-f]{32}\.\d+$/,
      'Showing the page again must resume from its cursor.',
    );

    // Disconnecting replaces the subscription: the old stream ends.
    const ended = settled(page, streamRequests[1]);
    await workspace.getByRole('button', { name: 'Disconnect' }).click();
    await ended;

    // The next streams are forged: the first is held back, as a browser
    // out of connections would, so the page reads without it, then starts
    // at the head, which reads again, and ends;
    // the resumed one brings a duplicate and an older event, which reload
    // nothing; the next one resets, which reloads once; the reconnection
    // after it waits while a commit lands elsewhere, and resuming from its
    // cursor brings that commit.
    const { feed, head } = await feedHead(server);
    // Clients treat cursors as opaque; this check forges the server's own
    // `<feed>.<sequence>` form.
    const at = (sequence) => `${feed}.${sequence}`;
    const latest = current + 4;
    const events = (body) => ({
      status: 200,
      headers: {
        'content-type': 'text/event-stream',
        'cache-control': 'no-store',
      },
      body,
    });
    let streams = 0;
    let startFirst;
    const firstStarted = new Promise((resolveStarted) => {
      startFirst = resolveStarted;
    });
    let sendCrafted;
    const craftedSent = new Promise((resolveSent) => {
      sendCrafted = resolveSent;
    });
    let sendReset;
    const resetSent = new Promise((resolveSent) => {
      sendReset = resolveSent;
    });
    let releaseResume;
    const resumeHeld = new Promise((resolveHeld) => {
      releaseResume = resolveHeld;
    });
    const resumedFrom = [];
    const feedRoute = async (route) => {
      streams += 1;
      if (streams === 1) {
        await firstStarted;
        return route.fulfill(
          events(
            `event: ready\nid: ${at(head)}\ndata: ${JSON.stringify({ cursor: at(head) })}\n\n`,
          ),
        );
      }
      resumedFrom.push(route.request().headers()['last-event-id']);
      if (streams === 2) {
        await craftedSent;
        return route.fulfill(
          events(
            change(at(head - 1), id, latest - 1) + change(at(head), id, latest),
          ),
        );
      }
      if (streams === 3) {
        await resetSent;
        return route.fulfill(
          events(
            `event: reset\nid: ${at(head)}\ndata: ${JSON.stringify({ cursor: at(head) })}\n\n`,
          ),
        );
      }
      if (streams === 4) await resumeHeld;
      return route.fallback();
    };
    await page.route('**/v1/changes', feedRoute);
    await workspace.getByLabel('Access token').fill(server.token);
    await workspace.getByRole('button', { name: 'Connect' }).click();
    // A new connection opens the newest artifact, now the agent's.
    await nav.getByRole('button', { name: /Smoke artifact/ }).click();
    await expect(badge(latest)).toBeVisible();
    assert.equal(streams, 1, 'The first stream was not held back.');
    // Reads of the list or the open artifact.
    let reloads = 0;
    const countReloads = (request) => {
      const { pathname } = new URL(request.url());
      if (
        request.method() === 'GET' &&
        (pathname === '/v1/artifacts' || pathname === `/v1/artifacts/${id}`)
      )
        reloads += 1;
    };
    page.on('request', countReloads);
    // Once the stream starts, changes before it may have been missed.
    startFirst();
    await expect.poll(() => reloads).toBe(2);
    sendCrafted();
    await expect.poll(() => resumedFrom.length).toBe(2);
    assert.equal(reloads, 2, 'A duplicate or older event reloaded the page.');
    // A reset means changes may have been missed: the page reads again.
    // Here it stands for a server whose data was replaced by an older copy,
    // where the artifact reads at revision 1. A draft open meanwhile stays
    // editable, with its file list, and says that another revision is
    // current.
    await workspace.getByRole('tab', { name: 'Source' }).click();
    await editor.fill(page_('Draft across a reset'));
    const olderCopy = async (route) => {
      const { pathname } = new URL(route.request().url());
      const artifactPath = `/v1/artifacts/${id}`;
      const read = route.request().method() === 'GET';
      if (
        !read ||
        ![artifactPath, `${artifactPath}/revisions`, '/v1/artifacts'].includes(
          pathname,
        )
      )
        return route.fallback();
      const response = await route.fetch();
      const body = await response.json();
      if (pathname === artifactPath) body.current_revision = 1;
      else if (pathname === '/v1/artifacts') {
        for (const item of body.artifacts)
          if (item.id === id) item.current_revision = 1;
      } else
        body.revisions = body.revisions.filter((item) => item.revision === 1);
      return route.fulfill({ response, json: body });
    };
    await page.route('**/v1/artifacts**', olderCopy);
    sendReset();
    await expect.poll(() => resumedFrom.length).toBe(3);
    await expect.poll(() => reloads).toBe(4);
    await expect(
      workspace.getByText('Revision 1 is now current'),
    ).toBeVisible();
    await expect(badge(1)).toBeVisible();
    await expect(editor).toHaveValue(/Draft across a reset/);
    await expect(editor).toBeEditable();
    await page.unroute('**/v1/artifacts**', olderCopy);
    await workspace.getByRole('button', { name: 'Load latest' }).click();
    await expect(editor).toHaveValue(/Saved while hidden/);
    await expect(badge(latest)).toBeVisible();
    assert.deepEqual(
      resumedFrom,
      [at(head), at(head), at(head)],
      'Each reconnection must resume after the last cursor.',
    );
    await saveElsewhere('Missed while away', latest + 1);
    releaseResume();
    await expect(badge(latest + 1)).toBeVisible();
    await expect(workspace.getByText('Live updates on')).toBeVisible();

    // A burst of commits elsewhere shares the request budget: each view
    // sends one read at a time and skips reads a newer one replaced, and a
    // rate-limited read waits for its Retry-After and reads again.
    const inFlight = { list: 0, artifact: 0 };
    const most = { list: 0, artifact: 0 };
    // Armed for one list read: the one after the last save, which no newer
    // read replaces.
    let limitNext = false;
    let limited = false;
    const pace = async (route) => {
      const { pathname } = new URL(route.request().url());
      const view =
        pathname === '/v1/artifacts'
          ? 'list'
          : pathname === `/v1/artifacts/${id}`
            ? 'artifact'
            : undefined;
      if (view === undefined || route.request().method() !== 'GET')
        return route.fallback();
      if (view === 'list' && limitNext) {
        limitNext = false;
        limited = true;
        return route.fulfill({
          status: 429,
          headers: { 'retry-after': '1' },
          json: { error: { code: 'rate_limited', message: 'Slow down.' } },
        });
      }
      inFlight[view] += 1;
      most[view] = Math.max(most[view], inFlight[view]);
      try {
        await new Promise((resolveLater) => setTimeout(resolveLater, 300));
        return await route.fulfill({ response: await route.fetch() });
      } finally {
        inFlight[view] -= 1;
      }
    };
    await page.route('**/v1/artifacts**', pace);
    let revision = latest + 1;
    for (const text of ['Burst one', 'Burst two', 'Burst three']) {
      await api(server, 'POST', `/v1/artifacts/${id}/revisions`, {
        base_revision: revision,
        changes: [
          {
            op: 'put',
            path: 'index.html',
            encoding: 'utf8',
            content: page_(text),
          },
        ],
      });
      revision += 1;
    }
    await expect(badge(revision)).toBeVisible();
    await expect(nav.getByText(`r${revision}`)).toBeVisible();
    assert.deepEqual(
      most,
      { list: 1, artifact: 1 },
      'Reads of one view overlapped.',
    );
    limitNext = true;
    await api(server, 'POST', `/v1/artifacts/${id}/revisions`, {
      base_revision: revision,
      changes: [
        {
          op: 'put',
          path: 'index.html',
          encoding: 'utf8',
          content: page_('Limited'),
        },
      ],
    });
    revision += 1;
    await expect(nav.getByText(`r${revision}`)).toBeVisible();
    assert.ok(limited, 'No list read was rate limited.');
    await expect(workspace.getByText('Could not list artifacts')).toBeHidden();
    await page.unroute('**/v1/artifacts**', pace);
    page.off('request', countReloads);
    page.off('request', recordStream);
    await page.unroute('**/v1/changes', feedRoute);
    assert.deepEqual(otherErrors, [], 'Second page JavaScript errors.');
    return { pages: 2, revisions: revision };
  } finally {
    await otherContext.close();
  }
}

/** A probe-free page whose label is `text`, like the workspace fixtures. */
function page_(text) {
  return `<!doctype html><html lang="en"><head><meta charset="utf-8"><title>Live</title></head><body><h1 id="label">${text}</h1></body></html>\n`;
}

/**
 * A route handler that fails the next GET it sees with 503; `failed`
 * resolves once it has.
 */
function failNextRead() {
  let fail;
  const failed = new Promise((resolveFailed) => {
    fail = resolveFailed;
  });
  let done = false;
  const handler = async (route) => {
    if (done || route.request().method() !== 'GET') return route.fallback();
    done = true;
    await route.fulfill({
      status: 503,
      json: { error: { code: 'unavailable', message: 'Read failed.' } },
    });
    fail();
  };
  return { handler, failed };
}

/** Makes the page report `state` as its visibility, as a tab switch does. */
async function setVisibility(page, state) {
  await page.evaluate((visibility) => {
    Object.defineProperty(document, 'visibilityState', {
      configurable: true,
      get: () => visibility,
    });
    document.dispatchEvent(new Event('visibilitychange'));
  }, state);
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

/** Resolves when `request` ends, or rejects after five seconds. */
function settled(page, request) {
  return new Promise((resolveEnded, rejectEnded) => {
    const timer = setTimeout(
      () => rejectEnded(new Error('The stream stayed open.')),
      5000,
    );
    const done = (ended) => {
      if (ended !== request) return;
      clearTimeout(timer);
      page.off('requestfailed', done);
      page.off('requestfinished', done);
      resolveEnded();
    };
    page.on('requestfailed', done);
    page.on('requestfinished', done);
  });
}
