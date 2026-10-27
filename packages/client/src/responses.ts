import type {
  Artifact,
  ArtifactChange,
  ArtifactKind,
  ChangeKind,
  CommitResponse,
  ErrorBody,
  ErrorCode,
  Preview,
  Revision,
  RevisionFile,
} from '@artifree/contracts';

/**
 * Runtime checks for API responses. Generated types describe the contract;
 * these guards verify that a response actually follows it before callers
 * rely on it.
 */

type Fields = Readonly<Record<string, unknown>>;

function isRecord(value: unknown): value is Fields {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function isCount(value: unknown): value is number {
  return Number.isSafeInteger(value) && typeof value === 'number' && value >= 0;
}

function isTime(value: unknown): value is number {
  return Number.isSafeInteger(value);
}

const ARTIFACT_KINDS: readonly ArtifactKind[] = [
  'app',
  'document',
  'slides',
  'dashboard',
  'design',
  'motion',
];

// A record, not a list: adding a code to the contract fails type checking
// here until it is accepted, instead of turning it into invalid_response.
const ERROR_CODES: Record<ErrorCode, true> = {
  invalid_request: true,
  unauthenticated: true,
  not_found: true,
  revision_conflict: true,
  request_key_reused: true,
  payload_too_large: true,
  rate_limited: true,
  unavailable: true,
  internal: true,
};

const CHANGE_KINDS: readonly ChangeKind[] = ['created', 'revised'];

function artifactKind(value: unknown): ArtifactKind | undefined {
  return ARTIFACT_KINDS.find((kind) => kind === value);
}

function isErrorCode(value: string): value is ErrorCode {
  return Object.hasOwn(ERROR_CODES, value);
}

function errorCode(value: unknown): ErrorCode | undefined {
  return typeof value === 'string' && isErrorCode(value) ? value : undefined;
}

function artifact(value: unknown): Artifact | undefined {
  if (!isRecord(value)) return undefined;
  const kind = artifactKind(value.kind);
  const { id, title, current_revision, created_at_ms, updated_at_ms } = value;
  if (
    typeof id !== 'string' ||
    kind === undefined ||
    typeof title !== 'string' ||
    !isCount(current_revision) ||
    !isTime(created_at_ms) ||
    !isTime(updated_at_ms)
  )
    return undefined;
  return { id, kind, title, current_revision, created_at_ms, updated_at_ms };
}

function revisionFile(value: unknown): RevisionFile | undefined {
  if (!isRecord(value)) return undefined;
  const { path, size, media_type } = value;
  if (
    typeof path !== 'string' ||
    !isCount(size) ||
    typeof media_type !== 'string'
  )
    return undefined;
  return { path, size, media_type };
}

function revision(value: unknown): Revision | undefined {
  if (!isRecord(value) || !Array.isArray(value.files)) return undefined;
  const { revision: number, created_by, created_at_ms } = value;
  const files = list(value.files, revisionFile);
  if (
    !isCount(number) ||
    typeof created_by !== 'string' ||
    !isTime(created_at_ms) ||
    files === undefined
  )
    return undefined;
  return { revision: number, created_by, created_at_ms, files };
}

function list<T>(
  values: unknown,
  parse: (value: unknown) => T | undefined,
): T[] | undefined {
  if (!Array.isArray(values)) return undefined;
  const parsed: T[] = [];
  for (const value of values) {
    const item = parse(value);
    if (item === undefined) return undefined;
    parsed.push(item);
  }
  return parsed;
}

/** Parses `GET /v1/artifacts/{id}` and similar artifact bodies. */
export function parseArtifact(value: unknown): Artifact | undefined {
  return artifact(value);
}

/** Parses `GET /v1/artifacts`. */
export function parseArtifactList(value: unknown): Artifact[] | undefined {
  return isRecord(value) ? list(value.artifacts, artifact) : undefined;
}

/** Parses `GET /v1/artifacts/{id}/revisions`. */
export function parseRevisionList(value: unknown): Revision[] | undefined {
  return isRecord(value) ? list(value.revisions, revision) : undefined;
}

/** Parses the response of a create or revision commit. */
export function parseCommit(value: unknown): CommitResponse | undefined {
  if (!isRecord(value) || typeof value.replayed !== 'boolean') return undefined;
  const parsedArtifact = artifact(value.artifact);
  const parsedRevision = revision(value.revision);
  if (parsedArtifact === undefined || parsedRevision === undefined)
    return undefined;
  return {
    artifact: parsedArtifact,
    revision: parsedRevision,
    replayed: value.replayed,
  };
}

/** Parses the response of a preview request. */
export function parsePreview(value: unknown): Preview | undefined {
  if (!isRecord(value)) return undefined;
  const { url, revision: number, expires_at_ms } = value;
  if (typeof url !== 'string' || !isCount(number) || !isTime(expires_at_ms))
    return undefined;
  return { url, revision: number, expires_at_ms };
}

/**
 * A committed change of the feed. `change` is `unknown` for a kind added to
 * the contract after this client was built; read the artifact again, as for
 * any other change.
 */
export type FeedChange = Omit<ArtifactChange, 'change'> & {
  readonly change: ChangeKind | 'unknown';
};

/** Parses the data of a `change` event. */
export function parseArtifactChange(value: unknown): FeedChange | undefined {
  if (!isRecord(value)) return undefined;
  const { cursor, artifact_id, change, revision, committed_at_ms } = value;
  if (
    !isCount(cursor) ||
    typeof artifact_id !== 'string' ||
    typeof change !== 'string' ||
    change === '' ||
    !isCount(revision) ||
    !isTime(committed_at_ms)
  )
    return undefined;
  const kind = CHANGE_KINDS.find((known) => known === change) ?? 'unknown';
  return { cursor, artifact_id, change: kind, revision, committed_at_ms };
}

/** Parses the cursor of a `ready` or `reset` event. */
export function parseFeedPosition(value: unknown): number | undefined {
  return isRecord(value) && isCount(value.cursor) ? value.cursor : undefined;
}

/** Parses a non-2xx JSON body. */
export function parseError(value: unknown): ErrorBody | undefined {
  if (!isRecord(value) || !isRecord(value.error)) return undefined;
  const code = errorCode(value.error.code);
  const { message, current_revision } = value.error;
  if (code === undefined || typeof message !== 'string') return undefined;
  // The schema allows null as well as absence; both mean "not a conflict".
  if (current_revision === undefined || current_revision === null)
    return { code, message };
  if (!isCount(current_revision)) return undefined;
  return { code, message, current_revision };
}
