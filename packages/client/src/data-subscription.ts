/**
 * Queries and subscriptions of application data (decision 0038).
 *
 * This module is imported only by callers that use it: the web entry does not,
 * and its size budget has no room for it. For that reason {@link followData}
 * repeats the reconnection loop of the change feed instead of sharing it.
 */
import type {
  DataChangeEvent,
  DataMember,
  DataMembers,
  DataMembersQuery,
  DataMemberSelect,
  DataPosition,
  DataSnapshotEvent,
  JsonValue,
} from '@artifree/contracts';
import { retryable, serverSentEvents, sleep } from './change-feed.js';
import type { ArtifreeClient, ReadOptions } from './client.js';
import { decodeText } from './client.js';
import { ArtifreeApiError } from './errors.js';
import { isCount, isCursor, isJsonValue, isRecord } from './responses.js';

/** A path of the data space: segments of `[A-Za-z0-9_-]` joined by `/`. */
const DATA_PATH = /^([A-Za-z0-9_-]{1,64}(\/[A-Za-z0-9_-]{1,64}){0,15})?$/;
/** A data cursor: `d.`, a feed identity, and a canonical data version. */
const DATA_CURSOR = /^d\.[0-9a-f]{32}\.(0|[1-9][0-9]{0,19})$/;

/** The parameters of {@link listDataMembers}; the server validates them. */
export interface MembersQuery {
  /** The collection; the root by default. */
  readonly path?: string;
  /** Only members whose name starts with this. */
  readonly prefix?: string;
  /** `names` (the default) or `values`. */
  readonly select?: DataMemberSelect;
  /** Page size, 1 to 100; 50 by default. */
  readonly limit?: number;
  /** Members strictly after this name; pass the previous `next_after`. */
  readonly after?: string;
}

/**
 * One page of the members of the object at a path, in your own data space of
 * the artifact. Pages are ordered by the UTF-8 bytes of the names and are not
 * tied to one data version (compare `canonical.data_version` between pages).
 * Rejects with `invalid_request` for a parameter that breaks a rule, with
 * `not_found` for an artifact that is missing, in the trash, purged, or
 * someone else's, and with `invalid_response` when the answer breaks the
 * contract or does not answer the page asked for.
 */
export async function listDataMembers(
  client: ArtifreeClient,
  artifactId: string,
  query: MembersQuery = {},
  options: ReadOptions = {},
): Promise<DataMembers> {
  const parts: string[] = [];
  for (const [name, value] of [
    ['path', query.path],
    ['prefix', query.prefix],
    ['select', query.select],
    ['limit', query.limit === undefined ? undefined : String(query.limit)],
    ['after', query.after],
  ] as const) {
    if (value !== undefined) parts.push(`${name}=${encodeURIComponent(value)}`);
  }
  const suffix = parts.length === 0 ? '' : `?${parts.join('&')}`;
  const page = await client.json(
    'GET',
    `/v1/artifacts/${encodeURIComponent(artifactId)}/data/members${suffix}`,
    parseDataMembers,
    { signal: options.signal },
  );
  const echoed = page.query;
  if (
    page.artifact_id !== artifactId ||
    echoed.path !== (query.path ?? '') ||
    echoed.select !== (query.select ?? 'names') ||
    (echoed.prefix ?? undefined) !== query.prefix ||
    (echoed.after ?? undefined) !== query.after ||
    (query.limit !== undefined && echoed.limit !== query.limit)
  )
    throw new ArtifreeApiError(
      'The data members answer is for another query than the one asked.',
      { status: 200, code: 'invalid_response' },
    );
  return page;
}

/** Whether `a` sorts before `b` by UTF-8 bytes, which is by code point. */
function bytewiseBefore(a: string, b: string): boolean {
  const left = Array.from(a);
  const right = Array.from(b);
  for (let index = 0; index < left.length && index < right.length; index += 1) {
    const x = left[index]?.codePointAt(0) ?? 0;
    const y = right[index]?.codePointAt(0) ?? 0;
    if (x !== y) return x < y;
  }
  return left.length < right.length;
}

function parseMember(
  value: unknown,
  select: DataMemberSelect,
): DataMember | undefined {
  if (!isRecord(value) || typeof value.name !== 'string') return undefined;
  const carries = Object.hasOwn(value, 'value');
  if (select === 'names') return carries ? undefined : { name: value.name };
  const found: unknown = value.value;
  if (!carries || !isJsonValue(found)) return undefined;
  return { name: value.name, value: found };
}

/** A parsed query: an absent `prefix` or `after` is left out, never null. */
type ParsedQuery = Omit<DataMembersQuery, 'after' | 'prefix'> & {
  readonly after?: string;
  readonly prefix?: string;
};

function parseQuery(value: unknown): ParsedQuery | undefined {
  if (!isRecord(value)) return undefined;
  const { path, prefix, select, limit, after } = value;
  if (
    typeof path !== 'string' ||
    !DATA_PATH.test(path) ||
    (select !== 'names' && select !== 'values') ||
    !isCount(limit) ||
    limit < 1 ||
    (prefix !== undefined && prefix !== null && typeof prefix !== 'string') ||
    (after !== undefined && after !== null && typeof after !== 'string')
  )
    return undefined;
  return {
    path,
    select,
    limit,
    ...(typeof prefix === 'string' ? { prefix } : {}),
    ...(typeof after === 'string' ? { after } : {}),
  };
}

/**
 * Parses `GET /v1/artifacts/{id}/data/members`. Beyond the shape it checks
 * what the contract promises of a page: at most `limit` members, strictly
 * ascending, all after `after` and starting with `prefix`, values exactly when
 * they were selected, and `next_after` the last name or absent.
 */
export function parseDataMembers(value: unknown): DataMembers | undefined {
  if (!isRecord(value) || typeof value.artifact_id !== 'string')
    return undefined;
  const query = parseQuery(value.query);
  const canonical = value.canonical;
  if (query === undefined || !isRecord(canonical)) return undefined;
  const { data_version, exists, members, next_after } = canonical;
  if (
    !isCount(data_version) ||
    typeof exists !== 'boolean' ||
    !Array.isArray(members) ||
    members.length > query.limit ||
    (next_after !== undefined &&
      next_after !== null &&
      typeof next_after !== 'string')
  )
    return undefined;
  const page: DataMember[] = [];
  const entries: unknown[] = members;
  for (const entry of entries) {
    const member = parseMember(entry, query.select);
    const previous = page.at(-1)?.name ?? query.after;
    if (
      member === undefined ||
      (previous !== undefined && !bytewiseBefore(previous, member.name)) ||
      (query.prefix !== undefined && !member.name.startsWith(query.prefix))
    )
      return undefined;
    page.push(member);
  }
  const next = typeof next_after === 'string' ? next_after : undefined;
  if (next !== undefined && (!exists || next !== page.at(-1)?.name))
    return undefined;
  if (!exists && page.length > 0) return undefined;
  return {
    artifact_id: value.artifact_id,
    query,
    canonical: {
      data_version,
      exists,
      members: page,
      ...(next === undefined ? {} : { next_after: next }),
    },
  };
}

/**
 * An event of `GET /v1/artifacts/{id}/data/events`. `snapshot` opens a stream
 * without a cursor; `reset` means the cursor could not be continued, so drop
 * what is held for the path and take its value; `change` states the value
 * after one committed write; `position` says everything up to a data version
 * has been sent.
 */
export type DataEvent =
  | {
      readonly type: 'snapshot' | 'reset';
      readonly data: DataSnapshotEvent;
    }
  | { readonly type: 'change'; readonly data: DataChangeEvent }
  | { readonly type: 'position'; readonly data: DataPosition };

/** The data cursor after `event`, to resume from. */
export function dataCursorOf(event: DataEvent): string {
  return event.data.cursor;
}

interface Located {
  readonly cursor: string;
  readonly data_version: number;
}

function parseLocated(value: unknown): Located | undefined {
  if (!isRecord(value)) return undefined;
  const { cursor, data_version } = value;
  if (!isCursor(cursor) || !DATA_CURSOR.test(cursor) || !isCount(data_version))
    return undefined;
  return { cursor, data_version };
}

/**
 * The value fields of a snapshot or change: `value` is present exactly when
 * `exists`, and a stored `null` is a present `null`.
 */
function parseState(
  value: unknown,
):
  (Located & { path: string; exists: boolean; value?: JsonValue }) | undefined {
  const located = parseLocated(value);
  if (located === undefined || !isRecord(value)) return undefined;
  const { path, exists } = value;
  if (typeof path !== 'string' || !DATA_PATH.test(path)) return undefined;
  if (typeof exists !== 'boolean') return undefined;
  const carries = Object.hasOwn(value, 'value');
  if (!exists) return carries ? undefined : { ...located, path, exists };
  const found: unknown = value.value;
  if (!carries || !isJsonValue(found)) return undefined;
  return { ...located, path, exists, value: found };
}

/**
 * The data events of a stream, until `signal` aborts. Event types this client
 * does not know are skipped; a known event whose data breaks the contract
 * rejects with `invalid_response`.
 */
async function* dataEvents(
  chunks: AsyncIterable<string>,
  signal: AbortSignal | undefined,
): AsyncGenerator<DataEvent> {
  for await (const { type, data } of serverSentEvents(chunks)) {
    // One chunk can hold several events; none follows an abort.
    if (signal?.aborted === true) return;
    if (
      type !== 'snapshot' &&
      type !== 'reset' &&
      type !== 'change' &&
      type !== 'position'
    )
      continue;
    let value: unknown;
    try {
      value = JSON.parse(data);
    } catch {
      value = undefined;
    }
    const event = toEvent(type, value);
    if (event === undefined)
      throw new ArtifreeApiError(
        `The data stream sent a ${type} event that does not match the contract.`,
        { status: 200, code: 'invalid_response' },
      );
    yield event;
  }
}

function toEvent(
  type: 'snapshot' | 'reset' | 'change' | 'position',
  value: unknown,
): DataEvent | undefined {
  if (type === 'position') {
    const data = parseLocated(value);
    return data === undefined ? undefined : { type, data };
  }
  const data = parseState(value);
  return data === undefined ? undefined : { type, data };
}

/** Options of {@link openData}. */
export interface OpenDataOptions {
  /** The watched path; the root by default. */
  readonly path?: string;
  /** Resume after this data cursor; without it the stream opens with a snapshot. */
  readonly after?: string;
  /** Ends the stream when it aborts. */
  readonly signal?: AbortSignal;
}

/**
 * Opens the data stream of a path once. Resolves when the server accepted the
 * stream, with its events until the server ends it or `signal` aborts.
 * Rejects with `invalid_request` for a path or cursor that breaks a rule
 * (a change feed cursor is one), `not_found` for an artifact that is missing,
 * in the trash, purged, or someone else's, `rate_limited` when the caller
 * already has 8 open data streams, and `invalid_response` when a successful
 * answer is not an event stream. Use {@link followData} to reconnect.
 */
export async function openData(
  client: ArtifreeClient,
  artifactId: string,
  options: OpenDataOptions = {},
): Promise<AsyncGenerator<DataEvent>> {
  const headers: Record<string, string> = { accept: 'text/event-stream' };
  if (options.after !== undefined) headers['last-event-id'] = options.after;
  const suffix =
    options.path === undefined || options.path === ''
      ? ''
      : `?path=${encodeURIComponent(options.path)}`;
  const path = `/v1/artifacts/${encodeURIComponent(artifactId)}/data/events${suffix}`;
  const response = await client.send('GET', path, {
    headers,
    signal: options.signal,
  });
  const mediaType = response.headers
    .get('content-type')
    ?.split(';')[0]
    ?.trim()
    .toLowerCase();
  if (mediaType !== 'text/event-stream' || response.body === null) {
    await response.body?.cancel().catch(() => undefined);
    throw new ArtifreeApiError(
      `GET ${path} answered HTTP ${response.status} with ${mediaType ?? 'no content type'}, not an event stream.`,
      { status: response.status, code: 'invalid_response' },
    );
  }
  return dataEvents(
    decodeText(response.body, options.signal, client.address.plain),
    options.signal,
  );
}

/** Options of {@link followData}. */
export interface FollowDataOptions {
  /** The watched path; the root by default. */
  readonly path?: string;
  /** Resume after this data cursor; without it, start with `snapshot`. */
  readonly after?: string;
  /** Stops following; the returned promise then resolves. */
  readonly signal: AbortSignal;
  /** Receives every event in order, across reconnections. */
  readonly onEvent: (event: DataEvent) => void;
  /** Called each time the server accepts a stream. */
  readonly onConnect?: () => void;
  /**
   * Called when a stream ends or cannot be opened and following continues,
   * before waiting `retryInMs` to reconnect from the last cursor.
   */
  readonly onDisconnect?: (
    error: ArtifreeApiError | undefined,
    retryInMs: number,
  ) => void;
  /**
   * Reconnection delays: from `min`, doubling after each failed attempt up to
   * `max`, in milliseconds. A `Retry-After` from the server is used instead
   * when given.
   */
  readonly retryDelayMs?: { readonly min: number; readonly max: number };
}

/**
 * Follows the data of a path until `options.signal` aborts, reconnecting from
 * the last cursor after a lost connection, a 429, or a 5xx. Events arrive in
 * the order of the data versions, each once; a `reset` says the server could
 * not continue the cursor (older than its window, another database, or a
 * restart) and carries the value to take instead. Rejects on a failure that
 * reconnecting cannot fix, such as a rejected token, an artifact that is gone,
 * or a response that is not the stream, and with what `onEvent` throws.
 */
export async function followData(
  client: ArtifreeClient,
  artifactId: string,
  options: FollowDataOptions,
): Promise<void> {
  const { signal, onEvent, onConnect, onDisconnect } = options;
  const { min, max } = options.retryDelayMs ?? { min: 500, max: 30_000 };
  let cursor = options.after;
  let attempt = 0;
  while (!signal.aborted) {
    let failure: ArtifreeApiError | undefined;
    // What onEvent threw: it ends following and is never retried.
    let rejected: { readonly error: unknown } | undefined;
    try {
      const events = await openData(client, artifactId, {
        ...(options.path === undefined ? {} : { path: options.path }),
        ...(cursor === undefined ? {} : { after: cursor }),
        signal,
      });
      attempt = 0;
      onConnect?.();
      for await (const event of events) {
        cursor = dataCursorOf(event);
        try {
          onEvent(event);
        } catch (error: unknown) {
          rejected = { error };
          break;
        }
      }
    } catch (error: unknown) {
      if (signal.aborted) return;
      if (!(error instanceof ArtifreeApiError) || !retryable(error))
        throw error;
      failure = error;
    }
    if (rejected !== undefined) throw rejected.error;
    if (signal.aborted) return;
    const retryInMs =
      failure?.retryAfterSeconds === undefined
        ? Math.min(max, min * 2 ** attempt)
        : failure.retryAfterSeconds * 1000;
    attempt += 1;
    onDisconnect?.(failure, retryInMs);
    await sleep(retryInMs, signal);
  }
}

/**
 * What a client holds of one watched path: the committed value, the data
 * version it is for, and the cursor to resume from. There is no pending state
 * here; a client that queues writes keeps them beside this (decision 0038).
 */
export interface DataView {
  /** The watched path. */
  readonly path: string;
  /** Whether anything is at the path. */
  readonly exists: boolean;
  /** The value at the path, present when `exists`. */
  readonly value?: JsonValue;
  /** The data version this view is for. */
  readonly dataVersion: number;
  /** Resume after this cursor. */
  readonly cursor: string;
}

function violation(message: string): ArtifreeApiError {
  return new ArtifreeApiError(message, {
    status: 200,
    code: 'invalid_response',
  });
}

/** The own member `name` of an object, or undefined. */
function memberOf(
  object: JsonValue | undefined,
  name: string,
): JsonValue | undefined {
  if (!isRecord(object) || !Object.hasOwn(object, name)) return undefined;
  const found: unknown = object[name];
  return isJsonValue(found) ? found : undefined;
}

/**
 * `root` with the value at `segments` replaced, or removed when `value` is
 * undefined. The objects above are made when missing; a value that is not an
 * object in the way breaks the protocol, because the server refuses such a
 * write. Members are set by definition, so a segment such as `__proto__` is an
 * ordinary name.
 */
function put(
  root: JsonValue | undefined,
  segments: readonly string[],
  value: JsonValue | undefined,
): JsonValue | undefined {
  const [head, ...rest] = segments;
  if (head === undefined) return value;
  if (root !== undefined && !isRecord(root))
    throw violation(
      'A data change goes through a value that is not an object.',
    );
  const below = memberOf(root, head);
  if (value === undefined && below === undefined) return root;
  const next = put(below, rest, value);
  // The order of members means nothing. `fromEntries` defines members, so a
  // name such as `__proto__` stays an ordinary member.
  const others = Object.entries(root ?? {}).filter(([name]) => name !== head);
  return Object.fromEntries(
    next === undefined ? others : [...others, [head, next]],
  );
}

/**
 * Applies one event to `view` and returns the new view. `snapshot` and
 * `reset` replace everything; `change` sets or removes the value at its path
 * (the watched path or below it) to the state the event carries; `position`
 * moves the version and cursor. An event that cannot follow the view (a change
 * that is not newer, a path outside the watched one, or a change through a
 * value that is not an object) throws `invalid_response`, so a stream that
 * breaks its contract is not applied silently. Start from `undefined` and the
 * first event, a `snapshot`.
 */
export function applyDataEvent(
  view: DataView | undefined,
  event: DataEvent,
): DataView {
  switch (event.type) {
    case 'snapshot':
    case 'reset': {
      const { data } = event;
      return {
        path: data.path,
        exists: data.exists,
        ...(data.value === undefined ? {} : { value: data.value }),
        dataVersion: data.data_version,
        cursor: data.cursor,
      };
    }
    case 'position': {
      if (view === undefined)
        throw violation('The data stream began with a position event.');
      if (event.data.data_version < view.dataVersion)
        throw violation('A data position went back in version.');
      return {
        ...view,
        dataVersion: event.data.data_version,
        cursor: event.data.cursor,
      };
    }
    case 'change': {
      if (view === undefined)
        throw violation('The data stream began with a change event.');
      const { data } = event;
      if (data.data_version <= view.dataVersion)
        throw violation('A data change is not newer than the view.');
      const below = relative(view.path, data.path);
      if (below === undefined)
        throw violation('A data change is outside the watched path.');
      const value = put(
        view.exists ? view.value : undefined,
        below,
        data.exists ? data.value : undefined,
      );
      return {
        path: view.path,
        exists: value !== undefined,
        ...(value === undefined ? {} : { value }),
        dataVersion: data.data_version,
        cursor: data.cursor,
      };
    }
    default: {
      const unreachable: never = event;
      return unreachable;
    }
  }
}

/** The segments of `path` below `watched`, or undefined if it is not below. */
function relative(watched: string, path: string): string[] | undefined {
  const base = watched === '' ? [] : watched.split('/');
  const full = path === '' ? [] : path.split('/');
  if (full.length < base.length) return undefined;
  if (base.some((segment, index) => full[index] !== segment)) return undefined;
  return full.slice(base.length);
}
