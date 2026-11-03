import { parseArgs } from 'node:util';
import { createUser, listUsers, setUserDisabled } from '@artifree/client';
import { UsageError } from './errors.js';
import { connect, connectionOptions } from './tokens.js';

export const USER_HELP = `Usage:
  artifree user list [--json]
  artifree user create <label> [--json]
  artifree user disable <id> [--json]
  artifree user enable <id> [--json]

Connection:
  --server <url>         API URL [env ARTIFREE_SERVER, ARTIFREE_URL, default http://127.0.0.1:7410]
  --token <val>          Access token [env ARTIFREE_TOKEN]
  --token-file <path>    Access token file [env ARTIFREE_TOKEN_FILE]
  --profile <name>       Connection profile to load
  --profile-file <path>  Path to profile configuration file [env ARTIFREE_PROFILES_FILE]

Every command needs the owner profile's bootstrap token; a token issued to any
user is refused. A user owns its own artifacts and sees no one else's. There
are no roles and no sharing, and a user holds no credential until a token is
issued for it: artifree token create <name> --user <id>.

Subcommands:
  list     List the registered users, the owner first.
  create   Register a user with a display label (1-64 characters, not another
           user's). Prints its id.
  disable  Refuse the user's tokens from now on and close its open change
           streams, runtime connections, preview links, and rooms.
           The owner profile cannot be disabled.
  enable   Let the user's tokens that are not revoked authenticate again.
           What disable closed stays closed.
`;

export async function runUser(args: string[]): Promise<void> {
  const [subcommand, ...rest] = args;
  if (
    subcommand === undefined ||
    subcommand === '--help' ||
    subcommand === '-h'
  ) {
    process.stdout.write(USER_HELP);
    return;
  }
  const { values, positionals } = parseArgs({
    args: rest,
    options: {
      ...connectionOptions,
      json: { type: 'boolean', default: false },
    },
    allowPositionals: true,
    strict: true,
  });
  switch (subcommand) {
    case 'list': {
      const client = await connect(values);
      const result = await listUsers(client);
      if (values.json) {
        console.log(JSON.stringify(result, null, 2));
        return;
      }
      for (const user of result.users) {
        const state =
          user.disabled_at === undefined || user.disabled_at === null
            ? ''
            : ` (disabled ${user.disabled_at})`;
        const owner = user.is_owner ? ' [owner]' : '';
        console.log(`${user.id.padEnd(36)} ${user.label}${owner}${state}`);
      }
      return;
    }
    case 'create': {
      const label = positionals[0];
      if (label === undefined) {
        throw new UsageError('artifree user create requires a label.');
      }
      const client = await connect(values);
      const user = await createUser(client, { label });
      if (values.json) {
        console.log(JSON.stringify(user, null, 2));
        return;
      }
      console.log(`Created user ${user.id} (${user.label})`);
      console.log(
        `It has no token yet: artifree token create <name> --user ${user.id}`,
      );
      return;
    }
    case 'disable':
    case 'enable': {
      const id = positionals[0];
      if (id === undefined) {
        throw new UsageError(`artifree user ${subcommand} requires a user id.`);
      }
      const client = await connect(values);
      const user = await setUserDisabled(client, id, subcommand === 'disable');
      if (values.json) {
        console.log(JSON.stringify(user, null, 2));
        return;
      }
      console.log(
        `${subcommand === 'disable' ? 'Disabled' : 'Enabled'} user ${user.id}`,
      );
      return;
    }
    default:
      throw new UsageError(`Unknown user command: ${subcommand}. Use --help.`);
  }
}
