import type {
  CommitResponse,
  ConflictingFile,
  EditFilesRequest,
  Revision,
} from '@artifree/contracts';
import { decodeBase64 } from './source-part.js';
import type { ArtifreeClient, MutationOptions } from './client.js';
import { ArtifreeApiError } from './errors.js';
import {
  isCount,
  isRecord,
  list,
  parseCommit,
  revision as parseRevision,
} from './responses.js';

/** The most files one `file_conflict` lists (decision 0035). */
const MAX_LISTED_CONFLICTS = 32;

/**
 * A `file_conflict` answer with the files it names. `files` holds the first
 * ones in path order and `total` how many were not as expected; a file with
 * no `current_sha256` is not in the artifact's current revision.
 */
export class FileConflictError extends ArtifreeApiError {
  readonly files: readonly ConflictingFile[];
  readonly total: number;

  constructor(
    cause: ArtifreeApiError,
    conflicts: { files: ConflictingFile[]; total: number },
  ) {
    super(cause.message, {
      status: cause.status,
      code: cause.code,
      currentRevision: cause.currentRevision,
      currentMetadataRevision: cause.currentMetadataRevision,
      currentDataVersion: cause.currentDataVersion,
      retryAfterSeconds: cause.retryAfterSeconds,
      body: cause.body,
      cause,
    });
    this.name = 'FileConflictError';
    this.files = conflicts.files;
    this.total = conflicts.total;
  }
}

function conflictingFile(value: unknown): ConflictingFile | undefined {
  if (!isRecord(value)) return undefined;
  const { path, current_sha256 } = value;
  if (typeof path !== 'string') return undefined;
  if (current_sha256 === null) return { path, current_sha256 };
  return typeof current_sha256 === 'string' &&
    /^[0-9a-f]{64}$/.test(current_sha256)
    ? { path, current_sha256 }
    : undefined;
}

/**
 * The conflicts an error body states, or undefined when it states none or
 * states numbers that do not fit: more than 32 files, or fewer in all than
 * are listed.
 */
function conflictsOf(
  body: unknown,
): { files: ConflictingFile[]; total: number } | undefined {
  if (!isRecord(body) || !isRecord(body.error)) return undefined;
  const files = list(body.error.conflicting_files, conflictingFile);
  const total = body.error.conflicting_files_total;
  if (
    files === undefined ||
    files.length > MAX_LISTED_CONFLICTS ||
    !isCount(total) ||
    total < files.length
  )
    return undefined;
  return { files, total };
}

/**
 * One revision's manifest. Unlike `listRevisions` it does not read the whole
 * history. Like `editFiles` it is a function, to keep the client class out of
 * the web workspace's way.
 */
export async function readRevision(
  client: ArtifreeClient,
  id: string,
  revision: number,
): Promise<Revision> {
  return client.json(
    'GET',
    `/v1/artifacts/${encodeURIComponent(id)}/revisions/${revision}`,
    parseRevision,
  );
}

/**
 * Per-file conditional edits (decision 0035). This is a function, not a
 * method of the client class, because that class is in the web workspace's
 * main bundle, which has no room, and the workspace never edits this way.
 *
 * Commits the edits as the next revision if every edited file is, in the
 * artifact's current revision, as its `expect` says (`absent`, or the
 * SHA-256 in lowercase hexadecimal), whatever revision that is. Files no edit
 * names carry over as they are now. It is not a merge: a file is replaced
 * whole. When any file is not as expected nothing is written and it rejects
 * with code `file_conflict`: a `FileConflictError` that lists the files, with
 * `currentRevision`. `revision_conflict` is never the answer, and
 * `saveRevision` keeps its strict meaning.
 *
 * The answer is checked against what was sent: the new revision must follow
 * `base_revision`, list each put file with the size and digest of the bytes
 * sent, and not list a file that was deleted. An answer that disagrees is an
 * `invalid_response` and the edit may have been committed: retry with the
 * same key to get its receipt.
 */
export async function editFiles(
  client: ArtifreeClient,
  id: string,
  request: EditFilesRequest,
  options: MutationOptions,
): Promise<CommitResponse> {
  let response: CommitResponse;
  try {
    response = await client.json(
      'POST',
      `/v1/artifacts/${encodeURIComponent(id)}/edits`,
      parseCommit,
      { mutation: { body: request, idempotencyKey: options.idempotencyKey } },
    );
  } catch (error: unknown) {
    const conflicts =
      error instanceof ArtifreeApiError && error.code === 'file_conflict'
        ? conflictsOf(error.body)
        : undefined;
    if (error instanceof ArtifreeApiError && conflicts !== undefined)
      throw new FileConflictError(error, conflicts);
    throw error;
  }
  if (!(await answersEdits(request, response)))
    throw new ArtifreeApiError(
      'The answer to the edit does not match what was sent. It may have been committed; retry with the same key.',
      { status: 0, code: 'invalid_response' },
    );
  return response;
}

async function answersEdits(
  request: EditFilesRequest,
  response: CommitResponse,
): Promise<boolean> {
  if (response.revision.revision <= request.base_revision) return false;
  const listed = new Map(
    response.revision.files.map((file) => [file.path, file]),
  );
  for (const edit of request.edits) {
    const file = listed.get(edit.path);
    if (edit.op === 'delete') {
      if (file !== undefined) return false;
      continue;
    }
    const bytes =
      edit.encoding === 'base64'
        ? decodeBase64(edit.content)
        : new TextEncoder().encode(edit.content);
    if (bytes === undefined || file === undefined) return false;
    const digest = await crypto.subtle.digest('SHA-256', bytes);
    const hex = Array.from(new Uint8Array(digest), (byte) =>
      byte.toString(16).padStart(2, '0'),
    ).join('');
    if (file.size !== bytes.length || file.sha256 !== hex) return false;
  }
  return true;
}
