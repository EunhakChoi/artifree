import { parseArgs } from 'node:util';
import { UsageError } from './errors.js';
import { connect, connectionOptions } from './tokens.js';

export const TYPE_HELP = `Usage:
  artifree type list
  artifree type show <id> [--release <n>] [--reference <path>]

Connection:
  --server <url>         API URL [env ARTIFREE_SERVER, ARTIFREE_URL, default http://127.0.0.1:7410]
  --token <val>          Access token [env ARTIFREE_TOKEN]
  --token-file <path>    Access token file [env ARTIFREE_TOKEN_FILE]
  --profile <name>       Connection profile to load
  --profile-file <path>  Path to profile configuration file [env ARTIFREE_PROFILES_FILE]

A type is a built-in kind of artifact with required files, a starter and a
guide (decision 0059). list prints the current release of each type, without
guides. show prints one release with its guide: the current one, or
--release <n>, the release an older artifact was made from. --reference
prints one reference file of that release instead. A guide describes how to
fill a type; it grants no permission. build_available says whether this
server can build the type's profile. Create from a type with
artifree artifact create --title <title> --type <id>. Results are JSON on
stdout; an unknown type, release or reference is 404 not_found.
`;

function print(value: unknown): void {
  process.stdout.write(`${JSON.stringify(value, null, 2)}\n`);
}

/** Runs `artifree type ...`. */
export async function runType(args: string[]): Promise<void> {
  const [command, ...rest] = args;
  if (command === undefined || command === '--help' || command === '-h') {
    if (rest.length > 0)
      throw new UsageError('--help must be used on its own.');
    process.stdout.write(TYPE_HELP);
    return;
  }
  switch (command) {
    case 'list':
      return list(rest);
    case 'show':
      return show(rest);
    default:
      throw new UsageError(
        `Unknown type command: ${command}. Use artifree type --help.`,
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
  if (positionals.length > 0)
    throw new UsageError('list takes no arguments. Use artifree type --help.');
  const client = await connect(values);
  print(await client.listTypes());
}

async function show(args: string[]): Promise<void> {
  const { values, positionals } = parseArgs({
    args,
    options: {
      ...connectionOptions,
      release: { type: 'string' },
      reference: { type: 'string' },
    },
    allowPositionals: true,
    strict: true,
  });
  const [id, ...others] = positionals;
  if (id === undefined || others.length > 0)
    throw new UsageError('Expected <id>. Use artifree type --help.');
  const release =
    values.release === undefined ? undefined : releaseNumber(values.release);
  const client = await connect(values);
  if (values.reference === undefined) {
    print(await client.getType(id, release === undefined ? {} : { release }));
    return;
  }
  // A reference belongs to one release; without --release it is the
  // current one's.
  const current = release ?? (await client.getType(id)).release.release;
  print(await client.getTypeReference(id, current, values.reference));
}

function releaseNumber(value: string): number {
  const number = Number(value);
  if (!/^[1-9][0-9]*$/.test(value) || !Number.isSafeInteger(number))
    throw new UsageError('--release must be a positive whole number.');
  return number;
}
