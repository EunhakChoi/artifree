import type { DataMember, DataMembers, DataSelect } from '@artifree/contracts';
import type { ArtifreeClient } from './client.js';
import { ArtifreeApiError } from './errors.js';
import {
  MAX_MEMBER_NAME_BYTES,
  MEMBERS_DEFAULT_LIMIT,
  MEMBERS_MAX_LIMIT,
  breach,
  checkValue,
  compareUtf8,
  cursorProblem,
  dataPathProblem,
  exactKeys,
  isObject,
  refusal,
  utf8Length,
  versionField,
} from './data-grammar.js';

/** What to list; each field has the server's default when absent. */
export interface ListDataMembersQuery {
  /** The collection; the empty path (default) is the root. */
  readonly path?: string;
  /** `names` (default) or `values`. */
  readonly select?: DataSelect;
  /** Members in the page, a whole number from 1 to 100 (default 50). */
  readonly limit?: number;
  /** The previous page's `next_after`. */
  readonly after?: string;
}

/**
 * The most bytes of a page this client reads: 256 KiB of values, up to 100
 * names of 256 bytes each, and their JSON framing, with room to spare.
 */
const MAX_PAGE_BYTES = 1024 * 1024;

/** The query as it is sent, with every default applied. */
interface Sent {
  readonly path: string;
  readonly select: DataSelect;
  readonly limit: number;
  readonly after: string | undefined;
}

function checkQuery(query: ListDataMembersQuery): Sent {
  const path = query.path ?? '';
  const problem = dataPathProblem(path);
  if (problem !== undefined) throw refusal(`path: ${problem}`);
  const select = query.select ?? 'names';
  if (select !== 'names' && select !== 'values')
    throw refusal('select: names or values');
  const limit = query.limit ?? MEMBERS_DEFAULT_LIMIT;
  if (!Number.isSafeInteger(limit) || limit < 1 || limit > MEMBERS_MAX_LIMIT)
    throw refusal(`limit: a whole number from 1 to ${MEMBERS_MAX_LIMIT}`);
  if (query.after !== undefined) {
    const cursor = cursorProblem(query.after);
    if (cursor !== undefined) throw refusal(`after: ${cursor}`);
  }
  return { path, select, limit, after: query.after };
}

/** The body of `response`, at most `MAX_PAGE_BYTES` of it. */
async function boundedText(response: Response): Promise<string> {
  const declared = Number(response.headers.get('content-length'));
  if (Number.isFinite(declared) && declared > MAX_PAGE_BYTES) {
    await response.body?.cancel().catch(() => undefined);
    throw breach(`A page of members is larger than ${MAX_PAGE_BYTES} bytes.`);
  }
  const reader = response.body?.getReader();
  if (reader === undefined) throw breach('A page of members has no body.');
  const chunks: Uint8Array[] = [];
  let total = 0;
  try {
    for (;;) {
      const chunk = await reader.read();
      if (chunk.done) break;
      total += chunk.value.length;
      if (total > MAX_PAGE_BYTES)
        throw breach(
          `A page of members is larger than ${MAX_PAGE_BYTES} bytes.`,
        );
      chunks.push(chunk.value);
    }
  } catch (error: unknown) {
    if (error instanceof ArtifreeApiError) throw error;
    throw new ArtifreeApiError('The connection ended while reading a page.', {
      status: response.status,
      code: 'network_error',
      cause: error,
    });
  } finally {
    await reader.cancel().catch(() => undefined);
  }
  const bytes = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.length;
  }
  try {
    return new TextDecoder('utf-8', { fatal: true }).decode(bytes);
  } catch {
    throw breach('A page of members is not valid UTF-8.');
  }
}

function checkMember(value: unknown, select: DataSelect): DataMember {
  if (!isObject(value)) throw breach('A member is not an object.');
  const { name } = value;
  if (typeof name !== 'string') throw breach('A member has no name.');
  const problem = cursorProblem(name);
  if (problem !== undefined)
    throw breach(`A member name is out of range: ${problem}`);
  if (select === 'names') {
    exactKeys(value, ['name'], [], 'A member of a names page');
    return { name };
  }
  exactKeys(value, ['name'], ['value', 'too_large'], 'A member');
  const hasValue = Object.hasOwn(value, 'value');
  const hasLarge = Object.hasOwn(value, 'too_large');
  if (hasValue === hasLarge)
    throw breach(
      'A member of a values page has exactly one of value and too_large.',
    );
  if (hasLarge) {
    if (value.too_large !== true)
      throw breach('too_large, when present, is true.');
    return { name, too_large: true };
  }
  const { value: found } = value;
  checkValue(found, 'A member value');
  return { name, value: found };
}

/** Checks a page against what was sent; every difference is a breach. */
function parseMembersPage(
  value: unknown,
  artifactId: string,
  sent: Sent,
): DataMembers {
  if (!isObject(value)) throw breach('A page of members is not an object.');
  exactKeys(
    value,
    [
      'artifact_id',
      'path',
      'select',
      'limit',
      'data_version',
      'is_collection',
      'members',
    ],
    ['after', 'next_after'],
    'A page of members',
  );
  if (value.artifact_id !== artifactId)
    throw breach('The page is for another artifact.');
  if (value.path !== sent.path) throw breach('The page echoes another path.');
  if (value.select !== sent.select)
    throw breach('The page echoes another select.');
  if (value.limit !== sent.limit)
    throw breach('The page echoes another limit.');
  if (
    Object.hasOwn(value, 'after')
      ? value.after !== sent.after
      : sent.after !== undefined
  )
    throw breach('The page echoes another cursor.');
  const dataVersion = versionField(value.data_version, 'data_version');
  if (typeof value.is_collection !== 'boolean')
    throw breach('is_collection is not a boolean.');
  const { members: listed } = value;
  if (!Array.isArray(listed)) throw breach('members is not an array.');
  if (listed.length > sent.limit)
    throw breach('The page has more members than its limit.');
  const members: DataMember[] = [];
  for (const entry of listed as unknown[]) {
    const member = checkMember(entry, sent.select);
    const before = members.at(-1)?.name ?? sent.after;
    if (before !== undefined && compareUtf8(before, member.name) >= 0)
      throw breach(
        'The members are not in strictly increasing order of their UTF-8 bytes after the cursor.',
      );
    members.push(member);
  }
  if (!value.is_collection && members.length > 0)
    throw breach('A path that is not a collection has members.');
  const result: DataMembers = {
    artifact_id: artifactId,
    path: sent.path,
    select: sent.select,
    limit: sent.limit,
    data_version: dataVersion,
    is_collection: value.is_collection,
    members,
  };
  if (sent.after !== undefined) result.after = sent.after;
  if (Object.hasOwn(value, 'next_after')) {
    const { next_after: next } = value;
    if (typeof next !== 'string' || next !== members.at(-1)?.name)
      throw breach('next_after is not the name of the last member.');
    if (utf8Length(next) > MAX_MEMBER_NAME_BYTES)
      throw breach('next_after is too long.');
    result.next_after = next;
  }
  return result;
}

/**
 * One page of the members of a collection of your application data
 * (decision 0040), in the byte order of their UTF-8 names. Every parameter is
 * checked against the server's grammar before anything is sent
 * (`invalid_request`), and the answer must echo what was sent, list members in
 * order after the cursor, and hold exactly one of `value` and `too_large` per
 * member under `select: 'values'`; otherwise this rejects with
 * `invalid_response`. `data_version` is the version of the data space the page
 * was read at, never a source revision; a page read later can have a later
 * one. A version above 2^53 - 1 is refused as `invalid_response`. Pass
 * `next_after` back as `after` for the next page.
 */
export async function listDataMembers(
  client: ArtifreeClient,
  artifactId: string,
  query: ListDataMembersQuery = {},
  options: { readonly signal?: AbortSignal } = {},
): Promise<DataMembers> {
  const sent = checkQuery(query);
  const parameters = new URLSearchParams();
  if (sent.path !== '') parameters.set('path', sent.path);
  parameters.set('select', sent.select);
  parameters.set('limit', String(sent.limit));
  if (sent.after !== undefined) parameters.set('after', sent.after);
  const path = `/v1/artifacts/${encodeURIComponent(artifactId)}/data/members?${parameters.toString()}`;
  const response = await client.send('GET', path, { signal: options.signal });
  const text = await boundedText(response);
  let page: unknown;
  try {
    page = JSON.parse(text);
  } catch {
    throw breach('A page of members is not JSON.');
  }
  return parseMembersPage(page, artifactId, sent);
}
