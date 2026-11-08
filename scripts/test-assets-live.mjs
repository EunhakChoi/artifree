import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium, firefox } from '@playwright/test';
import {
  accessibilityViolations,
  summarizeViolations,
} from './lib/accessibility.mjs';
import { buildServer, startServer } from './lib/server-process.mjs';
import { serveStatic } from './lib/static-web.mjs';
import { checkAssets } from './smoke-assets.mjs';

// Runs only the assets browser check against a real server and the built
// web app (npm run build first), served same-origin as scripts/smoke.mjs does.
const engines = { chromium, firefox };
const name = process.env.ARTIFREE_BROWSER ?? 'chromium';
if (!Object.hasOwn(engines, name)) throw new Error('Unsupported browser');
const root = fileURLToPath(new URL('../', import.meta.url));
const binary =
  process.env.ARTIFREE_SERVER_BIN === undefined
    ? buildServer(root)
    : resolve(process.env.ARTIFREE_SERVER_BIN);
const directory = await mkdtemp(join(tmpdir(), 'artifree-assets-'));
let server;
let web;
let browser;

async function checkAccessibility(label, target) {
  const violations = await accessibilityViolations(target);
  assert.deepEqual(
    summarizeViolations(violations),
    [],
    `Accessibility failures: ${label}`,
  );
}

try {
  server = await startServer(binary, directory, [
    '--frame-ancestors',
    'http://127.0.0.1:*',
  ]);
  web = await serveStatic(resolve(root, 'apps/web/dist'), 0, {
    apiUrl: server.api_url,
  });
  browser = await engines[name].launch();
  const result = await checkAssets({
    browser,
    origin: web.origin,
    server,
    checkAccessibility,
  });
  console.log(JSON.stringify({ status: 'passed', browser: name, ...result }));
} finally {
  await browser?.close();
  await web?.close();
  await server?.stop();
  await rm(directory, { recursive: true, force: true });
}
