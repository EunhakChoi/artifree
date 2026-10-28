import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { parseArgs } from 'node:util';
import {
  ArtifreeApiError,
  ArtifreeClient,
  encodeFile,
  newIdempotencyKey,
} from '@artifree/client';
import type {
  Artifact,
  FileChange,
  ListArtifactsQuery,
  UpdateArtifactRequest,
} from '@artifree/contracts';
import {
  readDirectory,
  readNamedFile,
  readRegularFile,
  writeNewFile,
} from './files.js';

export const ARTIFACT_HELP = `Usage:
  artifree artifact list [--search <text>] [--sort updated|created]
      [--pinned | --unpinned] [--deleted] [--limit <n>] [--page-token <token>]
  artifree artifact show <id>
  artifree artifact history <id>
  artifree artifact create --title <title> (--dir <dir> | --file <path>...)
  artifree artifact save <id> --base <revision>
      (--dir <dir> | [--put <package-path>=<local-path>]... [--delete <package-path>]...)
  artifree artifact restore <id> <revision> --base <revision>
  artifree artifact duplicate <id> <revision> [--title <title>]
  artifree artifact rename <id> <title> --base-metadata <n>
  artifree artifact pin <id> --base-metadata <n>
  artifree artifact unpin <id> --base-metadata <n>
  artifree artifact delete <id> --base-metadata <n>
  artifree artifact recover <id> --base-metadata <n>
  artifree artifact cat <id> <package-path> [--revision <n>]
      [--offset <byte>] [--length <bytes>]
  artifree artifact export <id> --output <file> [--revision <n>]
  artifree artifact preview <id> [--revision <n>]

Connection:
  --server <url>         API URL [env ARTIFREE_URL, default http://127.0.0.1:7410]
  --token-file <path>    Access token file [env ARTIFREE_TOKEN_FILE]
                         ARTIFREE_TOKEN may hold the token itself.

list prints every matching artifact, reading page after page, unless
--limit or --page-token asks for one page; that page carries next_page_token
when more follow; --deleted lists the trash instead. rename, pin, unpin,
delete, and recover change metadata only and need the metadata_revision read
with show, or with list --deleted for an artifact in the trash
(--base-metadata); they create no revision. delete moves an artifact to the
trash, where nothing can read or change it until recover takes it out.
restore commits the files of an earlier revision, exactly as they were, as
the next revision; --base names the current revision, as for save.
duplicate creates an artifact whose revision 1 has the files of that
revision, with its own history; it is unpinned and named --title or the
source's title followed by " (Copy)". Only files are copied.
export writes a ZIP archive of one revision's files, at their package paths
and byte for byte, with the manifest .artifree-export.json, to a new file
(--output; an existing file is never replaced). It prints the archive's size
and SHA-256. It is one revision, not a backup of the artifact or its history.

create, save, restore, duplicate, rename, pin, unpin, delete, and recover
accept --idempotency-key <key>. Without it a new key is
generated and reported on stderr if the outcome is unknown; retry with that key
to get the original result instead of a duplicate. save --dir replaces the
package with the directory's files (hidden entries skipped) and deletes
files the directory lacks. Results are JSON on stdout; cat writes raw bytes,
all of the file or, with --offset or --length, the bytes from --offset
(default 0), --length of them or to the end.
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

function positiveNumber(value: string | undefined, flag: string): number {
  const number = Number(value);
  if (value === undefined || !Number.isSafeInteger(number) || number < 1)
    throw new UsageError(`${flag} must be a positive whole number.`);
  return number;
}

function wholeNumber(value: string, flag: string): number {
  const number = Number(value);
  if (!/^\d+$/.test(value) || !Number.isSafeInteger(number))
    throw new UsageError(`${flag} must be a whole number of at least 0.`);
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
    case 'restore':
      return restore(rest);
    case 'duplicate':
      return duplicate(rest);
    case 'rename':
    case 'pin':
    case 'unpin':
    case 'delete':
    case 'recover':
      return update(command, rest);
    case 'cat':
      return cat(rest);
    case 'export':
      return exportRevision(rest);
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
    options: {
      ...connectionOptions,
      search: { type: 'string' },
      sort: { type: 'string' },
      pinned: { type: 'boolean' },
      unpinned: { type: 'boolean' },
      deleted: { type: 'boolean' },
      limit: { type: 'string' },
      'page-token': { type: 'string' },
    },
    allowPositionals: true,
    strict: true,
  });
  single(positionals, []);
  if (values.pinned === true && values.unpinned === true)
    throw new UsageError('Use --pinned or --unpinned, not both.');
  const { sort } = values;
  if (sort !== undefined && sort !== 'updated' && sort !== 'created')
    throw new UsageError('--sort must be updated or created.');
  const query: ListArtifactsQuery = {
    ...(values.search === undefined ? {} : { q: values.search }),
    ...(sort === undefined ? {} : { sort }),
    ...(values.pinned === true ? { pinned: true } : {}),
    ...(values.unpinned === true ? { pinned: false } : {}),
    ...(values.deleted === true ? { deleted: true } : {}),
    ...(values.limit === undefined
      ? {}
      : { limit: positiveNumber(values.limit, '--limit') }),
    ...(values['page-token'] === undefined
      ? {}
      : { page_token: values['page-token'] }),
  };
  const client = await connect(values);
  if (query.limit !== undefined || query.page_token !== undefined) {
    print(await client.listArtifacts(query));
    return;
  }
  // Without paging options, print the whole list as before pages existed.
  const artifacts: Artifact[] = [];
  let page = await client.listArtifacts(query);
  artifacts.push(...page.artifacts);
  while (page.next_page_token !== undefined && page.next_page_token !== null) {
    page = await client.listArtifacts({
      ...query,
      page_token: page.next_page_token,
    });
    artifacts.push(...page.artifacts);
  }
  print({ artifacts });
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
    else if (error instanceof ArtifreeApiError && error.code === 'rate_limited')
      // Only this attempt was refused. An earlier attempt with the same key
      // may have committed; retrying with the key then returns its result.
      process.stderr.write(
        `This attempt was not processed. Retry after ${error.retryAfterSeconds ?? 1}s with --idempotency-key ${key}.\n`,
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

type MetadataCommand = 'rename' | 'pin' | 'unpin' | 'delete' | 'recover';

/** What each metadata command changes, besides the title a rename names. */
const METADATA_CHANGES: Record<
  MetadataCommand,
  Pick<UpdateArtifactRequest, 'pinned' | 'deleted'>
> = {
  rename: {},
  pin: { pinned: true },
  unpin: { pinned: false },
  delete: { deleted: true },
  recover: { deleted: false },
};

async function update(command: MetadataCommand, args: string[]): Promise<void> {
  const { values, positionals } = parseArgs({
    args,
    options: {
      ...connectionOptions,
      'base-metadata': { type: 'string' },
      'idempotency-key': { type: 'string' },
    },
    allowPositionals: true,
    strict: true,
  });
  const [id = '', title] =
    command === 'rename'
      ? single(positionals, ['id', 'title'])
      : single(positionals, ['id']);
  if (values['base-metadata'] === undefined)
    throw new UsageError(
      `${command} requires --base-metadata <n>; read metadata_revision with artifree artifact show <id>.`,
    );
  const base = positiveNumber(values['base-metadata'], '--base-metadata');
  const client = await connect(values);
  const result = await mutate(
    values['idempotency-key'] ?? newIdempotencyKey(),
    (idempotencyKey) =>
      client.updateArtifact(
        id,
        {
          base_metadata_revision: base,
          ...(title === undefined ? {} : { title }),
          ...METADATA_CHANGES[command],
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
  const base = positiveNumber(values.base, '--base');
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

async function restore(args: string[]): Promise<void> {
  const { values, positionals } = parseArgs({
    args,
    options: {
      ...connectionOptions,
      base: { type: 'string' },
      'idempotency-key': { type: 'string' },
    },
    allowPositionals: true,
    strict: true,
  });
  const [id = '', revisionText] = single(positionals, ['id', 'revision']);
  const revision = positiveNumber(revisionText, '<revision>');
  if (values.base === undefined)
    throw new UsageError(
      'restore requires --base <revision>; read current_revision with artifree artifact show <id>.',
    );
  const base = positiveNumber(values.base, '--base');
  const client = await connect(values);
  const result = await mutate(
    values['idempotency-key'] ?? newIdempotencyKey(),
    (idempotencyKey) =>
      client.restoreRevision(
        id,
        revision,
        { base_revision: base },
        { idempotencyKey },
      ),
  );
  print(result);
}

async function duplicate(args: string[]): Promise<void> {
  const { values, positionals } = parseArgs({
    args,
    options: {
      ...connectionOptions,
      title: { type: 'string' },
      'idempotency-key': { type: 'string' },
    },
    allowPositionals: true,
    strict: true,
  });
  const [id = '', revisionText] = single(positionals, ['id', 'revision']);
  const revision = positiveNumber(revisionText, '<revision>');
  const client = await connect(values);
  const result = await mutate(
    values['idempotency-key'] ?? newIdempotencyKey(),
    (idempotencyKey) =>
      client.duplicateArtifact(
        id,
        revision,
        values.title === undefined ? {} : { title: values.title },
        { idempotencyKey },
      ),
  );
  print(result);
}

async function cat(args: string[]): Promise<void> {
  const { values, positionals } = parseArgs({
    args,
    options: {
      ...connectionOptions,
      revision: { type: 'string' },
      offset: { type: 'string' },
      length: { type: 'string' },
    },
    allowPositionals: true,
    strict: true,
  });
  const [id = '', path = ''] = single(positionals, ['id', 'package-path']);
  const offset =
    values.offset === undefined
      ? undefined
      : wholeNumber(values.offset, '--offset');
  const length =
    values.length === undefined
      ? undefined
      : positiveNumber(values.length, '--length');
  const client = await connect(values);
  const revision =
    values.revision === undefined
      ? (await client.getArtifact(id)).current_revision
      : positiveNumber(values.revision, '--revision');
  if (offset === undefined && length === undefined) {
    process.stdout.write(await client.readFile(id, revision, path));
    return;
  }
  const range = {
    offset: offset ?? 0,
    ...(length === undefined ? {} : { length }),
  };
  process.stdout.write(
    (await client.readFilePart(id, revision, path, range)).bytes,
  );
}

async function exportRevision(args: string[]): Promise<void> {
  const { values, positionals } = parseArgs({
    args,
    options: {
      ...connectionOptions,
      revision: { type: 'string' },
      output: { type: 'string' },
    },
    allowPositionals: true,
    strict: true,
  });
  const [id = ''] = single(positionals, ['id']);
  const output = values.output;
  if (output === undefined || output === '')
    throw new UsageError('export needs --output <file> for the ZIP archive.');
  const client = await connect(values);
  const revision =
    values.revision === undefined
      ? (await client.getArtifact(id)).current_revision
      : positiveNumber(values.revision, '--revision');
  const archive = await client.exportRevision(id, revision);
  await writeNewFile(output, archive);
  print({
    artifact_id: id,
    revision,
    output,
    bytes: archive.byteLength,
    sha256: createHash('sha256').update(archive).digest('hex'),
  });
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
      : positiveNumber(values.revision, '--revision');
  print(await client.createPreview(id, revision));
}
