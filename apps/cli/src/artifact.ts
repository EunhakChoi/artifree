import { readFile } from 'node:fs/promises';
import { parseArgs } from 'node:util';
import {
  ArtifreeApiError,
  ArtifreeClient,
  encodeFile,
  newIdempotencyKey,
} from '@artifree/client';
import type { FileChange } from '@artifree/contracts';
import { readDirectory, readNamedFile, readRegularFile } from './files.js';

export const ARTIFACT_HELP = `Usage:
  artifree artifact list
  artifree artifact show <id>
  artifree artifact history <id>
  artifree artifact create --title <title> (--dir <dir> | --file <path>...)
  artifree artifact save <id> --base <revision>
      (--dir <dir> | [--put <package-path>=<local-path>]... [--delete <package-path>]...)
  artifree artifact cat <id> <package-path> [--revision <n>]
  artifree artifact preview <id> [--revision <n>]

Connection:
  --server <url>         API URL [env ARTIFREE_URL, default http://127.0.0.1:7410]
  --token-file <path>    Access token file [env ARTIFREE_TOKEN_FILE]
                         ARTIFREE_TOKEN may hold the token itself.

create and save accept --idempotency-key <key>. Without it a new key is
generated and reported on stderr if the outcome is unknown; retry with that key
to get the original result instead of a duplicate. save --dir replaces the
package with the directory's files (hidden entries skipped) and deletes
files the directory lacks. Results are JSON on stdout; cat writes raw bytes.
`;

/** Invalid command-line input; the caller reports it with exit code 2. */
export class UsageError extends Error {
  override name = 'UsageError';
}

const connectionOptions = {
  server: { type: 'string' },
  'token-file': { type: 'string' },
} as const;

async function connect(values: {
  server?: string | undefined;
  'token-file'?: string | undefined;
}): Promise<ArtifreeClient> {
  const baseUrl =
    values.server ?? process.env.ARTIFREE_URL ?? 'http://127.0.0.1:7410';
  const tokenFile = values['token-file'] ?? process.env.ARTIFREE_TOKEN_FILE;
  let accessToken = process.env.ARTIFREE_TOKEN;
  if (tokenFile !== undefined)
    accessToken = (await readFile(tokenFile, 'utf8')).trim();
  if (accessToken === undefined || accessToken === '')
    throw new UsageError(
      'An access token is required: pass --token-file, or set ARTIFREE_TOKEN_FILE or ARTIFREE_TOKEN.',
    );
  return new ArtifreeClient({ baseUrl, accessToken });
}

function revisionNumber(value: string | undefined, flag: string): number {
  const number = Number(value);
  if (value === undefined || !Number.isSafeInteger(number) || number < 1)
    throw new UsageError(`${flag} must be a positive whole number.`);
  return number;
}

function single(positionals: string[], names: string[]): string[] {
  if (positionals.length !== names.length)
    throw new UsageError(
      `Expected ${names.map((name) => `<${name}>`).join(' ')}. Use artifree artifact --help.`,
    );
  return positionals;
}

function print(value: unknown): void {
  process.stdout.write(`${JSON.stringify(value, null, 2)}\n`);
}

/** Runs `artifree artifact ...`. */
export async function runArtifact(args: string[]): Promise<void> {
  const [command, ...rest] = args;
  if (command === undefined || command === '--help' || command === '-h') {
    if (rest.length > 0)
      throw new UsageError('--help must be used on its own.');
    process.stdout.write(ARTIFACT_HELP);
    return;
  }
  switch (command) {
    case 'list':
      return list(rest);
    case 'show':
    case 'history':
      return show(command, rest);
    case 'create':
      return create(rest);
    case 'save':
      return save(rest);
    case 'cat':
      return cat(rest);
    case 'preview':
      return preview(rest);
    default:
      throw new UsageError(
        `Unknown artifact command: ${command}. Use artifree artifact --help.`,
      );
  }
}

async function list(args: string[]): Promise<void> {
  const { values, positionals } = parseArgs({
    args,
    options: connectionOptions,
    allowPositionals: true,
    strict: true,
  });
  single(positionals, []);
  const client = await connect(values);
  print({ artifacts: await client.listArtifacts() });
}

async function show(
  command: 'show' | 'history',
  args: string[],
): Promise<void> {
  const { values, positionals } = parseArgs({
    args,
    options: connectionOptions,
    allowPositionals: true,
    strict: true,
  });
  const [id = ''] = single(positionals, ['id']);
  const client = await connect(values);
  if (command === 'show') print(await client.getArtifact(id));
  else print({ revisions: await client.listRevisions(id) });
}

async function mutate<T>(
  key: string,
  operation: (idempotencyKey: string) => Promise<T>,
): Promise<T> {
  try {
    return await operation(key);
  } catch (error: unknown) {
    if (
      error instanceof ArtifreeApiError &&
      ['network_error', 'unavailable', 'internal', 'invalid_response'].includes(
        error.code,
      )
    )
      process.stderr.write(
        `The outcome is unknown. Retry with --idempotency-key ${key} to receive the original result.\n`,
      );
    throw error;
  }
}

async function create(args: string[]): Promise<void> {
  const { values, positionals } = parseArgs({
    args,
    options: {
      ...connectionOptions,
      title: { type: 'string' },
      dir: { type: 'string' },
      file: { type: 'string', multiple: true },
      'idempotency-key': { type: 'string' },
    },
    allowPositionals: true,
    strict: true,
  });
  single(positionals, []);
  if (values.title === undefined)
    throw new UsageError('create requires --title <title>.');
  if ((values.dir === undefined) === (values.file === undefined))
    throw new UsageError('create requires either --dir or --file, not both.');
  const files =
    values.dir === undefined
      ? await Promise.all((values.file ?? []).map(readNamedFile))
      : await readDirectory(values.dir);
  const client = await connect(values);
  const title = values.title;
  const result = await mutate(
    values['idempotency-key'] ?? newIdempotencyKey(),
    (idempotencyKey) =>
      client.createArtifact(
        {
          title,
          files: files.map((file) => encodeFile(file.path, file.bytes)),
        },
        { idempotencyKey },
      ),
  );
  print(result);
}

function splitPut(value: string): [string, string] {
  const separator = value.indexOf('=');
  if (separator <= 0 || separator === value.length - 1)
    throw new UsageError(
      `--put expects <package-path>=<local-path>, got ${value}.`,
    );
  return [value.slice(0, separator), value.slice(separator + 1)];
}

async function save(args: string[]): Promise<void> {
  const { values, positionals } = parseArgs({
    args,
    options: {
      ...connectionOptions,
      base: { type: 'string' },
      dir: { type: 'string' },
      put: { type: 'string', multiple: true },
      delete: { type: 'string', multiple: true },
      'idempotency-key': { type: 'string' },
    },
    allowPositionals: true,
    strict: true,
  });
  const [id = ''] = single(positionals, ['id']);
  if (values.base === undefined)
    throw new UsageError(
      'save requires --base <revision>; read current_revision with artifree artifact show <id>.',
    );
  const base = revisionNumber(values.base, '--base');
  const explicit = values.put !== undefined || values.delete !== undefined;
  if (explicit === (values.dir !== undefined))
    throw new UsageError('save requires either --dir or --put/--delete.');
  const client = await connect(values);
  const changes: FileChange[] = [];
  if (values.dir === undefined) {
    for (const value of values.put ?? []) {
      const [path, location] = splitPut(value);
      changes.push({
        op: 'put',
        ...encodeFile(path, await readRegularFile(location)),
      });
    }
    for (const path of values.delete ?? [])
      changes.push({ op: 'delete', path });
  } else {
    const files = await readDirectory(values.dir);
    const present = new Set(files.map((file) => file.path));
    const revisions = await client.listRevisions(id);
    const baseRevision = revisions.find((item) => item.revision === base);
    // Revisions are numbered 1..current, so a missing base is newer than the
    // current revision: report it as the server does for --put/--delete.
    if (baseRevision === undefined) {
      const current = revisions[0]?.revision;
      throw new ArtifreeApiError(
        `Revision ${base} is not current; the current revision is ${current ?? 'unknown'}.`,
        {
          status: 0,
          code: 'revision_conflict',
          ...(current === undefined ? {} : { currentRevision: current }),
        },
      );
    }
    for (const file of files)
      changes.push({ op: 'put', ...encodeFile(file.path, file.bytes) });
    for (const file of baseRevision.files)
      if (!present.has(file.path))
        changes.push({ op: 'delete', path: file.path });
  }
  const result = await mutate(
    values['idempotency-key'] ?? newIdempotencyKey(),
    (idempotencyKey) =>
      client.saveRevision(
        id,
        { base_revision: base, changes },
        { idempotencyKey },
      ),
  );
  print(result);
}

async function cat(args: string[]): Promise<void> {
  const { values, positionals } = parseArgs({
    args,
    options: { ...connectionOptions, revision: { type: 'string' } },
    allowPositionals: true,
    strict: true,
  });
  const [id = '', path = ''] = single(positionals, ['id', 'package-path']);
  const client = await connect(values);
  const revision =
    values.revision === undefined
      ? (await client.getArtifact(id)).current_revision
      : revisionNumber(values.revision, '--revision');
  process.stdout.write(await client.readFile(id, revision, path));
}

async function preview(args: string[]): Promise<void> {
  const { values, positionals } = parseArgs({
    args,
    options: { ...connectionOptions, revision: { type: 'string' } },
    allowPositionals: true,
    strict: true,
  });
  const [id = ''] = single(positionals, ['id']);
  const client = await connect(values);
  const revision =
    values.revision === undefined
      ? (await client.getArtifact(id)).current_revision
      : revisionNumber(values.revision, '--revision');
  print(await client.createPreview(id, revision));
}
