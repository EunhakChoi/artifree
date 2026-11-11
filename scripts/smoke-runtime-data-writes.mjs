import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { chromium, expect, firefox } from '@playwright/test';
import {
  accessibilityViolations,
  summarizeViolations,
} from './lib/accessibility.mjs';
import { buildServer, startServer } from './lib/server-process.mjs';
import { serveStatic } from './lib/static-web.mjs';
import { api } from './smoke-workspace.mjs';

/**
 * Browser checks for the application data writing capability `data.write`
 * (decision 0053, rule 20) in the real preview of a real artifree-server. A
 * page that declares `data.read` and `data.write` tries every write before the
 * user allows changes: each is refused, and the server answered 403 for its
 * route and path, and the owner's own read finds that nothing was stored. The
 * user then allows changes, and `data.set` stores a value that the owner reads
 * back at the version the frame reported. After a reload, which ends the
 * connection and with it the grants, the user allows reading again, and the
 * page reads the value back with `data.get`.
 *
 * The suite (scripts/smoke.mjs) and this file run alone both use the engine
 * that ARTIFREE_BROWSER names, Chromium by default. Run this check alone with
 * `npm run build`, then `node scripts/smoke-runtime-data-writes.mjs`.
 */

const MANIFEST = JSON.stringify({
  sdk: 'artifree.app.v1',
  capabilities: ['data.read', 'data.write'],
});
const ALLOW_WRITE = 'Allow app data changes';
const ALLOW_READ = 'Allow app data reading';
// The warning the workspace shows beside the change control (decision 0053, rule 3).
const WRITE_WARNING =
  "Allowing this lets the app replace and remove the application data saved for this artifact without asking you again, until you revoke it or the artifact's source changes. Changes cannot be undone. Your agents can read what the app saves, and the app can tell whether something it writes was already saved.";
// The manifest's capabilities as the workspace lists them, in name order.
const DECLARED = 'data.read, data.write';
const PROBE_HEADING = 'Application data writes probe';
// What the page stores at `notes`, written out here rather than read back from
// the code under test.
const NOTES = { greeting: 'hello', count: 2 };
// The writes the page can press, one button each: the SDK method, the route
// the frame posts to, and the request without its retry identity. Each names
// base 0, the version of the empty space; the grant is checked before the
// version, so the refusal does not depend on that base.
const WRITES = [
  {
    label: 'Set notes',
    method: 'set',
    route: 'data/set',
    request: { path: 'notes', value: NOTES, baseDataVersion: 0 },
  },
  {
    label: 'Update notes',
    method: 'update',
    route: 'data/update',
    request: { path: 'notes', patch: { count: 3 }, baseDataVersion: 0 },
  },
  {
    label: 'Remove notes',
    method: 'remove',
    route: 'data/delete',
    request: { path: 'notes', baseDataVersion: 0 },
  },
];

/**
 * The page that drives the runtime. Each write button calls its SDK method with
 * a retry identity the page names: the smoke's nonce, then a counter. The read
 * button asks for `notes`. Each answer, or the refusal's code, goes to the
 * output, so the smoke clears the output, presses a button, and waits for the
 * answer.
 */
function probe(nonce) {
  const buttons = WRITES.map(
    ({ label }, index) =>
      `<button id="write-${index}" type="button">${label}</button>`,
  ).join('');
  const writes = JSON.stringify(
    WRITES.map(({ method, request }) => ({ method, request })),
  );
  return `<!doctype html><html lang="en"><head><!-- artifree-runtime -->
<meta charset="utf-8"><title>${PROBE_HEADING}</title></head><body>
<h1>${PROBE_HEADING}</h1><output id="result">waiting</output>
<div>${buttons}<button id="read" type="button">Read notes</button></div>
<script>
const writes = ${writes};
const nonce = ${JSON.stringify(nonce)};
let attempts = 0;
const out = document.querySelector('#result');
const show = (value) => { out.textContent = JSON.stringify(value); };
const failure = (error) => show({ error: error && error.code ? error.code : 'unknown' });
writes.forEach(({ method, request }, index) => {
  document.querySelector('#write-' + index).addEventListener('click', () => {
    attempts += 1;
    const call = { ...request, retryId: nonce + '-' + attempts };
    // A synchronous throw, such as a missing runtime, is answered like a refusal.
    Promise.resolve().then(() => window.artifree.data[method](call)).then(show, failure);
  });
});
document.querySelector('#read').addEventListener('click', () => {
  Promise.resolve().then(() => window.artifree.data.get({ path: 'notes' })).then(show, failure);
});
</script></body></html>`;
}

/** An artifact whose manifest asks for data.read and data.write, with the probe as its page. */
async function createArtifact(server, title) {
  const { artifact } = await api(server, 'POST', '/v1/artifacts', {
    title,
    files: [
      {
        path: 'index.html',
        encoding: 'utf8',
        content: probe(randomUUID().slice(0, 8)),
      },
      { path: 'artifree-runtime.json', encoding: 'utf8', content: MANIFEST },
    ],
  });
  return artifact;
}

/** Searches the artifact list for `title` and opens that artifact, as a user would. */
async function openListed(page, title) {
  const workspace = page.locator('#artifacts');
  await workspace.getByLabel('Search titles').fill(title);
  await workspace
    .getByRole('navigation', { name: 'Artifacts' })
    .getByRole('button', { name: new RegExp(`^${title}`) })
    .click();
}

/** Opens the artifact's preview in its own browser context, as a user would. */
async function open(browser, origin, server, artifact) {
  const context = await browser.newContext();
  try {
    const page = await context.newPage();
    page.setDefaultTimeout(15_000);
    const errors = [];
    // Each data call the frame makes, by route, with the status the server gave.
    const calls = [];
    page.on('pageerror', (error) => errors.push(error.message));
    page.on('response', (response) => {
      const request = response.request();
      const { pathname } = new URL(response.url());
      const at = pathname.lastIndexOf('/data/');
      if (request.method() !== 'POST' || at === -1) return;
      calls.push({
        route: pathname.slice(at + 1),
        status: response.status(),
        path: request.postDataJSON()?.path,
      });
    });
    await page.goto(origin);
    const workspace = page.locator('#artifacts');
    await workspace.getByLabel('Access token').fill(server.token);
    await workspace.getByRole('button', { name: 'Connect' }).click();
    await openListed(page, artifact.title);
    const permissions = workspace.getByRole('region', {
      name: 'Runtime permissions',
    });
    await expect(
      permissions.getByText(`Declared: ${DECLARED}`, { exact: true }),
    ).toBeVisible();
    await expect(
      permissions.getByText('Granted: none', { exact: true }),
    ).toBeVisible();
    return {
      context,
      page,
      permissions,
      errors,
      calls,
      frame: workspace.frameLocator('iframe[title^="Preview of"]'),
      element: workspace.locator('iframe[title^="Preview of"]'),
    };
  } catch (error) {
    await context.close();
    throw error;
  }
}

/** Presses a permission control and waits until the workspace and the preview show the new grant. */
async function change(opened, button, granted) {
  const before = await opened.element.getAttribute('src');
  await opened.permissions.getByRole('button', { name: button }).click();
  await expect(
    opened.permissions.getByText(`Granted: ${granted}`, { exact: true }),
  ).toBeVisible();
  await expect.poll(() => opened.element.getAttribute('src')).not.toBe(before);
  await expect(
    opened.frame.getByRole('heading', { name: PROBE_HEADING }),
  ).toBeVisible();
}

/**
 * Presses one button and waits for its answer and for the one call it made.
 * The answer is cleared first, so an older one cannot pass.
 */
async function act(opened, label) {
  const seen = opened.calls.length;
  const result = opened.frame.locator('#result');
  await result.evaluate((element) => {
    element.textContent = 'waiting';
  });
  await opened.frame.getByRole('button', { name: label }).click();
  await expect
    .poll(async () => result.textContent(), { timeout: 20_000 })
    .not.toBe('waiting');
  const answer = JSON.parse(await result.textContent());
  await expect.poll(() => opened.calls.length - seen).toBe(1);
  return { answer, calls: opened.calls.slice(seen) };
}

/** The route, status and path of each call, as the assertions compare them. */
function summary(calls) {
  return calls.map(({ route, status, path }) => [route, status, path]);
}

/** A write before the grant: the frame says `denied`, and the server said 403 for that route and path. */
async function refusedWrite(opened, write) {
  const { answer, calls } = await act(opened, write.label);
  assert.deepEqual(
    answer,
    { error: 'denied' },
    `${write.label} must be refused without the data.write grant.`,
  );
  assert.deepEqual(
    summary(calls),
    [[write.route, 403, 'notes']],
    `${write.label} must reach ${write.route} for notes and get 403.`,
  );
}

/** A set after the grant: the receipt names the new version, and the server said 200 for that route. */
async function storedSet(opened) {
  const { answer, calls } = await act(opened, 'Set notes');
  assert.deepEqual(
    answer,
    { path: 'notes', dataVersion: 1, changed: true },
    'Set notes must report the new version, 1, as a change.',
  );
  assert.deepEqual(
    summary(calls),
    [['data/set', 200, 'notes']],
    'Set notes must reach data/set for notes and get 200.',
  );
}

/** A read after the reload: the stored value at the reported version, under the reading grant. */
async function readBack(opened) {
  const { answer, calls } = await act(opened, 'Read notes');
  assert.deepEqual(
    answer,
    { path: 'notes', dataVersion: 1, exists: true, value: NOTES },
    'After the reload the page must read back the stored value at version 1.',
  );
  assert.deepEqual(
    summary(calls),
    [['data/read', 200, 'notes']],
    'Read notes must reach data/read for notes and get 200.',
  );
}

/** The owner's own read of `notes` over the HTTP API. */
function ownerRead(server, id) {
  return api(server, 'GET', `/v1/artifacts/${id}/data?path=notes`);
}

/**
 * The checks of decision 0053 for `data.write` in a real browser: every write
 * is refused before the user allows changes, and the allowed set stores a value
 * that the owner reads back. After a reload, under the reading grant, the page
 * reads the value back.
 */
export async function checkRuntimeDataWrites({
  browser,
  origin,
  server,
  checkAccessibility,
}) {
  const artifact = await createArtifact(
    server,
    `Application data writes ${randomUUID().slice(0, 8)}`,
  );
  const opened = await open(browser, origin, server, artifact);
  try {
    // Nothing is granted: every write is refused, and the server said 403.
    for (const write of WRITES) {
      await refusedWrite(opened, write);
    }
    // A refusal stores nothing: the owner still finds no value, at version 0.
    const refused = await ownerRead(server, artifact.id);
    assert.equal(refused.exists, false, 'A refused write must store nothing.');
    assert.equal(
      refused.data_version,
      0,
      'A refused write must keep version 0.',
    );

    // The control and its warning are visible before the user presses it.
    await expect(
      opened.permissions.getByText(WRITE_WARNING, { exact: true }),
    ).toBeVisible();
    await expect(
      opened.permissions.getByRole('button', { name: ALLOW_WRITE }),
    ).toHaveAccessibleDescription(WRITE_WARNING);
    await checkAccessibility('app-data-writing-consent', opened.page);

    // The user allows changes in the workspace; the next view may write.
    await change(opened, ALLOW_WRITE, 'data.write');
    await checkAccessibility('app-data-writing-allowed', opened.page);
    await storedSet(opened);
    const stored = await ownerRead(server, artifact.id);
    assert.equal(
      stored.exists,
      true,
      'The owner must find the value the page stored.',
    );
    assert.deepEqual(stored.value, NOTES);
    assert.equal(stored.data_version, 1, 'The owner must find version 1.');

    // A reload ends the connection, and the grants end with it (decision 0053,
    // rule 3): the reloaded page has none, so the user allows reading again.
    await opened.page.reload();
    await openListed(opened.page, artifact.title);
    await expect(
      opened.permissions.getByText('Granted: none', { exact: true }),
    ).toBeVisible();
    await change(opened, ALLOW_READ, 'data.read');
    await readBack(opened);
    assert.deepEqual(opened.errors, []);
    return {
      refused:
        'set, update and remove before the user allowed app data changes',
      stored:
        'set after the grant, reported at version 1 and read by the owner',
      readBack:
        'the stored value, read by the page after a reload under data.read',
    };
  } finally {
    await opened.context.close();
  }
}

async function runAccessibility(label, target) {
  const violations = await accessibilityViolations(target);
  assert.deepEqual(
    summarizeViolations(violations),
    [],
    `Accessibility failures: ${label}`,
  );
}

/** Runs this check alone against a fresh server and the built web app. */
async function runAlone() {
  const root = fileURLToPath(new URL('../', import.meta.url));
  const binary =
    process.env.ARTIFREE_SERVER_BIN === undefined
      ? buildServer(root)
      : resolve(process.env.ARTIFREE_SERVER_BIN);
  const directory = await mkdtemp(
    join(tmpdir(), 'artifree-runtime-data-writes-'),
  );
  let server;
  let web;
  let browser;
  try {
    server = await startServer(binary, directory, [
      '--frame-ancestors',
      'http://127.0.0.1:*',
    ]);
    web = await serveStatic(resolve(root, 'apps/web/dist'), 0, {
      apiUrl: server.api_url,
    });
    // The engine the whole suite uses (scripts/smoke.mjs), chosen the same way.
    const engines = { chromium, firefox };
    const name = process.env.ARTIFREE_BROWSER ?? 'chromium';
    assert.ok(Object.hasOwn(engines, name), `Unsupported browser: ${name}`);
    browser = await engines[name].launch();
    const result = await checkRuntimeDataWrites({
      browser,
      origin: web.origin,
      server,
      checkAccessibility: runAccessibility,
    });
    console.log(JSON.stringify({ status: 'passed', browser: name, ...result }));
  } finally {
    await browser?.close();
    await web?.close();
    await server?.stop();
    await rm(directory, { recursive: true, force: true });
  }
}

const isEntry =
  process.argv[1] !== undefined &&
  import.meta.url === pathToFileURL(resolve(process.argv[1])).href;
if (isEntry) await runAlone();
