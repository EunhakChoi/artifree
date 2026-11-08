import assert from 'node:assert/strict';
import { test } from 'node:test';
import { runShards } from '../smoke-parallel.mjs';

/** Collects what the runner writes, in place of process.stdout or stderr. */
function sink() {
  return {
    text: '',
    write(chunk) {
      this.text += chunk;
    },
  };
}

// Stands in for scripts/smoke.mjs. The ui shard fails at once; the outbox
// shard finishes later, so a run that returned at the first failure would miss it.
const shardScript = `
  const shard = process.env.ARTIFREE_SMOKE_SHARD;
  console.log('started ' + shard);
  if (shard === 'ui') process.exitCode = 3;
  else setTimeout(() => console.log('finished ' + shard), 200);
`;

test('a failing shard fails the run, and the run waits for the other shard', async () => {
  const stdout = sink();
  const stderr = sink();
  const failed = await runShards({
    command: process.execPath,
    args: ['-e', shardScript],
    stdout,
    stderr,
  });
  assert.deepEqual(failed, ['ui']);
  assert.ok(stdout.text.includes('[ui] started ui\n'));
  assert.ok(
    stdout.text.includes('[outbox] finished outbox\n'),
    'The run must not report before the passing shard has finished.',
  );
  assert.ok(stderr.text.includes('[ui] exited with 3\n'));
  assert.ok(!stderr.text.includes('[outbox] exited'));
});

test('each shard gets its name and the environment, and passing shards report no failure', async () => {
  const stdout = sink();
  const stderr = sink();
  const failed = await runShards({
    command: process.execPath,
    args: [
      '-e',
      "console.log('ran', process.env.ARTIFREE_SMOKE_SHARD, process.env.SMOKE_FIXTURE)",
    ],
    env: { ...process.env, SMOKE_FIXTURE: 'kept' },
    stdout,
    stderr,
  });
  assert.deepEqual(failed, []);
  assert.ok(stdout.text.includes('[outbox] ran outbox kept\n'));
  assert.ok(stdout.text.includes('[ui] ran ui kept\n'));
  assert.ok(!stderr.text.includes('exited with'));
});

test('a shard that cannot start counts as failed', async () => {
  const stderr = sink();
  const failed = await runShards({
    shards: ['ui'],
    command: 'artifree-smoke-command-that-does-not-exist',
    args: [],
    stdout: sink(),
    stderr,
  });
  assert.deepEqual(failed, ['ui']);
  assert.match(stderr.text, /^\[ui\] could not run: spawn .*ENOENT/);
});
