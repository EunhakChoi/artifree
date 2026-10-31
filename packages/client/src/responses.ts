import type {
  Artifact,
  ArtifactKind,
  ArtifactList,
  ChangeKind,
  CommitResponse,
  CopiedPart,
  DuplicateArtifactResponse,
  ErrorBody,
  ErrorCode,
  FeedPosition,
  Preview,
  Revision,
  RevisionFile,
  RuntimeConnection,
  RuntimeMetadata,
  RuntimeGrant,
  RuntimeView,
  UpdateArtifactResponse,
} from '@artifree/contracts';
import type { FeedChange } from './change-feed.js';

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
  runtime_session_expired: true,
  revision_conflict: true,
  metadata_conflict: true,
  request_key_reused: true,
  payload_too_large: true,
  range_not_satisfiable: true,
  rate_limited: true,
  unavailable: true,
  internal: true,
};

// A record for the same reason; a kind added later still reaches callers,
// as 'unknown'.
const CHANGE_KINDS: Record<ChangeKind, true> = {
  created: true,
  revised: true,
  metadata_updated: true,
  deleted: true,
  recovered: true,
};

// A record for the same reason as the error codes: a part added later must
// be accepted here before a copy is reported to callers.
const COPIED_PARTS: Record<CopiedPart, true> = { files: true };

function isCopiedPart(value: unknown): value is CopiedPart {
  return typeof value === 'string' && Object.hasOwn(COPIED_PARTS, value);
}

/**
 * An opaque feed cursor. It is sent back in `Last-Event-ID`, so it must be
 * a visible ASCII header value.
 */
function isCursor(value: unknown): value is string {
  return typeof value === 'string' && /^[\x21-\x7e]{1,256}$/.test(value);
}

function isChangeKind(value: string): value is ChangeKind {
  return Object.hasOwn(CHANGE_KINDS, value);
}

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
  const {
    id,
    title,
    pinned,
    current_revision,
    metadata_revision,
    deleted_at_ms,
    created_at_ms,
    updated_at_ms,
  } = value;
  if (
    typeof id !== 'string' ||
    kind === undefined ||
    typeof title !== 'string' ||
    typeof pinned !== 'boolean' ||
    !isCount(current_revision) ||
    !isCount(metadata_revision) ||
    // Present on every artifact: `null` outside the trash.
    (deleted_at_ms !== null && !isTime(deleted_at_ms)) ||
    !isTime(created_at_ms) ||
    !isTime(updated_at_ms)
  )
    return undefined;
  return {
    id,
    kind,
    title,
    pinned,
    current_revision,
    metadata_revision,
    deleted_at_ms,
    created_at_ms,
    updated_at_ms,
  };
}

function revisionFile(value: unknown): RevisionFile | undefined {
  if (!isRecord(value)) return undefined;
  const { path, size, sha256, media_type } = value;
  if (
    typeof path !== 'string' ||
    !isCount(size) ||
    typeof sha256 !== 'string' ||
    !/^[0-9a-f]{64}$/.test(sha256) ||
    typeof media_type !== 'string'
  )
    return undefined;
  return { path, size, sha256, media_type };
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
export function parseArtifactList(value: unknown): ArtifactList | undefined {
  if (!isRecord(value)) return undefined;
  const artifacts = list(value.artifacts, artifact);
  const { next_page_token } = value;
  if (artifacts === undefined) return undefined;
  if (next_page_token === undefined || next_page_token === null)
    return { artifacts };
  return typeof next_page_token === 'string'
    ? { artifacts, next_page_token }
    : undefined;
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

/** Parses the response of a duplicate. */
export function parseDuplicate(
  value: unknown,
): DuplicateArtifactResponse | undefined {
  const commit = parseCommit(value);
  const copied = isRecord(value)
    ? list(value.copied, (part) => (isCopiedPart(part) ? part : undefined))
    : undefined;
  if (commit === undefined || copied === undefined) return undefined;
  return { ...commit, copied };
}

/** Parses the response of a metadata update. */
export function parseUpdate(
  value: unknown,
): UpdateArtifactResponse | undefined {
  if (!isRecord(value) || typeof value.replayed !== 'boolean') return undefined;
  const parsed = artifact(value.artifact);
  return parsed === undefined
    ? undefined
    : { artifact: parsed, replayed: value.replayed };
}

/** Parses the response of a preview request. */
export function parsePreview(value: unknown): Preview | undefined {
  if (!isRecord(value)) return undefined;
  const { url, revision: number, expires_at_ms } = value;
  if (typeof url !== 'string' || !isCount(number) || !isTime(expires_at_ms))
    return undefined;
  return { url, revision: number, expires_at_ms };
}

/** Validates a runtime view before its values are used for frame binding. */
export function parseRuntimeConnection(
  value: unknown,
): RuntimeConnection | undefined {
  return isRecord(value) &&
    typeof value.session_id === 'string' &&
    /^[0-9a-f]{32}$/.test(value.session_id)
    ? { session_id: value.session_id }
    : undefined;
}

/** Validates a runtime view before its values are used for frame binding. */
export function parseRuntimeView(value: unknown): RuntimeView | undefined {
  if (!isRecord(value)) return undefined;
  const {
    url,
    enabled,
    session_id,
    artifact_id,
    revision,
    head_revision,
    sdk,
    declared,
    supported,
    granted,
    grant_version,
    view_id,
    generation,
    initialization,
    expires_at_ms,
    lease_ms,
  } = value;
  if (
    typeof url !== 'string' ||
    typeof enabled !== 'boolean' ||
    typeof session_id !== 'string' ||
    !/^[0-9a-f]{32}$/.test(session_id) ||
    typeof artifact_id !== 'string' ||
    !isCount(revision) ||
    revision === 0 ||
    !isCount(head_revision) ||
    head_revision < revision ||
    !(sdk === null || typeof sdk === 'string') ||
    (enabled ? sdk !== 'artifree.app.v1' : sdk !== null) ||
    !Array.isArray(declared) ||
    !declared.every((name: unknown) => typeof name === 'string') ||
    !Array.isArray(supported) ||
    !supported.every((name: unknown) => typeof name === 'string') ||
    !Array.isArray(granted) ||
    !granted.every(
      (name: unknown) =>
        typeof name === 'string' &&
        supported.includes(name) &&
        declared.includes(name),
    ) ||
    !isCount(grant_version) ||
    (granted.length > 0 && grant_version === 0) ||
    typeof view_id !== 'string' ||
    !/^[0-9a-f]{32}$/.test(view_id) ||
    !isCount(generation) ||
    generation === 0 ||
    typeof initialization !== 'string' ||
    !/^[0-9a-f]{64}$/.test(initialization) ||
    !isTime(expires_at_ms) ||
    !isCount(lease_ms) ||
    lease_ms > 2_147_483_647
  )
    return undefined;
  try {
    const address = new URL(url);
    if (!['http:', 'https:'].includes(address.protocol)) return undefined;
  } catch {
    return undefined;
  }
  return {
    url,
    enabled,
    session_id,
    artifact_id,
    revision,
    head_revision,
    sdk,
    declared,
    supported,
    granted,
    grant_version,
    view_id,
    generation,
    initialization,
    expires_at_ms,
    lease_ms,
  };
}

/** Validates a grant's version before it can guard another change. */
export function parseRuntimeGrant(value: unknown): RuntimeGrant | undefined {
  return isRecord(value) && isCount(value.version)
    ? { version: value.version }
    : undefined;
}

/** Validates metadata returned for a runtime view. */
export function parseRuntimeMetadata(
  value: unknown,
): RuntimeMetadata | undefined {
  if (!isRecord(value)) return undefined;
  const { artifact_id, title, current_revision, metadata_revision } = value;
  if (
    typeof artifact_id !== 'string' ||
    typeof title !== 'string' ||
    !isCount(current_revision) ||
    current_revision === 0 ||
    !isCount(metadata_revision) ||
    metadata_revision === 0
  )
    return undefined;
  return { artifact_id, title, current_revision, metadata_revision };
}

/** Parses the data of a `ready` or `reset` feed event. */
export function parseFeedPosition(value: unknown): FeedPosition | undefined {
  return isRecord(value) && isCursor(value.cursor)
    ? { cursor: value.cursor }
    : undefined;
}

/** Parses the data of a `change` feed event. */
export function parseFeedChange(value: unknown): FeedChange | undefined {
  if (!isRecord(value)) return undefined;
  const {
    cursor,
    artifact_id,
    change,
    revision: number,
    metadata_revision,
    committed_at_ms,
  } = value;
  if (
    !isCursor(cursor) ||
    typeof artifact_id !== 'string' ||
    typeof change !== 'string' ||
    !isCount(number) ||
    !isCount(metadata_revision) ||
    !isTime(committed_at_ms)
  )
    return undefined;
  return {
    cursor,
    artifact_id,
    change: isChangeKind(change) ? change : 'unknown',
    revision: number,
    metadata_revision,
    committed_at_ms,
  };
}

/** Parses a non-2xx JSON body. */
export function parseError(value: unknown): ErrorBody | undefined {
  if (!isRecord(value) || !isRecord(value.error)) return undefined;
  const code = errorCode(value.error.code);
  const { message, current_revision, current_metadata_revision } = value.error;
  if (code === undefined || typeof message !== 'string') return undefined;
  const body: ErrorBody = { code, message };
  // The schema allows null as well as absence; both mean "not a conflict".
  for (const [name, count] of [
    ['current_revision', current_revision],
    ['current_metadata_revision', current_metadata_revision],
  ] as const) {
    if (count === undefined || count === null) continue;
    if (!isCount(count)) return undefined;
    body[name] = count;
  }
  return body;
}
