import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { PassThrough } from 'node:stream';
import { setImmediate } from 'node:timers/promises';
import { test } from 'node:test';
import {
  ServerClosureUnconfirmedError,
  startServer,
} from '../lib/server-process.mjs';

/** Only the OS process boundary is replaced; readiness and shutdown are real helper code. */
function fakeChild(onSignal = () => {}) {
  const child = new EventEmitter();
  child.pid = 12345;
  child.stdout = new PassThrough();
  child.stderr = new PassThrough();
  child.signals = [];
  child.kill = (signal) => {
    child.signals.push(signal);
    onSignal(signal, child);
    return true;
  };
  child.close = (code, signal = null) => {
    child.emit('exit', code, signal);
    child.stdout.end();
    child.stderr.end();
    child.emit('close', code, signal);
  };
  return child;
}

async function directory(t) {
  const root = await mkdtemp(join(tmpdir(), 'artifree-process-helper-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  return root;
}

async function readyServer(t, child) {
  const root = await directory(t);
  const tokenFile = join(root, 'token');
  await writeFile(tokenFile, 'fixture-token');
  const server = await startServer('fixture', root, [], {
    spawnProcess() {
      queueMicrotask(() =>
        child.stdout.write(
          JSON.stringify({
            event: 'ready',
            token_file: tokenFile,
          }) + '\n',
        ),
      );
      return child;
    },
  });
  assert.equal(server.token, 'fixture-token');
  return server;
}

test('a missing native executable rejects normally rather than emitting an uncaught error', async (t) => {
  const root = await directory(t);
  await assert.rejects(
    startServer(join(root, 'absent-server'), join(root, 'data')),
    (error) => {
      assert.equal(error.message, 'Server process failed before ready.');
      assert.equal(error.cause.code, 'ENOENT');
      return true;
    },
  );
});

test('a spawn error followed by close needs no exit event or signal', async (t) => {
  const root = await directory(t);
  const child = fakeChild();
  child.pid = undefined;
  const denied = Object.assign(new Error('fixture permission denied'), {
    code: 'EACCES',
  });
  await assert.rejects(
    startServer('fixture', root, [], {
      spawnProcess() {
        queueMicrotask(() => {
          child.emit('error', denied);
          child.stdout.end();
          child.stderr.end();
          child.emit('close', -13, null);
        });
        return child;
      },
    }),
    (error) => {
      assert.equal(error.cause, denied);
      return true;
    },
  );
  assert.deepEqual(child.signals, []);
});

test('stop escalates ignored TERM, then waits for KILL exit and closed pipes', async (t) => {
  const child = fakeChild();
  const server = await readyServer(t, child);
  t.mock.timers.enable({ apis: ['setTimeout'] });
  let settled = false;
  const stopped = server.stop().then((code) => {
    settled = true;
    return code;
  });
  assert.deepEqual(child.signals, ['SIGTERM']);
  t.mock.timers.tick(5_000);
  await setImmediate();
  assert.deepEqual(child.signals, ['SIGTERM', 'SIGKILL']);
  assert.equal(settled, false);
  child.emit('exit', null, 'SIGKILL');
  await setImmediate();
  assert.equal(settled, false, 'exit alone does not confirm stdio closure');
  child.stdout.end();
  child.stderr.end();
  child.emit('close', null, 'SIGKILL');
  assert.equal(await stopped, null);
  assert.equal(await server.stop(), null);
  assert.deepEqual(
    child.signals,
    ['SIGTERM', 'SIGKILL'],
    'a closed child is not signalled again',
  );
});

test('stop rejects within its deadlines when neither TERM nor KILL closes the child', async (t) => {
  const child = fakeChild();
  const server = await readyServer(t, child);
  t.mock.timers.enable({ apis: ['setTimeout'] });
  let timeout;
  const rejected = assert.rejects(server.stop(), (error) => {
    assert.ok(error instanceof ServerClosureUnconfirmedError);
    assert.equal(error.pid, 12345);
    assert.match(error.message, /did not close after TERM then KILL/);
    assert.equal(error.isClosureConfirmed(), false);
    timeout = error;
    return true;
  });
  t.mock.timers.tick(5_000);
  await setImmediate();
  t.mock.timers.tick(5_000);
  await rejected;
  assert.deepEqual(child.signals, ['SIGTERM', 'SIGKILL']);
  child.close(null, 'SIGKILL');
  assert.equal(
    timeout.isClosureConfirmed(),
    true,
    'the original error tracks a later actual close',
  );
});

test('startup preserves both its original readiness error and bounded cleanup failure', async (t) => {
  const root = await directory(t);
  const child = fakeChild();
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const started = startServer('fixture', root, [], {
    spawnProcess() {
      queueMicrotask(() => child.stdout.write('invalid readiness\n'));
      return child;
    },
  });
  let timeout;
  const rejected = assert.rejects(started, (error) => {
    assert.ok(error instanceof AggregateError);
    assert.equal(error.errors.length, 2);
    assert.match(
      error.errors[0].message,
      /Unexpected readiness line: invalid readiness/,
    );
    assert.ok(error.errors[1] instanceof ServerClosureUnconfirmedError);
    assert.equal(error.errors[1].pid, 12345);
    assert.equal(error.errors[1].isClosureConfirmed(), false);
    timeout = error.errors[1];
    assert.match(error.errors[1].message, /did not close after KILL/);
    return true;
  });
  await setImmediate();
  assert.deepEqual(child.signals, ['SIGKILL']);
  t.mock.timers.tick(5_000);
  await rejected;
  child.close(null, 'SIGKILL');
  assert.equal(
    timeout.isClosureConfirmed(),
    true,
    'startup without a returned handle still tracks late close',
  );
});

test('a readiness timeout kills the child and waits for its closure before rejecting', async (t) => {
  const root = await directory(t);
  const child = fakeChild((signal, process) => process.close(null, signal));
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const rejected = assert.rejects(
    startServer('fixture', root, [], {
      spawnProcess: () => child,
    }),
    /Server was not ready within 30s/,
  );
  t.mock.timers.tick(30_000);
  await rejected;
  assert.deepEqual(child.signals, ['SIGKILL']);
});
