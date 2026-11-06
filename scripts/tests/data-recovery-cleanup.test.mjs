import assert from 'node:assert/strict';
import { access, readFile, rm, writeFile } from 'node:fs/promises';
import { dirname, isAbsolute, join } from 'node:path';
import { test } from 'node:test';
import { checkDataRecovery } from '../lib/data-recovery.mjs';
import { ServerClosureUnconfirmedError } from '../lib/server-process.mjs';

test('startup with an unconfirmed child preserves its directory and reports PID through nested errors', async (t) => {
  let scratch;
  const unclosed = new ServerClosureUnconfirmedError(54321, 'SIGKILL');
  const startup = new AggregateError(
    [new Error('fixture startup failed'), unclosed],
    'startup and cleanup failed',
  );
  // The fixture has no native child: teardown can remove it after checking the
  // caller preserved the sentinel. A real unconfirmed child requires follow-up.
  t.after(async () => {
    if (scratch !== undefined)
      await rm(scratch, { recursive: true, force: true });
  });
  await assert.rejects(
    checkDataRecovery('previous', 'current', {
      async startProcess(_binary, data) {
        scratch = dirname(data);
        await writeFile(join(scratch, 'active-database'), 'startup sentinel');
        throw startup;
      },
    }),
    (error) => {
      assert.ok(error instanceof AggregateError);
      assert.equal(error.errors[0], startup);
      assert.equal(
        error.errors[1].message,
        `Retained data-recovery directory ${scratch}: child closure is unconfirmed (PID 54321). Confirm those owned processes have exited before removing this directory.`,
      );
      return true;
    },
  );
  assert.equal(isAbsolute(scratch), true);
  assert.equal(
    await readFile(join(scratch, 'active-database'), 'utf8'),
    'startup sentinel',
  );
});

test('shutdown with an unconfirmed child preserves its directory and original request failure', async (t) => {
  let scratch;
  const unclosed = new ServerClosureUnconfirmedError(65432, 'SIGTERM');
  t.after(async () => {
    if (scratch !== undefined)
      await rm(scratch, { recursive: true, force: true });
  });
  await assert.rejects(
    checkDataRecovery('previous', 'current', {
      async startProcess(_binary, data) {
        scratch = dirname(data);
        await writeFile(join(scratch, 'active-database'), 'shutdown sentinel');
        return {
          api_url: 'invalid-url',
          token: 'fixture-token',
          async stop() {
            throw unclosed;
          },
        };
      },
    }),
    (error) => {
      assert.ok(error instanceof AggregateError);
      assert.ok(
        error.errors[0] instanceof TypeError,
        'original Fetch URL failure is retained',
      );
      assert.equal(error.errors[1], unclosed);
      assert.equal(
        error.errors[2].message,
        `Retained data-recovery directory ${scratch}: child closure is unconfirmed (PID 65432). Confirm those owned processes have exited before removing this directory.`,
      );
      return true;
    },
  );
  assert.equal(isAbsolute(scratch), true);
  assert.equal(
    await readFile(join(scratch, 'active-database'), 'utf8'),
    'shutdown sentinel',
  );
});

test('a startup child that closes late deletes its sentinel while keeping the original timeout error', async (t) => {
  let scratch;
  let closed = false;
  const unclosed = new ServerClosureUnconfirmedError(76543, 'SIGKILL', {
    isClosed: () => closed,
  });
  const startup = new AggregateError(
    [new Error('fixture readiness failed'), unclosed],
    'startup and cleanup failed',
  );
  t.after(async () => {
    if (scratch !== undefined)
      await rm(scratch, { recursive: true, force: true });
  });
  await assert.rejects(
    checkDataRecovery('previous', 'current', {
      async startProcess(_binary, data) {
        scratch = dirname(data);
        await writeFile(
          join(scratch, 'late-close-database'),
          'safe after close',
        );
        assert.equal(unclosed.isClosureConfirmed(), false);
        queueMicrotask(() => {
          closed = true;
        });
        throw startup;
      },
    }),
    (error) => error === startup,
  );
  assert.equal(unclosed.isClosureConfirmed(), true);
  await assert.rejects(access(scratch), { code: 'ENOENT' });
});

test('an ordinary startup failure still deletes its temporary directory', async (t) => {
  let scratch;
  const failed = new Error('fixture executable missing');
  t.after(async () => {
    if (scratch !== undefined)
      await rm(scratch, { recursive: true, force: true });
  });
  await assert.rejects(
    checkDataRecovery('previous', 'current', {
      async startProcess(_binary, data) {
        scratch = dirname(data);
        await writeFile(join(scratch, 'inactive-database'), 'safe to remove');
        throw failed;
      },
    }),
    (error) => error === failed,
  );
  await assert.rejects(access(scratch), { code: 'ENOENT' });
});

test('an ordinary cleanup failure preserves both errors and still deletes inactive data', async (t) => {
  let scratch;
  const failed = new Error('fixture ordinary cleanup failure');
  t.after(async () => {
    if (scratch !== undefined)
      await rm(scratch, { recursive: true, force: true });
  });
  await assert.rejects(
    checkDataRecovery('previous', 'current', {
      async startProcess(_binary, data) {
        scratch = dirname(data);
        await writeFile(join(scratch, 'inactive-database'), 'safe to remove');
        return {
          api_url: 'invalid-url',
          token: 'fixture-token',
          async stop() {
            throw failed;
          },
        };
      },
    }),
    (error) => {
      assert.ok(error instanceof AggregateError);
      assert.equal(error.errors.length, 2);
      assert.ok(error.errors[0] instanceof TypeError);
      assert.equal(error.errors[1], failed);
      return true;
    },
  );
  await assert.rejects(access(scratch), { code: 'ENOENT' });
});
