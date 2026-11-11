#!/usr/bin/env node
import { parseArgs } from 'node:util';
import { ArtifreeApiError, FileConflictError } from '@artifree/client';
import { scaffoldDescription } from '@artifree/contracts';
import { runArtifact, UsageError } from './artifact.js';
import { runAsset } from './asset.js';
import { runContent } from './content.js';
import { runData } from './data.js';
import { runJob } from './jobs.js';
import { runToken } from './tokens.js';
import { runUser } from './users.js';

const help = `Artifree — open artifacts, any agent.

Usage:
  artifree artifact <command>  Create, read, save, rename, pin, and preview artifacts
  artifree asset <command>     List, upload, read, and delete an artifact's uploaded assets
  artifree content <command>   Find and delete file content no revision refers to (cleanup)
  artifree data <command>      Read and write the application data of an artifact (get, set, update, delete)
  artifree job <command>       Queue, read, list, and cancel verification jobs (enqueue, show, list, cancel)
  artifree token <command>     Manage agent access tokens (list, create, rotate, revoke)
  artifree user <command>      Register and manage users (list, create, disable, enable)
  artifree describe [--json]  Inspect the build-time module inventory
  artifree --version         Print the version
  artifree --help            Show this help

Artifact commands talk to a running artifree-server; see artifree artifact --help.
Asset commands talk to a running artifree-server; see artifree asset --help.
Content commands talk to a running artifree-server; see artifree content --help.
Data commands talk to a running artifree-server; see artifree data --help.
Job commands talk to a running artifree-server; see artifree job --help. The server runs
jobs with its own worker unless it was started with --no-worker or as a one-shot session.
Token commands talk to a running artifree-server; see artifree token --help.
User commands talk to a running artifree-server; see artifree user --help.
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

// The client does not copy this field onto the error. It is read from the
// unchecked body here and validated before it is printed.
function currentAssetVersionOf(error: ArtifreeApiError): number | undefined {
  if (error.code !== 'asset_conflict') return undefined;
  const body = error.body;
  if (typeof body !== 'object' || body === null || !('error' in body))
    return undefined;
  const detail = body.error;
  if (
    typeof detail !== 'object' ||
    detail === null ||
    !('current_asset_version' in detail)
  )
    return undefined;
  const version = detail.current_asset_version;
  if (
    typeof version !== 'number' ||
    !Number.isSafeInteger(version) ||
    version < 0
  )
    return undefined;
  return version;
}

async function main(): Promise<void> {
  const args = process.argv.slice(2);
  if (args[0] === 'artifact') await runArtifact(args.slice(1));
  else if (args[0] === 'asset') await runAsset(args.slice(1));
  else if (args[0] === 'content') await runContent(args.slice(1));
  else if (args[0] === 'data') await runData(args.slice(1));
  else if (args[0] === 'job') await runJob(args.slice(1));
  else if (args[0] === 'token') await runToken(args.slice(1));
  else if (args[0] === 'user') await runUser(args.slice(1));
  else describe(args);
}

try {
  await main();
} catch (error: unknown) {
  if (error instanceof ArtifreeApiError) {
    console.error(`error[${error.code}]: ${error.message}`);
    if (error.currentRevision !== undefined)
      console.error(`current_revision=${error.currentRevision}`);
    if (error.currentMetadataRevision !== undefined)
      console.error(
        `current_metadata_revision=${error.currentMetadataRevision}`,
      );
    if (error.currentDataVersion !== undefined)
      console.error(`current_data_version=${error.currentDataVersion}`);
    const currentAssetVersion = currentAssetVersionOf(error);
    if (currentAssetVersion !== undefined)
      console.error(`current_asset_version=${currentAssetVersion}`);
    if (error instanceof FileConflictError) {
      for (const file of error.files)
        console.error(
          `conflict ${file.path} ${file.current_sha256 ?? 'absent'}`,
        );
      console.error(`conflicts=${error.total}`);
    }
    process.exitCode = 1;
  } else if (isUsageError(error)) {
    console.error(error instanceof Error ? error.message : 'Invalid usage.');
    process.exitCode = 2;
  } else {
    console.error(error instanceof Error ? error.message : 'CLI failed.');
    process.exitCode = 1;
  }
}
