import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { setImmediate } from 'node:timers/promises';
import { test } from 'node:test';
import { holdAnswer } from '../lib/held-answer.mjs';

test('A held answer waits for fetching, then waits for release before delivery', async () => {
  let handler;
  const held = await holdAnswer(
    { route: (_matches, next) => (handler = next) },
    'GET',
    (pathname) => pathname === '/file',
  );
  const fetched = Promise.withResolvers();
  const response = { status: () => 200 };
  const delivered = [];
  let observed = false;
  void held.seen.then(() => {
    observed = true;
  });
  const handling = handler({
    request: () => ({ method: () => 'GET' }),
    fetch: () => fetched.promise,
    fulfill: async (answer) => delivered.push(answer),
  });
  try {
    await setImmediate();
    assert.equal(observed, false, 'Interception alone is not a server answer.');
    assert.deepEqual(delivered, []);
    fetched.resolve(response);
    assert.equal(await held.seen, response);
    assert.deepEqual(delivered, [], 'The answer is still held from the page.');
  } finally {
    fetched.resolve(response);
    held.release();
    await handling;
  }
  assert.deepEqual(delivered, [{ response }]);
});

test('An immediate fetch failure remains handled until the click yields to its waiter', () => {
  // A child isolates process-level rejection events from the Node test runner.
  const result = spawnSync(
    process.execPath,
    [
      '--input-type=module',
      '--eval',
      `
        import assert from 'node:assert/strict';
        import { setImmediate } from 'node:timers/promises';
        const { holdAnswer } = await import(process.argv[1]);
        const unhandled = [];
        process.on('unhandledRejection', (error) => unhandled.push(error));
        let handler;
        const held = await holdAnswer(
          { route: (_matches, next) => { handler = next; } },
          'GET',
          () => true,
        );
        const failure = new Error('held fetch failed');
        let aborted = 0;
        await handler({
          request: () => ({ method: () => 'GET' }),
          fetch: () => { throw failure; },
          abort: async () => { aborted += 1; },
        });
        await setImmediate();
        assert.deepEqual(unhandled, [], 'No rejection escapes before consumption.');
        await assert.rejects(held.seen, (error) => error === failure);
        assert.equal(aborted, 1);
        console.log('unhandled=0; original error delivered; abort=1');
      `,
      new URL('../lib/held-answer.mjs', import.meta.url).href,
    ],
    { encoding: 'utf8', timeout: 5_000 },
  );
  assert.equal(result.error, undefined);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(
    result.stdout.trim(),
    'unhandled=0; original error delivered; abort=1',
  );
});
