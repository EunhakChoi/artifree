import { parseArgs } from 'node:util';
import { ArtifreeClient, newIdempotencyKey } from '@artifree/client';
import { mutate } from './artifact.js';
import { UsageError } from './errors.js';
import { resolveConnection } from './profiles.js';

export const CONTENT_HELP = `Usage:
  artifree content cleanup
  artifree content cleanup --apply [--base-digest <hex>] [--idempotency-key <key>]

Connection:
  --server <url>         API URL [env ARTIFREE_SERVER, ARTIFREE_URL, default http://127.0.0.1:7410]
  --token <val>          Access token [env ARTIFREE_TOKEN]
  --token-file <path>    Access token file [env ARTIFREE_TOKEN_FILE]
  --profile <name>       Connection profile to load
  --profile-file <path>  Path to profile configuration file [env ARTIFREE_PROFILES_FILE]

cleanup finds file content that no revision of any artifact refers to, the
trash included. Without --apply it removes nothing (a dry run): it prints
how many contents and bytes that is, and the digest that identifies the set.
With --apply it deletes exactly the set it reviewed: it reads the digest with
a dry run (or takes --base-digest from an earlier dry run) and sends it with
a new request key (or --idempotency-key). If the content is not that set any
more it deletes nothing and fails with content_conflict; run the dry run
again. It prints what the first run under the key deleted. A retry with the
same --idempotency-key and --base-digest prints the same numbers with
replayed: true, also when nothing is left. A purge already removes the content
it frees, so on a healthy server cleanup finds nothing. The database file does
not shrink; the space is reused. It needs the owner's access token: an issued
agent token is refused with 403 forbidden, the dry run too. Results are JSON
on stdout; the reviewed set and any retry advice go to stderr.
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
    options: {
      ...connectionOptions,
      apply: { type: 'boolean' },
      'base-digest': { type: 'string' },
      'idempotency-key': { type: 'string' },
    },
    allowPositionals: true,
    strict: true,
  });
  if (positionals.length > 0)
    throw new UsageError('cleanup takes no arguments. Use --apply to delete.');
  if (
    values.apply !== true &&
    (values['base-digest'] !== undefined ||
      values['idempotency-key'] !== undefined)
  )
    throw new UsageError(
      '--base-digest and --idempotency-key apply only with --apply.',
    );
  const { server, token } = await resolveConnection({
    server: values.server,
    token: values.token,
    tokenFile: values['token-file'],
    profile: values.profile,
    profileFile: values['profile-file'],
  });
  const client = new ArtifreeClient({ baseUrl: server, accessToken: token });
  if (values.apply !== true) {
    const found = await client.getUnreferencedContent();
    process.stdout.write(
      `${JSON.stringify({ applied: false, ...found }, null, 2)}\n`,
    );
    return;
  }
  let baseDigest = values['base-digest'];
  if (baseDigest === undefined) {
    const reviewed = await client.getUnreferencedContent();
    baseDigest = reviewed.digest;
    process.stderr.write(
      `Reviewed set ${baseDigest} (${reviewed.blobs} contents, ${reviewed.bytes} bytes). To retry an unknown outcome, pass --base-digest ${baseDigest} with the same --idempotency-key.\n`,
    );
  }
  const base = baseDigest;
  const report = await mutate(
    values['idempotency-key'] ?? newIdempotencyKey(),
    (idempotencyKey) =>
      client.removeUnreferencedContent(
        { base_digest: base },
        { idempotencyKey },
      ),
  );
  process.stdout.write(
    `${JSON.stringify({ applied: true, ...report }, null, 2)}\n`,
  );
}
