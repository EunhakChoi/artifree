import assert from 'node:assert/strict';
import { getEventListeners } from 'node:events';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The timing rules of the download question are web code without DOM or
// React imports; bundle them as they ship and drive them with a clock the
// test owns.
const bundle = await build({
  entryPoints: [
    fileURLToPath(
      new URL(
        '../../apps/web/src/artifacts/download-offer.ts',
        import.meta.url,
      ),
    ),
  ],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const {
  DISPLAY_PATH_CHARS,
  OFFER_TIMING,
  OfferDesk,
  OfferGate,
  SAVED_URL_LINGER_MS,
  SavedFileUrls,
  displayPath,
} = await import(
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
);

test('A press right after the question appears does not count', () => {
  const gate = new OfferGate();
  assert.equal(gate.canConfirm(0), false, 'nothing is open yet');
  assert.equal(gate.open(1_000), 'opened');
  assert.equal(gate.canConfirm(1_000), false);
  assert.equal(gate.canConfirm(1_000 + OFFER_TIMING.activationMs - 1), false);
  assert.equal(gate.canConfirm(1_000 + OFFER_TIMING.activationMs), true);
});

test('An unanswered question expires before the frame stops waiting', async () => {
  const gate = new OfferGate();
  gate.open(0);
  assert.equal(gate.canConfirm(OFFER_TIMING.lifetimeMs - 1), true);
  assert.equal(gate.canConfirm(OFFER_TIMING.lifetimeMs), false);
  // The frame's own wait for a download is in the runtime script.
  const script = await readFile(
    fileURLToPath(
      new URL('../../packages/runtime-sdk/src/bootstrap.ts', import.meta.url),
    ),
    'utf8',
  );
  const frameWait = /download:\s*([\d_]+)/.exec(script)?.[1];
  assert.ok(frameWait, 'the runtime script states how long a download waits');
  assert.ok(
    OFFER_TIMING.lifetimeMs < Number(frameWait.replaceAll('_', '')),
    'the question must end before the frame gives up',
  );
});

test('One question is open at a time and a refusal starts a pause', () => {
  const gate = new OfferGate();
  assert.equal(gate.open(0), 'opened');
  assert.equal(gate.isOpen, true);
  assert.equal(gate.open(10), 'busy');
  gate.decline(5_000);
  assert.equal(gate.isOpen, false);
  assert.equal(gate.canConfirm(6_000), false, 'nothing is open after a no');
  assert.equal(gate.open(5_000 + OFFER_TIMING.cooldownMs - 1), 'cooling');
  assert.equal(gate.open(5_000 + OFFER_TIMING.cooldownMs), 'opened');
});

test('A download that was saved starts no pause', () => {
  const gate = new OfferGate();
  gate.open(0);
  gate.close();
  assert.equal(gate.open(1), 'opened');
});

test('A path chosen by the frame is shown cut, whole characters only', () => {
  assert.equal(displayPath('a/b.txt'), 'a/b.txt');
  const exact = 'x'.repeat(DISPLAY_PATH_CHARS);
  assert.equal(displayPath(exact), exact);
  const cut = displayPath(`${exact}y`);
  assert.equal(cut, `${exact}…`);
  const emoji = '🙂'.repeat(DISPLAY_PATH_CHARS + 5);
  assert.equal(displayPath(emoji), `${'🙂'.repeat(DISPLAY_PATH_CHARS)}…`);
});

test('Only the first press that counts is taken', () => {
  const gate = new OfferGate();
  gate.open(0);
  assert.equal(gate.confirm(OFFER_TIMING.activationMs - 1), false);
  assert.equal(gate.confirm(OFFER_TIMING.activationMs), true);
  assert.equal(gate.confirm(OFFER_TIMING.activationMs), false);
  assert.equal(gate.canConfirm(OFFER_TIMING.activationMs), false);
  gate.close();
  gate.open(1);
  assert.equal(
    gate.confirm(1 + OFFER_TIMING.activationMs),
    true,
    'a new question',
  );
});

/**
 * A clock the test owns. Timers fire `early` milliseconds before their time,
 * as a browser's sometimes do.
 */
function fakeClock({ early = 0 } = {}) {
  let time = 0;
  let next = 1;
  const timers = new Map();
  return {
    now: () => time,
    after: (ms, run) => {
      const id = next++;
      // Only a long wait is cut short; the short rest is not.
      timers.set(id, {
        at: time + ms,
        run,
        early: ms > 10 * early ? early : 0,
      });
      return id;
    },
    cancel: (id) => {
      timers.delete(id);
    },
    get pending() {
      return timers.size;
    },
    advance(ms) {
      const target = time + ms;
      for (;;) {
        const due = [...timers]
          .filter(([, timer]) => timer.at - timer.early <= target)
          .sort(([, a], [, b]) => a.at - b.at)[0];
        if (due === undefined) break;
        timers.delete(due[0]);
        time = Math.max(time, Math.min(target, due[1].at - due[1].early));
        due[1].run();
      }
      time = target;
    },
  };
}

/** A request for a download, with a promise whose outcome is recorded. */
function asking(
  desk,
  path = 'notes.txt',
  signal = new AbortController().signal,
) {
  const outcome = { settled: false };
  desk.ask({ path, signal }).then(
    (receipt) => Object.assign(outcome, { settled: true, receipt }),
    (error) => Object.assign(outcome, { settled: true, error }),
  );
  return outcome;
}

const settle = () => new Promise((resolve) => setImmediate(resolve));

test('The button is enabled exactly when a press counts, even if timers run early', async () => {
  for (const early of [0, 3, 40]) {
    // Every instant around activation and expiry: whenever the page shows
    // an enabled button, the press is taken, and never otherwise.
    for (const at of [
      0,
      OFFER_TIMING.activationMs - 1,
      OFFER_TIMING.activationMs,
      OFFER_TIMING.activationMs + 1,
      OFFER_TIMING.lifetimeMs - 1,
      OFFER_TIMING.lifetimeMs,
    ]) {
      const clock = fakeClock({ early });
      const desk = new OfferDesk(clock);
      asking(desk);
      clock.advance(at);
      const shown = desk.getView();
      let fetched = 0;
      const pressed = desk.confirm(
        () => {
          fetched += 1;
          return Promise.resolve('bytes');
        },
        () => 'receipt',
      );
      await pressed;
      const counts =
        at >= OFFER_TIMING.activationMs && at < OFFER_TIMING.lifetimeMs;
      assert.equal(fetched === 1, counts, `press at ${at} (early ${early})`);
      assert.equal(
        shown?.phase === 'asking' && shown.active,
        counts,
        `button at ${at} (early ${early})`,
      );
    }
  }
});

test('An unanswered question is withdrawn, the frame is told no, and the pause starts', async () => {
  const clock = fakeClock({ early: 3 });
  const desk = new OfferDesk(clock);
  const first = asking(desk);
  clock.advance(OFFER_TIMING.lifetimeMs - 1);
  assert.equal(first.settled, false);
  clock.advance(1);
  await settle();
  assert.equal(first.error?.code, 'declined');
  assert.equal(desk.getView(), undefined);
  assert.equal(clock.pending, 0, 'no timer outlives the question');
  // A frame that is gone cannot be saved for by a late press.
  let fetched = 0;
  await desk.confirm(
    () => (fetched += 1),
    () => 'receipt',
  );
  assert.equal(fetched, 0);
  const again = asking(desk);
  await settle();
  assert.equal(again.error?.code, 'declined', 'during the pause');
  clock.advance(OFFER_TIMING.cooldownMs);
  const later = asking(desk);
  await settle();
  assert.equal(later.settled, false, 'after the pause a question opens');
  assert.equal(desk.getView()?.phase, 'asking');
});

test('A double click starts one download', async () => {
  const clock = fakeClock();
  const desk = new OfferDesk(clock);
  const outcome = asking(desk, 'a/b.txt');
  clock.advance(OFFER_TIMING.activationMs);
  let fetched = 0;
  let delivered = 0;
  let finish;
  const fetch = ({ path }) => {
    fetched += 1;
    assert.equal(path, 'a/b.txt');
    return new Promise((resolve) => (finish = resolve));
  };
  const deliver = (bytes) => {
    delivered += 1;
    return { status: 'started', bytes };
  };
  const first = desk.confirm(fetch, deliver);
  const second = desk.confirm(fetch, deliver);
  assert.equal(desk.getView()?.phase, 'saving');
  desk.decline();
  assert.equal(
    outcome.settled,
    false,
    'a question being saved is not declined',
  );
  finish('data');
  await Promise.all([first, second]);
  assert.deepEqual([fetched, delivered], [1, 1]);
  assert.deepEqual(outcome.receipt, { status: 'started', bytes: 'data' });
  assert.equal(desk.getView(), undefined);
  assert.equal(clock.pending, 0);
  // A saved download starts no pause.
  asking(desk);
  assert.equal(desk.getView()?.phase, 'asking');
});

test('A request that has already been aborted opens nothing and costs nothing', async () => {
  const clock = fakeClock();
  const desk = new OfferDesk(clock);
  const controller = new AbortController();
  controller.abort();
  const gone = asking(desk, 'notes.txt', controller.signal);
  await settle();
  assert.equal(gone.error?.code, 'view_unavailable');
  assert.equal(desk.getView(), undefined);
  assert.equal(clock.pending, 0);
  assert.equal(getEventListeners(controller.signal, 'abort').length, 0);
  // The gate was not touched: no pause, nothing left open.
  const next = asking(desk);
  assert.equal(next.settled, false);
  assert.equal(desk.getView()?.phase, 'asking');
});

test('Every way a question ends takes its abort listener off', async () => {
  const listeners = (signal) => getEventListeners(signal, 'abort').length;
  const ends = {
    'is declined': async ({ desk }) => desk.decline(),
    expires: async ({ clock }) => clock.advance(OFFER_TIMING.lifetimeMs),
    'is saved': async ({ desk, clock }) => {
      clock.advance(OFFER_TIMING.activationMs);
      await desk.confirm(
        () => Promise.resolve(1),
        () => 'receipt',
      );
    },
    fails: async ({ desk, clock }) => {
      clock.advance(OFFER_TIMING.activationMs);
      await desk.confirm(
        () => Promise.reject(new Error('no')),
        () => 'x',
      );
    },
    'loses its frame': async ({ desk }) => desk.dispose(),
  };
  for (const [name, end] of Object.entries(ends)) {
    const clock = fakeClock();
    const desk = new OfferDesk(clock);
    const controller = new AbortController();
    const outcome = asking(desk, 'notes.txt', controller.signal);
    assert.equal(listeners(controller.signal), 1, name);
    await end({ desk, clock });
    await settle();
    assert.equal(outcome.settled, true, name);
    assert.equal(
      listeners(controller.signal),
      0,
      `listener left when it ${name}`,
    );
    assert.equal(clock.pending, 0, `timer left when it ${name}`);
  }
});

test('A frame that goes away while the question is open ends it without a pause', async () => {
  const clock = fakeClock();
  const desk = new OfferDesk(clock);
  const controller = new AbortController();
  const outcome = asking(desk, 'notes.txt', controller.signal);
  controller.abort();
  await settle();
  assert.equal(outcome.error?.code, 'view_unavailable');
  assert.equal(desk.getView(), undefined);
  assert.equal(clock.pending, 0);
  asking(desk);
  assert.equal(
    desk.getView()?.phase,
    'asking',
    'no pause for a frame that left',
  );
});

test('A frame that goes away during the download gets no file and the fetch is aborted', async () => {
  const clock = fakeClock();
  const desk = new OfferDesk(clock);
  const outcome = asking(desk);
  clock.advance(OFFER_TIMING.activationMs);
  let seen;
  let delivered = 0;
  const pressed = desk.confirm(
    ({ signal }) => {
      seen = signal;
      return new Promise((resolve) =>
        signal.addEventListener('abort', () => resolve('late')),
      );
    },
    () => {
      delivered += 1;
      return 'receipt';
    },
  );
  desk.dispose();
  await pressed;
  assert.equal(seen.aborted, true);
  assert.equal(delivered, 0);
  assert.equal(outcome.error?.code, 'view_unavailable');
});

test('A failed download is shown until dismissed and starts no pause', async () => {
  const clock = fakeClock();
  const desk = new OfferDesk(clock);
  const outcome = asking(desk);
  clock.advance(OFFER_TIMING.activationMs);
  const failure = new Error('disk full');
  await desk.confirm(
    () => Promise.reject(failure),
    () => 'receipt',
  );
  assert.equal(outcome.error, failure);
  assert.deepEqual(
    [desk.getView()?.phase, desk.getView()?.error],
    ['failed', failure],
  );
  desk.dismiss();
  assert.equal(desk.getView(), undefined);
  asking(desk);
  assert.equal(desk.getView()?.phase, 'asking');
});

test('The view changes only when something did, for the page to subscribe to', () => {
  const clock = fakeClock();
  const desk = new OfferDesk(clock);
  let calls = 0;
  const off = desk.subscribe(() => (calls += 1));
  const before = desk.getView();
  assert.equal(desk.getView(), before);
  asking(desk);
  const opened = desk.getView();
  assert.equal(desk.getView(), opened);
  assert.ok(calls >= 1);
  const seen = calls;
  clock.advance(OFFER_TIMING.activationMs);
  assert.ok(calls > seen, 'activation is announced');
  off();
  const quiet = calls;
  desk.decline();
  assert.equal(calls, quiet, 'an unsubscribed listener is not called');
});

test('A saved file stays addressable after the click, and a newer one replaces it', () => {
  const clock = fakeClock();
  const revoked = [];
  const urls = new SavedFileUrls((url) => revoked.push(url), clock);
  urls.hold('blob:one');
  // A large archive can take the browser a while to read: a minute.
  clock.advance(59_999);
  assert.deepEqual(revoked, [], 'still readable by the browser');
  clock.advance(1);
  assert.deepEqual(revoked, ['blob:one']);
  assert.equal(SAVED_URL_LINGER_MS, 60_000);
  urls.hold('blob:two');
  urls.hold('blob:three');
  assert.deepEqual(revoked, ['blob:one', 'blob:two'], 'replaced at once');
  clock.advance(60_000);
  assert.deepEqual(revoked, ['blob:one', 'blob:two', 'blob:three']);
  assert.equal(clock.pending, 0);
});
