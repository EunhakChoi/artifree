import { parseArgs } from 'node:util';
import { ArtifreeClient, newIdempotencyKey } from '@artifree/client';
import type { ListJobsQuery } from '@artifree/contracts';
import { mutate } from './artifact.js';
import { UsageError } from './errors.js';
import { resolveConnection } from './profiles.js';

export const JOB_HELP = `Usage:
  artifree job enqueue <artifact-id> --revision <n> [--idempotency-key <key>]
  artifree job show <job-id>
  artifree job list [--artifact <id>] [--limit <n>] [--page-token <token>]
  artifree job cancel <job-id> [--idempotency-key <key>]

Connection:
  --server <url>         API URL [env ARTIFREE_SERVER, ARTIFREE_URL, default http://127.0.0.1:7410]
  --token <val>          Access token [env ARTIFREE_TOKEN]
  --token-file <path>    Access token file [env ARTIFREE_TOKEN_FILE]
  --profile <name>       Connection profile to load
  --profile-file <path>  Path to profile configuration file [env ARTIFREE_PROFILES_FILE]

enqueue queues a verification job for one revision of an artifact: a worker
re-reads every file of that revision and compares each SHA-256 digest and size
with the stored listing. It only queues the job. The server's own worker runs it
(artifree-server serve starts one unless started with --no-worker or as a
one-shot session), and nothing tells you when it ends: run show again. A job
that succeeded with verdict mismatch found a difference; the job itself worked.
The job lists at most 50 mismatched package paths and the full mismatch_count.
Jobs run trusted verification code only; they do not run the artifact's code and
are not a sandbox.
cancel asks a job to stop. A job that has not started ends cancelled at once.
A running one shows cancel_requested until its worker stops at its next
checkpoint. Cancelling stops work and undoes nothing. A job that already ended
is refused with 409 job_ended. At most 16 jobs may wait or run at once
(409 too_many_jobs). list prints one page, newest first, of the jobs of one
artifact (--artifact) or of every artifact outside the trash; next_page_token
is present when more follow. enqueue and cancel accept --idempotency-key; a
retry with the same key returns the job as it is now with replayed: true.
Results are JSON on stdout.
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

function count(value: string, flag: string, minimum: number): number {
  const number = Number(value);
  if (
    !/^[0-9]+$/.test(value) ||
    !Number.isSafeInteger(number) ||
    number < minimum
  )
    throw new UsageError(
      `${flag} must be a whole number of at least ${minimum}.`,
    );
  return number;
}

function print(value: unknown): void {
  process.stdout.write(`${JSON.stringify(value, null, 2)}\n`);
}

/** Runs `artifree job ...`. */
export async function runJob(args: string[]): Promise<void> {
  const [command, ...rest] = args;
  if (command === undefined || command === '--help' || command === '-h') {
    if (rest.length > 0)
      throw new UsageError('--help must be used on its own.');
    process.stdout.write(JOB_HELP);
    return;
  }
  switch (command) {
    case 'enqueue':
      return enqueue(rest);
    case 'show':
      return show(rest);
    case 'list':
      return list(rest);
    case 'cancel':
      return cancel(rest);
    default:
      throw new UsageError(
        `Unknown job command: ${command}. Use artifree job --help.`,
      );
  }
}

function one(positionals: string[], name: string): string {
  const [first, ...others] = positionals;
  if (first === undefined || others.length > 0)
    throw new UsageError(`Expected <${name}>. Use artifree job --help.`);
  return first;
}

async function enqueue(args: string[]): Promise<void> {
  const { values, positionals } = parseArgs({
    args,
    options: {
      ...connectionOptions,
      revision: { type: 'string' },
      'idempotency-key': { type: 'string' },
    },
    allowPositionals: true,
    strict: true,
  });
  const artifactId = one(positionals, 'artifact-id');
  if (values.revision === undefined)
    throw new UsageError(
      'enqueue requires --revision <n>; read current_revision with artifree artifact show.',
    );
  const revision = count(values.revision, '--revision', 1);
  const client = await connect(values);
  print(
    await mutate(
      values['idempotency-key'] ?? newIdempotencyKey(),
      (idempotencyKey) =>
        client.enqueueJob(
          artifactId,
          { kind: 'verify_revision', revision },
          { idempotencyKey },
        ),
    ),
  );
}

async function show(args: string[]): Promise<void> {
  const { values, positionals } = parseArgs({
    args,
    options: { ...connectionOptions },
    allowPositionals: true,
    strict: true,
  });
  const id = one(positionals, 'job-id');
  print(await (await connect(values)).getJob(id));
}

async function list(args: string[]): Promise<void> {
  const { values, positionals } = parseArgs({
    args,
    options: {
      ...connectionOptions,
      artifact: { type: 'string' },
      limit: { type: 'string' },
      'page-token': { type: 'string' },
    },
    allowPositionals: true,
    strict: true,
  });
  if (positionals.length > 0)
    throw new UsageError('list takes no arguments. Use --artifact <id>.');
  const query: ListJobsQuery = {};
  if (values.limit !== undefined)
    query.limit = count(values.limit, '--limit', 1);
  if (values['page-token'] !== undefined)
    query.page_token = values['page-token'];
  const client = await connect(values);
  print(
    await client.listJobs(
      query,
      values.artifact === undefined ? {} : { artifactId: values.artifact },
    ),
  );
}

async function cancel(args: string[]): Promise<void> {
  const { values, positionals } = parseArgs({
    args,
    options: { ...connectionOptions, 'idempotency-key': { type: 'string' } },
    allowPositionals: true,
    strict: true,
  });
  const id = one(positionals, 'job-id');
  const client = await connect(values);
  print(
    await mutate(
      values['idempotency-key'] ?? newIdempotencyKey(),
      (idempotencyKey) => client.cancelJob(id, { idempotencyKey }),
    ),
  );
}
