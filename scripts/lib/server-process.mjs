import { execFileSync, spawn } from 'node:child_process';
import { readFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { createInterface } from 'node:readline';

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
 * environment and working directory. `stop()` ends it gracefully;
 * `kill()` simulates a crash.
 */
export async function startServer(
  binary,
  dataDir,
  extraArgs = [],
  { env, cwd } = {},
) {
  const child = spawn(
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
  child.stderr.setEncoding('utf8');
  child.stderr.on('data', (chunk) => {
    stderr += chunk;
  });
  const exited = new Promise((resolve) => child.once('exit', resolve));
  // Any failure before the handle is returned must not leave the server
  // running: its pipes would keep the caller's event loop alive.
  try {
    return await connect();
  } catch (error) {
    if (child.exitCode === null) {
      child.kill('SIGKILL');
      await exited;
    }
    throw error;
  }

  async function connect() {
    const ready = await new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        child.kill('SIGKILL');
        reject(new Error(`Server was not ready within 30s. ${stderr}`));
      }, 30_000);
      const lines = createInterface({ input: child.stdout });
      lines.once('line', (line) => {
        clearTimeout(timer);
        try {
          resolve(JSON.parse(line));
        } catch (error) {
          reject(
            new Error(`Unexpected readiness line: ${line}`, { cause: error }),
          );
        }
      });
      child.once('exit', (code) => {
        clearTimeout(timer);
        reject(new Error(`Server exited with ${code} before ready. ${stderr}`));
      });
    });
    if (ready.event !== 'ready')
      throw new Error(`Unexpected readiness event: ${JSON.stringify(ready)}`);
    const token = (await readFile(ready.token_file, 'utf8')).trim();
    return {
      ...ready,
      token,
      stderr: () => stderr,
      async stop() {
        if (child.exitCode !== null) return child.exitCode;
        child.kill('SIGTERM');
        return exited;
      },
      async kill() {
        if (child.exitCode !== null) return child.exitCode;
        child.kill('SIGKILL');
        return exited;
      },
    };
  }
}
