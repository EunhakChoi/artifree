import { parseArgs } from 'node:util';
import { ArtifreeClient } from '@artifree/client';
import { UsageError } from './errors.js';
import { resolveConnection } from './profiles.js';

export const TOKEN_HELP = `Usage:
  artifree token list [--json]
  artifree token create <name> [--json]
  artifree token rotate <id> [--json]
  artifree token revoke <id>

Connection:
  --server <url>         API URL [env ARTIFREE_SERVER, ARTIFREE_URL, default http://127.0.0.1:7410]
  --token <val>          Access token [env ARTIFREE_TOKEN]
  --token-file <path>    Access token file [env ARTIFREE_TOKEN_FILE]
  --profile <name>       Connection profile to load
  --profile-file <path>  Path to profile configuration file [env ARTIFREE_PROFILES_FILE]

Subcommands:
  list    List all access tokens (bootstrap owner and agents).
  create  Create a new agent token with the given name.
          Prints the full secret token once; it is never shown again.
  rotate  Rotate an existing agent token by ID.
          Prints the new secret token once; it is never shown again.
  revoke  Revoke an agent token by ID.
`;

const connectionOptions = {
  server: { type: 'string' },
  token: { type: 'string' },
  'token-file': { type: 'string' },
  profile: { type: 'string' },
  'profile-file': { type: 'string' },
} as const;

async function connect(values: {
  server?: string | undefined;
  token?: string | undefined;
  'token-file'?: string | undefined;
  profile?: string | undefined;
  'profile-file'?: string | undefined;
}): Promise<ArtifreeClient> {
  const { server, token } = await resolveConnection({
    server: values.server,
    token: values.token,
    tokenFile: values['token-file'],
    profile: values.profile,
    profileFile: values['profile-file'],
  });
  return new ArtifreeClient({ baseUrl: server, accessToken: token });
}

export async function runToken(args: string[]): Promise<void> {
  const [subcommand, ...rest] = args;
  if (
    subcommand === undefined ||
    subcommand === '--help' ||
    subcommand === '-h'
  ) {
    process.stdout.write(TOKEN_HELP);
    return;
  }

  switch (subcommand) {
    case 'list': {
      const { values } = parseArgs({
        args: rest,
        options: {
          ...connectionOptions,
          json: { type: 'boolean', default: false },
        },
        strict: true,
      });
      const client = await connect(values);
      const list = await client.listTokens();
      if (values.json) {
        console.log(JSON.stringify(list, null, 2));
        return;
      }
      for (const t of list.tokens) {
        const adminTag = t.is_admin ? ' [admin]' : '';
        const status = t.revoked_at ? ` (revoked ${t.revoked_at})` : '';
        console.log(
          `${t.id.padEnd(20)} ${t.name.padEnd(24)} ${t.prefix.padEnd(16)}${adminTag}${status}`,
        );
      }
      return;
    }
    case 'create': {
      const { values, positionals } = parseArgs({
        args: rest,
        options: {
          ...connectionOptions,
          json: { type: 'boolean', default: false },
        },
        allowPositionals: true,
        strict: true,
      });
      const name = positionals[0];
      if (!name) {
        throw new UsageError('artifree token create requires a token name.');
      }
      const client = await connect(values);
      const result = await client.createToken({ name });
      if (values.json) {
        console.log(JSON.stringify(result, null, 2));
        return;
      }
      console.log(`Created token ${result.token.id} (${result.token.name})`);
      console.log(`Secret: ${result.secret}`);
      console.log('Store this token safely; it will not be displayed again.');
      return;
    }
    case 'rotate': {
      const { values, positionals } = parseArgs({
        args: rest,
        options: {
          ...connectionOptions,
          json: { type: 'boolean', default: false },
        },
        allowPositionals: true,
        strict: true,
      });
      const id = positionals[0];
      if (!id) {
        throw new UsageError('artifree token rotate requires a token ID.');
      }
      const client = await connect(values);
      const result = await client.rotateToken(id);
      if (values.json) {
        console.log(JSON.stringify(result, null, 2));
        return;
      }
      console.log(`Rotated token ${result.token.id} (${result.token.name})`);
      console.log(`New secret: ${result.secret}`);
      console.log('Store this token safely; it will not be displayed again.');
      return;
    }
    case 'revoke': {
      const { values, positionals } = parseArgs({
        args: rest,
        options: {
          ...connectionOptions,
        },
        allowPositionals: true,
        strict: true,
      });
      const id = positionals[0];
      if (!id) {
        throw new UsageError('artifree token revoke requires a token ID.');
      }
      const client = await connect(values);
      await client.revokeToken(id);
      console.log(`Revoked token ${id}`);
      return;
    }
    default:
      throw new UsageError(`Unknown token command: ${subcommand}. Use --help.`);
  }
}
