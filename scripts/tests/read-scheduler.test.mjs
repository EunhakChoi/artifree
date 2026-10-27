import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { afterEach, beforeEach, mock, test } from 'node:test';
import { build } from 'esbuild';

// The scheduler is browser code without DOM or React dependencies; bundle
// it as written so that these tests run the shipped source.
const { outputFiles } = await build({
  entryPoints: [
    fileURLToPath(
      new URL(
        '../../apps/web/src/artifacts/read-scheduler.ts',
        import.meta.url,
      ),
    ),
  ],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const { ReadScheduler, STALL_MS } = await import(
  `data:text/javascript;base64,${Buffer.from(outputFiles[0].text).toString('base64')}`
);

/** A page whose visibility the test sets. */
function page(visible = true) {
  const listeners = new Set();
  return {
    visible: () => visible,
    subscribe(onChange) {
      listeners.add(onChange);
      return () => listeners.delete(onChange);
    },
    listeners,
    set(next) {
      visible = next;
      for (const listener of listeners) listener();
    },
  };
}

/** Reads that record when they start and finish only when told. */
function reads() {
  const started = [];
  const finish = new Map();
  return {
    started,
    read(name) {
      return () => {
        started.push(name);
        return new Promise((resolve) => finish.set(name, resolve));
      };
    },
    async finish(name) {
      finish.get(name)();
      await settle();
    },
  };
}

/** Lets promise callbacks run; timers stay mocked. */
async function settle() {
  for (let index = 0; index < 5; index += 1) await Promise.resolve();
}

beforeEach(() => {
  mock.timers.enable({ apis: ['setTimeout', 'Date'], now: 0 });
});

afterEach(() => {
  mock.timers.reset();
});

test('runs one read at a time, in the order they were requested', async () => {
  const scheduler = new ReadScheduler(page());
  const log = reads();
  scheduler.request(log.read('a'));
  scheduler.request(log.read('b'));
  scheduler.request(log.read('c'));
  assert.deepEqual(log.started, ['a']);
  await log.finish('a');
  assert.deepEqual(log.started, ['a', 'b']);
  await log.finish('b');
  assert.deepEqual(log.started, ['a', 'b', 'c']);
});

test('a cancelled read never starts; a running one is not affected', async () => {
  const scheduler = new ReadScheduler(page());
  const log = reads();
  const cancelA = scheduler.request(log.read('a'));
  const cancelB = scheduler.request(log.read('b'));
  scheduler.request(log.read('c'));
  cancelA();
  cancelB();
  await log.finish('a');
  assert.deepEqual(log.started, ['a', 'c']);
});

test('a hidden page starts nothing, also a read queued before it was hidden', async () => {
  const visibility = page();
  const scheduler = new ReadScheduler(visibility);
  const log = reads();
  scheduler.request(log.read('running'));
  scheduler.request(log.read('queued'));
  visibility.set(false);
  await log.finish('running');
  scheduler.request(log.read('asked while hidden'));
  mock.timers.tick(60_000);
  await settle();
  assert.deepEqual(log.started, ['running']);
  visibility.set(true);
  assert.deepEqual(log.started, ['running', 'queued']);
  await log.finish('queued');
  assert.deepEqual(log.started, ['running', 'queued', 'asked while hidden']);
});

test('a Retry-After hold delays every read, also past the stall bound', async () => {
  const scheduler = new ReadScheduler(page());
  const log = reads();
  scheduler.request(log.read('stuck'));
  scheduler.holdFor(STALL_MS / 1000 + 20);
  scheduler.request(log.read('next'));
  mock.timers.tick(STALL_MS);
  await settle();
  assert.deepEqual(
    log.started,
    ['stuck'],
    'The stall bound must not end a hold.',
  );
  mock.timers.tick(19_999);
  await settle();
  assert.deepEqual(log.started, ['stuck']);
  mock.timers.tick(1);
  await settle();
  assert.deepEqual(log.started, ['stuck', 'next']);
});

test('a later, shorter hold does not shorten an earlier one', async () => {
  const scheduler = new ReadScheduler(page());
  const log = reads();
  scheduler.holdFor(5);
  scheduler.holdFor(1);
  scheduler.request(log.read('a'));
  mock.timers.tick(4_999);
  await settle();
  assert.deepEqual(log.started, []);
  mock.timers.tick(1);
  assert.deepEqual(log.started, ['a']);
});

test('a hold that ends while the page is hidden waits for it to be shown', async () => {
  const visibility = page();
  const scheduler = new ReadScheduler(visibility);
  const log = reads();
  scheduler.holdFor(2);
  scheduler.request(log.read('a'));
  visibility.set(false);
  mock.timers.tick(5_000);
  await settle();
  assert.deepEqual(log.started, []);
  visibility.set(true);
  assert.deepEqual(log.started, ['a']);
});

test('a read that never answers holds back the next one for the stall bound only', async () => {
  const scheduler = new ReadScheduler(page());
  const log = reads();
  scheduler.request(log.read('stuck'));
  scheduler.request(log.read('next'));
  mock.timers.tick(STALL_MS - 1);
  await settle();
  assert.deepEqual(log.started, ['stuck']);
  mock.timers.tick(1);
  assert.deepEqual(log.started, ['stuck', 'next']);
  // The stalled read ending late does not start a third one early.
  scheduler.request(log.read('third'));
  await log.finish('stuck');
  assert.deepEqual(log.started, ['stuck', 'next']);
  await log.finish('next');
  assert.deepEqual(log.started, ['stuck', 'next', 'third']);
});

test('a failed read frees the slot', async () => {
  const scheduler = new ReadScheduler(page());
  const started = [];
  scheduler.request(() => {
    started.push('failing');
    return Promise.reject(new Error('boom'));
  });
  scheduler.request(async () => {
    started.push('next');
  });
  await settle();
  assert.deepEqual(started, ['failing', 'next']);
});

test('listens for visibility only while reads wait', async () => {
  const visibility = page(false);
  const scheduler = new ReadScheduler(visibility);
  const cancel = scheduler.request(async () => {});
  assert.equal(visibility.listeners.size, 1);
  cancel();
  assert.equal(visibility.listeners.size, 0);
  scheduler.request(async () => {});
  visibility.set(true);
  await settle();
  assert.equal(visibility.listeners.size, 0);
});
