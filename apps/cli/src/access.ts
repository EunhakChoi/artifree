import { parseArgs } from 'node:util';
import {
  newIdempotencyKey,
  readAccess,
  readMe,
  revokeAccess,
  setAccess,
} from '@artifree/client';
import { mutate } from './mutate.js';
import { UsageError } from './errors.js';
import { connect, connectionOptions } from './tokens.js';

const CONNECTION = `Connection:
  --server <url>         API URL [env ARTIFREE_SERVER, ARTIFREE_URL, default http://127.0.0.1:7410]
  --token <val>          Access token [env ARTIFREE_TOKEN]
  --token-file <path>    Access token file [env ARTIFREE_TOKEN_FILE]
  --profile <name>       Connection profile to load
  --profile-file <path>  Path to profile configuration file [env ARTIFREE_PROFILES_FILE]
`;

export const ME_HELP = `Usage:
  artifree me [--json]

Prints the id and label of the user the token belongs to. Hand the id to an
owner who wants to share an artifact with you.

${CONNECTION}`;

export const ACCESS_HELP = `Usage:
  artifree artifact access list <id> [--json]
  artifree artifact access grant <id> <user-id> --role editor|viewer --base <n>
      [--idempotency-key <key>]
  artifree artifact access revoke <id> <user-id> --base <n>
      [--idempotency-key <key>]

list prints the owner and the members of an artifact, each member with a
role (editor or viewer), and the access version. The owner and the members may
list it; for anyone else the artifact does not exist. Only the owner is told
which members are disabled.

grant makes a user a member with a role, or changes the role they have;
revoke ends their membership. Only the owner may, with their own token: a
token issued to the owner profile is an agent's and is refused. --base is the
access_version that list printed; another version is access_conflict, and
current_access_version names the version it is at. A user who is not
registered, is disabled, or is the owner is one invalid_request. Without
--idempotency-key a new key is made; retry with the same key and the same
request to learn whether an earlier attempt committed. Revoking ends what the
user holds of the artifact; a change to viewer ends their source writes.

${CONNECTION}`;

function options(args: string[]) {
  return parseArgs({
    args,
    options: {
      ...connectionOptions,
      json: { type: 'boolean', default: false },
    },
    allowPositionals: true,
    strict: true,
  });
}

/** Whether `args` ask for help, wherever the flag stands. */
function isHelp(args: string[]): boolean {
  return args.includes('--help') || args.includes('-h');
}

/** Runs `artifree me`. */
export async function runMe(args: string[]): Promise<void> {
  if (isHelp(args)) {
    if (args.length > 1)
      throw new UsageError('--help must be used on its own.');
    process.stdout.write(ME_HELP);
    return;
  }
  const { values, positionals } = options(args);
  if (positionals.length > 0)
    throw new UsageError('artifree me takes no arguments.');
  const me = await readMe(await connect(values));
  if (values.json) console.log(JSON.stringify(me, null, 2));
  else console.log(`${me.id} ${me.label}`);
}

/** Runs `artifree artifact access ...`. */
export async function runAccess(args: string[]): Promise<void> {
  const [subcommand, ...rest] = args;
  if (subcommand === undefined || isHelp(args)) {
    // `access --help` and `access <command> --help`, and nothing else.
    const own = ['list', 'grant', 'revoke'].includes(subcommand ?? '')
      ? rest
      : args;
    if (own.length > 1) throw new UsageError('--help must be used on its own.');
    process.stdout.write(ACCESS_HELP);
    return;
  }
  if (subcommand === 'grant' || subcommand === 'revoke')
    return change(subcommand, rest);
  if (subcommand !== 'list')
    throw new UsageError(
      `Unknown access command: ${subcommand}. Use artifree artifact access --help.`,
    );
  const { values, positionals } = options(rest);
  const [id, ...extra] = positionals;
  if (id === undefined || extra.length > 0)
    throw new UsageError(
      'artifree artifact access list requires one artifact id.',
    );
  const access = await readAccess(await connect(values), id);
  if (values.json) {
    console.log(JSON.stringify(access, null, 2));
    return;
  }
  console.log(`${access.owner.id.padEnd(36)} owner   ${access.owner.label}`);
  for (const member of access.members) {
    const state = member.disabled === true ? ' (disabled)' : '';
    console.log(
      `${member.id.padEnd(36)} ${member.role.padEnd(7)} ${member.label}${state}`,
    );
  }
  console.log(`access_version=${access.access_version}`);
}

function baseVersion(value: string | undefined): number {
  const number = Number(value);
  if (
    value === undefined ||
    !/^[0-9]+$/.test(value) ||
    !Number.isSafeInteger(number)
  )
    throw new UsageError(
      '--base must be the access_version that artifree artifact access list printed.',
    );
  return number;
}

/** Runs `artifree artifact access grant|revoke`. */
async function change(
  command: 'grant' | 'revoke',
  args: string[],
): Promise<void> {
  const { values, positionals } = parseArgs({
    args,
    options: {
      ...connectionOptions,
      role: { type: 'string' },
      base: { type: 'string' },
      'idempotency-key': { type: 'string' },
    },
    allowPositionals: true,
    strict: true,
  });
  const [id, user, ...extra] = positionals;
  if (id === undefined || user === undefined || extra.length > 0)
    throw new UsageError(
      `artifree artifact access ${command} requires an artifact id and a user id.`,
    );
  const role = values.role;
  if (command === 'grant' && role !== 'editor' && role !== 'viewer')
    throw new UsageError('grant requires --role editor or --role viewer.');
  if (command === 'revoke' && role !== undefined)
    throw new UsageError('revoke takes no --role.');
  const base = baseVersion(values.base);
  const client = await connect(values);
  const result = await mutate(
    values['idempotency-key'] ?? newIdempotencyKey(),
    (idempotencyKey) =>
      role === 'editor' || role === 'viewer'
        ? setAccess(
            client,
            id,
            user,
            { role, baseAccessVersion: base },
            { idempotencyKey },
          )
        : revokeAccess(
            client,
            id,
            user,
            { baseAccessVersion: base },
            { idempotencyKey },
          ),
    'user, role and --base',
  );
  console.log(JSON.stringify(result, null, 2));
}
