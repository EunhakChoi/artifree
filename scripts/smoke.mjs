import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { createServer } from 'node:http';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { extname, resolve, sep } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { chromium } from '@playwright/test';

const root = fileURLToPath(new URL('../', import.meta.url));
const qa = resolve(root, '.artifree/qa');
await mkdir(qa, { recursive: true });
const rust = JSON.parse(
  execFileSync(
    'cargo',
    ['run', '--quiet', '--locked', '-p', 'artifree-server', '--', '--describe'],
    { cwd: root, encoding: 'utf8' },
  ),
);
const cli = JSON.parse(
  execFileSync(
    process.execPath,
    ['apps/cli/dist/main.js', 'describe', '--json'],
    { cwd: root, encoding: 'utf8' },
  ),
);
assert.deepEqual(cli, rust, 'Rust and CLI inventories must match.');
assert.equal(rust.http_listening, false);
assert.equal(rust.server_framework, null);
assert.equal(rust.async_runtime, null);
for (const args of [
  ['serve'],
  ['describe', 'extra'],
  ['--json'],
  ['--unknown'],
]) {
  const result = spawnSync(
    process.execPath,
    ['apps/cli/dist/main.js', ...args],
    { cwd: root, encoding: 'utf8' },
  );
  assert.equal(
    result.status,
    2,
    `Invalid CLI args must fail: ${args.join(' ')}`,
  );
  assert.equal(
    result.stdout,
    '',
    'Invalid CLI args must not emit success JSON.',
  );
  assert.ok(result.stderr.trim());
}

const webRoot = resolve(root, 'apps/web/dist');
const mime = {
  '.html': 'text/html',
  '.js': 'text/javascript',
  '.css': 'text/css',
  '.json': 'application/json',
  '.svg': 'image/svg+xml',
};
const server = createServer(async (request, response) => {
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
const pageErrors = [];
const externalRequests = [];
try {
  browser = await chromium.launch();
  const page = await browser.newPage({
    viewport: { width: 1440, height: 1000 },
    reducedMotion: 'reduce',
  });
  page.on('pageerror', (error) => pageErrors.push(error.message));
  page.on('request', (request) => {
    if (/^https?:/.test(request.url()) && !request.url().startsWith(origin))
      externalRequests.push(request.url());
  });
  await page.goto(origin);
  await page.getByRole('heading', { name: /A place for what/ }).waitFor();
  for (const layer of ['presentation', 'logic', 'provider', 'shared']) {
    await page.getByRole('button', { name: new RegExp(layer) }).click();
    assert.equal(
      await page.locator('article').count(),
      rust.components.filter((item) => item.layer === layer).length,
    );
  }
  assert.ok(
    (await page.locator('.notice').innerText()).includes('Development shell'),
  );
  await page.getByRole('button', { name: /presentation/ }).click();
  await page.screenshot({ path: resolve(qa, 'web-desktop.png') });

  await page.goto(`${origin}/architecture.html`);
  await page.locator('[data-module="web"]').waitFor();
  assert.equal(
    await page.locator('[data-module]').count(),
    rust.components.length,
  );
  for (const component of rust.components) {
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
    rust.components.length,
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
  await page.emulateMedia({ media: 'print' });
  await page.pdf({
    path: resolve(qa, 'architecture.pdf'),
    format: 'A4',
    printBackground: true,
  });
  await page.emulateMedia({ media: 'screen' });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(origin);
  assert.ok(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth,
    ),
    'Web shell overflow on mobile.',
  );
  await page.screenshot({ path: resolve(qa, 'web-mobile.png') });
  assert.deepEqual(pageErrors, [], 'Browser JavaScript errors.');
  assert.deepEqual(
    externalRequests,
    [],
    'Preview must not load external resources.',
  );
  const result = {
    status: 'passed',
    modules: rust.components.length,
    deploymentCases,
    flowSteps: 6,
    invalidCliCases: 4,
    widths: [375, 390, 768, 1440],
    downloadedHtml: 'passed',
    pageErrors,
    externalRequests,
  };
  await writeFile(
    resolve(qa, 'smoke-results.json'),
    `${JSON.stringify(result, null, 2)}\n`,
  );
  console.log(JSON.stringify(result, null, 2));
} finally {
  if (browser) await browser.close();
  await new Promise((resolveClosed) => server.close(resolveClosed));
}
