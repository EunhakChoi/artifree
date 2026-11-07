import { execFileSync, spawn } from 'node:child_process';
import { readFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { createInterface } from 'node:readline';

/** A close deadline elapsed; recheck `isClosureConfirmed()` before deleting child data. */
export class ServerClosureUnconfirmedError extends Error {
  constructor(pid, signal, { cause, isClosed = () => false } = {}) {
    super(
      `Server process ${pid ?? '(spawn failed)'} did not close after ${signal === 'SIGTERM' ? 'TERM then KILL' : 'KILL'}.`,
      { cause },
    );
    this.name = 'ServerClosureUnconfirmedError';
    this.pid = pid;
    // A timeout remains an error, but cleanup must recheck this child's latest
    // close observation instead of treating a historical timeout as liveness.
    this.isClosureConfirmed = isClosed;
  }
}

/**
 * Builds artifree-server and returns the executable path reported by Cargo,
 * so callers do not guess target directories or platform suffixes.
 */
export function buildServer(root, { release = false } = {}) {
  const output = execFileSync(
    'cargo',
    [
      'build',
      ...(release ? ['--release'] : []),
      '--quiet',
      '--locked',
      '-p',
      'artifree-server',
      '--bin',
      'artifree-server',
      '--message-format=json',
    ],
    { cwd: root, encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'] },
  );
  for (const line of output.split('\n')) {
    if (!line.startsWith('{')) continue;
    const message = JSON.parse(line);
    if (
      message.reason === 'compiler-artifact' &&
      message.target?.name === 'artifree-server' &&
      message.executable
    )
      return message.executable;
  }
  throw new Error('Cargo did not report the artifree-server executable.');
}

/** A port that was free a moment ago on the loopback interface. */
export async function freePort() {
  const probe = createServer();
  await new Promise((done) => probe.listen(0, '127.0.0.1', done));
  const { port } = probe.address();
  await new Promise((done) => probe.close(done));
  return port;
}

/**
 * Starts `artifree-server serve` on free loopback ports and resolves after
 * its readiness line. `options.env` and `options.cwd` replace the child's
 * environment and working directory. `stop()` allows five seconds for TERM,
 * then five for KILL and closed stdio; `kill()` immediately requests KILL.
 * Both wait for actual child closure and reject if closure is not confirmed.
 * `options.spawnProcess` replaces only the process boundary in deterministic tests.
 */
export async function startServer(
  binary,
  dataDir,
  extraArgs = [],
  { env, cwd, spawnProcess = spawn } = {},
) {
  const child = spawnProcess(
    binary,
    [
      'serve',
      '--data-dir',
      dataDir,
      '--api-listen',
      '127.0.0.1:0',
      '--content-listen',
      '127.0.0.1:0',
      ...extraArgs,
    ],
    // Without `env`/`cwd` the child inherits ours; release verification passes
    // a scrubbed environment and a directory outside the repository.
    {
      stdio: ['ignore', 'pipe', 'pipe'],
      ...(env && { env }),
      ...(cwd && { cwd }),
    },
  );
  let stderr = '';
  let processError;
  let didExit = false;
  let didClose = false;
  let exitCode;
  // An unsuccessful spawn emits error and close, but never exit. Keep an error
  // listener for the whole child lifetime, including signal-delivery errors.
  child.on('error', (error) => {
    processError = error;
  });
  child.once('exit', () => {
    didExit = true;
  });
  const closed = new Promise((resolve) =>
    child.once('close', (code) => {
      didClose = true;
      exitCode = code;
      resolve();
    }),
  );
  child.stderr.setEncoding('utf8');
  child.stderr.on('data', (chunk) => {
    stderr += chunk;
  });
  // Any failure before the handle is returned must not leave the server
  // running: its pipes would keep the caller's event loop alive.
  try {
    return await connect();
  } catch (error) {
    try {
      await terminate('SIGKILL');
    } catch (cleanup) {
      throw new AggregateError(
        [error, cleanup],
        'Server startup and cleanup failed.',
      );
    }
    throw error;
  }

  async function waitForClose() {
    if (didClose) return true;
    let timer;
    try {
      return await Promise.race([
        closed.then(() => true),
        new Promise((resolve) => {
          timer = setTimeout(() => resolve(false), 5_000);
        }),
      ]);
    } finally {
      clearTimeout(timer);
    }
  }

  async function terminate(signal) {
    if (didClose) return exitCode;
    if (!didExit && child.pid !== undefined) child.kill(signal);
    if (await waitForClose()) return exitCode;
    if (signal === 'SIGTERM') {
      if (!didExit && child.pid !== undefined) child.kill('SIGKILL');
      if (await waitForClose()) return exitCode;
    }
    throw new ServerClosureUnconfirmedError(child.pid, signal, {
      cause: processError,
      isClosed: () => didClose,
    });
  }

  async function connect() {
    const lines = createInterface({ input: child.stdout });
    let timer;
    let onLine;
    let onError;
    let onClose;
    let ready;
    try {
      ready = await new Promise((resolve, reject) => {
        timer = setTimeout(() => {
          reject(new Error(`Server was not ready within 30s. ${stderr}`));
        }, 30_000);
        onError = (error) => {
          reject(
            new Error('Server process failed before ready.', { cause: error }),
          );
        };
        onClose = (code) => {
          reject(
            new Error(`Server exited with ${code} before ready. ${stderr}`),
          );
        };
        onLine = (line) => {
          try {
            resolve(JSON.parse(line));
          } catch (error) {
            reject(
              new Error(`Unexpected readiness line: ${line}`, { cause: error }),
            );
          }
        };
        lines.once('line', onLine);
        child.once('error', onError);
        child.once('close', onClose);
      });
    } finally {
      clearTimeout(timer);
      lines.removeListener('line', onLine);
      lines.close();
      child.removeListener('error', onError);
      child.removeListener('close', onClose);
    }
    if (ready.event !== 'ready')
      throw new Error(`Unexpected readiness event: ${JSON.stringify(ready)}`);
    const token = (await readFile(ready.token_file, 'utf8')).trim();
    return {
      ...ready,
      token,
      stderr: () => stderr,
      stop: () => terminate('SIGTERM'),
      kill: () => terminate('SIGKILL'),
    };
  }
}
