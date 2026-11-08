import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { fileURLToPath } from 'node:url';

/** The two shards of scripts/smoke.mjs that together cover the whole browser smoke. */
export const parallelShards = ['outbox', 'ui'];

/**
 * Runs each shard as its own scripts/smoke.mjs process with the same
 * environment plus ARTIFREE_SMOKE_SHARD, prefixes every output line with the
 * shard name, and waits for all shards. Resolves to the shards that did not
 * pass. A shard that cannot start, or that exits without status 0, did not pass.
 */
export async function runShards({
  shards = parallelShards,
  env = process.env,
  command = process.execPath,
  args = [fileURLToPath(new URL('./smoke.mjs', import.meta.url))],
  stdout = process.stdout,
  stderr = process.stderr,
} = {}) {
  const passed = await Promise.all(
    shards.map((shard) =>
      runShard(shard, { env, command, args, stdout, stderr }),
    ),
  );
  return shards.filter((_shard, index) => !passed[index]);
}

function runShard(shard, { env, command, args, stdout, stderr }) {
  return new Promise((resolve) => {
    const child = spawn(command, args, {
      env: { ...env, ARTIFREE_SMOKE_SHARD: shard },
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let startError;
    // A command that cannot start emits 'error' and then 'close'. The verdict
    // waits for 'close', so all of the shard's output has been relayed by then.
    child.on('error', (error) => {
      startError = error;
    });
    child.once('close', (code, signal) => {
      if (startError !== undefined)
        stderr.write(`[${shard}] could not run: ${startError.message}\n`);
      else if (code !== 0)
        stderr.write(`[${shard}] exited with ${code ?? signal}\n`);
      resolve(startError === undefined && code === 0);
    });
    relayLines(child.stdout, shard, stdout);
    relayLines(child.stderr, shard, stderr);
  });
}

function relayLines(input, shard, output) {
  createInterface({ input }).on('line', (line) => {
    output.write(`[${shard}] ${line}\n`);
  });
}

async function main() {
  const failed = await runShards();
  if (failed.length > 0) {
    console.error(`Browser smoke shard failed: ${failed.join(', ')}.`);
    process.exitCode = 1;
    return;
  }
  console.log(`Browser smoke shards passed: ${parallelShards.join(', ')}.`);
}

if (process.argv[1] === fileURLToPath(import.meta.url)) await main();
