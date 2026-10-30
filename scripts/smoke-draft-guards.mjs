import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { expect } from '@playwright/test';
import { api } from './smoke-workspace.mjs';

const root = fileURLToPath(new URL('../', import.meta.url));
const ownerKey = 'artifree.draftOwner';

/** Report unexpected script and console errors from these separate pages. */
function observe(context, contentOrigin) {
  const errors = [];
  const artifactConsole = [];
  context.on('page', (page) => {
    page.on('pageerror', (error) => errors.push(error.message));
    page.on('console', (message) => {
      if (message.type() !== 'error' && message.type() !== 'warning') return;
      if (message.location().url.startsWith(contentOrigin))
        artifactConsole.push(message.text());
      else errors.push(message.text());
    });
  });
  return { errors, artifactConsole };
}

/** The kept text of one artifact, read from the browser's real IndexedDB. */
function kept(page, artifactId) {
  return page.evaluate(
    (id) =>
      new Promise((resolveRows, reject) => {
        const opening = indexedDB.open('artifree-source-drafts', 1);
        opening.onerror = () => reject(opening.error);
        opening.onsuccess = () => {
          const db = opening.result;
          const request = db
            .transaction('drafts')
            .objectStore('drafts')
            .getAll();
          request.onerror = () => reject(request.error);
          request.onsuccess = () => {
            const rows = request.result
              .filter((row) => row.artifactId === id)
              .map((row) => ({
                owner: row.owner,
                text: row.draft.changes.get('index.html')?.text,
              }));
            db.close();
            resolveRows(rows);
          };
        };
      }),
    artifactId,
  );
}

/** Opens the newly created artifact in a separate page. */
async function open(context, origin, server, title, copiedOwner, incomplete) {
  const page = await context.newPage();
  page.setDefaultTimeout(10_000);
  if (copiedOwner !== undefined)
    await page.addInitScript(
      ([key, owner]) => {
        if (window !== window.top) return;
        Object.defineProperty(navigator, 'locks', {
          configurable: true,
          value: undefined,
        });
        sessionStorage.setItem(key, owner);
      },
      [ownerKey, copiedOwner],
    );
  if (incomplete !== undefined)
    await page.addInitScript((kind) => {
      if (window !== window.top) return;
      Object.defineProperty(navigator.locks, 'query', {
        configurable: true,
        value: async () =>
          kind === 'missing-held'
            ? {}
            : { held: [{ mode: 'exclusive', clientId: 'unknown' }] },
      });
    }, incomplete);
  await page.goto(origin);
  const area = page.locator('#artifacts');
  await area.getByLabel('Access token').fill(server.token);
  await area.getByRole('button', { name: 'Connect' }).click();
  await expect(
    area.getByRole('heading', { name: title, exact: true }),
  ).toBeVisible();
  await area.getByRole('tab', { name: 'Source' }).click();
  return { page, area, text: area.getByLabel('index.html', { exact: true }) };
}

async function create(server, title) {
  return (
    await api(server, 'POST', '/v1/artifacts', {
      title,
      files: [{ path: 'index.html', encoding: 'utf8', content: '<p>base</p>' }],
    })
  ).artifact;
}

/** A copied owner without Web Locks cannot overwrite or delete a live draft. */
export async function checkDraftsWithoutLocks({
  browser,
  origin,
  server,
  checkAccessibility,
}) {
  const artifact = await create(server, 'No Web Locks');
  const context = await browser.newContext();
  const { errors, artifactConsole } = observe(context, server.content_origin);
  try {
    const first = await open(context, origin, server, artifact.title);
    await first.text.fill('<p>first draft</p>');
    const owner = await first.page.evaluate(
      (key) => sessionStorage.getItem(key),
      ownerKey,
    );
    assert.ok(owner);
    const original = [{ owner, text: '<p>first draft</p>' }];
    await expect.poll(() => kept(first.page, artifact.id)).toEqual(original);

    // Both pages receive the copied id but have no reliable way to prove
    // that its owner is still alive. They must report the unavailable store.
    const second = await open(context, origin, server, artifact.title, owner);
    const third = await open(context, origin, server, artifact.title, owner);
    for (const tab of [second, third]) {
      const lookup = tab.area.getByRole('alert').filter({
        hasText: 'Drafts kept in this browser could not be read',
      });
      await expect(
        lookup,
        'Without Web Locks, draft lookup must fail.',
      ).toBeVisible();
      await expect(
        lookup.getByText(
          /Web Locks are required to read or keep drafts safely/,
        ),
      ).toBeVisible();
      await checkAccessibility(
        tab === second ? 'drafts-no-lock-second' : 'drafts-no-lock-third',
        tab.page,
      );
      await tab.area.getByRole('button', { name: 'Edit without them' }).click();
      await tab.text.fill(
        tab === second ? '<p>second draft</p>' : '<p>third draft</p>',
      );
      await expect(
        tab.area.getByRole('alert').filter({
          hasText: 'This draft is not kept in this browser',
        }),
      ).toBeVisible();
    }
    await expect.poll(() => kept(first.page, artifact.id)).toEqual(original);
    await expect(first.text).toHaveValue('<p>first draft</p>');

    // Discard before the other page saves; a later revision changes this
    // editor's available action to "Load latest".
    await expect(third.text).toHaveValue('<p>third draft</p>');
    await third.area.getByRole('button', { name: 'Discard changes' }).click();
    await expect(
      third.area.getByRole('alert').filter({
        hasText: 'Could not remove the kept draft',
      }),
    ).toBeVisible();
    await third.area
      .getByRole('button', { name: 'Continue without removing it' })
      .click();
    await expect.poll(() => kept(first.page, artifact.id)).toEqual(original);

    // A server save can succeed even though local cleanup cannot. Its
    // explicit continuation must leave the first page's record untouched.
    await second.area.getByRole('button', { name: 'Save revision' }).click();
    await expect(
      second.area.getByRole('alert').filter({
        hasText: 'Could not remove the kept draft',
      }),
    ).toBeVisible();
    await expect(second.area.getByText(/The revision was saved/)).toBeVisible();
    await second.area
      .getByRole('button', { name: 'Continue without removing it' })
      .click();
    await expect.poll(() => kept(first.page, artifact.id)).toEqual(original);

    assert.deepEqual(errors, []);
    return { protectedOwner: true, unkeptPages: 2, artifactConsole };
  } finally {
    await context.close();
  }
}

/** An incomplete lock snapshot cannot make an active draft look abandoned. */
export async function checkIncompleteLockSnapshot({
  browser,
  origin,
  server,
  checkAccessibility,
}) {
  const artifact = await create(server, 'Incomplete lock snapshot');
  const context = await browser.newContext();
  const { errors, artifactConsole } = observe(context, server.content_origin);
  try {
    const first = await open(context, origin, server, artifact.title);
    await first.text.fill('<p>active owner</p>');
    const owner = await first.page.evaluate(
      (key) => sessionStorage.getItem(key),
      ownerKey,
    );
    assert.ok(owner);
    const original = [{ owner, text: '<p>active owner</p>' }];
    await expect.poll(() => kept(first.page, artifact.id)).toEqual(original);

    for (const incomplete of ['missing-held', 'missing-name']) {
      const other = await open(
        context,
        origin,
        server,
        artifact.title,
        undefined,
        incomplete,
      );
      const lookup = other.area.getByRole('alert').filter({
        hasText: 'Drafts kept in this browser could not be read',
      });
      await expect(
        lookup,
        `An incomplete ${incomplete} lock snapshot must block lookup.`,
      ).toBeVisible();
      await expect(
        lookup.getByText('The browser could not list active draft owners.'),
      ).toBeVisible();
      await expect(
        other.area.getByRole('button', { name: 'Recover draft' }),
      ).toHaveCount(0);
      await checkAccessibility(`drafts-incomplete-${incomplete}`, other.page);
      await other.area
        .getByRole('button', { name: 'Edit without them' })
        .click();
      await other.text.fill('<p>separate draft</p>');
      const otherOwner = await other.page.evaluate(
        (key) => sessionStorage.getItem(key),
        ownerKey,
      );
      assert.ok(otherOwner && otherOwner !== owner);
      await expect
        .poll(() => kept(first.page, artifact.id))
        .toContainEqual({
          owner: otherOwner,
          text: '<p>separate draft</p>',
        });
      await expect
        .poll(() => kept(first.page, artifact.id))
        .toContainEqual(original[0]);
      await other.area.getByRole('button', { name: 'Discard changes' }).click();
      await expect.poll(() => kept(first.page, artifact.id)).toEqual(original);
    }
    await expect(first.text).toHaveValue('<p>active owner</p>');
    assert.deepEqual(errors, []);
    return { protectedOwner: true, incompleteSnapshots: 2, artifactConsole };
  } finally {
    await context.close();
  }
}

/** React's development Strict Mode replays effects after recovering a draft. */
export async function checkStrictDraftRecovery({
  browser,
  server,
  checkAccessibility,
}) {
  const artifact = await create(server, 'Strict draft recovery');
  const require = createRequire(resolve(root, 'apps/web/package.json'));
  const { createServer } = await import(
    pathToFileURL(require.resolve('vite')).href
  );
  const context = await browser.newContext();
  const { errors, artifactConsole } = observe(context, server.content_origin);
  const previousApi = process.env.ARTIFREE_API_URL;
  const previousMode = process.env.NODE_ENV;
  process.env.ARTIFREE_API_URL = server.api_url;
  process.env.NODE_ENV = 'development';
  let dev;
  try {
    dev = await createServer({
      root: resolve(root, 'apps/web'),
      configFile: resolve(root, 'apps/web/vite.config.ts'),
      server: { host: '127.0.0.1', port: 0 },
    });
    await dev.listen();
    const address = dev.httpServer?.address();
    assert.ok(address && typeof address === 'object');
    const tab = await open(
      context,
      `http://127.0.0.1:${address.port}`,
      server,
      artifact.title,
    );
    const original = '<p>recovered under Strict Mode</p>';
    await tab.text.fill(original);
    const owner = await tab.page.evaluate(
      (key) => sessionStorage.getItem(key),
      ownerKey,
    );
    assert.ok(owner);
    await expect
      .poll(() => kept(tab.page, artifact.id))
      .toEqual([{ owner, text: original }]);
    await tab.page.reload();
    await tab.area.getByRole('tab', { name: 'Source' }).click();
    await expect(
      tab.area.getByRole('button', { name: 'Recover draft' }),
    ).toBeVisible();
    await checkAccessibility('drafts-strict-choice', tab.page);
    await tab.area.getByRole('button', { name: 'Recover draft' }).click();
    await expect(
      tab.text,
      'A recovered draft must survive Strict Mode effect replay.',
    ).toHaveValue(original);
    await expect(
      tab.area.getByRole('button', { name: 'Disconnect' }),
    ).toBeDisabled();
    await tab.area.getByRole('button', { name: 'Discard changes' }).click();
    await expect(
      tab.area.getByRole('button', { name: 'Disconnect' }),
    ).toBeEnabled();
    await expect.poll(() => kept(tab.page, artifact.id)).toEqual([]);
    assert.deepEqual(errors, []);
    return { recovered: true, released: true, artifactConsole };
  } finally {
    try {
      await context.close();
    } finally {
      try {
        await dev?.close();
      } finally {
        if (previousApi === undefined) delete process.env.ARTIFREE_API_URL;
        else process.env.ARTIFREE_API_URL = previousApi;
        if (previousMode === undefined) delete process.env.NODE_ENV;
        else process.env.NODE_ENV = previousMode;
      }
    }
  }
}
