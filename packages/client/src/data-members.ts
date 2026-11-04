import type {
  DataMember,
  DataMembers,
  DataSelect,
  JsonValue,
} from '@artifree/contracts';
import type { ClientConfiguration, ReadOptions } from './client.js';
import {
  DATA_LIMITS,
  canonicalBytes,
  compareUtf8,
  isDataPath,
  hasRepeatedMember,
  isMemberName,
  nestingAllowedAt,
  pathDepth,
  storedJsonNodes,
  utf8Length,
} from './data-json.js';
import { checkArtifactId, getResponse } from './data-http.js';
import { ArtifreeApiError } from './errors.js';
import { isCount, isRecord } from './responses.js';

/** The query of {@link listDataMembers}; every part is optional. */
export interface DataMembersRequest {
  /** The collection's path; the root when absent or empty. */
  readonly path?: string;
  /** `names` (default) or `values`. */
  readonly select?: DataSelect;
  /** Most members, 1 to 100; 50 when absent. */
  readonly limit?: number;
  /** Only members whose name follows this one in UTF-8 byte order. */
  readonly after?: string;
}

function refused(message: string): ArtifreeApiError {
  return new ArtifreeApiError(message, { status: 0, code: 'invalid_request' });
}

function broken(message: string): ArtifreeApiError {
  return new ArtifreeApiError(message, {
    status: 200,
    code: 'invalid_response',
  });
}

interface Normalized {
  readonly path: string;
  readonly select: DataSelect;
  readonly limit: number;
  readonly after: string | undefined;
}

/** Checks every parameter before anything is sent. */
function normalize(request: DataMembersRequest): Normalized {
  const path = request.path ?? '';
  if (!isDataPath(path))
    throw refused('The path breaks the data path grammar.');
  const select = request.select ?? 'names';
  if (select !== 'names' && select !== 'values')
    throw refused('select must be names or values.');
  const limit = request.limit ?? DATA_LIMITS.defaultMembersLimit;
  if (
    !Number.isInteger(limit) ||
    limit < 1 ||
    limit > DATA_LIMITS.maxMembersLimit
  )
    throw refused(`limit must be from 1 to ${DATA_LIMITS.maxMembersLimit}.`);
  const { after } = request;
  if (
    after !== undefined &&
    (after === '' || utf8Length(after) > DATA_LIMITS.maxMemberNameBytes)
  )
    throw refused('after must be a member name of 1 to 256 bytes.');
  return { path, select, limit, after };
}

const MEMBER_KEYS = new Set(['name', 'value', 'too_large']);
const PAGE_KEYS = new Set([
  'artifact_id',
  'path',
  'select',
  'limit',
  'after',
  'data_version',
  'exists',
  'members',
  'next_after',
]);

function onlyKeys(
  record: Record<string, unknown>,
  allowed: Set<string>,
): boolean {
  return Object.keys(record).every((key) => allowed.has(key));
}

/** A member of a page and the bytes the server counts for it. */
interface Parsed {
  readonly member: DataMember;
  readonly bytes: number;
  /** Values the member holds, counting itself; 0 when it has no value. */
  readonly nodes: number;
}

function parseMember(
  value: unknown,
  query: Normalized,
  memberNesting: number,
  nodesLeft: number,
): Parsed | undefined {
  if (!isRecord(value) || !onlyKeys(value, MEMBER_KEYS)) return undefined;
  const { name } = value;
  if (typeof name !== 'string' || !isMemberName(name)) return undefined;
  const nameBytes = utf8Length(name);
  const hasValue = Object.hasOwn(value, 'value');
  const hasTooLarge = Object.hasOwn(value, 'too_large');
  if (query.select === 'names') {
    return hasValue || hasTooLarge
      ? undefined
      : { member: { name }, bytes: nameBytes, nodes: 0 };
  }
  // Under values exactly one of the two is present.
  if (hasValue === hasTooLarge) return undefined;
  // A member over its bound is announced, never sent, and costs its name only.
  if (hasTooLarge)
    return value.too_large === true
      ? { member: { name, too_large: true }, bytes: nameBytes, nodes: 0 }
      : undefined;
  const found: unknown = value.value;
  const nodes = storedJsonNodes(found, memberNesting, nodesLeft);
  if (nodes === undefined) return undefined;
  // Checked by storedJsonNodes above, which has no type guard form that also
  // returns the count.
  const stored = found as JsonValue;
  const valueBytes = canonicalBytes(stored);
  if (valueBytes > DATA_LIMITS.memberMaxBytes) return undefined;
  return {
    member: { name, value: stored },
    bytes: nameBytes + valueBytes,
    nodes,
  };
}

/**
 * Checks a response against the query that was sent: it echoes the
 * normalized query, the members are in strictly increasing UTF-8 byte order
 * after `after`, none is repeated, there are at most `limit`, the shapes fit
 * `select`, every value is JSON the server could have stored and within the
 * member bound (a larger one comes as `too_large`), the names and values of
 * the page together are within the page bound, and the cursor names the last
 * member. Sizes are the canonical JSON text's UTF-8 bytes ({@link canonicalBytes}).
 */
function parsePage(
  answer: unknown,
  artifactId: string,
  query: Normalized,
): DataMembers | undefined {
  if (!isRecord(answer) || !onlyKeys(answer, PAGE_KEYS)) return undefined;
  const { exists, data_version, next_after } = answer;
  if (
    answer.artifact_id !== artifactId ||
    answer.path !== query.path ||
    answer.select !== query.select ||
    answer.limit !== query.limit ||
    answer.after !== query.after ||
    !isCount(data_version) ||
    typeof exists !== 'boolean' ||
    !Array.isArray(answer.members)
  )
    return undefined;
  const raw: unknown[] = answer.members;
  if (raw.length > query.limit || (!exists && raw.length > 0)) return undefined;
  const nesting = nestingAllowedAt(pathDepth(query.path) + 1);
  const members: DataMember[] = [];
  let previous = query.after;
  let pageBytes = 0;
  // The values of the members and the containers on the way to them are all
  // values of one space (the root is not counted).
  let nodesLeft = DATA_LIMITS.maxSpaceValues - pathDepth(query.path);
  for (const item of raw) {
    const parsed = parseMember(item, query, nesting, nodesLeft);
    if (parsed === undefined) return undefined;
    const { member } = parsed;
    if (previous !== undefined && compareUtf8(member.name, previous) <= 0)
      return undefined;
    nodesLeft -= parsed.nodes;
    pageBytes += parsed.bytes;
    if (pageBytes > DATA_LIMITS.pageMaxBytes) return undefined;
    previous = member.name;
    members.push(member);
  }
  if (next_after !== undefined) {
    if (
      typeof next_after !== 'string' ||
      next_after !== previous ||
      members.length === 0
    )
      return undefined;
  }
  const page: DataMembers = {
    artifact_id: artifactId,
    path: query.path,
    select: query.select,
    limit: query.limit,
    data_version,
    exists,
    members,
  };
  if (query.after !== undefined) page.after = query.after;
  if (typeof next_after === 'string') page.next_after = next_after;
  return page;
}

/**
 * One page of the members of a collection of the caller's application data
 * (decision 0040). The request is validated before it is sent, so a bad path,
 * select, limit, or cursor rejects with `invalid_request` and no call is
 * made. The response must echo the normalized query and keep to the rules
 * above; anything else rejects with `invalid_response`. `data_version` is
 * the version of the data space, never a source revision.
 */
export async function listDataMembers(
  configuration: ClientConfiguration,
  artifactId: string,
  request: DataMembersRequest = {},
  options: ReadOptions = {},
): Promise<DataMembers> {
  checkArtifactId(artifactId);
  const query = normalize(request);
  const parts = new URLSearchParams();
  if (query.path !== '') parts.set('path', query.path);
  parts.set('select', query.select);
  parts.set('limit', String(query.limit));
  if (query.after !== undefined) parts.set('after', query.after);
  const response = await getResponse(
    configuration,
    `/v1/artifacts/${artifactId}/data/members?${parts.toString()}`,
    'application/json',
    options.signal,
  );
  let text: string;
  try {
    text = await response.text();
  } catch (error: unknown) {
    throw new ArtifreeApiError(
      'The connection ended while reading a response.',
      {
        status: 200,
        code: 'network_error',
        cause: error,
      },
    );
  }
  let answer: unknown;
  try {
    answer = JSON.parse(text);
  } catch {
    throw broken('The members response is not JSON.');
  }
  // JSON.parse keeps the last of two equal names; the domain stores neither.
  if (hasRepeatedMember(text))
    throw broken('The members response repeats a member name.');
  const page = parsePage(answer, artifactId, query);
  if (page === undefined)
    throw broken('The members response does not match the contract.');
  return page;
}
