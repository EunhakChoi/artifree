import { readFile } from 'node:fs/promises';
import { parseArgs } from 'node:util';
import { ArtifreeClient, JsonText, newIdempotencyKey } from '@artifree/client';
import { mutate } from './artifact.js';
import { UsageError } from './errors.js';
import { resolveConnection } from './profiles.js';

export const DATA_HELP = `Usage:
  artifree data get <artifact-id> [--path <path>]
  artifree data set <artifact-id> --path <path> --base-data-version <n>
      (--value <json> | --value-file <file>) [--idempotency-key <key>]
  artifree data update <artifact-id> --path <path> --base-data-version <n>
      (--patch <json> | --patch-file <file>) [--idempotency-key <key>]
  artifree data delete <artifact-id> --path <path> --base-data-version <n>
      [--idempotency-key <key>]

Connection:
  --server <url>         API URL [env ARTIFREE_SERVER, ARTIFREE_URL, default http://127.0.0.1:7410]
  --token <val>          Access token [env ARTIFREE_TOKEN]
  --token-file <path>    Access token file [env ARTIFREE_TOKEN_FILE]
  --profile <name>       Connection profile to load
  --profile-file <path>  Path to profile configuration file [env ARTIFREE_PROFILES_FILE]

Application data is one JSON object per artifact that you keep on the server
(decision 0031). It has its own data_version: 0 until written, then one more
for every write that changes something. It is unrelated to the artifact's
revisions and metadata revision; saving or restoring a revision does not touch
it, and it is not in a revision, an export, or a preview. A page running in a
preview cannot read or write it. There is no history: a write replaces the
value for good, and a duplicate gets a copy only with
artifree artifact duplicate --copy-data.
A path is up to 16 segments of 1-64 ASCII letters, digits, "_" or "-"
separated by "/", such as todos/first; the empty path "" is the root. A path
addresses object members, never array elements. get prints the value at the
path ("exists": false, without a value, where nothing is) and the data_version.
set replaces the value at the path and makes the objects above it that are
missing; the root must be given an object. update applies a JSON Merge Patch
(RFC 7396): objects merge member by member, a null member removes that member,
arrays and scalars replace. delete removes the value at the path; the empty
path empties everything. Every write needs --path (use --path "" for the
root) and --base-data-version, the data_version you read; a write over a
newer version fails with data_conflict and prints current_data_version on
stderr. A write that changes nothing keeps the version (changed: false).
A value that starts with "-" (a negative number) is written --value=-5, with
the "=", or read from --value-file; --value -5 is a usage error.
Values are JSON text and are sent exactly as written, so a repeated member
name or a number beyond 2^53-1 in any spelling (9007199254740993, 1e16) is
refused by the server and not silently changed; 0.1e-400 is stored as 0.
Limits: 256 KiB per value, 1 MiB and 10000 values in all, 32 levels.
delete sends its path and version as a JSON body on a DELETE request. A proxy
or tunnel that strips the body of a DELETE makes the server answer 400
invalid_request and deletes nothing. Remove a value through such a proxy with
update, --patch null, and the same --path; or reach the server directly.
set, update, and delete accept --idempotency-key <key>; without it a new key
is generated and reported on stderr if the outcome is unknown, and a retry
with that key returns the original result with replayed: true. Results are
JSON on stdout.
`;

const connectionOptions = {
  server: { type: 'string' },
  token: { type: 'string' },
  'token-file': { type: 'string' },
  profile: { type: 'string' },
  'profile-file': { type: 'string' },
} as const;

type Connection = {
  server?: string | undefined;
  token?: string | undefined;
  'token-file'?: string | undefined;
  profile?: string | undefined;
  'profile-file'?: string | undefined;
};

async function connect(values: Connection): Promise<ArtifreeClient> {
  const { server, token } = await resolveConnection({
    server: values.server,
    token: values.token,
    tokenFile: values['token-file'],
    profile: values.profile,
    profileFile: values['profile-file'],
  });
  return new ArtifreeClient({ baseUrl: server, accessToken: token });
}

function print(value: unknown): void {
  process.stdout.write(`${JSON.stringify(value, null, 2)}\n`);
}

function one(positionals: string[]): string {
  const [first, ...others] = positionals;
  if (first === undefined || others.length > 0)
    throw new UsageError('Expected <artifact-id>. Use artifree data --help.');
  return first;
}

function baseVersion(value: string | undefined): number {
  const number = Number(value);
  if (
    value === undefined ||
    !/^[0-9]+$/.test(value) ||
    !Number.isSafeInteger(number)
  )
    throw new UsageError(
      '--base-data-version <n> is required: the data_version read with artifree data get.',
    );
  return number;
}

function pathOf(value: string | undefined): string {
  if (value === undefined)
    throw new UsageError(
      '--path is required for a write; use --path "" for the root.',
    );
  return value;
}

/** The JSON text of `--<name> <json>` or `--<name>-file <file>`. */
async function jsonText(
  name: string,
  inline: string | undefined,
  file: string | undefined,
): Promise<JsonText> {
  if ((inline === undefined) === (file === undefined))
    throw new UsageError(`Give exactly one of --${name} and --${name}-file.`);
  if (inline !== undefined) return new JsonText(inline);
  const bytes = await readFile(file ?? '');
  try {
    return new JsonText(
      new TextDecoder('utf-8', { fatal: true }).decode(bytes),
    );
  } catch {
    throw new UsageError(`--${name}-file is not valid UTF-8 text.`);
  }
}

/** Runs `artifree data ...`. */
export async function runData(args: string[]): Promise<void> {
  const [command, ...rest] = args;
  if (command === undefined || command === '--help' || command === '-h') {
    if (rest.length > 0)
      throw new UsageError('--help must be used on its own.');
    process.stdout.write(DATA_HELP);
    return;
  }
  switch (command) {
    case 'get':
      return get(rest);
    case 'set':
      return set(rest);
    case 'update':
      return update(rest);
    case 'delete':
      return remove(rest);
    default:
      throw new UsageError(
        `Unknown data command: ${command}. Use artifree data --help.`,
      );
  }
}

async function get(args: string[]): Promise<void> {
  const { values, positionals } = parseArgs({
    args,
    options: { ...connectionOptions, path: { type: 'string' } },
    allowPositionals: true,
    strict: true,
  });
  const id = one(positionals);
  print(await (await connect(values)).getData(id, values.path));
}

async function set(args: string[]): Promise<void> {
  const { values, positionals } = parseArgs({
    args,
    options: {
      ...connectionOptions,
      path: { type: 'string' },
      'base-data-version': { type: 'string' },
      value: { type: 'string' },
      'value-file': { type: 'string' },
      'idempotency-key': { type: 'string' },
    },
    allowPositionals: true,
    strict: true,
  });
  const id = one(positionals);
  const path = pathOf(values.path);
  const base = baseVersion(values['base-data-version']);
  const value = await jsonText('value', values.value, values['value-file']);
  const client = await connect(values);
  print(
    await mutate(
      values['idempotency-key'] ?? newIdempotencyKey(),
      (idempotencyKey) =>
        client.setData(
          id,
          { path, base_data_version: base, value },
          { idempotencyKey },
        ),
    ),
  );
}

async function update(args: string[]): Promise<void> {
  const { values, positionals } = parseArgs({
    args,
    options: {
      ...connectionOptions,
      path: { type: 'string' },
      'base-data-version': { type: 'string' },
      patch: { type: 'string' },
      'patch-file': { type: 'string' },
      'idempotency-key': { type: 'string' },
    },
    allowPositionals: true,
    strict: true,
  });
  const id = one(positionals);
  const path = pathOf(values.path);
  const base = baseVersion(values['base-data-version']);
  const patch = await jsonText('patch', values.patch, values['patch-file']);
  const client = await connect(values);
  print(
    await mutate(
      values['idempotency-key'] ?? newIdempotencyKey(),
      (idempotencyKey) =>
        client.updateData(
          id,
          { path, base_data_version: base, patch },
          { idempotencyKey },
        ),
    ),
  );
}

async function remove(args: string[]): Promise<void> {
  const { values, positionals } = parseArgs({
    args,
    options: {
      ...connectionOptions,
      path: { type: 'string' },
      'base-data-version': { type: 'string' },
      'idempotency-key': { type: 'string' },
    },
    allowPositionals: true,
    strict: true,
  });
  const id = one(positionals);
  const path = pathOf(values.path);
  const base = baseVersion(values['base-data-version']);
  const client = await connect(values);
  print(
    await mutate(
      values['idempotency-key'] ?? newIdempotencyKey(),
      (idempotencyKey) =>
        client.deleteData(
          id,
          { path, base_data_version: base },
          { idempotencyKey },
        ),
    ),
  );
}
