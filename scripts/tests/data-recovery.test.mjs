import assert from 'node:assert/strict';
import { readFile, rm, writeFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { before, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { checkDataRecovery } from '../lib/data-recovery.mjs';
import {
  buildServer,
  ServerClosureUnconfirmedError,
  startServer,
} from '../lib/server-process.mjs';

let binary;
before(() => {
  binary = buildServer(fileURLToPath(new URL('../../', import.meta.url)));
});

test('real server restart and backup restore preserve data bytes, receipts and restore fencing', async () => {
  await checkDataRecovery(binary, binary);
});

test('a closed previous handle cannot authorize deletion when the replacement child is unconfirmed', async (t) => {
  let previous;
  let scratch;
  let calls = 0;
  let previousExit;
  const replacement = new ServerClosureUnconfirmedError(87654, 'SIGKILL');
  const startup = new AggregateError(
    [new Error('replacement readiness failed'), replacement],
    'replacement cleanup failed',
  );
  // The previous process is real; the replacement boundary never spawns a
  // native child. Teardown therefore may safely remove its retained sentinel.
  t.after(async () => {
    await previous?.stop();
    if (scratch !== undefined)
      await rm(scratch, { recursive: true, force: true });
  });
  await assert.rejects(
    checkDataRecovery(binary, binary, {
      async startProcess(executable, data) {
        calls += 1;
        if (calls === 1) {
          previous = await startServer(executable, data);
          return previous;
        }
        previousExit = await previous.stop();
        scratch = dirname(data);
        await writeFile(
          join(scratch, 'replacement-database'),
          'replacement may still use this',
        );
        throw startup;
      },
    }),
    (error) => {
      assert.ok(error instanceof AggregateError);
      assert.equal(error.errors[0], startup);
      assert.equal(
        error.errors[1].message,
        `Retained data-recovery directory ${scratch}: child closure is unconfirmed (PID 87654). Confirm those owned processes have exited before removing this directory.`,
      );
      return true;
    },
  );
  assert.equal(calls, 2);
  assert.equal(
    await previous.stop(),
    previousExit,
    'the old handle stays closed',
  );
  assert.equal(
    await readFile(join(scratch, 'replacement-database'), 'utf8'),
    'replacement may still use this',
  );
});
