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
  {
    corruptInitialization = false,
    contextOptions = {},
    clockSkew = { parent: 0, frame: 0 },
    retrySkew = false,
  } = {},
) {
  const context = await browser.newContext(contextOptions);
  try {
    if (clockSkew.parent !== 0 || clockSkew.frame !== 0)
      await context.addInitScript((skew) => {
        const original = Date.now;
        Date.now = () =>
          original() + (window === window.top ? skew.parent : skew.frame);
      }, clockSkew);
    return await openInContext(
      context,
      origin,
      server,
      artifact,
      corruptInitialization,
      retrySkew,
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
  retrySkew,
) {
  const page = await context.newPage();
  page.setDefaultTimeout(10_000);
  const metadataRequests = [];
  const contentRequests = [];
  const errors = [];
  let viewAttempts = 0;
  if (retrySkew) {
    await page.addInitScript(() => {
      if (window !== window.top) return;
      let advance = 0;
      const original = performance.now.bind(performance);
      Object.defineProperty(performance, 'now', {
        configurable: true,
        value: () => original() + advance,
      });
      globalThis.advanceViewClock = (milliseconds) => {
        advance = milliseconds;
      };
    });
    await page.route('**/v1/artifacts/**/runtime-views', async (route) => {
      assert.equal(route.request().method(), 'POST');
      viewAttempts += 1;
      if (viewAttempts !== 1) {
        await route.continue();
        return;
      }
      await route.fulfill({
        status: 429,
        headers: { 'retry-after': '1', 'content-type': 'application/json' },
        body: JSON.stringify({ error: { code: 'rate_limited' } }),
      });
      await page.evaluate(() => globalThis.advanceViewClock(2_147_483_648));
    });
  }
  await page.addInitScript(() => {
    if (window !== window.top) return;
    globalThis.runtimeConnects = [];
    globalThis.runtimeResponses = [];
    globalThis.runtimeViews = [];
    // Observe real responses before the application receives them. Holding
    // the next metadata body lets the expiry test release a successful read
    // only after advancing the workspace clock.
    globalThis.metadataHold = { phase: 'idle' };
    let heldMetadata = Promise.resolve();
    let releaseMetadata = () => undefined;
    globalThis.armMetadataHold = () => {
      globalThis.metadataHold = { phase: 'armed' };
      heldMetadata = new Promise((resolve) => {
        releaseMetadata = resolve;
      });
    };
    globalThis.releaseMetadataHold = () => releaseMetadata();
    const sendRequest = window.fetch.bind(window);
    window.fetch = async (input, init) => {
      const response = await sendRequest(input, init);
      if (typeof input === 'string' && init?.method === 'POST') {
        const path = new URL(input, location.href).pathname;
        if (response.ok && path.endsWith('/runtime-views'))
          globalThis.runtimeViews.push(await response.clone().json());
        if (
          path.endsWith('/metadata') &&
          globalThis.metadataHold.phase === 'armed'
        ) {
          const body = await response.clone().text();
          globalThis.metadataHold = {
            phase: 'held',
            status: response.status,
            body,
          };
          await heldMetadata;
          globalThis.metadataHold.phase = 'released';
        }
      }
      return response;
    };
    const readText = Response.prototype.text;
    Response.prototype.text = async function () {
      const body = await readText.call(this);
      if (
        globalThis.metadataHold.phase === 'released' &&
        body === globalThis.metadataHold.body
      )
        globalThis.metadataHold.phase = 'parsed';
      return body;
    };
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
  const permissions = workspace.getByRole('region', {
    name: 'Runtime permissions',
  });
  await expect(permissions.getByText('Declared: metadata.read')).toBeVisible();
  await expect(permissions.getByText('Granted: none')).toBeVisible();
  if (!corruptInitialization) {
    await expect(
      workspace.frameLocator('iframe[title^="Preview of"]').locator('#result'),
    ).toHaveText('The runtime metadata request was refused.');
  }
  await permissions
    .getByRole('button', { name: 'Allow metadata access' })
    .click();
  await expect(permissions.getByText('Granted: metadata.read')).toBeVisible();
  const element = workspace.locator('iframe[title^="Preview of"]');
  await expect(element).toHaveAttribute(
    'src',
    new RegExp(`^${server.content_origin}/r/`),
  );
  let view;
  await expect
    .poll(async () => {
      const src = await element.getAttribute('src');
      const views = await page.evaluate(() => globalThis.runtimeViews);
      view = views.find((candidate) => candidate.url === src);
      return view !== undefined;
    })
    .toBe(true);
  assert.ok(view, 'The shown view must be one actually issued by the server.');
  return {
    context,
    page,
    element,
    view,
    metadataRequests,
    contentRequests,
    errors,
    viewAttempts: () => viewAttempts,
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

async function checkExpiredReply(browser, origin, server, artifact) {
  const opened = await open(browser, origin, server, artifact);
  try {
    const frame = opened.page.frameLocator('iframe[title^="Preview of"]');
    await expect(frame.locator('#result')).toContainText(artifact.id);
    const before = await opened.page.evaluate(
      () => globalThis.runtimeResponses.length,
    );
    await opened.page.evaluate(() => globalThis.armMetadataHold());
    await frame.locator('#read').click();
    await expect
      .poll(() => opened.page.evaluate(() => globalThis.metadataHold.phase), {
        message: 'The real metadata response must reach the workspace.',
        timeout: 10_000,
      })
      .toBe('held');
    const held = await opened.page.evaluate(() => globalThis.metadataHold);
    assert.equal(held.status, 200);
    assert.equal(JSON.parse(held.body).artifact_id, artifact.id);
    await opened.page.evaluate((advance) => {
      const original = performance.now.bind(performance);
      Object.defineProperty(performance, 'now', {
        configurable: true,
        value: () => original() + advance,
      });
    }, opened.view.lease_ms + 1);
    await opened.page.evaluate(() => globalThis.releaseMetadataHold());
    await expect
      .poll(() => opened.page.evaluate(() => globalThis.metadataHold.phase), {
        message: 'The workspace must read the held metadata response.',
        timeout: 10_000,
      })
      .toBe('parsed');
    await opened.page.evaluate(
      () => new Promise((resolve) => requestAnimationFrame(resolve)),
    );
    assert.equal(
      await opened.page.evaluate(() => globalThis.runtimeResponses.length),
      before,
      'An expired metadata reply must not reach the frame.',
    );
    assert.deepEqual(opened.errors, []);
  } finally {
    try {
      await opened.page.evaluate(() => globalThis.releaseMetadataHold());
    } finally {
      await opened.context.close();
    }
  }
}

async function checkAbandonedCreation(browser, origin, server, artifact) {
  const context = await browser.newContext();
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));
  let captured;
  let release = () => undefined;
  const held = new Promise((resolve) => {
    release = resolve;
  });
  try {
    await page.route('**/v1/artifacts/**/runtime-views', async (route) => {
      if (captured !== undefined) {
        await route.continue();
        return;
      }
      captured = route.request().postDataJSON();
      await held;
      await route.continue();
    });
    await page.goto(origin);
    const workspace = page.locator('#artifacts');
    await workspace.getByLabel('Access token').fill(server.token);
    await workspace.getByRole('button', { name: 'Connect' }).click();
    await workspace.getByLabel('Search titles').fill(artifact.title);
    await workspace
      .getByRole('navigation', { name: 'Artifacts' })
      .getByRole('button', { name: new RegExp(`^${artifact.title}`) })
      .click();
    await expect
      .poll(() => captured !== undefined, {
        message: 'The pending runtime create must reach the browser boundary.',
        timeout: 10_000,
      })
      .toBe(true);
    const closePath = `/v1/runtime-views/${captured.view_id}/${captured.generation}`;
    let closeStatus;
    page.on('response', (response) => {
      if (
        response.request().method() === 'DELETE' &&
        new URL(response.url()).pathname === closePath
      )
        closeStatus = response.status();
    });
    await workspace.getByRole('tab', { name: 'Source' }).click();
    await expect
      .poll(() => closeStatus, {
        message: 'An abandoned create must close its generation.',
        timeout: 10_000,
      })
      .toBe(204);
    const late = page.waitForResponse(
      (response) =>
        response.request().method() === 'POST' &&
        new URL(response.url()).pathname.endsWith('/runtime-views'),
    );
    release();
    assert.equal(
      (await late).status(),
      409,
      'A closed pending generation must not be issued later.',
    );
    await expect(workspace.locator('iframe[title^="Preview of"]')).toHaveCount(
      0,
    );
    await expect(
      workspace.getByRole('tab', { name: 'Source' }),
    ).toHaveAttribute('aria-selected', 'true');
    await expect(
      workspace.getByRole('textbox', { name: 'index.html' }),
    ).toBeVisible();
    await workspace.getByRole('tab', { name: 'Preview' }).click();
    await expect(
      workspace.getByRole('tab', { name: 'Preview' }),
    ).toHaveAttribute('aria-selected', 'true');
    await workspace
      .getByRole('button', { name: 'Allow metadata access' })
      .click();
    await expect(
      workspace.frameLocator('iframe[title^="Preview of"]').locator('#result'),
    ).toContainText(artifact.id);
    assert.deepEqual(errors, []);
  } finally {
    release();
    await context.close();
  }
}

async function metadataStatus(server, view) {
  const response = await fetch(
    `${server.api_url}/v1/artifacts/${view.artifact_id}/revisions/${view.revision}/runtime-views/${view.view_id}/${view.generation}/metadata`,
    {
      method: 'POST',
      headers: {
        authorization: `Bearer ${server.token}`,
        'content-type': 'application/json',
      },
      body: JSON.stringify({ initialization: view.initialization }),
    },
  );
  return response.status;
}

async function checkGrantLifecycle(
  browser,
  origin,
  server,
  artifact,
  checkAccessibility,
) {
  const first = await open(browser, origin, server, artifact);
  const second = await open(browser, origin, server, artifact);
  try {
    const firstFrame = first.page.frameLocator('iframe[title^="Preview of"]');
    const secondFrame = second.page.frameLocator('iframe[title^="Preview of"]');
    await expect(firstFrame.locator('#result')).toContainText(artifact.id);
    await expect(secondFrame.locator('#result')).toContainText(artifact.id);
    assert.equal(await metadataStatus(server, first.view), 200);
    assert.equal(await metadataStatus(server, second.view), 200);
    const permissions = first.page.getByRole('region', {
      name: 'Runtime permissions',
    });
    await permissions
      .getByRole('button', { name: 'Revoke metadata access' })
      .click();
    await expect(permissions.getByText('Granted: none')).toBeVisible();
    await expect(firstFrame.locator('#result')).toHaveText(
      'The runtime metadata request was refused.',
    );
    assert.equal(await metadataStatus(server, first.view), 404);
    assert.equal(
      await metadataStatus(server, second.view),
      200,
      'A separate connection retains its own grant.',
    );
    let delayedGrant;
    const loseGrant = async (route) => {
      delayedGrant = {
        path: new URL(route.request().url()).pathname,
        body: route.request().postDataJSON(),
      };
      await route.abort('failed');
    };
    const grantRoute = '**/v1/runtime-sessions/*/artifacts/*/revisions/*/grant';
    await first.page.route(grantRoute, loseGrant);
    await permissions
      .getByRole('button', { name: 'Allow metadata access' })
      .click();
    await expect(
      permissions.getByText('Permission status is unknown.', { exact: false }),
    ).toBeVisible();
    await expect(first.page.locator('iframe[title^="Preview of"]')).toHaveCount(
      0,
    );
    assert.ok(delayedGrant);
    await first.page.unroute(grantRoute, loseGrant);
    await first.page.getByRole('tab', { name: 'Source' }).click();
    await first.page.getByRole('tab', { name: 'Preview' }).click();
    await expect(
      permissions.getByText('Permission status is unknown.', { exact: false }),
    ).toBeVisible();
    await expect(first.page.locator('iframe[title^="Preview of"]')).toHaveCount(
      0,
    );
    await checkAccessibility('runtime-permission-unknown', first.page);
    await permissions
      .getByRole('button', { name: 'Resolve by revoking access' })
      .click();
    await expect(permissions.getByText('Granted: none')).toBeVisible();
    await expect(firstFrame.locator('#result')).toHaveText(
      'The runtime metadata request was refused.',
    );
    const late = await fetch(`${server.api_url}${delayedGrant.path}`, {
      method: 'PUT',
      headers: {
        authorization: `Bearer ${server.token}`,
        'content-type': 'application/json',
      },
      body: JSON.stringify(delayedGrant.body),
    });
    assert.equal(
      late.status,
      409,
      'The confirmed clear must fence a late permission grant.',
    );
    assert.equal((await late.json()).error.code, 'invalid_request');
    await permissions
      .getByRole('button', { name: 'Allow metadata access' })
      .click();
    await expect(permissions.getByText('Granted: metadata.read')).toBeVisible();
    await expect(firstFrame.locator('#result')).toContainText(artifact.id);
    const src = await first.element.getAttribute('src');
    const current = await first.page.evaluate(
      (url) => globalThis.runtimeViews.find((view) => view.url === url),
      src,
    );
    assert.ok(current);
    assert.equal(
      await metadataStatus(server, first.view),
      404,
      'Regrant cannot revive the old view.',
    );
    assert.equal(await metadataStatus(server, current), 200);

    // Lose only the acknowledgement after the real close has succeeded.
    const loseClose = async (route) => {
      assert.equal(route.request().method(), 'DELETE');
      const response = await route.fetch();
      assert.equal(response.status(), 204);
      await route.abort('failed');
    };
    await first.page.route('**/v1/runtime-sessions/*', loseClose);
    await first.page.getByRole('button', { name: 'Disconnect' }).click();
    await expect(
      first.page.getByText('Disconnect not confirmed'),
    ).toBeVisible();
    const retryDisconnect = first.page.getByRole('button', {
      name: 'Disconnect',
    });
    await expect(retryDisconnect).toBeEnabled();
    assert.equal(
      await retryDisconnect.evaluate(
        (button) => getComputedStyle(button).opacity,
      ),
      '1',
      'An enabled retry must not appear faded during the accessibility check.',
    );
    await checkAccessibility('runtime-disconnect-unknown', first.page);
    await expect(first.page.getByLabel('Access token')).toHaveCount(0);
    await expect(first.page.locator('iframe[title^="Preview of"]')).toHaveCount(
      0,
    );
    assert.equal(await metadataStatus(server, current), 404);
    assert.equal(await metadataStatus(server, second.view), 200);
    await first.page.unroute('**/v1/runtime-sessions/*', loseClose);
    await first.page.getByRole('button', { name: 'Disconnect' }).click();
    await expect(first.page.getByLabel('Access token')).toBeVisible();
    assert.deepEqual(first.errors, []);
    assert.deepEqual(second.errors, []);
  } finally {
    await first.context.close();
    await second.context.close();
  }
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
  await checkGrantLifecycle(
    browser,
    origin,
    server,
    artifact,
    checkAccessibility,
  );
  await checkAbandonedCreation(browser, origin, server, artifact);
  // The workspace and sandbox can each have a wall clock different from
  // the server, or from each other. A valid server lease must still work.
  for (const skew of [
    { parent: 86_400_000, frame: 0 },
    { parent: 0, frame: 86_400_000 },
    { parent: -86_400_000, frame: -86_400_000 },
  ]) {
    const shifted = await open(browser, origin, server, artifact, {
      clockSkew: skew,
    });
    try {
      await expect(
        shifted.page
          .frameLocator('iframe[title^="Preview of"]')
          .locator('#result'),
        `A valid lease must survive parent ${skew.parent} and frame ${skew.frame} clock offsets.`,
      ).toContainText(artifact.id);
      assert.deepEqual(shifted.errors, []);
    } finally {
      await shifted.context.close();
    }
  }
  await checkExpiredReply(browser, origin, server, artifact);
  const retried = await open(browser, origin, server, artifact, {
    retrySkew: true,
  });
  try {
    await expect(
      retried.page
        .frameLocator('iframe[title^="Preview of"]')
        .locator('#result'),
      'A queued retry must start its lease on actual dispatch.',
    ).toContainText(artifact.id);
    assert.equal(retried.viewAttempts(), 3);
    assert.deepEqual(retried.errors, []);
  } finally {
    await retried.context.close();
  }
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
      {
        contextOptions: {
          proxy: { server: proxy.server, bypass: '127.0.0.1,localhost' },
        },
      },
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
  const normal = await open(browser, origin, server, artifact);
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

    // A wall-clock correction after the view is live does not end its lease.
    const beforeClockChange = await normal.page.evaluate(
      () => globalThis.runtimeResponses.length,
    );
    await normal.page.evaluate(() => {
      const original = Date.now;
      globalThis.restoreWallClock = () => {
        Date.now = original;
      };
      Date.now = () => original() + 86_400_000;
    });
    await frame.locator('#read').click();
    await expect
      .poll(() =>
        normal.page.evaluate(() => globalThis.runtimeResponses.length),
      )
      .toBeGreaterThan(beforeClockChange);
    const afterClockChange = await normal.page.evaluate(() =>
      globalThis.runtimeResponses.at(-1),
    );
    assert.equal(afterClockChange.ok, true);
    assert.equal(afterClockChange.metadata.artifactId, artifact.id);
    await normal.page.evaluate(() => globalThis.restoreWallClock());

    const beforeFrameExpiry = normal.metadataRequests.length;
    await frame.locator('body').evaluate((element, advance) => {
      const clock = element.ownerDocument.defaultView.performance;
      const descriptor = Object.getOwnPropertyDescriptor(clock, 'now');
      const original = clock.now.bind(clock);
      element.ownerDocument.defaultView.restoreFrameClock = () => {
        if (descriptor === undefined) delete clock.now;
        else Object.defineProperty(clock, 'now', descriptor);
      };
      Object.defineProperty(clock, 'now', {
        configurable: true,
        value: () => original() + advance,
      });
    }, normal.view.lease_ms + 1);
    await frame.locator('#read').click();
    await expect(
      frame.locator('#result'),
      'An expired frame must reject before sending a request.',
    ).toHaveText('The runtime view has expired.');
    assert.equal(normal.metadataRequests.length, beforeFrameExpiry);
    await frame
      .locator('body')
      .evaluate((element) =>
        element.ownerDocument.defaultView.restoreFrameClock(),
      );

    // Advance only the host's monotonic clock. The frame still sends a
    // message, but an expired binding must not issue another RPC, even
    // when the host wall clock is behind the server.
    const beforeExpiry = normal.metadataRequests.length;
    await normal.page.evaluate((advance) => {
      const descriptor = Object.getOwnPropertyDescriptor(performance, 'now');
      const original = performance.now.bind(performance);
      const wall = Date.now;
      globalThis.restoreRuntimeClock = () => {
        if (descriptor === undefined) delete performance.now;
        else Object.defineProperty(performance, 'now', descriptor);
        Date.now = wall;
      };
      Date.now = () => wall() - 86_400_000;
      Object.defineProperty(performance, 'now', {
        configurable: true,
        value: () => original() + advance,
      });
    }, normal.view.lease_ms + 1);
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

  const forged = await open(browser, origin, server, artifact, {
    corruptInitialization: true,
  });
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
    consentAndReconciliation: 'passed',
    metadata: 'passed',
    navigationAndDelayedReply: 'passed',
    expiredMessage: 'passed',
    wrongInitialization: 'passed',
    wrongWindow: 'passed',
    accountTokenIsolation: 'passed',
    withoutRandomUUID: 'passed',
  };
}
