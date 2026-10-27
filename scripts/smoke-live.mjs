import assert from 'node:assert/strict';
import { expect } from '@playwright/test';
import { api } from './smoke-workspace.mjs';

/**
 * Browser checks for following committed changes (decision 0007), against a
 * real artifree-server. Each page runs in its own browser context, so its
 * reads, stream, and visibility are its own. Commits come from the API, as
 * another client's would. Forged streams and rewritten responses stand in
 * for what one server cannot be made to do on demand: repeat old changes,
 * or answer `reset` after its database was replaced.
 */
export async function checkLiveUpdates({ browser, origin, server }) {
  const created = await api(server, 'POST', '/v1/artifacts', {
    title: 'Live artifact',
    files: [file('Live 1')],
  });
  const id = created.artifact.id;
  const pages = [];
  const opened = async (options) => {
    const page = await open(browser, origin, options);
    pages.push(page);
    return page;
  };
  try {
    const main = await opened();
    await connect(main, server);
    const { workspace, reads } = main;
    const editor = workspace.getByLabel('index.html');
    const badge = (revision) =>
      workspace.getByText(`Revision ${revision}`, { exact: true });

    // W5, W7: the first read waits for the stream, so loading reads each
    // view once, one read at a time.
    await expect(workspace.getByText('Live updates on.')).toBeVisible();
    await expect(badge(1)).toBeVisible();
    await expect(preview(main).locator('#label')).toHaveText('Live 1');
    await reads.quiet();
    assert.deepEqual(
      reads.counts(0),
      { list: 1, artifact: 1, revisions: 1, file: 1, preview: 1 },
      'Loading must read each view once.',
    );

    // W1, W3: a commit elsewhere reads each view once more.
    let mark = reads.mark();
    await commit(server, id, 'Live 2');
    await expect(badge(2)).toBeVisible();
    await expect(preview(main).locator('#label')).toHaveText('Live 2');
    await reads.quiet();
    assert.deepEqual(
      reads.counts(mark),
      { list: 1, artifact: 1, revisions: 1, file: 1, preview: 1 },
      'A commit elsewhere must read each view once.',
    );

    // W2: a pinned preview stays on its revision, also across the tabs.
    const revisionMenu = workspace.getByLabel('Revision', { exact: true });
    await revisionMenu.selectOption('1');
    await expect(preview(main).locator('#label')).toHaveText('Live 1');
    await commit(server, id, 'Live 3');
    await expect(badge(3)).toBeVisible();
    await workspace.getByRole('tab', { name: 'Source' }).click();
    await expect(editor).toHaveValue(/Live 3/);
    await workspace.getByRole('tab', { name: 'Preview' }).click();
    await expect(preview(main).locator('#label')).toHaveText('Live 1');
    await expect(revisionMenu).toHaveValue('1');
    await revisionMenu.selectOption({ label: 'Latest (3)' });
    await expect(preview(main).locator('#label')).toHaveText('Live 3');

    // W3: a repeated or older change reads nothing.
    await reads.quiet();
    mark = reads.mark();
    await restream(main, server, [change(id, 3), change(id, 2)]);
    await main.page.waitForTimeout(1000);
    assert.deepEqual(
      reads.counts(mark),
      {},
      'A repeated or older change must not read.',
    );

    // W4: a draft survives a newer revision and a failed refresh; the page
    // says what happened and lets the user decide.
    await workspace.getByRole('tab', { name: 'Source' }).click();
    await editor.fill(html('Draft A'));
    await commit(server, id, 'Live 4');
    await expect(
      workspace.getByText('Revision 4 was saved elsewhere'),
    ).toBeVisible();
    await expect(editor).toHaveValue(/Draft A/);
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
    await expect(badge(4)).toBeVisible();
    await expect(editor).toHaveValue(/Draft A/);
    await refreshFailure.getByRole('button', { name: 'Try again' }).click();
    await expect(badge(5)).toBeVisible();
    await expect(
      workspace.getByText('Revision 5 was saved elsewhere'),
    ).toBeVisible();
    await expect(editor).toHaveValue(/Draft A/);
    await workspace.getByRole('button', { name: 'Load latest' }).click();
    await expect(editor).toHaveValue(/Live 5/);

    // W1: an artifact an agent creates is listed; the open one stays.
    await api(server, 'POST', '/v1/artifacts', {
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

    // W6: a hidden page gives up its stream and reads nothing; shown again,
    // it resumes from its cursor and catches up.
    await reads.quiet();
    await setVisible(main.page, false);
    await expect(
      workspace.getByText('Live updates paused while this tab is hidden.'),
    ).toBeVisible();
    await expect.poll(() => main.streams.open()).toBe(0);
    mark = reads.mark();
    await commit(server, id, 'Live 6');
    await main.page.waitForTimeout(1000);
    assert.deepEqual(reads.counts(mark), {}, 'A hidden page must not read.');
    await setVisible(main.page, true);
    await expect(badge(6)).toBeVisible();
    assert.match(
      main.streams.last().headers()['last-event-id'] ?? '',
      /^[0-9a-f]{32}\.\d+$/,
      'A shown page must resume from its cursor.',
    );

    // W6: reads asked for before the page was hidden do not start while it
    // is hidden. The list read is held in flight; meanwhile another commit
    // asks for more reads, which wait behind it; then the page is hidden.
    await reads.quiet();
    const held = main.routes.hold(read('list'));
    await commit(server, id, 'Live 7');
    await within(held.arrived, 'A commit must read the list');
    await commit(server, id, 'Live 8');
    await main.page.waitForTimeout(500);
    await setVisible(main.page, false);
    mark = reads.mark();
    held.release();
    await main.page.waitForTimeout(1500);
    assert.deepEqual(
      reads.counts(mark),
      {},
      'A read queued before the page was hidden must wait until it is shown.',
    );
    await setVisible(main.page, true);
    await expect(badge(8)).toBeVisible();
    await workspace.getByRole('tab', { name: 'Preview' }).click();
    await expect(preview(main).locator('#label')).toHaveText('Live 8');

    // W7: the open artifact does not read again for a revision announced
    // while its own read is pending, since that read may bring it.
    await reads.quiet();
    mark = reads.mark();
    const pending = main.routes.hold(read('artifact'));
    await commit(server, id, 'Live 9');
    await within(pending.arrived, 'A commit must read the open artifact');
    await commit(server, id, 'Live 10');
    await main.page.waitForTimeout(500);
    pending.release();
    await expect(badge(10)).toBeVisible();
    await reads.quiet();
    assert.equal(
      reads.counts(mark).artifact,
      1,
      'A revision announced during a read must not cause another read.',
    );

    // W7: a burst of commits reads one at a time and far less than once per
    // commit.
    await reads.quiet();
    mark = reads.mark();
    for (let index = 11; index <= 15; index += 1)
      await commit(server, id, `Live ${index}`);
    await expect(badge(15)).toBeVisible();
    await reads.quiet();
    assert.ok(
      (reads.counts(mark).list ?? 0) < 5,
      `A burst must not read the list once per commit: ${JSON.stringify(reads.counts(mark))}`,
    );

    // W7: a 429 holds every read of the page for its Retry-After, also when
    // that is longer than the bound on waiting for a stalled read.
    await reads.quiet();
    let limitedAt;
    let limitedMark;
    main.routes.once(read('list'), (route) => {
      limitedAt = Date.now();
      limitedMark = reads.mark();
      return route.fulfill({
        status: 429,
        headers: { 'retry-after': '12' },
        json: { error: { code: 'rate_limited', message: 'Slow down.' } },
      });
    });
    await commit(server, id, 'Live 16');
    await expect.poll(() => limitedAt).toBeDefined();
    await commit(server, id, 'Live 17');
    await expect(badge(17)).toBeVisible({ timeout: 20_000 });
    const timeline = reads
      .since(limitedMark)
      .map((entry) => `${entry.path} at +${entry.start - limitedAt} ms`);
    assert.ok(
      reads
        .since(limitedMark)
        .every((entry) => entry.start - limitedAt >= 11_900),
      `No read may start before Retry-After has passed: ${timeline.join(', ')}`,
    );

    // W5, decision 7: after a reset, content is read again even for the
    // same revision number, once, for the revision the new metadata names.
    await expect(preview(main).locator('#label')).toHaveText('Live 17');
    await reads.quiet();
    mark = reads.mark();
    const restored = main.routes.always(read('file'), (route) =>
      route.fulfill({ contentType: 'text/html', body: html('Restored copy') }),
    );
    await restream(main, server, ['reset']);
    await expect(
      preview(main).locator('#label'),
      'A preview of the same revision number is issued again.',
    ).toHaveText('Live 17');
    await reads.quiet();
    assert.deepEqual(
      reads.counts(mark),
      { list: 1, artifact: 1, revisions: 1, file: 1, preview: 1 },
      'A reset must read each view once, content included.',
    );
    await workspace.getByRole('tab', { name: 'Source' }).click();
    await expect(editor).toHaveValue(/Restored copy/);
    restored();

    // When the new metadata names another revision, content is read once,
    // for that revision, not first for the one shown before.
    await workspace.getByRole('tab', { name: 'Preview' }).click();
    await reads.quiet();
    mark = reads.mark();
    const behind = replaceData(main, { id, revision: 16 });
    await restream(main, server, ['reset']);
    await expect(badge(16)).toBeVisible();
    await expect(preview(main).locator('#label')).toHaveText('Live 16');
    await reads.quiet();
    assert.deepEqual(
      reads.counts(mark),
      { list: 1, artifact: 1, revisions: 1, file: 1, preview: 1 },
      'Content must be read once, for the revision the new metadata names.',
    );
    behind();
    await restream(main, server, ['reset']);
    await expect(badge(17)).toBeVisible();
    await workspace.getByRole('tab', { name: 'Source' }).click();

    // W4: after a reset to older data, a draft is kept but cannot be saved,
    // since revision numbers may now name other content.
    await editor.fill(html('Draft B'));
    await reads.quiet();
    mark = reads.mark();
    const older = replaceData(main, { id, revision: 1 });
    await restream(main, server, ['reset']);
    await expect(
      workspace.getByText("The server's data may have been replaced"),
    ).toBeVisible();
    await expect(badge(1)).toBeVisible();
    await reads.quiet();
    // The list shown before the reset still names revision 17. Taken as
    // current, it would make the page read the artifact again for it.
    assert.deepEqual(
      reads.counts(mark),
      { list: 1, artifact: 1, revisions: 1 },
      'A reset to older data must read once, by what the new data says.',
    );
    await expect(editor).toHaveValue(/Draft B/);
    await expect(
      workspace.getByRole('button', { name: 'Save revision' }),
    ).toBeDisabled();
    await workspace.getByRole('button', { name: 'Load latest' }).click();
    await expect(editor).toHaveValue(/Live 1/);
    older();

    // W4: a reset to an empty server keeps the open draft until the user
    // discards it.
    await restream(main, server, ['reset']);
    await expect(badge(17)).toBeVisible();
    await editor.fill(html('Draft C'));
    const empty = replaceData(main, { id, revision: undefined });
    await restream(main, server, ['reset']);
    await expect(workspace.getByText('No artifacts yet')).toBeVisible();
    await expect(
      workspace.getByText('Could not refresh the artifact'),
    ).toBeVisible();
    await expect(editor).toHaveValue(/Draft C/);
    await workspace.getByRole('button', { name: 'Load latest' }).click();
    await expect(editor).toBeHidden();
    empty();
    await restream(main, server, ['reset']);
    await expect(badge(17)).toBeVisible();

    // W7: a stream refused with 429 holds the page's reads for its
    // Retry-After, as a refused read does: the limit is the actor's.
    const limited = await opened();
    let streamLimitedAt;
    limited.routes.once(stream(), (route) => {
      streamLimitedAt = Date.now();
      return route.fulfill({
        status: 429,
        headers: { 'retry-after': '3' },
        json: { error: { code: 'rate_limited', message: 'Slow down.' } },
      });
    });
    await connect(limited, server);
    await expect(
      limited.workspace.getByText('Revision 17', { exact: true }),
    ).toBeVisible();
    assert.ok(streamLimitedAt !== undefined, 'The stream must be refused.');
    const early = limited.reads
      .since(0)
      .filter((entry) => entry.start - streamLimitedAt < 2_900)
      .map((entry) => `${entry.path} at +${entry.start - streamLimitedAt} ms`);
    assert.deepEqual(
      early,
      [],
      "No read may start before the stream's Retry-After has passed.",
    );

    // W5: a stream that starts only after the page has read: the reads are
    // not tied to the feed, so a draft written meanwhile is marked as
    // possibly replaced.
    const late = await opened();
    const firstStream = late.routes.hold(stream());
    await connect(late, server);
    await expect(
      late.workspace.getByText('Revision 17', { exact: true }),
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

    // W6: a page opened in the background reads nothing until it is shown.
    const background = await opened({ hidden: true });
    await connect(background, server);
    await background.page.waitForTimeout(2500);
    assert.deepEqual(
      background.reads.counts(0),
      {},
      'A background page must not read.',
    );
    assert.equal(
      background.streams.count(),
      0,
      'A background page must not stream.',
    );
    await setVisible(background.page, true);
    await expect(
      background.workspace.getByText('Revision 17', { exact: true }),
    ).toBeVisible();
    await expect(
      background.workspace.getByText('Live updates on.'),
    ).toBeVisible();

    // Disconnecting ends the subscription.
    await expect.poll(() => main.streams.open()).toBe(1);
    await workspace.getByRole('button', { name: 'Disconnect' }).click();
    await expect.poll(() => main.streams.open()).toBe(0);

    const maxInFlight = Math.max(
      ...pages.map((page) => page.reads.maxInFlight()),
    );
    assert.equal(maxInFlight, 1, 'A page must send one read at a time.');
    const errors = pages.flatMap((page) => page.errors);
    assert.deepEqual(errors, [], 'Browser errors in live update pages.');
    return { pages: pages.length, maxInFlight, rateLimitHeldMs: 12_000 };
  } finally {
    // A failed check can leave a held request waiting for its release;
    // closing waits for running route handlers unless they are dropped.
    for (const { page, context } of pages) {
      await page.unrouteAll({ behavior: 'ignoreErrors' });
      await context.close();
    }
  }
}

/** A new context and page on the workspace, with reads tracked. */
async function open(browser, origin, { hidden = false } = {}) {
  const context = await browser.newContext({
    viewport: { width: 1440, height: 1000 },
    reducedMotion: 'reduce',
  });
  if (hidden) await context.addInitScript(hide);
  const page = await context.newPage();
  page.setDefaultTimeout(10_000);
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));
  page.on('console', (message) => {
    // Failed API responses provoked on purpose are logged by Chromium.
    if (message.type() === 'error' && !/status of \d+/.test(message.text()))
      errors.push(message.text());
  });
  const tracked = { context, page, errors, ...track(page) };
  tracked.routes = await control(page);
  await page.goto(origin);
  tracked.workspace = page.locator('#artifacts');
  await tracked.workspace.scrollIntoViewIfNeeded();
  return tracked;
}

async function connect({ workspace }, server) {
  await workspace.getByLabel('Access token').fill(server.token);
  await workspace.getByRole('button', { name: 'Connect' }).click();
}

function hide() {
  Object.defineProperty(document, 'visibilityState', {
    configurable: true,
    get: () => 'hidden',
  });
}

/** Shows or hides the page as a browser does when its tab changes. */
async function setVisible(page, visible) {
  await page.evaluate(
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

/** What a request reads, or undefined for the feed and for writes. */
function kind(request) {
  const { pathname } = new URL(request.url());
  if (!pathname.startsWith('/v1/artifacts')) return undefined;
  if (request.method() === 'POST')
    return pathname.endsWith('/previews') ? 'preview' : undefined;
  if (pathname === '/v1/artifacts') return 'list';
  if (pathname.includes('/files/')) return 'file';
  if (pathname.endsWith('/revisions')) return 'revisions';
  return 'artifact';
}

const read = (name) => (request) => kind(request) === name;
const stream = () => (request) =>
  new URL(request.url()).pathname === '/v1/changes';

/** Records the page's reads and change streams. */
function track(page) {
  const entries = [];
  const open = new Map();
  let inFlight = 0;
  let maxInFlight = 0;
  let lastActivity = Date.now();
  const streams = [];
  const openStreams = new Set();
  page.on('request', (request) => {
    if (stream()(request)) {
      streams.push(request);
      openStreams.add(request);
      return;
    }
    const name = kind(request);
    if (name === undefined) return;
    const entry = {
      kind: name,
      path: new URL(request.url()).pathname,
      start: Date.now(),
    };
    entries.push(entry);
    open.set(request, entry);
    inFlight += 1;
    maxInFlight = Math.max(maxInFlight, inFlight);
    lastActivity = Date.now();
  });
  const done = (request) => {
    openStreams.delete(request);
    if (!open.delete(request)) return;
    inFlight -= 1;
    lastActivity = Date.now();
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
      maxInFlight: () => maxInFlight,
      /** Waits until no read ran for half a second. */
      async quiet() {
        await expect
          .poll(() => inFlight === 0 && Date.now() - lastActivity > 500, {
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
 * next matching request, `always` every one until removed, and `hold`
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
 * Makes the page open a new stream, answered with `events` instead of the
 * server; `'reset'` stands for a reset event. The page then reconnects to
 * the server after the forged position, which is its real head.
 */
async function restream(tracked, server, events) {
  const position = await head(server);
  let served;
  const answered = new Promise((resolve) => {
    served = resolve;
  });
  tracked.routes.once(stream(), (route) => {
    served();
    const body = events
      .map((event) => {
        if (event === 'reset')
          return sse('reset', position, { cursor: position });
        return sse('change', position, { ...event, cursor: position });
      })
      .join('');
    return route.fulfill({
      status: 200,
      headers: { 'content-type': 'text/event-stream' },
      body,
    });
  });
  await setVisible(tracked.page, false);
  await setVisible(tracked.page, true);
  await within(answered, 'A page shown again must open a new stream');
}

/**
 * Waits for `promise`, failing if it has not settled within ten seconds, so
 * a missing request fails the check instead of stopping it.
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

function sse(type, cursor, data) {
  return `event: ${type}\nid: ${cursor}\ndata: ${JSON.stringify(data)}\n\n`;
}

function change(id, revision) {
  return {
    artifact_id: id,
    change: 'revised',
    revision,
    committed_at_ms: Date.now(),
  };
}

/** The feed's current position, from a stream opened without a cursor. */
async function head(server) {
  const controller = new AbortController();
  const response = await fetch(`${server.api_url}/v1/changes`, {
    headers: { authorization: `Bearer ${server.token}` },
    signal: controller.signal,
  });
  const reader = response.body.getReader();
  let text = '';
  while (!/\n\n/.test(text)) {
    const { value, done } = await reader.read();
    if (done) break;
    text += new TextDecoder().decode(value);
  }
  controller.abort();
  const cursor = /^id: (.+)$/m.exec(text)?.[1];
  assert.ok(cursor, `The feed must start with ready: ${text}`);
  return cursor;
}

/**
 * Answers the page's reads as a replaced server would: the artifact at
 * `revision` with only that revision listed, or, without a revision, an
 * empty server. Returns a function that removes the replacement.
 */
function replaceData(tracked, { id, revision }) {
  const removers = [
    tracked.routes.always(read('list'), async (route) => {
      const response = await route.fetch();
      const body = await response.json();
      body.artifacts =
        revision === undefined
          ? []
          : body.artifacts
              .filter((artifact) => artifact.id === id)
              .map((artifact) => ({ ...artifact, current_revision: revision }));
      return route.fulfill({ response, json: body });
    }),
    tracked.routes.always(read('artifact'), async (route) => {
      if (revision === undefined)
        return route.fulfill({
          status: 404,
          json: { error: { code: 'not_found', message: 'artifact not found' } },
        });
      const response = await route.fetch();
      const body = await response.json();
      return route.fulfill({
        response,
        json: { ...body, current_revision: revision },
      });
    }),
    tracked.routes.always(read('revisions'), async (route) => {
      const response = await route.fetch();
      const body = await response.json();
      body.revisions = body.revisions.filter(
        (item) => item.revision === revision,
      );
      return route.fulfill({ response, json: body });
    }),
  ];
  return () => {
    for (const remove of removers) remove();
  };
}

function preview({ page }) {
  return page.frameLocator('iframe[title^="Preview of"]');
}

function html(label) {
  return `<!doctype html><html lang="en"><head><meta charset="utf-8"><title>${label}</title></head><body><h1 id="label">${label}</h1></body></html>\n`;
}

function file(label) {
  return { path: 'index.html', encoding: 'utf8', content: html(label) };
}

/** Commits `label` as the next revision, as another client would. */
async function commit(server, id, label) {
  const { current_revision: base } = await api(
    server,
    'GET',
    `/v1/artifacts/${id}`,
  );
  await api(server, 'POST', `/v1/artifacts/${id}/revisions`, {
    base_revision: base,
    changes: [{ op: 'put', ...file(label) }],
  });
}
