import type {
  DataMember,
  DataMemberSelect,
  DataMembers,
  JsonValue,
} from '@artifree/contracts';
import type { ArtifreeClient } from './client.js';
import { ArtifreeApiError } from './errors.js';
import { isCount, isJsonValue, isRecord } from './responses.js';

/**
 * Member pages of application data (decision 0040). This module is loaded on
 * demand and is not part of the client class: the question is validated here
 * before anything is sent, and the answer is checked against what was asked.
 */

/**
 * The grammar and bounds the server enforces. A test compares each number with
 * the constant of the same meaning in the server's source.
 */
export const DATA_LIMITS = {
  /** Most segments of a path. */
  maxPathSegments: 16,
  /** Most bytes of a path segment. */
  maxSegmentBytes: 64,
  /** Most bytes of the name of a member. */
  maxMemberNameBytes: 256,
  /** Most members of a page. */
  maxMemberLimit: 100,
  /** Members of a page when the query gives no limit. */
  defaultMemberLimit: 50,
  /** Largest canonical value a listed member carries. */
  memberValueBytes: 32 * 1024,
  /** Most bytes of names and values one page lists. */
  memberPageBytes: 256 * 1024,
  /** Largest canonical value a subscription sends whole. */
  watchedValueBytes: 128 * 1024,
} as const;

/**
 * The client measures a value with `JSON.stringify`, which spells a few
 * numbers longer than the canonical form the server counts (`0.00001` against
 * `1e-5`). It therefore refuses a member or a page only beyond twice the
 * server's bound: that keeps an unbounded answer out, and leaves the exact
 * accounting to the server's tests.
 */
const SPELLING_SLACK = 2;

const SEGMENT = /^[A-Za-z0-9_-]+$/;
const ARTIFACT_ID = /^art_[0-9a-f]{32}$/;
const LONE_SURROGATE =
  /[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/;
const encoder = new TextEncoder();

function invalidRequest(message: string): ArtifreeApiError {
  return new ArtifreeApiError(message, { status: 0, code: 'invalid_request' });
}

function invalidResponse(message: string): ArtifreeApiError {
  return new ArtifreeApiError(message, {
    status: 200,
    code: 'invalid_response',
  });
}

/** Refuses what the server would not take as the identifier of an artifact. */
export function checkArtifactId(artifactId: string): void {
  if (!ARTIFACT_ID.test(artifactId))
    throw invalidRequest(
      'An artifact identifier is art_ followed by 32 lowercase hexadecimal digits.',
    );
}

/**
 * Refuses a path that breaks the grammar of application data: no segment or
 * 1 to 16 of them, each 1 to 64 ASCII letters, digits, `_`, or `-`, joined by
 * single slashes. The empty path is the root.
 */
export function checkDataPath(path: string): void {
  if (path === '') return;
  const segments = path.split('/');
  if (
    segments.length > DATA_LIMITS.maxPathSegments ||
    !segments.every(
      (segment) =>
        segment.length <= DATA_LIMITS.maxSegmentBytes && SEGMENT.test(segment),
    )
  )
    throw invalidRequest(
      'A data path is up to 16 segments of 1 to 64 ASCII letters, digits, _ or -, joined by single slashes.',
    );
}

/** The UTF-8 bytes of `text`, which must be well formed. */
function utf8(text: string): Uint8Array {
  return encoder.encode(text);
}

/** Compares as the server orders names: by their UTF-8 bytes. */
export function compareUtf8(left: string, right: string): number {
  const a = utf8(left);
  const b = utf8(right);
  const shared = Math.min(a.length, b.length);
  for (let index = 0; index < shared; index += 1) {
    const difference = (a[index] ?? 0) - (b[index] ?? 0);
    if (difference !== 0) return difference;
  }
  return a.length - b.length;
}

/** What to list and from where; every part is optional. */
export interface DataMembersQuery {
  /** The object's path; the root when absent. */
  readonly path?: string;
  /** `names` when absent. */
  readonly select?: DataMemberSelect;
  /** Members per page, 1 to 100; 50 when absent. */
  readonly limit?: number;
  /** A member name: only the members after it are listed. */
  readonly after?: string;
}

/** The query as it is sent and as the answer must echo it. */
interface Normalized {
  readonly path: string;
  readonly select: DataMemberSelect;
  readonly limit: number;
  readonly after: string | undefined;
}

function normalize(query: DataMembersQuery): Normalized {
  const path = query.path ?? '';
  checkDataPath(path);
  const select = query.select ?? 'names';
  if (select !== 'names' && select !== 'values')
    throw invalidRequest('select is names or values.');
  const limit = query.limit ?? DATA_LIMITS.defaultMemberLimit;
  if (
    !Number.isInteger(limit) ||
    limit < 1 ||
    limit > DATA_LIMITS.maxMemberLimit
  )
    throw invalidRequest(
      `limit is a whole number from 1 to ${DATA_LIMITS.maxMemberLimit}.`,
    );
  const { after } = query;
  if (
    after !== undefined &&
    (typeof after !== 'string' ||
      LONE_SURROGATE.test(after) ||
      utf8(after).length < 1 ||
      utf8(after).length > DATA_LIMITS.maxMemberNameBytes)
  )
    throw invalidRequest('after is a member name of 1 to 256 bytes.');
  return { path, select, limit, after };
}

const PAGE_FIELDS = new Set([
  'artifact_id',
  'path',
  'select',
  'limit',
  'after',
  'data_version',
  'exists',
  'collection',
  'members',
  'next_after',
]);
const MEMBER_FIELDS = new Set(['name', 'value', 'too_large']);

function onlyFields(
  value: Record<string, unknown>,
  allowed: Set<string>,
): boolean {
  return Object.keys(value).every((key) => allowed.has(key));
}

function jsonBytes(value: JsonValue): number {
  return utf8(JSON.stringify(value)).length;
}

function parseMember(
  value: unknown,
  select: DataMemberSelect,
): { member: DataMember; bytes: number } | undefined {
  if (!isRecord(value) || !onlyFields(value, MEMBER_FIELDS)) return undefined;
  const { name } = value;
  if (
    typeof name !== 'string' ||
    LONE_SURROGATE.test(name) ||
    utf8(name).length < 1 ||
    utf8(name).length > DATA_LIMITS.maxMemberNameBytes
  )
    return undefined;
  const hasValue = Object.hasOwn(value, 'value');
  const hasTooLarge = Object.hasOwn(value, 'too_large');
  const nameBytes = utf8(name).length;
  if (select === 'names')
    return hasValue || hasTooLarge
      ? undefined
      : { member: { name }, bytes: nameBytes };
  // Exactly one of the two.
  if (hasValue === hasTooLarge) return undefined;
  if (hasTooLarge)
    return value.too_large === true
      ? { member: { name, too_large: true }, bytes: nameBytes }
      : undefined;
  const found: unknown = value.value;
  if (!isJsonValue(found)) return undefined;
  const size = jsonBytes(found);
  if (size > DATA_LIMITS.memberValueBytes * SPELLING_SLACK) return undefined;
  return { member: { name, value: found }, bytes: nameBytes + size };
}

/**
 * Checks an answer against the question: the echoed query, the artifact, the
 * order of the names by their UTF-8 bytes, the bounds of a page, the shape of
 * each member, and the agreement of `exists`, `collection`, and `next_after`.
 * Returns `undefined` for an answer that breaks any of it.
 */
function parsePage(
  value: unknown,
  artifactId: string,
  sent: Normalized,
): DataMembers | undefined {
  if (!isRecord(value) || !onlyFields(value, PAGE_FIELDS)) return undefined;
  const {
    artifact_id,
    path,
    select,
    limit,
    after,
    data_version,
    exists,
    collection,
    members,
    next_after,
  } = value;
  if (
    artifact_id !== artifactId ||
    path !== sent.path ||
    select !== sent.select ||
    limit !== sent.limit ||
    (Object.hasOwn(value, 'after') ? after : undefined) !== sent.after ||
    !isCount(data_version) ||
    typeof exists !== 'boolean' ||
    typeof collection !== 'boolean' ||
    !Array.isArray(members) ||
    (sent.path === '' && !(exists && collection)) ||
    (collection && !exists)
  )
    return undefined;
  const page: DataMember[] = [];
  let bytes = 0;
  let previous = sent.after;
  for (const item of members as unknown[]) {
    const parsed = parseMember(item, sent.select);
    if (parsed === undefined) return undefined;
    // Strictly after the cursor and after the member before.
    if (
      previous !== undefined &&
      compareUtf8(previous, parsed.member.name) >= 0
    )
      return undefined;
    previous = parsed.member.name;
    bytes += parsed.bytes;
    page.push(parsed.member);
  }
  // A page is bounded.
  if (
    page.length > sent.limit ||
    bytes > DATA_LIMITS.memberPageBytes * SPELLING_SLACK ||
    (!collection && page.length > 0)
  )
    return undefined;
  const more = Object.hasOwn(value, 'next_after');
  const last = page[page.length - 1];
  if (more && (typeof next_after !== 'string' || next_after !== last?.name))
    return undefined;
  // What the answer echoed is what was sent, as checked above.
  const result: DataMembers = {
    artifact_id: artifactId,
    path: sent.path,
    select: sent.select,
    limit: sent.limit,
    data_version,
    exists,
    collection,
    members: page,
  };
  if (sent.after !== undefined) result.after = sent.after;
  if (more) result.next_after = next_after as string;
  return result;
}

/** Options of {@link listDataMembers}. */
export interface ListDataMembersOptions {
  /** Cancels the request. */
  readonly signal?: AbortSignal;
}

/**
 * One page of the members of the object at `query.path` in the caller's data
 * space of an artifact, in the order of their names as UTF-8 bytes. Pass
 * `next_after` back as `after`, with the same other parameters, for the next
 * page. A path with nothing at it, or a value that is not an object, is a
 * successful answer with `collection: false` and no members.
 *
 * The query is checked here against the server's grammar before anything is
 * sent, and rejects with `invalid_request`; the answer is checked against the
 * query and rejects with `invalid_response` when it differs. Rejects with
 * `not_found` for an artifact that is missing, in the trash, purged, or
 * someone else's.
 */
export async function listDataMembers(
  client: ArtifreeClient,
  artifactId: string,
  query: DataMembersQuery = {},
  options: ListDataMembersOptions = {},
): Promise<DataMembers> {
  checkArtifactId(artifactId);
  const sent = normalize(query);
  const parameters = new URLSearchParams();
  if (sent.path !== '') parameters.set('path', sent.path);
  parameters.set('select', sent.select);
  parameters.set('limit', String(sent.limit));
  if (sent.after !== undefined) parameters.set('after', sent.after);
  const uri = `/v1/artifacts/${artifactId}/data/members?${parameters.toString()}`;
  const answer = await client.json(
    'GET',
    uri,
    (value): DataMembers | undefined => parsePage(value, artifactId, sent),
    { signal: options.signal },
  );
  // `json` reports an answer the parser refused as invalid; this is a guard
  // for the type only.
  if (answer === undefined) throw invalidResponse('No page.');
  return answer;
}
