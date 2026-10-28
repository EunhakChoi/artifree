import assert from 'node:assert/strict';
import { createServer, request as httpRequest } from 'node:http';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { extname, join, resolve, sep } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { chromium, firefox, expect } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';
import { buildServer, startServer } from './lib/server-process.mjs';
import { checkDuplicate } from './smoke-duplicate.mjs';
import { checkHistory } from './smoke-history.mjs';
import { checkLiveUpdates } from './smoke-live.mjs';
import { checkListAndMetadata } from './smoke-list.mjs';
import { checkTrash } from './smoke-trash.mjs';
import { checkWorkspace } from './smoke-workspace.mjs';

const root = fileURLToPath(new URL('../', import.meta.url));
const engines = { chromium, firefox };
const browserName = process.env.ARTIFREE_BROWSER ?? 'chromium';
assert.ok(
  Object.hasOwn(engines, browserName),
  `Unsupported browser: ${browserName}`,
);
const qa = resolve(root, '.artifree/qa', browserName);
await mkdir(qa, { recursive: true });
const inventory = JSON.parse(
  await readFile(
    new URL('../contracts/generated/scaffold-inventory.json', import.meta.url),
    'utf8',
  ),
);

// CI browser containers have no Rust toolchain; they receive a prebuilt binary.
const serverBinary =
  process.env.ARTIFREE_SERVER_BIN === undefined
    ? buildServer(root)
    : resolve(root, process.env.ARTIFREE_SERVER_BIN);
const dataDir = await mkdtemp(join(tmpdir(), 'artifree-smoke-'));
const api = await startServer(serverBinary, dataDir);

const webRoot = resolve(root, 'apps/web/dist');
const mime = {
  '.html': 'text/html',
  '.js': 'text/javascript',
  '.css': 'text/css',
  '.json': 'application/json',
  '.svg': 'image/svg+xml',
};
// Like the Vite dev server, forward /v1 to the API so the page stays
// same-origin with it; previews load from the API's content origin.
function forward(request, response) {
  const target = new URL(request.url ?? '/', api.api_url);
  const upstream = httpRequest(
    target,
    {
      method: request.method,
      headers: { ...request.headers, host: target.host },
    },
    (reply) => {
      response.writeHead(reply.statusCode ?? 502, reply.headers);
      reply.pipe(response);
    },
  );
  upstream.on('error', () => {
    if (!response.headersSent) response.writeHead(502);
    response.end();
  });
  // A client that leaves ends the upstream request, as a real proxy does;
  // otherwise the API keeps every abandoned change stream and its slot.
  response.on('close', () => {
    if (!response.writableFinished) upstream.destroy();
  });
  request.pipe(upstream);
}

const server = createServer(async (request, response) => {
  if ((request.url ?? '').startsWith('/v1/')) {
    forward(request, response);
    return;
  }
  try {
    const pathname = decodeURIComponent(
      new URL(request.url ?? '/', 'http://localhost').pathname,
    );
    const file =
      pathname === '/architecture.html'
        ? resolve(root, 'docs/architecture.html')
        : resolve(webRoot, `.${pathname === '/' ? '/index.html' : pathname}`);
    if (
      pathname !== '/architecture.html' &&
      !file.startsWith(`${webRoot}${sep}`)
    ) {
      response.writeHead(403).end();
      return;
    }
    const content = await readFile(file);
    response.writeHead(200, {
      'content-type': `${mime[extname(file)] ?? 'application/octet-stream'}; charset=utf-8`,
    });
    response.end(content);
  } catch {
    response.writeHead(404).end();
  }
});
await new Promise((resolveReady) =>
  server.listen(0, '127.0.0.1', resolveReady),
);
const address = server.address();
assert.ok(address && typeof address === 'object');
const origin = `http://127.0.0.1:${address.port}`;
let browser;
let context;
let page;
const pageErrors = [];
const consoleErrors = [];
const artifactConsole = [];
const expectedApiFailures = [];
const externalRequests = [];
async function checkAccessibility(label, target = page) {
  const report = await new AxeBuilder({ page: target })
    .withTags(['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa'])
    .analyze();
  await writeFile(
    resolve(qa, `accessibility-${label}.json`),
    `${JSON.stringify(report.violations, null, 2)}\n`,
  );
  const summary = report.violations.map((violation) => ({
    id: violation.id,
    targets: violation.nodes.map((node) => node.target),
  }));
  assert.equal(
    report.violations.length,
    0,
    `Accessibility failures: ${label}: ${JSON.stringify(summary)}. See the saved accessibility report.`,
  );
}
try {
  browser = await engines[browserName].launch();
  context = await browser.newContext({
    viewport: { width: 1440, height: 1000 },
    reducedMotion: 'reduce',
  });
  await context.tracing.start({
    screenshots: true,
    snapshots: true,
    sources: true,
  });
  page = await context.newPage();
  page.setDefaultTimeout(10_000);
  page.setDefaultNavigationTimeout(15_000);
  page.on('pageerror', (error) => pageErrors.push(error.message));
  page.on('console', (message) => {
    if (message.type() !== 'error' && message.type() !== 'warning') return;
    // Sandbox and CSP refusals inside the preview are expected evidence.
    const source = message.location().url;
    if (source.startsWith(api.content_origin))
      artifactConsole.push(message.text());
    // Browsers log the 401, 409, and 503 the workspace test provokes on purpose.
    else if (
      source.startsWith(`${origin}/v1/`) &&
      /status of (401|409|503) /.test(message.text())
    )
      expectedApiFailures.push(message.text().match(/status of (\d+)/)[1]);
    else consoleErrors.push(message.text());
  });
  page.on('request', (request) => {
    const url = request.url();
    if (
      /^https?:/.test(url) &&
      !url.startsWith(origin) &&
      !url.startsWith(api.content_origin)
    )
      externalRequests.push(url);
  });
  await page.goto(origin);
  await page.getByRole('heading', { name: /A place for what/ }).waitFor();
  let accessibilityCases = 0;
  for (const theme of ['light', 'dark']) {
    if (theme === 'dark')
      await page.getByRole('button', { name: 'Toggle color theme' }).click();
    for (const layer of ['presentation', 'logic', 'provider', 'shared']) {
      await page.getByRole('tab', { name: layer, exact: true }).click();
      await expect(page.getByRole('tabpanel').getByRole('article')).toHaveCount(
        inventory.components.filter((item) => item.layer === layer).length,
      );
      await checkAccessibility(`${theme}-${layer}`);
      accessibilityCases += 1;
    }
    const about = page.getByRole('button', { name: 'About this foundation' });
    await about.click();
    const dialog = page.getByRole('dialog', { name: 'What works today' });
    await expect(dialog).toBeVisible();
    // Focus moves in after the dialog opens. Wait for it instead of sampling
    // once: a single sample raced the move on a slower CI runner.
    await expect
      .poll(
        () =>
          dialog.evaluate((element) =>
            element.contains(document.activeElement),
          ),
        { message: 'Dialog must receive keyboard focus.' },
      )
      .toBe(true);
    await checkAccessibility(`${theme}-dialog`);
    accessibilityCases += 1;
    await page.keyboard.press('Escape');
    await expect(dialog).toBeHidden();
    await expect(about).toBeFocused();
  }
  await page.getByRole('button', { name: 'Toggle color theme' }).click();
  await expect(
    page.getByText('Build-time inventory', { exact: true }),
  ).toBeVisible();
  await page.getByRole('tab', { name: 'presentation', exact: true }).click();
  await page
    .getByRole('tab', { name: 'presentation', exact: true })
    .press('ArrowRight');
  await expect(
    page.getByRole('tab', { name: 'logic', exact: true }),
  ).toBeFocused();
  await page.getByRole('tab', { name: 'logic', exact: true }).press('Enter');
  await expect(
    page.getByRole('tab', { name: 'logic', exact: true }),
  ).toHaveAttribute('aria-selected', 'true');
  await page.getByRole('tab', { name: 'presentation', exact: true }).click();
  await page.screenshot({ path: resolve(qa, 'web-desktop.png') });
  const workspace = await checkWorkspace({
    page,
    origin,
    server: api,
    qa,
    checkAccessibility,
  });
  const live = await checkLiveUpdates({ browser, origin, server: api });
  const list = await checkListAndMetadata({
    browser,
    origin,
    server: api,
    checkAccessibility,
  });
  const history = await checkHistory({
    browser,
    origin,
    server: api,
    checkAccessibility,
  });
  const duplicate = await checkDuplicate({
    browser,
    origin,
    server: api,
    checkAccessibility,
  });
  const trash = await checkTrash({
    browser,
    origin,
    server: api,
    checkAccessibility,
  });
  await page.getByRole('button', { name: 'Toggle color theme' }).click();
  await checkAccessibility('dark-workspace-preview');
  await page.getByRole('button', { name: 'Toggle color theme' }).click();

  await page.goto(`${origin}/architecture.html`);
  await page.locator('[data-module="web"]').waitFor();
  assert.equal(
    await page.locator('[data-module]').count(),
    inventory.components.length,
  );
  for (const component of inventory.components) {
    await page.locator(`[data-module="${component.id}"]`).click();
    assert.equal(
      await page.locator('#inspector-content h3').innerText(),
      component.name,
    );
    assert.ok(
      (await page.locator('.source-path').getAttribute('href')).endsWith(
        component.path,
      ),
    );
    assert.equal(
      await page.locator('[data-module][aria-pressed="true"]').count(),
      1,
    );
  }
  await page.locator('[data-module="api"]').focus();
  await page.keyboard.press('Enter');
  assert.equal(
    await page.locator('#inspector-content h3').innerText(),
    'Application API',
  );
  await page.screenshot({ path: resolve(qa, 'architecture-desktop.png') });
  await page
    .locator('#layers')
    .screenshot({ path: resolve(qa, 'architecture-layers.png') });

  let deploymentCases = 0;
  for (const mode of ['local', 'server', 'hosted']) {
    await page.locator(`[data-mode="${mode}"]`).click();
    for (const persistent of [true, false]) {
      await page.locator('#persistent').setChecked(persistent, { force: true });
      for (const access of ['direct', 'tunnel', 'private']) {
        await page.locator('#access').selectOption(access);
        assert.equal(
          await page.locator('#profile-label').innerText(),
          `${mode.toUpperCase()} / ${persistent ? 'PERSISTENT' : 'TEMPORARY'}`,
        );
        assert.ok(
          (await page.locator('#profile-storage').innerText()).length > 10,
        );
        assert.ok(
          (await page.locator('#profile-access').innerText()).length > 10,
        );
        assert.equal(
          await page.locator('[data-mode][aria-pressed="true"]').count(),
          1,
        );
        if (mode === 'hosted' && !persistent)
          assert.ok(
            (await page.locator('#profile-storage').innerText()).includes(
              'TTL',
            ),
          );
        deploymentCases += 1;
      }
    }
  }
  for (let index = 0; index < 6; index += 1) {
    await page.locator(`[data-step="${index}"]`).click();
    assert.equal(
      await page.locator('[data-step][aria-pressed="true"]').count(),
      1,
    );
    assert.ok((await page.locator('#flow-code').innerText()).length > 50);
  }
  await page.locator('.source-tree summary').click();
  assert.equal(await page.locator('.tree-link').count(), 15);
  const [download] = await Promise.all([
    page.waitForEvent('download'),
    page.locator('#download-html').click(),
  ]);
  const downloadPath = resolve(qa, 'artifree-architecture-downloaded.html');
  await download.saveAs(downloadPath);
  assert.equal(download.suggestedFilename(), 'artifree-architecture.html');
  await page.goto(pathToFileURL(downloadPath).href);
  await page.locator('[data-module="web"]').waitFor();
  assert.equal(
    await page.locator('[data-module]').count(),
    inventory.components.length,
    'Downloaded HTML must not duplicate hydrated modules.',
  );
  assert.equal(
    await page.locator('#profile-label').innerText(),
    'LOCAL / PERSISTENT',
  );
  await page.locator('[data-module="contracts"]').click();
  assert.equal(
    await page.locator('#inspector-content h3').innerText(),
    'Shared contracts',
  );

  await page.goto(`${origin}/architecture.html`);
  for (const width of [375, 390, 768, 1440]) {
    await page.setViewportSize({ width, height: 1000 });
    assert.ok(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= window.innerWidth,
      ),
      `Architecture overflow at ${width}px.`,
    );
    if (width === 390)
      await page.screenshot({ path: resolve(qa, 'architecture-mobile.png') });
  }
  if (browserName === 'chromium') {
    await page.emulateMedia({ media: 'print' });
    await page.pdf({
      path: resolve(qa, 'architecture.pdf'),
      format: 'A4',
      printBackground: true,
    });
    await page.emulateMedia({ media: 'screen' });
  }
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(origin);
  assert.ok(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth,
    ),
    'Web shell overflow on mobile.',
  );
  await expect(
    page.locator('iframe[title^="Preview of"]'),
    'A reload keeps the tab session and reopens the artifact.',
  ).toBeVisible();
  await page.screenshot({ path: resolve(qa, 'web-mobile.png') });
  assert.deepEqual(pageErrors, [], 'Browser JavaScript errors.');
  assert.deepEqual(consoleErrors, [], 'Browser console errors or warnings.');
  // Firefox does not log failed fetch responses; Chromium logs each one.
  if (browserName === 'chromium')
    assert.deepEqual(
      expectedApiFailures.sort(),
      // The rejected token is refused twice: by the list and by the stream.
      ['401', '401', '409', '503', '503', '503', '503'],
      'Only the deliberate rejected token, stale save, lost responses, and failed load may fail.',
    );
  assert.deepEqual(
    externalRequests,
    [],
    'Preview must not load external resources.',
  );
  const result = {
    status: 'passed',
    browser: browserName,
    accessibilityCases,
    pdf: browserName === 'chromium' ? 'passed' : 'not supported by Firefox',
    modules: inventory.components.length,
    deploymentCases,
    flowSteps: 6,
    widths: [375, 390, 768, 1440],
    downloadedHtml: 'passed',
    workspace,
    live,
    list,
    history,
    duplicate,
    trash,
    artifactConsole,
    expectedApiFailures,
    pageErrors,
    consoleErrors,
    externalRequests,
  };
  await writeFile(
    resolve(qa, 'smoke-results.json'),
    `${JSON.stringify(result, null, 2)}\n`,
  );
  console.log(JSON.stringify(result, null, 2));
} catch (error) {
  try {
    await writeFile(
      resolve(qa, 'failure.json'),
      `${JSON.stringify({ status: 'failed', browser: browserName, message: error instanceof Error ? error.message : String(error), stack: error instanceof Error ? error.stack : undefined }, null, 2)}\n`,
    );
  } catch (reportError) {
    console.error('Could not save the browser failure report:', reportError);
  }
  if (page)
    await page
      .screenshot({ path: resolve(qa, 'failure.png'), fullPage: true })
      .catch(() => undefined);
  throw error;
} finally {
  // Run every cleanup step even if an earlier one fails, so no browser or
  // server outlives the run. Report failures without masking the run's own
  // error, and still fail the run.
  const cleanup = [
    () => context?.tracing.stop({ path: resolve(qa, 'trace.zip') }),
    () => browser?.close(),
    () => new Promise((resolveClosed) => server.close(resolveClosed)),
    () => api.stop(),
    () => rm(dataDir, { recursive: true, force: true }),
  ];
  for (const step of cleanup) {
    try {
      await step();
    } catch (error) {
      console.error('Smoke cleanup step failed:', error);
      process.exitCode = 1;
    }
  }
}
