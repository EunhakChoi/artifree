#!/usr/bin/env node
import { parseArgs } from 'node:util';
import { ArtifreeApiError } from '@artifree/client';
import { scaffoldDescription } from '@artifree/contracts';
import { runArtifact, UsageError } from './artifact.js';

const help = `Artifree — open artifacts, any agent.

Usage:
  artifree artifact <command>  Create, read, save, and preview artifacts
  artifree describe [--json]  Inspect the build-time module inventory
  artifree --version         Print the version
  artifree --help            Show this help

Artifact commands talk to a running artifree-server; see artifree artifact --help.
describe prints an inventory bundled at build time; it does not probe a server.
`;

function describe(args: string[]): void {
  const { values, positionals } = parseArgs({
    args,
    options: {
      json: { type: 'boolean', default: false },
      version: { type: 'boolean', short: 'v', default: false },
      help: { type: 'boolean', short: 'h', default: false },
    },
    allowPositionals: true,
    strict: true,
  });
  if (positionals.length > 1)
    throw new UsageError('Expected at most one command.');
  const command = positionals[0];
  if (values.help) {
    if (command !== undefined || values.version || values.json) {
      throw new UsageError('--help must be used on its own.');
    }
    process.stdout.write(help);
    return;
  }
  if (values.version) {
    if (command || values.json)
      throw new UsageError('--version does not accept other arguments.');
    console.log(scaffoldDescription.version);
    return;
  }
  if (command === undefined) {
    if (values.json)
      throw new UsageError('--json requires the describe command.');
    process.stdout.write(help);
    return;
  }
  if (command !== 'describe')
    throw new UsageError(`Unknown command: ${command}. Use --help.`);
  if (values.json) {
    console.log(JSON.stringify(scaffoldDescription, null, 2));
    return;
  }
  console.log(
    `Artifree ${scaffoldDescription.version} · ${scaffoldDescription.phase}`,
  );
  console.log(
    'Build-time inventory; it does not report whether a server is running.',
  );
  for (const item of scaffoldDescription.components) {
    console.log(
      `${item.status.padEnd(11)} ${item.name.padEnd(23)} ${item.path}`,
    );
  }
}

function isUsageError(error: unknown): boolean {
  if (error instanceof UsageError) return true;
  // Node's parseArgs reports unknown or malformed options with these codes.
  return (
    error instanceof Error &&
    'code' in error &&
    typeof error.code === 'string' &&
    error.code.startsWith('ERR_PARSE_ARGS_')
  );
}

async function main(): Promise<void> {
  const args = process.argv.slice(2);
  if (args[0] === 'artifact') await runArtifact(args.slice(1));
  else describe(args);
}

try {
  await main();
} catch (error: unknown) {
  if (error instanceof ArtifreeApiError) {
    console.error(`error[${error.code}]: ${error.message}`);
    if (error.currentRevision !== undefined)
      console.error(`current_revision=${error.currentRevision}`);
    process.exitCode = 1;
  } else if (isUsageError(error)) {
    console.error(error instanceof Error ? error.message : 'Invalid usage.');
    process.exitCode = 2;
  } else {
    console.error(error instanceof Error ? error.message : 'CLI failed.');
    process.exitCode = 1;
  }
}
