import { chromium, firefox } from '@playwright/test';
import { checkOutboxStore } from './smoke-outbox-store.mjs';
const engines = { chromium, firefox };
const name = process.env.ARTIFREE_BROWSER ?? 'chromium';
if (!Object.hasOwn(engines, name)) throw new Error('Unsupported browser');
const browser = await engines[name].launch();
try {
  await checkOutboxStore(browser);
} finally {
  await browser.close();
}
