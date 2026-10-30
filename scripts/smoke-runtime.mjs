import assert from 'node:assert/strict';
import { createServer, request as httpRequest } from 'node:http';
import { expect } from '@playwright/test';
import { api } from './smoke-workspace.mjs';

const entry = `<!doctype html><html lang="en"><head><!-- artifree-runtime -->
<meta charset="utf-8"><title>Runtime probe</title></head><body>
<h1>Runtime probe</h1><output id="result">waiting</output>
<button id="read">Read metadata</button><button id="navigate">Navigate</button>
<script>
async function read() {
  try { document.querySelector('#result').textContent = JSON.stringify(await window.artifree.currentMetadata()); }
  catch (error) { document.querySelector('#result').textContent = error.message; }
}
document.querySelector('#read').addEventListener('click', read);
document.querySelector('#navigate').addEventListener('click', () => { location.href = 'next.html'; });
read();
</script></body></html>`;
const successor = `<!doctype html><html lang="en"><head><title>Successor</title></head>
<body><p id="successor">no runtime</p><script>
document.querySelector('#successor').textContent = window.artifree === undefined ? 'no runtime' : 'unexpected runtime';
</script></body></html>`;

async function open(
  browser,
  origin,
  server,
  artifact,
  corruptInitialization,
  contextOptions = {},
) {
  const context = await browser.newContext(contextOptions);
  try {
    return await openInContext(
      context,
      origin,
      server,
      artifact,
      corruptInitialization,
    );
  } catch (error) {
    await context.close();
    throw error;
  }
}

async function openInContext(
  context,
  origin,
  server,
  artifact,
  corruptInitialization,
) {
  const page = await context.newPage();
  page.setDefaultTimeout(10_000);
  const views = [];
  const metadataRequests = [];
  const contentRequests = [];
  const errors = [];
  await page.addInitScript(() => {
    if (window !== window.top) return;
    globalThis.runtimeConnects = [];
    globalThis.runtimeResponses = [];
    window.addEventListener('message', (event) => {
      if (event.data?.type === 'artifree-runtime-connect')
        globalThis.runtimeConnects.push(event.data);
    });
    const send = MessagePort.prototype.postMessage;
    MessagePort.prototype.postMessage = function (data, ...rest) {
      if (typeof data?.id === 'number' && typeof data.ok === 'boolean')
        globalThis.runtimeResponses.push(data);
      return send.call(this, data, ...rest);
    };
  });
  page.on('pageerror', (error) => errors.push(error.message));
  page.on('request', (request) => {
    if (request.url().startsWith(server.content_origin))
      contentRequests.push(request);
    if (
      request.method() === 'POST' &&
      /\/runtime-views\/[^/]+\/\d+\/metadata$/.test(
        new URL(request.url()).pathname,
      )
    )
      metadataRequests.push(request.url());
  });
  page.on('response', (response) => {
    if (
      response.ok() &&
      response.request().method() === 'POST' &&
      new URL(response.url()).pathname.endsWith('/runtime-views')
    )
      response
        .json()
        .then((body) => views.push(body))
        .catch((error) => errors.push(String(error)));
  });
  if (corruptInitialization) {
    await page.route(`${server.content_origin}/r/*/`, async (route) => {
      const response = await route.fetch();
      const body = await response.text();
      const pattern = /("initialization":")[0-9a-f]{64}(")/;
      assert.match(
        body,
        pattern,
        'The test must alter the frame initialization.',
      );
      await route.fulfill({
        response,
        body: body.replace(pattern, `$1${'0'.repeat(64)}$2`),
      });
    });
  }
  await page.goto(origin);
  const workspace = page.locator('#artifacts');
  await workspace.getByLabel('Access token').fill(server.token);
  await workspace.getByRole('button', { name: 'Connect' }).click();
  await workspace.getByLabel('Search titles').fill(artifact.title);
  await workspace
    .getByRole('navigation', { name: 'Artifacts' })
    .getByRole('button', { name: new RegExp(`^${artifact.title}`) })
    .click();
  const element = workspace.locator('iframe[title^="Preview of"]');
  await expect(element).toHaveAttribute(
    'src',
    new RegExp(`^${server.content_origin}/r/`),
  );
  const src = await element.getAttribute('src');
  await expect.poll(() => views.some((view) => view.url === src)).toBe(true);
  const view = views.find((candidate) => candidate.url === src);
  assert.ok(view, 'The shown view must be one actually issued by the server.');
  return {
    context,
    page,
    element,
    view,
    metadataRequests,
    contentRequests,
    errors,
  };
}

// The test browser sees a non-loopback HTTP origin, while all traffic stays
// on loopback. Only the workspace and its preview origin are forwarded.
async function insecureProxy(origin, contentOrigin) {
  const proxy = createServer((request, response) => {
    const requested = new URL(request.url);
    const target =
      requested.origin === 'http://insecure.invalid'
        ? new URL(`${requested.pathname}${requested.search}`, origin)
        : requested.origin === contentOrigin
          ? requested
          : undefined;
    if (target === undefined) {
      response.writeHead(403).end();
      return;
    }
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
    response.on('close', () => {
      if (!response.writableFinished) upstream.destroy();
    });
    request.pipe(upstream);
  });
  await new Promise((ready) => proxy.listen(0, '127.0.0.1', ready));
  const address = proxy.address();
  assert.ok(address && typeof address === 'object');
  return {
    server: `http://127.0.0.1:${address.port}`,
    close: () => new Promise((done) => proxy.close(done)),
  };
}

/** Exercise the real server and an opaque-origin frame, including rejected messages. */
export async function checkRuntime({
  browser,
  origin,
  server,
  checkAccessibility,
}) {
  const { artifact } = await api(server, 'POST', '/v1/artifacts', {
    title: 'Runtime bridge probe',
    files: [
      { path: 'index.html', encoding: 'utf8', content: entry },
      { path: 'next.html', encoding: 'utf8', content: successor },
    ],
  });
  // A real non-loopback HTTP origin lacks randomUUID but has getRandomValues.
  // Test runtime and ordinary previews and a new mutation key there.
  const proxy = await insecureProxy(origin, server.content_origin);
  let compatible;
  try {
    compatible = await open(
      browser,
      'http://insecure.invalid',
      server,
      artifact,
      false,
      { proxy: { server: proxy.server, bypass: '127.0.0.1,localhost' } },
    );
    assert.equal(await compatible.page.evaluate(() => isSecureContext), false);
    assert.equal(
      await compatible.page.evaluate(() => typeof crypto.randomUUID),
      'undefined',
    );
    await expect(
      compatible.page
        .frameLocator('iframe[title^="Preview of"]')
        .locator('#result'),
    ).toContainText(artifact.id);
    await checkAccessibility('runtime-insecure-preview', compatible.page);
    const workspace = compatible.page.locator('#artifacts');
    await workspace.getByRole('button', { name: 'New artifact' }).click();
    const dialog = compatible.page.getByRole('dialog', {
      name: 'New HTML artifact',
    });
    await dialog.getByLabel('Title').fill('Preview without randomUUID');
    await dialog.getByLabel('index.html').fill('<p id="ordinary">ready</p>');
    await dialog.getByRole('button', { name: 'Create' }).click();
    await expect(dialog).toBeHidden();
    await expect(
      compatible.page
        .frameLocator('iframe[title^="Preview of"]')
        .locator('#ordinary'),
    ).toHaveText('ready');
    await checkAccessibility('runtime-insecure-ordinary', compatible.page);
    assert.deepEqual(compatible.errors, []);
  } finally {
    await compatible?.context.close();
    await proxy.close();
  }
  const normal = await open(browser, origin, server, artifact, false);
  let release = () => undefined;
  try {
    const frame = normal.page.frameLocator('iframe[title^="Preview of"]');
    await expect(frame.locator('#result')).toContainText(
      'Runtime bridge probe',
    );
    await expect(frame.locator('#result')).toContainText(artifact.id);
    assert.equal(normal.view.enabled, true);
    await checkAccessibility('runtime-preview', normal.page);
    for (const request of normal.contentRequests) {
      const headers = await request.allHeaders();
      assert.equal(
        headers.authorization,
        undefined,
        'The account token must not reach the frame.',
      );
      assert.ok(!request.url().includes(server.token));
    }

    // Advance only the host's clock. The frame still sends a message, but an
    // expired host binding must not issue another authenticated request.
    const beforeExpiry = normal.metadataRequests.length;
    await normal.page.evaluate((expiredAt) => {
      const original = Date.now;
      globalThis.restoreRuntimeClock = () => {
        Date.now = original;
      };
      Date.now = () => expiredAt;
    }, normal.view.expires_at_ms + 1);
    await frame.locator('#read').click();
    await normal.page.waitForTimeout(500);
    assert.equal(
      normal.metadataRequests.length,
      beforeExpiry,
      'An expired frame message must not reach the metadata RPC.',
    );
    await normal.page.evaluate(() => globalThis.restoreRuntimeClock());

    // Hold a real successful reply after the server has read metadata, then
    // navigate the frame before releasing it. No reply may reach the new view.
    let reached;
    const held = new Promise((resolve) => {
      release = resolve;
    });
    const arrived = new Promise((resolve) => {
      reached = resolve;
    });
    let finished;
    let failed;
    const delivered = new Promise((resolve, reject) => {
      finished = resolve;
      failed = reject;
    });
    await normal.page.route('**/runtime-views/*/*/metadata', async (route) => {
      try {
        const response = await route.fetch();
        assert.equal(
          response.status(),
          200,
          'The held request must have actually succeeded at the server.',
        );
        reached();
        await held;
        await route.fulfill({ response });
        finished();
      } catch (error) {
        failed(error);
      }
    });
    const previousResponses = await normal.page.evaluate(
      () => globalThis.runtimeResponses.length,
    );
    await frame.locator('#read').click();
    await Promise.race([
      arrived,
      new Promise((_, reject) =>
        setTimeout(
          () =>
            reject(
              new Error('The held metadata request did not reach the server.'),
            ),
          10_000,
        ),
      ),
    ]);
    const closed = normal.page.waitForResponse(
      (response) =>
        response.request().method() === 'DELETE' &&
        response
          .url()
          .endsWith(
            `/v1/runtime-views/${normal.view.view_id}/${normal.view.generation}`,
          ),
    );
    await frame.locator('#navigate').click();
    await expect(frame.locator('#successor')).toHaveText('no runtime');
    assert.equal((await closed).status(), 204);
    release();
    await delivered;
    await normal.page.waitForTimeout(100);
    assert.equal(
      await normal.page.evaluate(() => globalThis.runtimeResponses.length),
      previousResponses,
      'An old result must not be delivered after navigation.',
    );
    assert.deepEqual(normal.errors, []);
  } finally {
    release();
    await normal.context.close();
  }

  const forged = await open(browser, origin, server, artifact, true);
  try {
    await expect
      .poll(() => forged.page.evaluate(() => globalThis.runtimeConnects.length))
      .toBeGreaterThan(0);
    await forged.page.waitForTimeout(500);
    assert.equal(
      forged.metadataRequests.length,
      0,
      'A frame with the wrong initialization must not reach the metadata RPC.',
    );
    await forged.page.evaluate(() => {
      const fake = document.createElement('iframe');
      fake.id = 'runtime-impostor';
      fake.setAttribute('sandbox', 'allow-scripts');
      fake.srcdoc = `<script>addEventListener('message', event => {
        const channel = new MessageChannel();
        parent.postMessage({ type: 'artifree-runtime-connect', ...event.data }, '*', [channel.port2]);
        channel.port1.postMessage({ type: 'metadata', id: 1 });
      });</script>`;
      document.body.append(fake);
    });
    await forged.page
      .frameLocator('#runtime-impostor')
      .locator('body')
      .waitFor({ state: 'attached' });
    const prior = await forged.page.evaluate(
      () => globalThis.runtimeConnects.length,
    );
    await forged.page.locator('#runtime-impostor').evaluate((frame, view) => {
      frame.contentWindow.postMessage(
        {
          viewId: view.view_id,
          generation: view.generation,
          initialization: view.initialization,
        },
        '*',
      );
    }, forged.view);
    await expect
      .poll(() => forged.page.evaluate(() => globalThis.runtimeConnects.length))
      .toBeGreaterThan(prior);
    await forged.page.waitForTimeout(500);
    assert.equal(
      forged.metadataRequests.length,
      0,
      'A different opaque frame must not use even a correct initialization.',
    );
    assert.deepEqual(forged.errors, []);
  } finally {
    await forged.context.close();
  }
  return {
    metadata: 'passed',
    navigationAndDelayedReply: 'passed',
    expiredMessage: 'passed',
    wrongInitialization: 'passed',
    wrongWindow: 'passed',
    accountTokenIsolation: 'passed',
    withoutRandomUUID: 'passed',
  };
}
