import { chromium, firefox } from '@playwright/test';
import { checkOutboxDelivery } from './smoke-outbox-delivery.mjs';
const engines = { chromium, firefox };
const name = process.env.ARTIFREE_BROWSER ?? 'chromium';
if (!Object.hasOwn(engines, name)) throw new Error('Unsupported browser');
const browser = await engines[name].launch();
try {
  await checkOutboxDelivery(browser);
} finally {
  await browser.close();
}
