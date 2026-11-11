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
 * Browser checks for keeping a runtime view alive across a change of its view
 * (decision 0054, rules 2, 8, 11 and 12, and the R10c row of rule 17) in the
 * real preview of a real artifree-server. A page whose manifest says `"lifecycle": "adopt"`
 * keeps its document, and so a value typed into it, across a permission change,
 * across its own save followed by a second save, and across a save by another
 * writer. Beside such a preview the workspace shows the notices that rules 8 and 12
 * call for. A page that does not opt in is reloaded by a permission change, as before.
 *
 * Chromium only. Firefox is suspended in CI (docs/quality.md, "Browser engines"),
 * so this file has no Firefox path. Run this check alone with `npm run build`,
 * then `node scripts/smoke-runtime-adoption.mjs`.
 */

const ADOPT_MANIFEST = JSON.stringify({
  sdk: 'artifree.app.v1',
  capabilities: ['metadata.read', 'source.write'],
  lifecycle: 'adopt',
});
// The same manifest without `lifecycle`: a permission change restarts the page.
const RESTART_MANIFEST = JSON.stringify({
  sdk: 'artifree.app.v1',
  capabilities: ['metadata.read', 'source.write'],
});
const ALLOW_METADATA = 'Allow metadata access';
const ALLOW_EDIT = 'Allow source editing';
// The manifest's capabilities as the workspace lists them, in name order.
const DECLARED = 'metadata.read, source.write';
const PROBE_HEADING = 'Runtime view adoption probe';
// The value typed into the page before the first change, kept across the adoptions.
const KEPT = 'kept-1';
// The value typed before the second save. The server refuses a save of the bytes
// a file already holds, so the second save writes a new value and makes a revision.
const KEPT_AGAIN = 'kept-2';
// A new artifact is at revision 1, and each save or other write makes the next one.
const FIRST_SAVE = 2;
const SECOND_SAVE = 3;
const OTHER_WRITE = 4;
const KEPT_NOTICE = 'Preview kept running across a save';
const NEWER_NOTICE = 'A newer revision exists';
const LOAD_NEWER = `Load revision ${OTHER_WRITE}`;
// The longest an adoption may take to show. It is a request to the server, and the
// pause waits for calls already on their way, so this bound is generous.
const DELIVERY_MS = 20_000;

/**
 * The page that keeps a value. It registers its adoption listener at load and shows
 * the reasons it is told of in #adoptions. It makes no call at load: an ungranted
 * call is a 403 that every workspace showing this artifact would log. "Read
 * metadata" reads `window.artifree.currentMetadata()`, and "Save" writes the value of
 * #kept to notes.txt with source.write under a new retry identity each time. Each
 * answer, or the refusal's code, is written to #result as JSON.
 */
function probe(nonce) {
  return `<!doctype html><html lang="en"><head><!-- artifree-runtime -->
<meta charset="utf-8"><title>${PROBE_HEADING}</title></head><body>
<h1>${PROBE_HEADING}</h1>
<input id="kept" aria-label="Kept value">
<output id="adoptions">[]</output>
<output id="result">waiting</output>
<div><button id="metadata" type="button">Read metadata</button><button id="save" type="button">Save</button></div>
<script>
const nonce = ${JSON.stringify(nonce)};
const reasons = [];
let attempts = 0;
const shown = (id, value) => { document.querySelector('#' + id).textContent = JSON.stringify(value); };
const failure = (error) => ({ error: error && error.code ? error.code : 'unknown' });
const answer = (work) => Promise.resolve().then(work).then((value) => shown('result', value), (error) => shown('result', failure(error)));
window.artifree.view.onAdopt(({ reason }) => {
  reasons.push(reason);
  shown('adoptions', reasons);
});
document.querySelector('#metadata').addEventListener('click', () => {
  answer(() => window.artifree.currentMetadata());
});
document.querySelector('#save').addEventListener('click', () => {
  attempts += 1;
  const retryId = nonce + '-' + attempts;
  const content = document.querySelector('#kept').value;
  answer(() => window.artifree.source.write({ path: 'notes.txt', content, retryId }));
});
</script></body></html>`;
}

/** An artifact with the probe as its page, the given manifest and a text file to save over. */
async function createArtifact(server, title, manifest) {
  const { artifact } = await api(server, 'POST', '/v1/artifacts', {
    title,
    files: [
      {
        path: 'index.html',
        encoding: 'utf8',
        content: probe(randomUUID().slice(0, 8)),
      },
      { path: 'artifree-runtime.json', encoding: 'utf8', content: manifest },
      { path: 'notes.txt', encoding: 'utf8', content: 'original\n' },
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
    // Each answer the server gave with 403: a call the page made without its grant.
    const refusals = [];
    page.on('pageerror', (error) => errors.push(error.message));
    page.on('response', (response) => {
      if (response.status() !== 403) return;
      refusals.push(
        `${response.request().method()} ${new URL(response.url()).pathname}`,
      );
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
      workspace,
      permissions,
      errors,
      refusals,
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
 * that must hear of another writer's save is brought to the front, and that is checked.
 */
async function expectVisible(opened) {
  await opened.page.bringToFront();
  await expect
    .poll(() => opened.page.evaluate(() => document.visibilityState), {
      timeout: DELIVERY_MS,
    })
    .toBe('visible');
}

/** Presses a permission control and waits until the workspace shows the grant it asked for. */
async function grant(opened, button, granted) {
  await opened.permissions.getByRole('button', { name: button }).click();
  await expect(
    opened.permissions.getByText(`Granted: ${granted}`, { exact: true }),
  ).toBeVisible();
}

/**
 * Presses one button of the probe and returns its answer. The answer is cleared
 * first, so an older one cannot pass.
 */
async function press(opened, name) {
  const result = opened.frame.locator('#result');
  await result.evaluate((element) => {
    element.textContent = 'waiting';
  });
  await opened.frame.getByRole('button', { name, exact: true }).click();
  await expect
    .poll(() => result.textContent(), { timeout: DELIVERY_MS })
    .not.toBe('waiting');
  return JSON.parse(await result.textContent());
}

/** The reasons of the adoptions the frame has been told of, in order. */
async function adoptions(opened) {
  return JSON.parse(await opened.frame.locator('#adoptions').textContent());
}

/** Waits until the frame has been told of `count` adoptions for `reason`. */
async function expectAdopted(opened, reason, count) {
  await expect
    .poll(
      async () =>
        (await adoptions(opened)).filter((item) => item === reason).length,
      { timeout: DELIVERY_MS },
    )
    .toBe(count);
}

/**
 * The checks of decision 0054 in a real browser. The page that opted in keeps its
 * document and its typed value across a permission change, across its own save and a
 * second save after editing is allowed again, and across another writer's save, after
 * which loading the newer revision starts a new document. The page that did not opt
 * in is reloaded by a permission change.
 */
export async function checkRuntimeAdoption({
  browser,
  origin,
  server,
  checkAccessibility,
}) {
  const id = randomUUID().slice(0, 8);
  const artifact = await createArtifact(
    server,
    `Runtime adoption ${id}`,
    ADOPT_MANIFEST,
  );
  const restartArtifact = await createArtifact(
    server,
    `Runtime restart ${id}`,
    RESTART_MANIFEST,
  );
  let adopt;
  let restart;
  try {
    adopt = await open(browser, origin, server, artifact);
    await expect(
      adopt.frame.getByRole('heading', { name: PROBE_HEADING }),
    ).toBeVisible();
    const initialSrc = await adopt.element.getAttribute('src');
    const keptInput = adopt.frame.getByLabel('Kept value');

    // 1. A permission change adopts the view. The value typed into the page stays,
    // and the adopted view holds the grant the user just gave.
    await keptInput.fill(KEPT);
    await grant(adopt, ALLOW_METADATA, 'metadata.read');
    await expect(adopt.element).toHaveAttribute('src', initialSrc);
    await expect(keptInput).toHaveValue(KEPT);
    await expectAdopted(adopt, 'permissions', 1);
    const metadata = await press(adopt, 'Read metadata');
    assert.equal(
      metadata.artifactId,
      artifact.id,
      `Read metadata must answer once the adopted view holds the grant: ${JSON.stringify(metadata)}`,
    );

    // 2. The page's own save adopts the view at the revision it made. A save moves
    // the head, so the adopted view holds no grant, and editing must be allowed again
    // before the next save, which adopts the view at the revision after that one.
    await grant(adopt, ALLOW_EDIT, 'metadata.read, source.write');
    await expectAdopted(adopt, 'permissions', 2);
    const saved = await press(adopt, 'Save');
    assert.equal(
      saved.revision,
      FIRST_SAVE,
      `The first save must make revision ${FIRST_SAVE}: ${JSON.stringify(saved)}`,
    );
    await expectAdopted(adopt, 'saved', 1);
    await expect(adopt.workspace.getByText(KEPT_NOTICE)).toBeVisible();
    await expect(adopt.element).toHaveAttribute('src', initialSrc);
    await expect(keptInput).toHaveValue(KEPT);
    await expect(
      adopt.permissions.getByText('Granted: none', { exact: true }),
    ).toBeVisible();
    await grant(adopt, ALLOW_EDIT, 'source.write');
    await expectAdopted(adopt, 'permissions', 3);
    await keptInput.fill(KEPT_AGAIN);
    const again = await press(adopt, 'Save');
    assert.equal(
      again.revision,
      SECOND_SAVE,
      `The second save must make revision ${SECOND_SAVE}: ${JSON.stringify(again)}`,
    );
    await expectAdopted(adopt, 'saved', 2);
    await expect(adopt.element).toHaveAttribute('src', initialSrc);
    await expect(keptInput).toHaveValue(KEPT_AGAIN);

    // 3. Another writer moves the head. The page keeps its value and its document,
    // the workspace says that a newer revision exists and offers to load it, and
    // loading it starts a new document.
    await expectVisible(adopt);
    await api(server, 'POST', `/v1/artifacts/${artifact.id}/revisions`, {
      base_revision: SECOND_SAVE,
      changes: [
        { op: 'put', path: 'notes.txt', encoding: 'utf8', content: 'other' },
      ],
    });
    await expectAdopted(adopt, 'head', 1);
    await expect(adopt.workspace.getByText(NEWER_NOTICE)).toBeVisible();
    await expect(
      adopt.workspace.getByRole('button', { name: LOAD_NEWER, exact: true }),
    ).toBeVisible();
    await expect(adopt.element).toHaveAttribute('src', initialSrc);
    await expect(keptInput).toHaveValue(KEPT_AGAIN);
    await checkAccessibility('runtime-adoption-notices', adopt.page);
    await adopt.workspace
      .getByRole('button', { name: LOAD_NEWER, exact: true })
      .click();
    await expect
      .poll(() => adopt.element.getAttribute('src'))
      .not.toBe(initialSrc);
    await expect(keptInput).toHaveValue('');

    // 4. A page that did not opt in is reloaded by a permission change, as before.
    restart = await open(browser, origin, server, restartArtifact);
    await expect(
      restart.frame.getByRole('heading', { name: PROBE_HEADING }),
    ).toBeVisible();
    const restartSrc = await restart.element.getAttribute('src');
    await restart.frame.getByLabel('Kept value').fill('gone');
    await grant(restart, ALLOW_METADATA, 'metadata.read');
    await expect
      .poll(() => restart.element.getAttribute('src'))
      .not.toBe(restartSrc);
    await expect(restart.frame.getByLabel('Kept value')).toHaveValue('');
    await expect(restart.frame.locator('#adoptions')).toHaveText('[]');

    assert.deepEqual(
      adopt.errors,
      [],
      'The adopted page must raise no page error.',
    );
    assert.deepEqual(
      restart.errors,
      [],
      'The page that did not opt in must raise no page error.',
    );
    assert.deepEqual(
      adopt.refusals,
      [],
      'The adopted page must log no 403: its probe makes no call it was not granted.',
    );
    assert.deepEqual(
      restart.refusals,
      [],
      'The page that did not opt in must log no 403.',
    );
    return {
      permissions:
        'a permission change adopted the view: the typed value stayed, and the adopted view answered Read metadata',
      saved:
        'the view adopted its own save at revision 2, and its second save, after editing was allowed again, at revision 3, with the typed value kept',
      head: 'another writer moved the head to revision 4: the frame kept its value and its document, the notice offered the newer revision, and loading it started a new document',
      restart:
        'a page that did not opt in was reloaded by a permission change, and its typed value was lost',
    };
  } finally {
    await adopt?.context.close();
    await restart?.context.close();
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
  const directory = await mkdtemp(join(tmpdir(), 'artifree-runtime-adoption-'));
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
    const result = await checkRuntimeAdoption({
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
