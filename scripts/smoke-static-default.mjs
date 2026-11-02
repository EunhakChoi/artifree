import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createServer } from 'node:http';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { extname, join, resolve, sep } from 'node:path';
import { expect } from '@playwright/test';
import { startServer } from './lib/server-process.mjs';
import { api } from './smoke-workspace.mjs';

const mime = {
  '.html': 'text/html',
  '.js': 'text/javascript',
  '.css': 'text/css',
};

/**
 * Browser checks for a static web build with a default API server (decision
 * 0028), against real servers. The page is built with
 * `VITE_ARTIFREE_SERVER` and served by a plain file server on its own origin,
 * so every API call is cross-origin, as on a static host. It checks that the
 * default is used, that a `?server=` link overrides it, that unsafe addresses
 * are refused with a message before any request is made to them, and that no
 * request goes to the server that was not chosen. Not covered: HTTPS, a real
 * static host, or a non-loopback origin.
 */
export async function checkStaticDefault({ browser, root, serverBinary, qa }) {
  const scratch = await mkdtemp(join(tmpdir(), 'artifree-static-default-'));
  let defaultApi;
  let otherApi;
  let staticServer;
  let context;
  try {
    defaultApi = await startServer(serverBinary, join(scratch, 'default'));
    otherApi = await startServer(serverBinary, join(scratch, 'other'));
    await api(defaultApi, 'POST', '/v1/artifacts', {
      title: 'Default server artifact',
      files: [{ path: 'index.html', encoding: 'utf8', content: '<p>d</p>' }],
    });
    await api(otherApi, 'POST', '/v1/artifacts', {
      title: 'Linked server artifact',
      files: [{ path: 'index.html', encoding: 'utf8', content: '<p>o</p>' }],
    });

    const dist = join(qa, 'static-default-dist');
    const build = spawnSync(
      process.execPath,
      [
        join(root, 'node_modules/vite/bin/vite.js'),
        'build',
        '--outDir',
        dist,
        '--emptyOutDir',
      ],
      {
        cwd: join(root, 'apps/web'),
        encoding: 'utf8',
        env: { ...process.env, VITE_ARTIFREE_SERVER: defaultApi.api_url },
        timeout: 170_000,
      },
    );
    assert.equal(build.status, 0, build.stderr + build.stdout);

    staticServer = createServer(async (request, response) => {
      try {
        const pathname = decodeURIComponent(
          new URL(request.url ?? '/', 'http://localhost').pathname,
        );
        const file = resolve(
          dist,
          `.${pathname === '/' ? '/index.html' : pathname}`,
        );
        if (!file.startsWith(`${dist}${sep}`)) {
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
    await new Promise((ready) => staticServer.listen(0, '127.0.0.1', ready));
    const { port } = staticServer.address();
    const origin = `http://127.0.0.1:${port}`;

    context = await browser.newContext();
    const open = async (query = '', storage = {}) => {
      const page = await context.newPage();
      if (Object.keys(storage).length > 0) {
        await page.addInitScript((entries) => {
          // Only the page itself: a sandboxed preview frame has no storage.
          if (window !== window.top) return;
          for (const [key, value] of Object.entries(entries)) {
            window.sessionStorage.setItem(key, value);
          }
        }, storage);
      }
      page.setDefaultTimeout(10_000);
      const apiHosts = new Set();
      const errors = [];
      page.on('pageerror', (error) => errors.push(error.message));
      page.on('request', (request) => {
        const url = new URL(request.url());
        if (url.pathname.startsWith('/v1/')) apiHosts.add(url.origin);
      });
      await page.goto(`${origin}/${query}`);
      return { page, apiHosts, errors, workspace: page.locator('#artifacts') };
    };
    const connect = async ({ workspace }, server) => {
      await workspace.getByLabel('Access token').fill(server.token);
      await workspace.getByRole('button', { name: 'Connect' }).click();
    };

    // The default: the field is filled in, and connecting reads that server.
    const byDefault = await open();
    await expect(byDefault.workspace.getByLabel('Server URL')).toHaveValue(
      defaultApi.api_url,
    );
    await expect(
      byDefault.workspace.getByText('the default this page was built with'),
    ).toBeVisible();
    await connect(byDefault, defaultApi);
    await expect(
      byDefault.workspace.getByText('Default server artifact').first(),
    ).toBeVisible();
    assert.deepEqual(
      [...byDefault.apiHosts],
      [new URL(defaultApi.api_url).origin],
    );
    await byDefault.page.screenshot({
      path: resolve(qa, 'static-default.png'),
    });

    // An empty field still means the default, not the static host itself.
    const cleared = await open();
    await cleared.workspace.getByLabel('Server URL').fill('');
    await connect(cleared, defaultApi);
    await expect(
      cleared.workspace.getByText('Default server artifact').first(),
    ).toBeVisible();
    assert.deepEqual(
      [...cleared.apiHosts],
      [new URL(defaultApi.api_url).origin],
    );

    // ?server= beats the default and reads only the linked server.
    const linked = await open(
      `?server=${encodeURIComponent(otherApi.api_url)}`,
    );
    await expect(linked.workspace.getByLabel('Server URL')).toHaveValue(
      otherApi.api_url,
    );
    await connect(linked, otherApi);
    await expect(
      linked.workspace.getByText('Linked server artifact').first(),
    ).toBeVisible();
    await expect(
      linked.workspace.getByText('Default server artifact'),
    ).toHaveCount(0);
    assert.deepEqual([...linked.apiHosts], [new URL(otherApi.api_url).origin]);

    // Unsafe links are reported and the default stays; nothing is requested.
    for (const unsafe of [
      'javascript:alert(1)',
      `http://user:secret@127.0.0.1:${port}`,
      'ftp://elsewhere.example',
    ]) {
      const refused = await open(`?server=${encodeURIComponent(unsafe)}`);
      await expect(
        refused.workspace.getByText(
          'The server address in the link was ignored.',
        ),
      ).toBeVisible();
      await expect(refused.workspace.getByLabel('Server URL')).toHaveValue(
        defaultApi.api_url,
      );
      assert.deepEqual([...refused.apiHosts], [], unsafe);
    }

    // A tab that kept an unusable address and a token for the default server
    // must not skip the notice: the form is shown, nothing is requested.
    const legacy = await open('', {
      'artifree.serverUrl': 'javascript:alert(1)',
      [`artifree.accessToken.${new URL(defaultApi.api_url).origin}`]:
        defaultApi.token,
    });
    await expect(
      legacy.workspace.getByText(
        'The server address kept for this tab was ignored.',
      ),
    ).toBeVisible();
    await expect(
      legacy.workspace.getByRole('heading', { name: 'Connect to your server' }),
    ).toBeVisible();
    await expect(legacy.workspace.getByLabel('Server URL')).toHaveValue(
      defaultApi.api_url,
    );
    assert.deepEqual([...legacy.apiHosts], []);
    // Entering the token connects to the default; after a disconnect the form
    // does not repeat a notice the person has already dealt with.
    await connect(legacy, defaultApi);
    await expect(
      legacy.workspace.getByText('Default server artifact').first(),
    ).toBeVisible();
    await legacy.workspace.getByRole('button', { name: 'Disconnect' }).click();
    await expect(
      legacy.workspace.getByRole('heading', { name: 'Connect to your server' }),
    ).toBeVisible();
    await expect(
      legacy.workspace.getByText(
        'The server address kept for this tab was ignored.',
      ),
    ).toHaveCount(0);

    // A refused address typed into the form is explained and sends nothing.
    const typed = await open();
    await typed.workspace
      .getByLabel('Server URL')
      .fill('ftp://elsewhere.example');
    await connect(typed, defaultApi);
    await expect(
      typed.workspace.getByText(
        'The address must start with http:// or https://.',
      ),
    ).toBeVisible();
    await expect(
      typed.workspace.getByRole('heading', { name: 'Connect to your server' }),
    ).toBeVisible();
    assert.deepEqual([...typed.apiHosts], []);

    for (const tracked of [byDefault, cleared, linked, legacy, typed]) {
      assert.deepEqual(tracked.errors, [], 'Browser JavaScript errors.');
    }
    return { status: 'passed', pages: 8 };
  } finally {
    await context?.close();
    await new Promise((closed) =>
      staticServer === undefined ? closed() : staticServer.close(closed),
    );
    await defaultApi?.stop();
    await otherApi?.stop();
    await rm(scratch, { recursive: true, force: true });
    await rm(join(qa, 'static-default-dist'), { recursive: true, force: true });
  }
}
