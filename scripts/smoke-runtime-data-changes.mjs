import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { chromium, expect } from '@playwright/test';
import {
  accessibilityViolations,
  summarizeViolations,
} from './lib/accessibility.mjs';
import { buildServer, startServer } from './lib/server-process.mjs';
import { serveStatic } from './lib/static-web.mjs';
import { api } from './smoke-workspace.mjs';

/**
 * Browser checks for the change hint of the application data reading capability
 * (decision 0053, rules 16 and 17, and the R14c row of rule 20) in the real
 * preview of a real artifree-server. A frame that holds `data.read` is told,
 * through `window.artifree.data.onChange`, of each new version of the artifact's
 * data, and reads the value with `data.get`. A frame without the grant is never
 * told, not even when the owner changes the data while that frame is open.
 *
 * Tab B is the frame that is told. Tab A holds both grants and stores the value;
 * a reload ends its grants, so it allows reading again and reads the value back.
 *
 * Chromium only. Firefox is suspended in CI (docs/quality.md, "Browser
 * engines"), so this file has no Firefox path. Run this check alone with
 * `npm run build`, then `node scripts/smoke-runtime-data-changes.mjs`.
 */

const MANIFEST = JSON.stringify({
  sdk: 'artifree.app.v1',
  capabilities: ['data.read', 'data.write'],
});
const ALLOW_READ = 'Allow app data reading';
const ALLOW_WRITE = 'Allow app data changes';
// The manifest's capabilities as the workspace lists them, in name order.
const DECLARED = 'data.read, data.write';
const PROBE_HEADING = 'Application data change probe';
// The owner's value, and the version its write makes (the space starts at 0).
const OWNER_NOTES = { greeting: 'from the owner', count: 1 };
const OWNER_VERSION = 1;
// The value the granted tab stores, and the version its set makes. The value
// differs from the owner's on purpose: an unchanged value is not a change, so the
// space would keep its version and no change would reach the feed.
const NOTES = { greeting: 'hello', count: 2 };
const SET_VERSION = 2;
// The longest a read, or a hint on its way from the change feed, may take to
// show. The feed is pushed over server-sent events, so this bound is generous.
const DELIVERY_MS = 20_000;
// How long a frame without data.read is given to hear of the owner's change. A
// refused frame has no event to wait for, so a negative check has to wait a fixed
// bound; the change reaches the workspace within milliseconds, so this is generous.
const QUIET_MS = 2_000;

/**
 * The page that is told of changes. It registers its listener at load, appends
 * each `dataVersion` it is told of to #changes, and reads `notes` with data.get at
 * load and again on each change, showing the answer in #told. "Set notes" reads
 * the space's version with data.get and stores NOTES with data.set at that base,
 * so the set names the version it was based on. "Read notes" reads `notes` into
 * #result. Each answer, or the refusal's code, is written to its output as JSON.
 */
function probe(nonce) {
  return `<!doctype html><html lang="en"><head><!-- artifree-runtime -->
<meta charset="utf-8"><title>${PROBE_HEADING}</title></head><body>
<h1>${PROBE_HEADING}</h1>
<output id="changes">[]</output>
<output id="told">waiting</output>
<output id="result">waiting</output>
<div><button id="set" type="button">Set notes</button><button id="read" type="button">Read notes</button></div>
<script>
const notes = ${JSON.stringify(NOTES)};
const nonce = ${JSON.stringify(nonce)};
const changes = [];
let attempts = 0;
const shown = (id, value) => { document.querySelector('#' + id).textContent = JSON.stringify(value); };
const failure = (error) => ({ error: error && error.code ? error.code : 'unknown' });
const readNotes = () => Promise.resolve().then(() => window.artifree.data.get({ path: 'notes' }));
const showTold = () => readNotes().then((value) => shown('told', value), (error) => shown('told', failure(error)));
window.artifree.data.onChange(({ dataVersion }) => {
  changes.push(dataVersion);
  shown('changes', changes);
  showTold();
});
showTold();
document.querySelector('#set').addEventListener('click', () => {
  attempts += 1;
  const retryId = nonce + '-' + attempts;
  readNotes()
    .then((current) => window.artifree.data.set({ path: 'notes', value: notes, baseDataVersion: current.dataVersion, retryId }))
    .then((receipt) => shown('result', receipt), (error) => shown('result', failure(error)));
});
document.querySelector('#read').addEventListener('click', () => {
  readNotes().then((value) => shown('result', value), (error) => shown('result', failure(error)));
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

/**
 * The workspace follows the change feed only while its page is visible, so a tab
 * that must hear of a change is brought to the front, and that is checked.
 */
async function expectVisible(opened) {
  await opened.page.bringToFront();
  await expect
    .poll(() => opened.page.evaluate(() => document.visibilityState), {
      timeout: DELIVERY_MS,
    })
    .toBe('visible');
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

/** The route, status and path of each call, as the assertions compare them. */
function summary(calls) {
  return calls.map(({ route, status, path }) => [route, status, path]);
}

/**
 * Presses one button and waits for its answer and for the one call to `route`
 * that the press made. Other data calls the frame makes meanwhile, such as the
 * read a granted tab makes when it is told of a change, are not counted. The
 * answer is cleared first, so an older one cannot pass.
 */
async function press(opened, label, route) {
  const seen = opened.calls.length;
  const result = opened.frame.locator('#result');
  await result.evaluate((element) => {
    element.textContent = 'waiting';
  });
  await opened.frame.getByRole('button', { name: label }).click();
  await expect
    .poll(async () => result.textContent(), { timeout: DELIVERY_MS })
    .not.toBe('waiting');
  const answer = JSON.parse(await result.textContent());
  const made = () =>
    opened.calls.slice(seen).filter((call) => call.route === route);
  await expect.poll(() => made().length).toBe(1);
  return { answer, calls: summary(made()) };
}

/** What the frame's output `id` holds as JSON; undefined until the page has answered. */
async function output(opened, id) {
  const text = await opened.frame.locator(`#${id}`).textContent();
  return text === 'waiting' ? undefined : JSON.parse(text);
}

/** The owner writes `notes` over the HTTP API at base 0, the version of the empty space. */
async function ownerWrite(server, id) {
  await api(server, 'PUT', `/v1/artifacts/${id}/data`, {
    path: 'notes',
    base_data_version: 0,
    value: OWNER_NOTES,
  });
}

/** The owner's own read of `notes` over the HTTP API. */
function ownerRead(server, id) {
  return api(server, 'GET', `/v1/artifacts/${id}/data?path=notes`);
}

/** Waits the fixed bound a frame without data.read is given to hear of a change. */
function quiet() {
  return new Promise((resolve) => {
    setTimeout(resolve, QUIET_MS);
  });
}

/**
 * The checks of decision 0053 for the change hint in a real browser. A frame
 * without data.read is never told of a change the owner makes. Once it holds the
 * grant, a frame is told of the version a granted set reports and reads the value
 * with data.get. After a reload, under the reading grant, the stored value is read
 * back.
 */
export async function checkRuntimeDataChanges({
  browser,
  origin,
  server,
  checkAccessibility,
}) {
  const artifact = await createArtifact(
    server,
    `Application data changes ${randomUUID().slice(0, 8)}`,
  );
  const tabB = await open(browser, origin, server, artifact);
  let tabA;
  try {
    // Tab B's probe is loaded and listening: its first read, refused, is shown.
    await expect(
      tabB.frame.getByRole('heading', { name: PROBE_HEADING }),
    ).toBeVisible();
    await expect
      .poll(() => output(tabB, 'told'), { timeout: DELIVERY_MS })
      .toEqual({ error: 'denied' });

    // Nothing is granted to tab B, so the owner's change must not reach its frame.
    await expectVisible(tabB);
    await ownerWrite(server, artifact.id);
    const owner = await ownerRead(server, artifact.id);
    assert.equal(
      owner.data_version,
      OWNER_VERSION,
      'The owner must find version 1.',
    );
    assert.deepEqual(
      owner.value,
      OWNER_NOTES,
      'The owner must find what it wrote.',
    );
    await quiet();
    assert.deepEqual(
      await output(tabB, 'changes'),
      [],
      'A frame without data.read must not be told of the owner change.',
    );

    // Once tab B may read, its frame reads the value the owner stored, at that version.
    await change(tabB, ALLOW_READ, 'data.read');
    await expect
      .poll(() => output(tabB, 'told'), { timeout: DELIVERY_MS })
      .toEqual({
        path: 'notes',
        dataVersion: OWNER_VERSION,
        exists: true,
        value: OWNER_NOTES,
      });

    // Tab A holds both grants, and its set stores NOTES at the version after the owner's.
    tabA = await open(browser, origin, server, artifact);
    await change(tabA, ALLOW_READ, 'data.read');
    await change(tabA, ALLOW_WRITE, DECLARED);
    await expect
      .poll(() => output(tabA, 'told'), { timeout: DELIVERY_MS })
      .toEqual({
        path: 'notes',
        dataVersion: OWNER_VERSION,
        exists: true,
        value: OWNER_NOTES,
      });
    await expectVisible(tabB);
    const stored = await press(tabA, 'Set notes', 'data/set');
    assert.deepEqual(
      stored.answer,
      { path: 'notes', dataVersion: SET_VERSION, changed: true },
      'Set notes must report version 2 as a change.',
    );
    assert.deepEqual(
      stored.calls,
      [['data/set', 200, 'notes']],
      'Set notes must reach data/set for notes and get 200.',
    );

    // Tab B is told of the version the set reported, and no later version; its
    // frame then reads NOTES at that version with data.get.
    await expect
      .poll(async () => (await output(tabB, 'changes')).at(-1), {
        timeout: DELIVERY_MS,
      })
      .toBe(SET_VERSION);
    assert.deepEqual(
      (await output(tabB, 'changes')).filter(
        (version) => version > SET_VERSION,
      ),
      [],
      'Tab B must not be told of a version above the one the set reported.',
    );
    await expect
      .poll(() => output(tabB, 'told'), { timeout: DELIVERY_MS })
      .toEqual({
        path: 'notes',
        dataVersion: SET_VERSION,
        exists: true,
        value: NOTES,
      });

    // A reload ends tab A's grants: it allows reading again, and reads the value back.
    await tabA.page.reload();
    await openListed(tabA.page, artifact.title);
    await expect(
      tabA.permissions.getByText('Granted: none', { exact: true }),
    ).toBeVisible();
    await change(tabA, ALLOW_READ, 'data.read');
    await expect
      .poll(() => output(tabA, 'told'), { timeout: DELIVERY_MS })
      .toEqual({
        path: 'notes',
        dataVersion: SET_VERSION,
        exists: true,
        value: NOTES,
      });
    const readBack = await press(tabA, 'Read notes', 'data/read');
    assert.deepEqual(
      readBack.answer,
      { path: 'notes', dataVersion: SET_VERSION, exists: true, value: NOTES },
      'After the reload the page must read back the stored value at version 2.',
    );
    assert.deepEqual(
      readBack.calls,
      [['data/read', 200, 'notes']],
      'Read notes must reach data/read for notes and get 200.',
    );

    await checkAccessibility('app-data-change-told', tabB.page);
    assert.deepEqual(tabA.errors, []);
    assert.deepEqual(tabB.errors, []);
    return {
      quiet:
        'the owner changed the data while tab B held no data.read, and its frame was not told',
      told: 'tab B was told of version 2 after the set and read NOTES at version 2 with data.get',
      readBack:
        'after a reload under data.read, tab A read NOTES at version 2 with data.get',
    };
  } finally {
    await tabA?.context.close();
    await tabB.context.close();
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
    join(tmpdir(), 'artifree-runtime-data-changes-'),
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
    browser = await chromium.launch();
    const result = await checkRuntimeDataChanges({
      browser,
      origin: web.origin,
      server,
      checkAccessibility: runAccessibility,
    });
    console.log(
      JSON.stringify({ status: 'passed', browser: 'chromium', ...result }),
    );
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
