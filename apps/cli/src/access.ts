import { parseArgs } from 'node:util';
import { readAccess, readMe } from '@artifree/client';
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

Lists the owner and the members of an artifact, each member with a role
(editor or viewer), and the access version. The owner and the members may
list it; for anyone else the artifact does not exist. Only the owner is told
which members are disabled. No command grants, changes, or revokes a role yet.

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
    // `access --help` and `access list --help`, and nothing else beside it.
    const own = subcommand === 'list' ? rest : args;
    if (own.length > 1) throw new UsageError('--help must be used on its own.');
    process.stdout.write(ACCESS_HELP);
    return;
  }
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
