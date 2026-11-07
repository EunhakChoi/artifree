import assert from 'node:assert/strict';
import AxeBuilder from '@axe-core/playwright';
import { expect } from '@playwright/test';
import { waitForPopupEntrance } from './lib/popup-animation.mjs';

/** Real browser promises/animations exercise the accessibility measurement boundary. */
export async function checkPopupAnimationBoundary(browser) {
  const context = await browser.newContext();
  try {
    const page = await context.newPage();
    const popup = page.getByRole('dialog', { name: 'Download example' });
    async function fixture(kind = 'finite') {
      await page.setContent(`<!doctype html><html lang="en"><head>
        <title>Popup boundary fixture</title><style>
        body { background: white; color: #222; }
        [role=dialog] { background: white; padding: 16px; }
        dt { color: #333; font-size: 14px; }
        </style></head><body><main>
        <section role="dialog" aria-labelledby="title" data-open>
        <h1 id="title">Download example</h1><dl><dt>Saved as</dt><dd>example.txt</dd></dl>
        </section></main></body></html>`);
      await popup.evaluate((element, mode) => {
        if (mode === 'none') return;
        const animation = element.animate([{ opacity: 0 }, { opacity: 1 }], {
          duration: 1_000,
          iterations: mode === 'infinite' ? Infinity : 1,
          fill: 'both',
        });
        animation.pause();
        animation.currentTime = 500;
        // Observe access to the actual native promise without replacing it.
        // This makes finish/cancel ordering deterministic across both engines.
        const finished = Object.getOwnPropertyDescriptor(
          Animation.prototype,
          'finished',
        ).get;
        Object.defineProperty(animation, 'finished', {
          get() {
            document.documentElement.dataset.entryObserved = 'yes';
            return finished.call(animation);
          },
        });
      }, kind);
    }
    const observed = () =>
      expect(page.locator('html')).toHaveAttribute(
        'data-entry-observed',
        'yes',
      );
    const act = (action) =>
      popup.evaluate((element, name) => {
        if (name === 'close') {
          element.removeAttribute('data-open');
          element.setAttribute('data-closed', '');
        } else if (name === 'remove') element.remove();
        else element.getAnimations()[0][name]();
      }, action);

    await fixture();
    let settled = false;
    const waiting = waitForPopupEntrance(popup).then(() => {
      settled = true;
    });
    await observed();
    assert.equal(
      settled,
      false,
      'A paused entry cannot be measured as settled.',
    );
    await act('finish');
    await waiting;
    assert.equal(settled, true);

    await fixture('infinite');
    await assert.rejects(waitForPopupEntrance(popup), /finite end time/);

    await fixture();
    const cancelled = assert.rejects(
      waitForPopupEntrance(popup),
      /cancelled or failed/,
    );
    await observed();
    await act('cancel');
    await cancelled;

    await fixture();
    const closed = assert.rejects(waitForPopupEntrance(popup), /Popup closed/);
    await observed();
    await act('close');
    await closed;

    await fixture();
    const removed = assert.rejects(waitForPopupEntrance(popup), /Popup closed/);
    await observed();
    await act('remove');
    await removed;

    await fixture();
    await assert.rejects(
      waitForPopupEntrance(popup, { timeoutMs: 100 }),
      /exceeded 100ms/,
    );

    await fixture('none');
    await popup.locator('dd').evaluate((element) => {
      element.animate(
        [{ transform: 'rotate(0deg)' }, { transform: 'rotate(360deg)' }],
        {
          duration: 1_000,
          iterations: Infinity,
        },
      );
    });
    await waitForPopupEntrance(popup);

    await fixture('none');
    await waitForPopupEntrance(popup);
    const axe = () =>
      new AxeBuilder({ page })
        .withTags(['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa'])
        .analyze();
    assert.deepEqual((await axe()).violations, []);
    await popup.locator('dt').evaluate((element) => {
      element.style.color = '#aaa';
    });
    await waitForPopupEntrance(popup);
    assert.ok(
      (await axe()).violations.some(
        (violation) => violation.id === 'color-contrast',
      ),
      'Waiting for settled state must still reject genuinely insufficient contrast.',
    );
    await assert.rejects(
      waitForPopupEntrance(popup, { timeoutMs: Infinity }),
      /positive safe integer/,
    );
    await assert.rejects(
      waitForPopupEntrance(page.getByRole('dialog', { name: 'Absent popup' }), {
        timeoutMs: 100,
      }),
      /exceeded 100ms/,
    );
    console.log(
      'popup animation boundary: finite finish, infinite/cancel/close/detach/deadline rejection and unchanged contrast oracle passed',
    );
  } finally {
    await context.close();
  }
}
