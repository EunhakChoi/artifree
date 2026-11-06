import { chromium, firefox } from '@playwright/test';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { buildServer, startServer } from './lib/server-process.mjs';
import { checkOutboxDelivery } from './smoke-outbox-delivery.mjs';
const engines = { chromium, firefox };
const name = process.env.ARTIFREE_BROWSER ?? 'chromium';
if (!Object.hasOwn(engines, name)) throw new Error('Unsupported browser');
const root = fileURLToPath(new URL('../', import.meta.url));
const binary =
  process.env.ARTIFREE_SERVER_BIN === undefined
    ? buildServer(root)
    : resolve(process.env.ARTIFREE_SERVER_BIN);
const directory = await mkdtemp(join(tmpdir(), 'artifree-delivery-'));
let server;
let browser;
try {
  server = await startServer(binary, directory);
  browser = await engines[name].launch();
  await checkOutboxDelivery(browser, {
    origin: server.api_url,
    token: server.token,
  });
} finally {
  await browser?.close();
  await server?.stop();
  await rm(directory, { recursive: true, force: true });
}
