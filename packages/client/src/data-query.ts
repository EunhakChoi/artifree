import type {
  DataMember,
  DataMembers,
  DataSelect,
  JsonValue,
} from '@artifree/contracts';
import type { ArtifreeClient, ReadOptions } from './client.js';
import { ArtifreeApiError } from './errors.js';
import { isCount, isRecord } from './responses.js';

/**
 * Member queries over application data (decision 0040). A function, not a
 * method of the client class: that class is in the web workspace's main
 * bundle, which has no room, and the workspace does not query data.
 */

/** Most members the server lists in one page. */
export const MAX_PAGE_MEMBERS = 100;
/** Members in a page when the query gives no limit. */
export const DEFAULT_PAGE_MEMBERS = 25;
/** Longest member name, prefix, or cursor, in UTF-8 bytes. */
export const MAX_MEMBER_NAME_BYTES = 256;

/** What to list: the members of the object at `path`. */
export interface DataMembersRequest {
  /** `/`-separated segments; absent or empty is the root. */
  readonly path?: string;
  /** Only members whose name starts with this (at most 256 bytes). */
  readonly prefix?: string;
  /** `names` (default) or `values`. */
  readonly select?: DataSelect;
  /** 1 to 100; default 25. */
  readonly limit?: number;
  /** A member name: list only names that sort after it, by UTF-8 bytes. */
  readonly after?: string;
}

/** The request after defaults: what the server must echo. */
interface Expected {
  readonly artifactId: string;
  readonly path: string;
  readonly prefix: string;
  readonly select: DataSelect;
  readonly limit: number;
  readonly after: string | undefined;
}

const encoder = new TextEncoder();

function byteLength(text: string): number {
  return encoder.encode(text).length;
}

/** Compares by UTF-8 bytes, the order the server lists in. */
export function compareUtf8(left: string, right: string): number {
  const a = encoder.encode(left);
  const b = encoder.encode(right);
  const common = Math.min(a.length, b.length);
  for (let index = 0; index < common; index += 1) {
    const difference = (a[index] ?? 0) - (b[index] ?? 0);
    if (difference !== 0) return difference;
  }
  return a.length - b.length;
}

function invalidRequest(message: string): ArtifreeApiError {
  return new ArtifreeApiError(message, { status: 0, code: 'invalid_request' });
}

function isJsonValue(value: unknown): value is JsonValue {
  return (
    value === null ||
    typeof value === 'string' ||
    typeof value === 'boolean' ||
    (typeof value === 'number' && Number.isFinite(value)) ||
    Array.isArray(value) ||
    isRecord(value)
  );
}

function isSelect(value: unknown): value is DataSelect {
  return value === 'names' || value === 'values';
}

function parseMember(
  value: unknown,
  select: DataSelect,
): DataMember | undefined {
  if (!isRecord(value) || typeof value.name !== 'string') return undefined;
  const { name } = value;
  const size = byteLength(name);
  if (size < 1 || size > MAX_MEMBER_NAME_BYTES) return undefined;
  const keys = Object.keys(value);
  if (select === 'names') return keys.length === 1 ? { name } : undefined;
  // Under `values` a member has exactly one of `value` and `too_large`.
  if (keys.length !== 2) return undefined;
  if (Object.hasOwn(value, 'too_large'))
    return value.too_large === true ? { name, too_large: true } : undefined;
  if (!Object.hasOwn(value, 'value')) return undefined;
  const carried: unknown = value.value;
  return isJsonValue(carried) ? { name, value: carried } : undefined;
}

/**
 * Validates a members response against the request that asked for it: the
 * echoed query is the request after defaults, members are strictly ordered by
 * their UTF-8 bytes, begin with the prefix, come after the cursor, are no
 * more than the limit, and have exactly the fields the selection allows;
 * `next_after` names the last member of a non-empty page.
 */
export function parseDataMembers(
  value: unknown,
  expected: Expected,
): DataMembers | undefined {
  if (!isRecord(value)) return undefined;
  const { artifact_id, query, data_version, collection, members, next_after } =
    value;
  if (
    artifact_id !== expected.artifactId ||
    !isCount(data_version) ||
    typeof collection !== 'boolean' ||
    !Array.isArray(members) ||
    !isRecord(query)
  )
    return undefined;
  if (
    query.path !== expected.path ||
    query.prefix !== expected.prefix ||
    query.select !== expected.select ||
    query.limit !== expected.limit ||
    query.after !== expected.after ||
    Object.keys(query).length !== (expected.after === undefined ? 4 : 5)
  )
    return undefined;
  if (members.length > expected.limit || (!collection && members.length > 0))
    return undefined;
  const parsed: DataMember[] = [];
  let previous = expected.after;
  for (const raw of members as unknown[]) {
    const member = parseMember(raw, expected.select);
    if (
      member === undefined ||
      !member.name.startsWith(expected.prefix) ||
      (previous !== undefined && compareUtf8(member.name, previous) <= 0)
    )
      return undefined;
    parsed.push(member);
    previous = member.name;
  }
  const last = parsed.at(-1);
  if (Object.hasOwn(value, 'next_after')) {
    if (typeof next_after !== 'string' || last?.name !== next_after)
      return undefined;
  }
  const page: DataMembers = {
    artifact_id: expected.artifactId,
    query: {
      path: expected.path,
      prefix: expected.prefix,
      select: expected.select,
      limit: expected.limit,
      ...(expected.after === undefined ? {} : { after: expected.after }),
    },
    data_version,
    collection,
    members: parsed,
  };
  if (typeof next_after === 'string') page.next_after = next_after;
  return page;
}

/**
 * One page of the members of the object at `request.path`, ordered by the
 * UTF-8 bytes of their names and read from one snapshot of the data space.
 * Pass the answer's `next_after` as `after`, with the other fields unchanged,
 * for the next page; a cursor is a name, so writes between pages never repeat
 * or skip a member that was not written. `data_version` is the version of the
 * data space the page was read at, not a source revision. A path with nothing
 * at it, or a value that is not an object, answers `collection: false`.
 *
 * Rejects with `invalid_request` and without sending for a request that
 * breaks the limits above; with `invalid_response` when the answer does not
 * match the request; and with `not_found` for an artifact that is missing,
 * in the trash, purged, or someone else's.
 */
export async function listDataMembers(
  client: ArtifreeClient,
  artifactId: string,
  request: DataMembersRequest = {},
  options: ReadOptions = {},
): Promise<DataMembers> {
  const limit = request.limit ?? DEFAULT_PAGE_MEMBERS;
  if (!Number.isInteger(limit) || limit < 1 || limit > MAX_PAGE_MEMBERS)
    throw invalidRequest(
      `limit is a whole number from 1 to ${MAX_PAGE_MEMBERS}.`,
    );
  const select = request.select ?? 'names';
  if (!isSelect(select)) throw invalidRequest('select is "names" or "values".');
  const prefix = request.prefix ?? '';
  if (byteLength(prefix) > MAX_MEMBER_NAME_BYTES)
    throw invalidRequest(`prefix is at most ${MAX_MEMBER_NAME_BYTES} bytes.`);
  const { after } = request;
  if (
    after !== undefined &&
    (after === '' || byteLength(after) > MAX_MEMBER_NAME_BYTES)
  )
    throw invalidRequest(
      `after is a member name of 1 to ${MAX_MEMBER_NAME_BYTES} bytes.`,
    );
  const path = request.path ?? '';
  const parameters = new URLSearchParams();
  if (path !== '') parameters.set('path', path);
  if (prefix !== '') parameters.set('prefix', prefix);
  parameters.set('select', select);
  parameters.set('limit', String(limit));
  if (after !== undefined) parameters.set('after', after);
  const expected: Expected = {
    artifactId,
    path,
    prefix,
    select,
    limit,
    after,
  };
  return client.json(
    'GET',
    `/v1/artifacts/${encodeURIComponent(artifactId)}/data/members?${parameters.toString()}`,
    (value) => parseDataMembers(value, expected),
    { signal: options.signal },
  );
}
