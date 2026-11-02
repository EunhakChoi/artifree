import assert from 'node:assert/strict';
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
const { DISPLAY_PATH_CHARS, OFFER_TIMING, OfferGate, displayPath } =
  await import(
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
