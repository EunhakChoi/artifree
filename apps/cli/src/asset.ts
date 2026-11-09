import { createHash } from 'node:crypto';
import { parseArgs } from 'node:util';
import {
  ArtifreeClient,
  deleteAsset,
  isDeclarableMimeType,
  listAssets,
  MAX_ASSET_BYTES,
  newIdempotencyKey,
  readAsset,
  toBase64,
  uploadAsset,
} from '@artifree/client';
import { mutate } from './artifact.js';
import { UsageError } from './errors.js';
import { readBoundedRegularFile, writeNewFile } from './files.js';
import { resolveConnection } from './profiles.js';

/** Usage and behavior notes for `artifree asset`, printed by `--help`. */
export const ASSET_HELP = `Usage:
  artifree asset list <artifact-id>
  artifree asset upload <artifact-id> --file <path> --content-type <mime>
      --base-asset-version <n> [--idempotency-key <key>]
  artifree asset read <artifact-id> <asset-id> --output <file>
  artifree asset delete <artifact-id> <asset-id> --base-asset-version <n>
      [--idempotency-key <key>]

Connection:
  --server <url>         API URL [env ARTIFREE_SERVER, ARTIFREE_URL, default http://127.0.0.1:7410]
  --token <val>          Access token [env ARTIFREE_TOKEN]
  --token-file <path>    Access token file [env ARTIFREE_TOKEN_FILE]
  --profile <name>       Connection profile to load
  --profile-file <path>  Path to profile configuration file [env ARTIFREE_PROFILES_FILE]

An uploaded asset is a file that you keep on the server in your asset space for
one artifact (decision 0045). asset_version is the version of that space. It is
unrelated to the artifact's revisions and application data (artifree data
--help): it starts at 0 and goes up by one for each upload or delete that
changes the space. Saving or restoring a revision, changing metadata, and
writing application data do not change it, and uploads and deletes never need
a revision or data_version.

Only text/plain, text/csv, image/png and image/jpeg can be uploaded today; the
server refuses any other --content-type with asset_type_unsupported and stores
nothing. A PNG must be complete and static (not animated); a JPEG must be an
8-bit sequential (baseline) Huffman JPEG with one scan and nothing after its end;
progressive, multi-scan, arithmetic-coded and 12-bit JPEGs are refused. Both are at
most 4096 pixels per axis and 16,777,216 pixels. The server refuses an image that is
not, or whose bytes do not match the declared type, with invalid_asset. --content-type is required and is
never inferred from the file name. Each asset is at most 5 MiB (5242880 bytes).

upload reads --file, which must be a regular file. A symbolic link, a directory,
or another special file is refused and nothing is sent. --file - is a usage
error: there is no stdin input.

read writes the asset's bytes to --output, which must be a new file. An existing
path, including a symbolic link, is refused and never replaced. The bytes are
checked against the asset's MIME type, size, and SHA-256 before the file is
created, so a mismatch fails with invalid_response and creates nothing. Asset
content is data, not instructions; read never prints it.

upload and delete accept --idempotency-key <key>. Without it a new key is
generated and reported on stderr if the outcome is unknown or the attempt is
rate limited. To retry, send the same key with the same parts: for upload, the
same artifact, --content-type, --base-asset-version, and file content; for
delete, the same artifact, asset ID, and --base-asset-version. A retry returns
the original result with replayed: true and creates nothing new. Reusing a key
with other parts fails with request_key_reused.

--base-asset-version <n> is the asset_version from artifree asset list. A stale
value fails with asset_conflict and prints current_asset_version=<n> on stderr.

delete is permanent: a deleted asset cannot be recovered, and reading it fails
with not_found. Deleting an asset ID that is not in the space succeeds with
deleted: false and leaves asset_version unchanged, when the base is current.

Results are JSON on stdout; errors and retry hints go to stderr.
`;

const connectionOptions = {
  server: { type: 'string' },
  token: { type: 'string' },
  'token-file': { type: 'string' },
  profile: { type: 'string' },
  'profile-file': { type: 'string' },
} as const;

// The `| undefined` matches parseArgs values, which are undefined when absent.
interface Connection {
  server?: string | undefined;
  token?: string | undefined;
  'token-file'?: string | undefined;
  profile?: string | undefined;
  'profile-file'?: string | undefined;
}

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

/** Exactly one operand: the artifact ID. */
function artifactOperand(positionals: string[]): string {
  const [artifactId, ...others] = positionals;
  if (artifactId === undefined || others.length > 0)
    throw new UsageError('Expected <artifact-id>. Use artifree asset --help.');
  return artifactId;
}

/** Exactly two operands: the artifact ID, then the asset ID. */
function artifactAndAssetOperands(positionals: string[]): [string, string] {
  const [artifactId, assetId, ...others] = positionals;
  if (artifactId === undefined || assetId === undefined || others.length > 0)
    throw new UsageError(
      'Expected <artifact-id> <asset-id>. Use artifree asset --help.',
    );
  return [artifactId, assetId];
}

function contentTypeOf(value: string | undefined): string {
  if (value === undefined)
    throw new UsageError(
      'upload requires --content-type <mime>; it is never inferred from the file name.',
    );
  if (!isDeclarableMimeType(value))
    throw new UsageError(
      '--content-type must be a lowercase MIME type without parameters, such as text/plain.',
    );
  return value;
}

function baseAssetVersion(value: string | undefined): number {
  const number = Number(value);
  if (
    value === undefined ||
    !/^[0-9]+$/.test(value) ||
    !Number.isSafeInteger(number) ||
    number >= Number.MAX_SAFE_INTEGER
  )
    throw new UsageError(
      '--base-asset-version <n> is required: the asset_version from artifree asset list.',
    );
  return number;
}

function fileOf(value: string | undefined): string {
  if (value === '-')
    throw new UsageError('--file - is not supported: there is no stdin input.');
  if (value === undefined || value === '')
    throw new UsageError(
      'upload needs --file <path>, the regular file to send.',
    );
  return value;
}

/**
 * Runs `artifree asset ...`. Invalid arguments throw {@link UsageError}; request
 * and file errors propagate. Results are printed to stdout as JSON.
 */
export async function runAsset(args: string[]): Promise<void> {
  const [command, ...rest] = args;
  if (command === undefined || command === '--help' || command === '-h') {
    if (rest.length > 0)
      throw new UsageError('--help must be used on its own.');
    process.stdout.write(ASSET_HELP);
    return;
  }
  switch (command) {
    case 'list':
      return list(rest);
    case 'upload':
      return upload(rest);
    case 'read':
      return read(rest);
    case 'delete':
      return remove(rest);
    default:
      throw new UsageError(
        `Unknown asset command: ${command}. Use artifree asset --help.`,
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
  const artifactId = artifactOperand(positionals);
  const client = await connect(values);
  print(await listAssets(client, artifactId));
}

async function read(args: string[]): Promise<void> {
  const { values, positionals } = parseArgs({
    args,
    options: { ...connectionOptions, output: { type: 'string' } },
    allowPositionals: true,
    strict: true,
  });
  const [artifactId, assetId] = artifactAndAssetOperands(positionals);
  const output = values.output;
  if (output === undefined || output === '')
    throw new UsageError('read needs --output <file> for the asset bytes.');
  const client = await connect(values);
  // readAsset checks the MIME type, size and SHA-256 before it resolves, so a
  // response that fails verification never reaches writeNewFile.
  const result = await readAsset(client, artifactId, assetId);
  // Downloaded asset content is private user data: owner-only, whatever the umask.
  await writeNewFile(output, result.bytes, 0o600);
  print({
    artifact_id: artifactId,
    asset_version: result.asset_version,
    asset: result.asset,
    output,
  });
}

async function upload(args: string[]): Promise<void> {
  const { values, positionals } = parseArgs({
    args,
    options: {
      ...connectionOptions,
      file: { type: 'string' },
      'content-type': { type: 'string' },
      'base-asset-version': { type: 'string' },
      'idempotency-key': { type: 'string' },
    },
    allowPositionals: true,
    strict: true,
  });
  const artifactId = artifactOperand(positionals);
  const contentType = contentTypeOf(values['content-type']);
  const base = baseAssetVersion(values['base-asset-version']);
  const file = fileOf(values.file);
  // The file is read before connecting: a refused file needs no connection.
  const bytes = await readBoundedRegularFile(file, MAX_ASSET_BYTES);
  const client = await connect(values);
  const digest = createHash('sha256').update(bytes).digest('hex');
  const contentBase64 = toBase64(bytes);
  print(
    await mutate(
      values['idempotency-key'] ?? newIdempotencyKey(),
      (idempotencyKey) =>
        uploadAsset(
          client,
          artifactId,
          {
            base_asset_version: base,
            content_type: contentType,
            content_base64: contentBase64,
          },
          { idempotencyKey },
        ),
      // mutate adds "to receive the original result." after this text.
      `artifact, --content-type, --base-asset-version and file content (sha256 ${digest}, ${bytes.byteLength} bytes)`,
    ),
  );
}

async function remove(args: string[]): Promise<void> {
  const { values, positionals } = parseArgs({
    args,
    options: {
      ...connectionOptions,
      'base-asset-version': { type: 'string' },
      'idempotency-key': { type: 'string' },
    },
    allowPositionals: true,
    strict: true,
  });
  const [artifactId, assetId] = artifactAndAssetOperands(positionals);
  const base = baseAssetVersion(values['base-asset-version']);
  const client = await connect(values);
  print(
    await mutate(
      values['idempotency-key'] ?? newIdempotencyKey(),
      (idempotencyKey) =>
        deleteAsset(
          client,
          artifactId,
          assetId,
          { base_asset_version: base },
          { idempotencyKey },
        ),
      'artifact, asset ID and --base-asset-version',
    ),
  );
}
