import assert from 'node:assert/strict';
import { expect } from '@playwright/test';
import { api } from './smoke-workspace.mjs';

/**
 * Browser checks for following committed changes (decision 0007) against a
 * real artifree-server. Each page has its own browser context, so its
 * requests, stream, and visibility are its own. Commits come from the API,
 * as another client's would. Forged streams and rewritten answers stand in
 * for what one server cannot be made to do on demand: repeat old changes,
 * send a kind this client does not know, or answer `reset` after its data
 * was replaced. No real backup is restored.
 */
export async function checkLiveUpdates({ browser, origin, server }) {
  const { artifact } = await api(server, 'POST', '/v1/artifacts', {
    title: 'Live artifact',
    files: [file('Live 1')],
  });
  const { id } = artifact;
  // The checks below open the first listed artifact; it must be this one.
  const [newest] = (await api(server, 'GET', '/v1/artifacts?limit=1'))
    .artifacts;
  assert.equal(
    newest.id,
    id,
    `The new artifact must be listed first; ${newest.title} was updated at ${newest.updated_at}, the new one at ${artifact.updated_at}.`,
  );
  const pages = [];
  const opened = async (options) => {
    const tracked = await open(browser, origin, options);
    pages.push(tracked);
    return tracked;
  };
  try {
    const main = await opened();
    const { workspace, reads } = main;
    const editor = workspace.getByLabel('index.html');
    const badge = (revision) =>
      workspace.getByText(`Revision ${revision}`, { exact: true });
    const label = preview(main).locator('#label');

    // W5, W7: the first read waits for the stream; each view reads once.
    await connect(main, server);
    await expect(workspace.getByText('Live updates on.')).toBeVisible();
    await expect(badge(1)).toBeVisible();
    await expect(label).toHaveText('Live 1');
    await expect(editor).toHaveValue(html('Live 1'));
    await reads.quiet();
    assert.deepEqual(
      reads.counts(0),
      { list: 1, artifact: 1, revisions: 1, file: 1, preview: 1 },
      'Loading reads each view once.',
    );

    // W1: a commit elsewhere reads each view once more.
    let mark = reads.mark();
    await commit(server, id, 'Live 2');
    await expect(badge(2)).toBeVisible();
    await expect(label).toHaveText('Live 2');
    await expect(editor).toHaveValue(html('Live 2'));
    await reads.quiet();
    assert.deepEqual(
      reads.counts(mark),
      { list: 1, artifact: 1, revisions: 1, file: 1, preview: 1 },
      'A commit elsewhere reads each view once.',
    );

    // W2: a revision chosen for the preview stays, also across the tabs.
    const revisionMenu = workspace.getByLabel('Revision', { exact: true });
    await revisionMenu.selectOption('1');
    await expect(label).toHaveText('Live 1');
    await commit(server, id, 'Live 3');
    await expect(badge(3)).toBeVisible();
    await workspace.getByRole('tab', { name: 'Source' }).click();
    await expect(editor).toHaveValue(html('Live 3'));
    await workspace.getByRole('tab', { name: 'Preview' }).click();
    await expect(revisionMenu).toHaveValue('1');
    await expect(label).toHaveText('Live 1');
    await revisionMenu.selectOption('latest');
    await expect(label).toHaveText('Live 3');

    // W3: a repeated or older change reads nothing.
    await reads.quiet();
    mark = reads.mark();
    await restream(main, server, [change(id, 3), change(id, 2)]);
    await main.page.waitForTimeout(1000);
    assert.deepEqual(reads.counts(mark), {}, 'Known changes must not read.');

    // A kind this client does not know reads the list and the open
    // artifact again, although its revision is known.
    mark = reads.mark();
    await restream(main, server, [
      { ...change(id, 3), change: 'retitled_in_a_later_version' },
    ]);
    await expect.poll(() => reads.counts(mark).revisions ?? 0).toBe(1);
    await reads.quiet();
    assert.deepEqual(
      reads.counts(mark),
      { list: 1, artifact: 1, revisions: 1 },
      'An unknown kind reads the list and the open artifact once.',
    );

    // W4: a draft survives a newer revision and a failed refresh; only
    // "Load latest" replaces it.
    await workspace.getByRole('tab', { name: 'Source' }).click();
    await editor.fill(html('Draft A'));
    await commit(server, id, 'Live 4');
    await expect(
      workspace.getByText('Revision 4 was saved elsewhere'),
    ).toBeVisible();
    await expect(editor).toHaveValue(html('Draft A'));
    // Decision 8: a save the server refused committed nothing, so the
    // notice stays.
    main.routes.once(
      (request) =>
        request.method() === 'POST' &&
        new URL(request.url()).pathname.endsWith('/revisions'),
      (route) =>
        route.fulfill({
          status: 400,
          json: { error: { code: 'invalid_request', message: 'Refused.' } },
        }),
    );
    await workspace.getByRole('button', { name: 'Save revision' }).click();
    await expect(
      workspace.getByText('Refused.', { exact: true }),
    ).toBeVisible();
    await expect(
      workspace.getByText('Revision 4 was saved elsewhere'),
    ).toBeVisible();
    main.routes.once(read('artifact'), (route) =>
      route.fulfill({
        status: 503,
        json: { error: { code: 'unavailable', message: 'Storage is busy.' } },
      }),
    );
    await commit(server, id, 'Live 5');
    const refreshFailure = workspace
      .getByRole('alert')
      .filter({ hasText: 'Could not refresh the artifact' });
    await expect(refreshFailure).toBeVisible();
    await expect(editor).toHaveValue(html('Draft A'));
    await refreshFailure.getByRole('button', { name: 'Try again' }).click();
    await expect(
      workspace.getByText('Revision 5 was saved elsewhere'),
    ).toBeVisible();
    await expect(editor).toHaveValue(html('Draft A'));
    await workspace.getByRole('button', { name: 'Load latest' }).click();
    await expect(editor).toHaveValue(html('Live 5'));
    await workspace.getByRole('tab', { name: 'Preview' }).click();

    // W1: an artifact an agent creates appears; the open one stays open.
    const { artifact: agent } = await api(server, 'POST', '/v1/artifacts', {
      title: 'Agent artifact',
      files: [file('Agent')],
    });
    const nav = workspace.getByRole('navigation', { name: 'Artifacts' });
    await expect(
      nav.getByRole('button', { name: /Agent artifact/ }),
    ).toBeVisible();
    await expect(
      nav.getByRole('button', { name: /Live artifact/ }),
    ).toHaveAttribute('aria-current', 'true');

    // Decision 0008: a metadata change elsewhere reads the list and the open
    // artifact once, although no revision was committed; the same notice
    // again reads nothing.
    await reads.quiet();
    mark = reads.mark();
    const { metadata_revision: metadataRevision } = await api(
      server,
      'GET',
      `/v1/artifacts/${id}`,
    );
    await api(server, 'PATCH', `/v1/artifacts/${id}`, {
      base_metadata_revision: metadataRevision,
      title: 'Live artifact, renamed',
    });
    await expect(
      workspace.getByRole('heading', { name: 'Live artifact, renamed' }),
    ).toBeVisible();
    await expect(
      nav.getByRole('button', { name: /Live artifact, renamed/ }),
    ).toHaveAttribute('aria-current', 'true');
    await reads.quiet();
    assert.deepEqual(
      reads.counts(mark),
      { list: 1, artifact: 1, revisions: 1 },
      'A metadata change reads the list and the open artifact once.',
    );
    mark = reads.mark();
    await restream(main, server, [
      { ...change(id, 5, metadataRevision + 1), change: 'metadata_updated' },
    ]);
    await main.page.waitForTimeout(1000);
    assert.deepEqual(reads.counts(mark), {}, 'A known metadata change read.');
    // After a reset, a list read vouches for the metadata revision, so the
    // notice a resumed stream repeats reads nothing either.
    await restream(main, server, ['reset']);
    await reads.quiet();
    mark = reads.mark();
    await restream(main, server, [
      { ...change(id, 5, metadataRevision + 1), change: 'metadata_updated' },
    ]);
    await main.page.waitForTimeout(1000);
    assert.deepEqual(
      reads.counts(mark),
      {},
      'A metadata change already listed read.',
    );

    // Decision 7: an artifact this page creates opens, and the list read
    // before it, still shown while the new one loads, cannot close it.
    await reads.quiet();
    const listing = main.routes.hold(read('list'));
    await workspace.getByRole('button', { name: 'New artifact' }).click();
    const dialog = main.page.getByRole('dialog', { name: 'New HTML artifact' });
    await dialog.getByLabel('Title').fill('Page artifact');
    await dialog.getByLabel('index.html').fill(html('Page'));
    await dialog.getByRole('button', { name: 'Create' }).click();
    await within(listing.arrived, 'Creating must read the list');
    await expect(dialog).toBeHidden();
    await expect(nav.locator('[aria-current="true"]')).toHaveCount(0);
    listing.release();
    await expect(
      nav.getByRole('button', { name: /Page artifact/ }),
    ).toHaveAttribute('aria-current', 'true');
    await nav.getByRole('button', { name: /Live artifact/ }).click();
    await expect(badge(5)).toBeVisible();

    // W6: a hidden page gives up its stream and reads nothing; shown again,
    // it resumes after its cursor.
    await reads.quiet();
    await main.setVisible(false);
    await expect(
      workspace.getByText('Live updates paused while this tab is hidden.'),
    ).toBeVisible();
    await expect.poll(() => main.streams.open()).toBe(0);
    mark = reads.mark();
    await commit(server, id, 'Live 6');
    await main.page.waitForTimeout(1000);
    assert.deepEqual(reads.counts(mark), {}, 'A hidden page must not read.');
    await main.setVisible(true);
    await expect(badge(6)).toBeVisible();
    assert.match(
      main.streams.last().headers()['last-event-id'] ?? '',
      /^[0-9a-f]{32}\.\d+$/,
      'A page shown again resumes after its cursor.',
    );

    // W6: a view that reads twice in a row, metadata then history, sends
    // the second request only once the page is visible again.
    await reads.quiet();
    const metadata = main.routes.hold(read('artifact'));
    await commit(server, id, 'Live 7');
    await within(metadata.arrived, 'A commit must read the open artifact');
    await main.setVisible(false);
    mark = reads.mark();
    metadata.release();
    await main.page.waitForTimeout(1500);
    assert.deepEqual(
      reads.counts(mark),
      {},
      'No request may start while the page is hidden.',
    );
    await main.setVisible(true);
    await expect(badge(7)).toBeVisible();

    // W6: a read asked for before the page was hidden waits until shown.
    await reads.quiet();
    const list = main.routes.hold(read('list'));
    await commit(server, id, 'Live 8');
    await within(list.arrived, 'A commit must read the list');
    await commit(server, id, 'Live 9');
    await main.page.waitForTimeout(500);
    await main.setVisible(false);
    mark = reads.mark();
    list.release();
    await main.page.waitForTimeout(1500);
    assert.deepEqual(
      reads.counts(mark),
      {},
      'A read queued before the page was hidden waits until it is shown.',
    );
    await main.setVisible(true);
    await expect(badge(9)).toBeVisible();

    // W7: the open artifact does not read again for a revision announced
    // while its own read is pending, since that read may bring it.
    await reads.quiet();
    mark = reads.mark();
    const pending = main.routes.hold(read('artifact'));
    await commit(server, id, 'Live 10');
    await within(pending.arrived, 'A commit must read the open artifact');
    await commit(server, id, 'Live 11');
    await main.page.waitForTimeout(500);
    pending.release();
    await expect(badge(11)).toBeVisible();
    await reads.quiet();
    assert.equal(
      reads.counts(mark).artifact,
      1,
      'A revision announced during a read does not cause another read.',
    );

    // W7: notices that arrive while a read runs coalesce: of the reads they
    // ask for, only the last is sent.
    await reads.quiet();
    mark = reads.mark();
    const burst = main.routes.hold(read('list'));
    await commit(server, id, 'Live 12');
    await within(burst.arrived, 'A commit must read the list');
    for (let revision = 13; revision <= 16; revision += 1)
      await commit(server, id, `Live ${revision}`);
    await main.page.waitForTimeout(500);
    burst.release();
    await expect(badge(16)).toBeVisible();
    await reads.quiet();
    assert.equal(
      reads.counts(mark).list,
      2,
      'Commits during a read must coalesce into one more list read.',
    );

    // W7: a 429 holds every read for its Retry-After, also one longer than
    // the bound on waiting for a stalled read.
    await expectHold(main, read('list'), {
      seconds: 12,
      body: { json: { error: { code: 'rate_limited', message: 'Slow.' } } },
      cause: () => commit(server, id, 'Live 17'),
      outcome: async () => {
        await commit(server, id, 'Live 18');
        await expect(badge(18)).toBeVisible({ timeout: 25_000 });
      },
    });

    // W7: so does a proxy's 429 whose body is not an API error.
    await expectHold(main, read('artifact'), {
      seconds: 3,
      body: { contentType: 'text/html', body: '<h1>Too Many Requests</h1>' },
      cause: () => commit(server, id, 'Live 19'),
      outcome: () => expect(badge(19)).toBeVisible({ timeout: 15_000 }),
    });

    // W7: and a 429 refusing the stream itself.
    const limited = await opened();
    await expectHold(limited, stream(), {
      seconds: 3,
      body: { json: { error: { code: 'rate_limited', message: 'Slow.' } } },
      cause: () => connect(limited, server),
      outcome: () =>
        expect(
          limited.workspace.getByText('Revision 19', { exact: true }),
        ).toBeVisible({ timeout: 15_000 }),
    });

    // W5, decision 5: after a reset, content is read again although its
    // revision number is the same, once.
    await expect(label).toHaveText('Live 19');
    await reads.quiet();
    mark = reads.mark();
    let restore = replaceData(main, id, { files: { 19: 'Restored copy' } });
    await restream(main, server, ['reset']);
    await expect(editor).toHaveValue(html('Restored copy'));
    await reads.quiet();
    assert.deepEqual(
      reads.counts(mark),
      { list: 1, artifact: 1, revisions: 1, file: 1, preview: 1 },
      'A reset reads each view once, content included.',
    );
    restore();

    // Decision 5: a reset to data one revision behind reads the content
    // that revision names, once.
    mark = reads.mark();
    restore = replaceData(main, id, { upTo: 18 });
    await restream(main, server, ['reset']);
    await expect(badge(18)).toBeVisible();
    await expect(label).toHaveText('Live 18');
    await reads.quiet();
    assert.deepEqual(
      reads.counts(mark),
      { list: 1, artifact: 1, revisions: 1, file: 1, preview: 1 },
      'A reset to older data reads each view once.',
    );
    restore();

    // Decision 4: revisions known before a reset are forgotten, so the
    // revision the page saw before it is news again.
    await restream(main, server, [change(id, 19, 2)]);
    await expect(badge(19)).toBeVisible();
    await expect(label).toHaveText('Live 19');

    // Decision 4: what the open artifact showed before a reset is behind
    // what a list read after it reports, also when its own first read after
    // the reset failed.
    main.routes.once(read('artifact'), (route) =>
      route.fulfill({
        status: 503,
        json: { error: { code: 'unavailable', message: 'Storage is busy.' } },
      }),
    );
    restore = replaceData(main, id, { upTo: 18 });
    await restream(main, server, ['reset']);
    await expect(badge(18)).toBeVisible();
    await expect(label).toHaveText('Live 18');
    restore();
    await restream(main, server, [change(id, 19)]);
    await expect(badge(19)).toBeVisible();

    // W4: a reset to older data keeps a draft, marks it as possibly
    // replaced, and disables saving.
    await workspace.getByRole('tab', { name: 'Source' }).click();
    await editor.fill(html('Draft B'));
    await reads.quiet();
    mark = reads.mark();
    restore = replaceData(main, id, { upTo: 1 });
    await restream(main, server, ['reset']);
    await expect(badge(1)).toBeVisible();
    await expect(
      workspace.getByText("The server's data may have been replaced"),
    ).toBeVisible();
    await expect(
      workspace.getByRole('button', { name: 'Save revision' }),
    ).toBeDisabled();
    await expect(editor).toHaveValue(html('Draft B'));
    await reads.quiet();
    assert.deepEqual(
      reads.counts(mark),
      { list: 1, artifact: 1, revisions: 1 },
      'A draft keeps its content; only metadata is read.',
    );
    await workspace.getByRole('button', { name: 'Load latest' }).click();
    await expect(editor).toHaveValue(html('Live 1'));
    restore();

    // W4: a reset to an empty server keeps a draft until the user lets it go.
    await editor.fill(html('Draft C'));
    restore = replaceData(main, id, { upTo: 0 });
    await restream(main, server, ['reset']);
    await expect(
      workspace.getByText('No artifacts', { exact: true }),
    ).toBeVisible();
    await expect(refreshFailure).toBeVisible();
    await expect(editor).toHaveValue(html('Draft C'));
    await workspace.getByRole('button', { name: 'Load latest' }).click();
    await expect(editor).toBeHidden();
    restore();

    // W5: a stream that starts after the page read: those reads are not
    // tied to the feed, so a draft written meanwhile may be replaced.
    const late = await opened();
    const firstStream = late.routes.hold(stream());
    await connect(late, server);
    await expect(
      late.workspace.getByText('Revision 19', { exact: true }),
    ).toBeVisible();
    await expect(
      late.workspace.getByText('Connecting to live updates…'),
    ).toBeVisible();
    await late.workspace.getByRole('tab', { name: 'Source' }).click();
    await late.workspace.getByLabel('index.html').fill(html('Draft D'));
    firstStream.release();
    await expect(late.workspace.getByText('Live updates on.')).toBeVisible();
    await expect(
      late.workspace.getByText("The server's data may have been replaced"),
    ).toBeVisible();

    // W6: a page opened in the background neither reads nor subscribes.
    const background = await opened({ hidden: true });
    await connect(background, server);
    await background.page.waitForTimeout(3000);
    assert.equal(background.reads.mark(), 0, 'A background page read.');
    assert.equal(background.streams.count(), 0, 'A background page streamed.');

    // Decision 7: an artifact opened because it was listed first stays open
    // when a commit elsewhere moves another one ahead of it.
    const implicit = await opened();
    await connect(implicit, server);
    const implicitNav = implicit.workspace.getByRole('navigation', {
      name: 'Artifacts',
    });
    await expect(
      implicitNav.getByRole('button', { name: /Live artifact/ }),
    ).toHaveAttribute('aria-current', 'true');
    await commit(server, agent.id, 'Agent 2');
    await expect(implicitNav.getByRole('button').first()).toHaveAccessibleName(
      /Agent artifact/,
    );
    await expect(
      implicitNav.getByRole('button', { name: /Live artifact/ }),
    ).toHaveAttribute('aria-current', 'true');
    await expect(
      implicit.workspace.getByText('Revision 19', { exact: true }),
    ).toBeVisible();

    // Decision 8: while a save's answer is lost, a newer revision may be
    // that save, so no notice is shown; saving again replays it.
    await nav.getByRole('button', { name: /Agent artifact/ }).click();
    await expect(badge(2)).toBeVisible();
    await workspace.getByRole('tab', { name: 'Source' }).click();
    await editor.fill(html('Draft E'));
    main.routes.once(
      (request) =>
        request.method() === 'POST' &&
        new URL(request.url()).pathname.endsWith('/revisions'),
      async (route) => {
        await route.fetch();
        await route.abort('connectionreset');
      },
    );
    await workspace.getByRole('button', { name: 'Save revision' }).click();
    await expect(
      nav.getByRole('button', { name: /Agent artifact\s*r3/ }),
    ).toBeVisible();
    await main.page.waitForTimeout(500);
    await expect(workspace.getByText(/was saved elsewhere/)).toBeHidden();
    await workspace.getByRole('button', { name: 'Save revision' }).click();
    await expect(badge(3)).toBeVisible();
    await expect(editor).toHaveValue(html('Draft E'));

    // Disconnecting ends the stream.
    await workspace.getByRole('button', { name: 'Disconnect' }).click();
    await expect.poll(() => main.streams.open()).toBe(0);

    for (const [index, tracked] of pages.entries()) {
      assert.deepEqual(
        await tracked.page.evaluate(() => globalThis.readsOverlapping),
        [],
        `Page ${index} must have one read outstanding at a time.`,
      );
      assert.deepEqual(tracked.errors, [], `Page ${index} errors.`);
    }
    return { revisions: 19, pages: pages.length };
  } finally {
    // A failed check can leave a held request waiting for its release;
    // closing a context waits for running route handlers unless dropped.
    for (const { page, context } of pages) {
      await page.unrouteAll({ behavior: 'ignoreErrors' });
      await context.close();
    }
  }
}

/**
 * Refuses the next request `matches` selects with a 429 and `Retry-After:
 * seconds`, after `cause` asks for it, and checks that no read of the page
 * starts until the delay has passed while `outcome` waits for the result.
 */
async function expectHold(tracked, matches, { seconds, body, cause, outcome }) {
  await tracked.reads.quiet();
  let refusedAt;
  let mark;
  tracked.routes.once(matches, (route) => {
    refusedAt = Date.now();
    mark = tracked.reads.mark();
    return route.fulfill({
      status: 429,
      headers: { 'retry-after': String(seconds) },
      ...body,
    });
  });
  await cause();
  await expect.poll(() => refusedAt).toBeDefined();
  await outcome();
  const early = tracked.reads
    .since(mark)
    .filter((entry) => entry.start - refusedAt < seconds * 1000 - 100)
    .map((entry) => `${entry.kind} at +${entry.start - refusedAt} ms`);
  assert.deepEqual(early, [], `No read may start within Retry-After.`);
}

async function open(browser, origin, { hidden = false } = {}) {
  const context = await browser.newContext();
  await context.addInitScript(observeReads);
  if (hidden)
    await context.addInitScript(() => {
      Object.defineProperty(document, 'visibilityState', {
        configurable: true,
        get: () => 'hidden',
      });
    });
  const page = await context.newPage();
  page.setDefaultTimeout(10_000);
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));
  const tracked = {
    context,
    page,
    errors,
    ...track(page),
    routes: await control(page),
    /** Shows or hides the page as a browser does when its tab changes. */
    setVisible: (visible) =>
      page.evaluate(
        (state) => {
          Object.defineProperty(document, 'visibilityState', {
            configurable: true,
            get: () => state,
          });
          document.dispatchEvent(new Event('visibilitychange'));
        },
        visible ? 'visible' : 'hidden',
      ),
  };
  await page.goto(origin);
  tracked.workspace = page.locator('#artifacts');
  await tracked.workspace.scrollIntoViewIfNeeded();
  return tracked;
}

/**
 * Runs in the page: counts the reads the page has outstanding, from the
 * call to `fetch` until the answer's body was read, failed, or cancelled,
 * and records each read started while another was outstanding.
 */
function observeReads() {
  const send = globalThis.fetch;
  const overlapping = [];
  globalThis.readsOverlapping = overlapping;
  let outstanding = 0;
  globalThis.fetch = async (input, init) => {
    const method = (init?.method ?? 'GET').toUpperCase();
    const { pathname } = new URL(
      typeof input === 'string' ? input : input.url,
      location.href,
    );
    const isRead =
      pathname.startsWith('/v1/artifacts') &&
      (method === 'GET' || pathname.endsWith('/previews'));
    if (!isRead) return send(input, init);
    if (outstanding > 0) overlapping.push(`${method} ${pathname}`);
    outstanding += 1;
    let ended = false;
    const end = () => {
      if (!ended) outstanding -= 1;
      ended = true;
    };
    let response;
    try {
      response = await send(input, init);
    } catch (error) {
      end();
      throw error;
    }
    if (response.body === null) {
      end();
      return response;
    }
    const reader = response.body.getReader();
    const body = new ReadableStream({
      async pull(controller) {
        try {
          const chunk = await reader.read();
          if (chunk.done) {
            end();
            controller.close();
          } else controller.enqueue(chunk.value);
        } catch (error) {
          end();
          controller.error(error);
        }
      },
      cancel(reason) {
        end();
        return reader.cancel(reason);
      },
    });
    return new Response(body, {
      status: response.status,
      statusText: response.statusText,
      headers: response.headers,
    });
  };
}

async function connect({ workspace }, server) {
  await workspace.getByLabel('Access token').fill(server.token);
  await workspace.getByRole('button', { name: 'Connect' }).click();
}

/** Which view a request reads, or undefined for the stream and writes. */
function kind(request) {
  const { pathname } = new URL(request.url());
  const parts = pathname.split('/').filter(Boolean);
  if (parts[0] !== 'v1' || parts[1] !== 'artifacts') return undefined;
  if (request.method() === 'POST')
    return parts.at(-1) === 'previews' ? 'preview' : undefined;
  if (request.method() !== 'GET') return undefined;
  if (parts.length === 2) return 'list';
  if (parts.length === 3) return 'artifact';
  if (parts.length === 4 && parts[3] === 'revisions') return 'revisions';
  if (parts[5] === 'files') return 'file';
  return 'revision';
}

function read(name) {
  return (request) => kind(request) === name;
}

function stream() {
  return (request) => new URL(request.url()).pathname === '/v1/changes';
}

/** Records the page's reads and streams as the browser sends them. */
function track(page) {
  const entries = [];
  const running = new Map();
  let lastActivity = Date.now();
  const streams = [];
  const openStreams = new Set();
  page.on('request', (request) => {
    lastActivity = Date.now();
    if (stream()(request)) {
      streams.push(request);
      openStreams.add(request);
      return;
    }
    const name = kind(request);
    if (name === undefined) return;
    entries.push({ kind: name, start: Date.now() });
    running.set(request, entries.length - 1);
  });
  const done = (request) => {
    lastActivity = Date.now();
    openStreams.delete(request);
    running.delete(request);
  };
  page.on('requestfinished', done);
  page.on('requestfailed', done);
  return {
    reads: {
      mark: () => entries.length,
      since: (mark) => entries.slice(mark),
      counts(mark) {
        const counts = {};
        for (const entry of entries.slice(mark))
          counts[entry.kind] = (counts[entry.kind] ?? 0) + 1;
        return counts;
      },
      /** Waits until no read runs and none started for half a second. */
      async quiet() {
        await expect
          .poll(() => running.size === 0 && Date.now() - lastActivity > 500, {
            intervals: [100],
          })
          .toBe(true);
      },
    },
    streams: {
      count: () => streams.length,
      open: () => openStreams.size,
      last: () => streams.at(-1),
    },
  };
}

/**
 * Answers chosen API requests instead of the server. `once` handles the
 * next matching request, `always` each one until removed, and `hold`
 * delays the next one until released.
 */
async function control(page) {
  const rules = [];
  await page.route('**/v1/**', async (route) => {
    const rule = rules.find((candidate) => candidate.matches(route.request()));
    if (rule === undefined) return route.fallback();
    if (rule.once) rules.splice(rules.indexOf(rule), 1);
    return rule.handle(route);
  });
  return {
    once(matches, handle) {
      rules.push({ matches, handle, once: true });
    },
    always(matches, handle) {
      const rule = { matches, handle, once: false };
      rules.push(rule);
      return () => rules.splice(rules.indexOf(rule), 1);
    },
    hold(matches) {
      let arrive;
      let release;
      const arrived = new Promise((resolve) => {
        arrive = resolve;
      });
      const released = new Promise((resolve) => {
        release = resolve;
      });
      rules.push({
        matches,
        once: true,
        async handle(route) {
          arrive();
          await released;
          await route.fallback();
        },
      });
      return { arrived, release };
    },
  };
}

/**
 * Answers the reads of the page as a server whose data was replaced: the
 * artifact `id` stops at revision `upTo` (0: there is no artifact at all),
 * and `files` gives other content for a revision's `index.html`. Returns
 * the function that stops.
 */
function replaceData(tracked, id, { upTo, files = {} }) {
  const answer = async (route) => {
    const request = route.request();
    const name = kind(request);
    const { pathname } = new URL(request.url());
    if (name === 'file') {
      const revision = Number(pathname.split('/')[5]);
      if (files[revision] !== undefined)
        return route.fulfill({
          contentType: 'text/html',
          body: html(files[revision]),
        });
      return route.fallback();
    }
    if (upTo === undefined) return route.fallback();
    if (name === 'list') {
      const body = await (await route.fetch()).json();
      const artifacts =
        upTo === 0
          ? []
          : body.artifacts.map((item) =>
              item.id === id ? { ...item, current_revision: upTo } : item,
            );
      return route.fulfill({ json: { artifacts } });
    }
    if (!pathname.includes(id)) return route.fallback();
    if (upTo === 0)
      return route.fulfill({
        status: 404,
        json: { error: { code: 'not_found', message: 'No such artifact.' } },
      });
    if (name === 'artifact') {
      const body = await (await route.fetch()).json();
      return route.fulfill({ json: { ...body, current_revision: upTo } });
    }
    if (name === 'revisions') {
      const body = await (await route.fetch()).json();
      return route.fulfill({
        json: {
          revisions: body.revisions.filter(({ revision }) => revision <= upTo),
        },
      });
    }
    return route.fallback();
  };
  return tracked.routes.always(
    (request) => kind(request) !== undefined && kind(request) !== 'preview',
    answer,
  );
}

/**
 * Makes the page open a new stream, answered with `events` instead of the
 * server; `'reset'` stands for a reset. The page then reconnects to the
 * server after the forged cursor, which is the real head.
 */
async function restream(tracked, server, events) {
  const cursor = await head(server);
  let served;
  const answered = new Promise((resolve) => {
    served = resolve;
  });
  tracked.routes.once(stream(), (route) => {
    served();
    const body = events
      .map((event) =>
        event === 'reset'
          ? sse('reset', { cursor })
          : sse('change', { ...event, cursor }),
      )
      .join('');
    return route.fulfill({
      headers: { 'content-type': 'text/event-stream' },
      body,
    });
  });
  await tracked.setVisible(false);
  await tracked.setVisible(true);
  await within(answered, 'A page shown again must open a new stream');
}

/**
 * Waits for `promise`, failing if it has not settled within ten seconds, so
 * a request that never comes fails the check instead of stopping it.
 */
async function within(promise, expectation) {
  let timer;
  const timeout = new Promise((resolve, reject) => {
    timer = setTimeout(
      () => reject(new Error(`${expectation} within 10 s.`)),
      10_000,
    );
  });
  try {
    return await Promise.race([promise, timeout]);
  } finally {
    clearTimeout(timer);
  }
}

function sse(type, data) {
  return `event: ${type}\ndata: ${JSON.stringify(data)}\n\n`;
}

function change(id, revision, metadataRevision = 1) {
  return {
    artifact_id: id,
    change: 'revised',
    revision,
    metadata_revision: metadataRevision,
    committed_at_ms: Date.now(),
  };
}

/** The feed's current position, from a stream opened without a cursor. */
async function head(server) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 10_000);
  try {
    const response = await fetch(`${server.api_url}/v1/changes`, {
      headers: { authorization: `Bearer ${server.token}` },
      signal: controller.signal,
    });
    assert.equal(response.status, 200, 'The feed must open.');
    const decoder = new TextDecoder();
    let text = '';
    for await (const chunk of response.body) {
      text += decoder.decode(chunk, { stream: true });
      for (const block of text.split('\n\n').slice(0, -1)) {
        const lines = block.split('\n');
        if (!lines.includes('event: ready')) continue;
        const data = lines.find((line) => line.startsWith('data: '));
        if (data !== undefined) return JSON.parse(data.slice(6)).cursor;
      }
    }
    throw new Error('The feed ended before ready.');
  } finally {
    clearTimeout(timer);
    controller.abort();
  }
}

async function commit(server, id, text) {
  const { current_revision: base } = await api(
    server,
    'GET',
    `/v1/artifacts/${id}`,
  );
  await api(server, 'POST', `/v1/artifacts/${id}/revisions`, {
    base_revision: base,
    changes: [{ op: 'put', ...file(text) }],
  });
}

function file(text) {
  return { path: 'index.html', encoding: 'utf8', content: html(text) };
}

function html(text) {
  return `<!doctype html><title>${text}</title><p id="label">${text}</p>\n`;
}

function preview({ page }) {
  return page.frameLocator('iframe[title^="Preview of"]');
}
