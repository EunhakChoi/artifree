import { parseArgs } from 'node:util';
import { ArtifreeClient } from '@artifree/client';
import { UsageError } from './errors.js';
import { resolveConnection } from './profiles.js';

export const CONTENT_HELP = `Usage:
  artifree content cleanup [--apply]

Connection:
  --server <url>         API URL [env ARTIFREE_SERVER, ARTIFREE_URL, default http://127.0.0.1:7410]
  --token <val>          Access token [env ARTIFREE_TOKEN]
  --token-file <path>    Access token file [env ARTIFREE_TOKEN_FILE]
  --profile <name>       Connection profile to load
  --profile-file <path>  Path to profile configuration file [env ARTIFREE_PROFILES_FILE]

cleanup finds file content that no revision of any artifact refers to, the
trash included, and prints how many contents and bytes it is. Without --apply
it removes nothing (a dry run). With --apply it deletes that content and
prints what it deleted; the server decides again when it deletes, so the
numbers can differ from an earlier dry run. A purge already removes the
content it frees, so on a healthy server cleanup finds nothing. The database
file does not shrink; the space is reused. It needs the owner's access token:
an issued agent token is refused with 403 forbidden, the dry run too. Results
are JSON on stdout.
`;

const connectionOptions = {
  server: { type: 'string' },
  token: { type: 'string' },
  'token-file': { type: 'string' },
  profile: { type: 'string' },
  'profile-file': { type: 'string' },
} as const;

/** Runs `artifree content ...`. */
export async function runContent(args: string[]): Promise<void> {
  const [command, ...rest] = args;
  if (command === undefined || command === '--help' || command === '-h') {
    if (rest.length > 0)
      throw new UsageError('--help must be used on its own.');
    process.stdout.write(CONTENT_HELP);
    return;
  }
  if (command !== 'cleanup')
    throw new UsageError(
      `Unknown content command: ${command}. Use artifree content --help.`,
    );
  const { values, positionals } = parseArgs({
    args: rest,
    options: { ...connectionOptions, apply: { type: 'boolean' } },
    allowPositionals: true,
    strict: true,
  });
  if (positionals.length > 0)
    throw new UsageError('cleanup takes no arguments. Use --apply to delete.');
  const { server, token } = await resolveConnection({
    server: values.server,
    token: values.token,
    tokenFile: values['token-file'],
    profile: values.profile,
    profileFile: values['profile-file'],
  });
  const client = new ArtifreeClient({ baseUrl: server, accessToken: token });
  const applied = values.apply === true;
  const report = applied
    ? await client.removeUnreferencedContent()
    : await client.getUnreferencedContent();
  process.stdout.write(`${JSON.stringify({ applied, ...report }, null, 2)}\n`);
}
