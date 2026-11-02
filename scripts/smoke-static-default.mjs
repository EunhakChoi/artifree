import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { expect } from '@playwright/test';
import { freePort, startServer } from './lib/server-process.mjs';
import { buildStaticWeb, serveStatic } from './lib/static-web.mjs';
import { api } from './smoke-workspace.mjs';

const SERVER_KEY = 'artifree.serverUrl';
const SECRET = 'hunter2';

/**
 * Browser checks for a static web build that has a default server
 * (decision 0028). The page is served by a plain static file server, with no
 * proxy, so it reaches two real artifree-server processes across origins:
 * the build default and another one named by `?server=`. Everything that
 * starts is stopped in `finally`, also when a check fails. No real hosting
 * service, HTTPS, or remote machine is involved.
 */
export async function checkStaticDefault({ browser, binary }) {
  const scratch = await mkdtemp(join(tmpdir(), 'artifree-static-default-'));
  const servers = [];
  let site;
  const contexts = [];
  try {
    const sitePort = await freePort();
    const siteOrigin = `http://127.0.0.1:${sitePort}`;
    const start = async (name, title) => {
      const server = await startServer(binary, join(scratch, name), [
        '--allowed-origin',
        siteOrigin,
        '--frame-ancestors',
        siteOrigin,
      ]);
      servers.push(server);
      await api(server, 'POST', '/v1/artifacts', {
        title,
        files: [
          { path: 'index.html', encoding: 'utf8', content: `<p>${title}</p>` },
        ],
      });
      return server;
    };
    const defaultServer = await start('default', 'Default server artifact');
    const otherServer = await start('other', 'Override server artifact');

    const dist = join(scratch, 'dist');
    const built = buildStaticWeb(dist, defaultServer.api_url);
    assert.equal(built.status, 0, `The static build failed: ${built.stderr}`);
    site = await serveStatic(dist, sitePort);

    const open = async (url, { init } = {}) => {
      const context = await browser.newContext();
      contexts.push(context);
      if (init !== undefined)
        await context.addInitScript(init.script, init.arg);
      const page = await context.newPage();
      page.setDefaultTimeout(10_000);
      const problems = [];
      const apiRequests = [];
      page.on('pageerror', (error) => problems.push(error.message));
      page.on('console', (message) => {
        if (message.type() !== 'error' && message.type() !== 'warning') return;
        // Sandbox and CSP refusals inside a preview are expected evidence.
        const source = message.location().url;
        if (servers.some((server) => source.startsWith(server.content_origin)))
          return;
        problems.push(message.text());
      });
      page.on('request', (request) => {
        const { origin, pathname } = new URL(request.url());
        if (pathname.startsWith('/v1/')) apiRequests.push(origin);
      });
      await page.goto(url);
      return {
        page,
        problems,
        apiRequests,
        workspace: page.locator('#artifacts'),
        sessionServer: () =>
          page.evaluate((key) => sessionStorage.getItem(key), SERVER_KEY),
      };
    };
    const connect = async ({ workspace }, server) => {
      await workspace.getByLabel('Access token').fill(server.token);
      await workspace.getByRole('button', { name: 'Connect' }).click();
    };
    const listed = (view, title) =>
      expect(view.workspace.getByText(title).first()).toBeVisible();
    const noProblems = (view, label) =>
      assert.deepEqual(view.problems, [], `${label}: browser problems.`);

    // 1. The build default is used with no link, and is not kept for the tab.
    const plain = await open(siteOrigin);
    await expect(plain.workspace.getByLabel('Server URL')).toHaveValue(
      defaultServer.api_url,
    );
    await expect(
      plain.workspace.getByText('the address built into this page'),
    ).toBeVisible();
    await connect(plain, defaultServer);
    await listed(plain, 'Default server artifact');
    assert.equal(
      await plain.sessionServer(),
      null,
      'The build default must not be written to the tab.',
    );
    assert.deepEqual(
      [...new Set(plain.apiRequests)],
      [defaultServer.api_url],
      'Every request goes to the build default, none to the static host.',
    );
    // A reload restores the token for the default: nothing was ignored.
    await plain.page.reload();
    await listed(plain, 'Default server artifact');
    noProblems(plain, 'default server');

    // 2. A ?server= link overrides the default; the other server is used.
    const linked = await open(
      `${siteOrigin}/?server=${encodeURIComponent(otherServer.api_url)}`,
    );
    await expect(linked.workspace.getByLabel('Server URL')).toHaveValue(
      otherServer.api_url,
    );
    await connect(linked, otherServer);
    await listed(linked, 'Override server artifact');
    assert.deepEqual(
      [...new Set(linked.apiRequests)],
      [otherServer.api_url],
      'The link must replace the default, not add to it.',
    );
    assert.equal(
      await linked.sessionServer(),
      otherServer.api_url,
      'A server the person chose is kept for the tab.',
    );
    noProblems(linked, 'link override');

    // 3. A tab address that is not valid is explained and never requested,
    // although a token for the default is at hand; acting clears the notice.
    const stale = await open(siteOrigin, {
      init: {
        arg: {
          siteOrigin,
          server: `javascript:alert('${SECRET}')`,
          tokenKey: `artifree.accessToken.${defaultServer.api_url}`,
          token: defaultServer.token,
        },
        script: ({ siteOrigin: expected, server, tokenKey, token }) => {
          if (window.top !== window || location.origin !== expected) return;
          sessionStorage.setItem('artifree.serverUrl', server);
          sessionStorage.setItem(tokenKey, token);
        },
      },
    });
    const notice = stale.workspace.getByText(
      'The server address kept for this tab was ignored',
    );
    await expect(notice).toBeVisible();
    await expect(stale.workspace.getByLabel('Access token')).toBeVisible();
    assert.deepEqual(stale.apiRequests, [], 'Nothing is requested yet.');
    assert.ok(
      !(await stale.page.content()).includes(SECRET),
      'The ignored address must not be shown.',
    );
    await connect(stale, defaultServer);
    await listed(stale, 'Default server artifact');
    await expect(notice).toBeHidden();
    // Back at the form, what it said before the person acted is not repeated.
    await stale.workspace
      .getByRole('button', { name: 'Disconnect' })
      .first()
      .click();
    await expect(stale.workspace.getByLabel('Access token')).toBeVisible();
    await expect(notice).toBeHidden();
    noProblems(stale, 'ignored tab address');

    // 4. A link with credentials is explained and never requested.
    const credentialed = await open(
      `${siteOrigin}/?server=${encodeURIComponent(`https://user:${SECRET}@evil.invalid`)}`,
    );
    await expect(
      credentialed.workspace.getByText(
        'The server address in the link was ignored because it contains a user name or password',
      ),
    ).toBeVisible();
    await expect(credentialed.workspace.getByLabel('Server URL')).toHaveValue(
      defaultServer.api_url,
    );
    assert.ok(!(await credentialed.page.content()).includes(SECRET));
    assert.deepEqual(credentialed.apiRequests, []);
    // The form checks an address typed into it the same way.
    await credentialed.workspace
      .getByLabel('Server URL')
      .fill(`https://user:${SECRET}@evil.invalid`);
    await credentialed.workspace.getByLabel('Access token').fill('x');
    await credentialed.workspace
      .getByRole('button', { name: 'Connect' })
      .click();
    await expect(
      credentialed.workspace.getByText(
        'This server address cannot be used because it contains a user name or password',
      ),
    ).toBeVisible();
    // What the person typed stays in the field; what the page says must not
    // repeat it.
    const said = await credentialed.workspace
      .getByRole('alert')
      .allInnerTexts();
    assert.ok(
      said.length > 0 && !said.join(' ').includes(SECRET),
      said.join(' '),
    );
    assert.deepEqual(credentialed.apiRequests, []);
    noProblems(credentialed, 'credentialed link');

    return {
      status: 'passed',
      defaultServerUsed: true,
      linkOverrides: true,
      defaultKeptForTab: false,
      ignoredAddressesExplained: 2,
    };
  } finally {
    // Stop everything even if one step fails, then report without masking.
    const cleanup = [
      ...contexts.map((context) => () => context.close()),
      () => site?.close(),
      ...servers.map((server) => () => server.stop()),
      () => rm(scratch, { recursive: true, force: true }),
    ];
    for (const step of cleanup) {
      try {
        await step();
      } catch (error) {
        console.error('Static default cleanup step failed:', error);
        process.exitCode = 1;
      }
    }
  }
}
