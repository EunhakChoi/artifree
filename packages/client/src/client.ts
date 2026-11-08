import type {
  AgentSession,
  AppData,
  Artifact,
  ArtifactList,
  CommitResponse,
  CreateArtifactRequest,
  CreateTokenRequest,
  CancelJobRequest,
  CancelJobResponse,
  CreateTokenResponse,
  DataWriteResponse,
  DataMembers,
  DeleteDataRequest,
  DuplicateArtifactRequest,
  DuplicateArtifactResponse,
  EnqueueJobRequest,
  EnqueueJobResponse,
  FileUpload,
  Handoff,
  Job,
  JobList,
  JsonValue,
  ListArtifactsQuery,
  ListJobsQuery,
  Preview,
  PurgeArtifactRequest,
  PurgeArtifactResponse,
  RestoreRevisionRequest,
  Revision,
  RotateTokenResponse,
  RuntimeDownloadOffer,
  RuntimeMetadata,
  RuntimeGrant,
  RuntimeSourceList,
  RuntimeView,
  SaveRevisionRequest,
  SetDataRequest,
  StageHandoffRequest,
  TokenList,
  RemoveUnreferencedContentRequest,
  RemoveUnreferencedContentResponse,
  UnreferencedContent,
  UpdateArtifactRequest,
  UpdateArtifactResponse,
  UpdateDataRequest,
} from '@artifree/contracts';
import {
  type ChangeFeedEvent,
  type FollowOptions,
  changeFeedEvents,
  follow,
} from './change-feed.js';
import {
  BodyReader,
  type Clock,
  cancelQuietly,
  decodeText,
  readErrorJson,
  readSuccessJson,
  readSuccessBytes,
  systemClock,
} from './transport.js';
import { toBase64 } from './base64.js';
import { ArtifreeApiError, RuntimeConnectionReplacedError } from './errors.js';
import type * as RuntimeSource from './runtime-source.js';
import type { FollowDataOptions } from './follow-data.js';
import type {
  RuntimeDownloadTarget,
  RuntimeSourcePartBytes,
  RuntimeSourceRequest,
} from './runtime-source.js';
import {
  parseAgentSessions,
  parseAppData,
  parseArtifact,
  parseArtifactList,
  parseCommit,
  parseCreateToken,
  parseCancelJob,
  parseDataWrite,
  parseDataMembers,
  parseFollowedAppData,
  parseDuplicate,
  parseEnqueueJob,
  parseError,
  parseHandoff,
  parseJob,
  parseJobList,
  parsePreview,
  parsePurge,
  parseRemoveUnreferencedContent,
  parseRevisionList,
  parseRotateToken,
  parseRuntimeConnection,
  parseRuntimeMetadata,
  parseRuntimeGrant,
  parseRuntimeView,
  parseTokenList,
  parseUnreferencedContent,
  parseUpdate,
} from './responses.js';

/** Connection settings for one Artifree API. */
export interface ClientConfiguration {
  /**
   * API origin such as `http://127.0.0.1:7410`, or an empty string for the
   * page's own origin (for example behind a development proxy).
   */
  readonly baseUrl: string;
  /** Bearer token from the server's `access-token` file. */
  readonly accessToken: string;
  /** Fetch implementation; defaults to the global `fetch`. */
  readonly fetch?: typeof fetch;
  /**
   * Time for the stall limit of streams and the waits of
   * {@link ArtifreeClient.followChanges}; the system clock by default.
   */
  readonly clock?: Clock;
}

/** Options for requests that only read. */
export interface ReadOptions {
  /** Cancels the request, also while a `fetch` wrapper holds it back. */
  readonly signal?: AbortSignal;
}

/** Options for listing one page of application-data member names. */
export interface DataMembersOptions extends ReadOptions {
  readonly path?: string;
  readonly limit?: number;
  readonly pageToken?: string;
  readonly atDataVersion?: number;
}

/** Options for a runtime view request managed by a cooperating scheduler. */
export interface RuntimeViewOptions extends ReadOptions {
  /**
   * Called immediately before each actual attempt by a scheduler that
   * supports dispatch notifications. Without one, it is not called.
   */
  readonly onDispatch?: () => void;
}

// Symbol.for keeps the notification shared when a scheduler and client are
// bundled separately. It is local to the request and is not sent over HTTP.
const REQUEST_DISPATCH = Symbol.for('artifree.request-dispatch.v1');

/** Notifies a request's observer just before a scheduler sends an attempt. */
export function notifyRequestDispatched(init: RequestInit | undefined): void {
  const callback: unknown =
    init === undefined ? undefined : Reflect.get(init, REQUEST_DISPATCH);
  if (typeof callback === 'function') callback();
}

interface DispatchRequestInit extends RequestInit {
  [REQUEST_DISPATCH]?: () => void;
}

/** Which bytes of a file {@link ArtifreeClient.readFilePart} reads. */
export interface FileRange {
  /** The first byte, counted from 0. */
  readonly offset: number;
  /** How many bytes at most; the rest of the file when absent. */
  readonly length?: number;
}

/** Part of a file. */
export interface FilePart {
  /** The part's bytes. */
  readonly content: Uint8Array<ArrayBuffer>;
  /** Where the part starts in the file. */
  readonly offset: number;
  /** The whole file's size in bytes. */
  readonly size: number;
}

/** Options for requests that change state. */
export interface MutationOptions {
  /**
   * Retry identity. Reuse the same key when retrying the same request after a
   * timeout or network failure; the server then returns the original result.
   */
  readonly idempotencyKey: string;
}

/**
 * A 128-bit random identifier in hexadecimal. `getRandomValues` is also
 * available to HTTP pages outside a secure context; no weak fallback is used.
 */
export function newRandomId(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join(
    '',
  );
}

/** A random key suitable for `Idempotency-Key`. */
export function newIdempotencyKey(): string {
  const id = newRandomId();
  // Keep retry keys distinct from common API-token formats.
  return `key-${id.slice(0, 8)}-${id.slice(8)}`;
}

/** Queries must preserve scalar identity instead of replacing lone surrogates. */
function queryDataPath(path: string): string {
  for (const scalar of path) {
    const point = scalar.codePointAt(0);
    if (point !== undefined && point >= 0xd800 && point <= 0xdfff)
      throw new TypeError(
        'A data query path must contain well-formed Unicode.',
      );
  }
  return path;
}

/**
 * Encodes file bytes for upload in whichever form is smaller on the wire:
 * UTF-8 text when the bytes are valid UTF-8 and their JSON escaping does not
 * outgrow base64, otherwise base64. The server's body limit assumes base64
 * sizes, so quote- or control-heavy text must not be sent escaped. The
 * server stores exactly these bytes, so a byte order mark is kept as text.
 */
export function encodeFile(path: string, bytes: Uint8Array): FileUpload {
  const base64 = (): FileUpload => ({
    path,
    encoding: 'base64',
    content: toBase64(bytes),
  });
  let text: string;
  try {
    text = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(
      bytes,
    );
  } catch {
    return base64();
  }
  const escaped = new TextEncoder().encode(JSON.stringify(text)).length - 2;
  if (escaped > Math.ceil(bytes.length / 3) * 4) return base64();
  return { path, encoding: 'utf8', content: text };
}

/**
 * Encodes a package path for a URL. URL parsing resolves `.` and `..`
 * segments (also when percent-encoded), which would request another route,
 * so such paths are rejected here; the server validates every other rule.
 */
function filePath(path: string): string {
  const segments = path.split('/');
  if (segments.some((segment) => ['', '.', '..'].includes(segment)))
    throw new ArtifreeApiError(
      `Invalid package path ${JSON.stringify(path)}: empty, '.', and '..' segments are not allowed.`,
      { status: 0, code: 'invalid_request' },
    );
  return segments.map(encodeURIComponent).join('/');
}

/**
 * Whether `range` can be asked for: whole numbers, a length of at least 1,
 * and a last position that is still a safe integer.
 */
function rangeIsValid({ offset, length }: FileRange): boolean {
  if (!Number.isSafeInteger(offset) || offset < 0) return false;
  if (length === undefined) return true;
  return (
    Number.isSafeInteger(length) &&
    length >= 1 &&
    offset <= Number.MAX_SAFE_INTEGER - (length - 1)
  );
}

/**
 * The part a `206` holds, when its `Content-Range` is exactly the part
 * `range` asks for: from `range.offset` to the end of the range or of the
 * file, whichever comes first. Its body must then have `length` bytes.
 */
function statedPart(
  range: FileRange,
  contentRange: string | null,
): { readonly size: number; readonly length: number } | undefined {
  const match = /^bytes ([0-9]+)-([0-9]+)\/([0-9]+)$/i.exec(contentRange ?? '');
  if (match === null) return undefined;
  const [first, last, size] = match.slice(1).map(Number);
  if (
    first === undefined ||
    last === undefined ||
    size === undefined ||
    !Number.isSafeInteger(size)
  )
    return undefined;
  const end = Math.min(
    range.length === undefined ? size : range.offset + range.length,
    size,
  );
  const exact =
    range.offset < end && first === range.offset && last === end - 1;
  return exact ? { size, length: end - range.offset } : undefined;
}

/**
 * JSON text that {@link ArtifreeClient.setData} and
 * {@link ArtifreeClient.updateData} send exactly as written, instead of
 * serializing a JavaScript value. A JavaScript value cannot hold what the
 * server refuses to guess at: a repeated member name or an integer beyond
 * 2^53 - 1 is already changed by the time it is a value; a JavaScript value
 * that `JSON.stringify` would change silently is refused before it is sent:
 * `NaN` and `Infinity`, `undefined`, functions (a `toJSON` function too),
 * symbols, a `bigint`, a `Date` or class instance, an array with holes or extra
 * properties, and a getter, a hidden property or a symbol-keyed one. Only `-0`
 * is let through, and is written as `0`. Plain objects and arrays from another
 * realm are accepted. The server parses the text under the rules of application data and rejects with
 * `invalid_request` what is not JSON or breaks them.
 */
export class JsonText {
  readonly text: string;

  constructor(text: string) {
    this.text = text;
  }
}

/** A value to store: JSON the client serializes, or {@link JsonText}. */
export type DataInput = JsonValue | JsonText;

/** {@link SetDataRequest} whose `value` can be {@link JsonText}. */
export type SetDataInput = Omit<SetDataRequest, 'value'> & {
  readonly value: DataInput;
};

/** {@link UpdateDataRequest} whose `patch` can be {@link JsonText}. */
export type UpdateDataInput = Omit<UpdateDataRequest, 'patch'> & {
  readonly patch: DataInput;
};

const DATA_VALUE_LEVELS = 64;

function notData(where: string, why: string): ArtifreeApiError {
  return new ArtifreeApiError(
    `${where} cannot be sent as application data: ${why}. Use a JSON value, or JsonText for exact text.`,
    { status: 0, code: 'invalid_request' },
  );
}

/** The value of own data property `name`, read once; refuses anything else. */
function ownData(
  holder: object,
  name: string,
  where: string,
  what: string,
): unknown {
  const descriptor = Object.getOwnPropertyDescriptor(holder, name);
  if (descriptor === undefined) throw notData(where, `${what} is missing`);
  if (!('value' in descriptor))
    throw notData(where, `${what} is a getter or setter, not a value`);
  if (descriptor.enumerable !== true)
    throw notData(where, `${what} is hidden from JSON.stringify`);
  return descriptor.value;
}

/**
 * Returns a plain copy of `value`, or throws unless `JSON.stringify` would
 * write `value` faithfully. The copy is what is serialized, and every property
 * is read exactly once, so a getter, a proxy or a mutation after the check
 * cannot change what is sent. `JSON.stringify` would otherwise turn `NaN` and
 * `Infinity` into `null`, drop members that are `undefined`, functions, or
 * symbols, turn such array elements and array holes into `null`, ignore
 * symbol-keyed, non-enumerable and non-index array properties, call a
 * `toJSON` function, throw on a `bigint` or a cycle, and write a `Date`, a
 * `Map`, or a class instance as something else. All of those are refused.
 * An array is anything `Array.isArray` accepts; an object is plain when its
 * prototype is `null` or itself has none, so a value from another realm
 * (`vm`, an iframe) is accepted. `-0` is the one change that is allowed: it is
 * written as `0`. The server's own rules (the size of numbers, limits) are
 * left to the server.
 */
function plainData(value: unknown, where: string, levels: number): unknown {
  if (value === null || typeof value === 'string' || typeof value === 'boolean')
    return value;
  if (typeof value === 'number') {
    if (!Number.isFinite(value))
      throw notData(where, `${String(value)} is not a JSON number`);
    return value;
  }
  if (typeof value !== 'object')
    throw notData(where, `a ${typeof value} has no JSON form`);
  if (levels <= 0)
    throw notData(where, 'it nests too deeply, or refers to itself');
  const keys = Reflect.ownKeys(value);
  if (Array.isArray(value)) {
    const length = value.length;
    // Own keys are the indexes and `length`; a hole or extra property breaks
    // the count before any loop runs, so `new Array(1e9)` is cheap to refuse.
    if (keys.length !== length + 1)
      throw notData(
        where,
        'the array has holes or properties beyond its elements',
      );
    const items: unknown[] = [];
    for (let index = 0; index < length; index += 1)
      items.push(
        plainData(
          ownData(value, String(index), `${where}[${index}]`, 'the element'),
          `${where}[${index}]`,
          levels - 1,
        ),
      );
    return items;
  }
  const prototype: unknown = Object.getPrototypeOf(value);
  if (prototype !== null && Object.getPrototypeOf(prototype) !== null)
    throw notData(where, 'only plain objects and arrays have a JSON form');
  const members: [string, unknown][] = [];
  for (const key of keys) {
    if (typeof key === 'symbol')
      throw notData(where, 'a symbol-keyed property has no JSON form');
    const at = `${where}.${key}`;
    members.push([
      key,
      plainData(ownData(value, key, at, 'the member'), at, levels - 1),
    ]);
  }
  return Object.fromEntries(members);
}

function checkBase(base: number): void {
  if (!Number.isSafeInteger(base) || base < 0)
    throw notData('base_data_version', 'it is not a whole number from 0');
}

/**
 * The text of a data write. Members are written in a fixed order; a
 * {@link JsonText} goes in as it is, the rest is checked and then written
 * through `JSON.stringify`.
 */
function dataBodyText(
  request: { readonly path: string; readonly base_data_version: number },
  field: 'value' | 'patch',
  input: DataInput,
): string {
  checkBase(request.base_data_version);
  const text = dataValueText(input, field);
  return `{"base_data_version":${JSON.stringify(request.base_data_version)},"path":${JSON.stringify(request.path)},"${field}":${text}}`;
}

/** Internal shared serialization: raw JsonText is deliberately not parsed. */
export function dataValueText(
  input: DataInput,
  field: 'value' | 'patch',
): string {
  return input instanceof JsonText
    ? input.text
    : JSON.stringify(plainData(input, field, DATA_VALUE_LEVELS));
}

interface SendOptions {
  readonly mutation?:
    { body: unknown; idempotencyKey: string; text?: string } | undefined;
  readonly body?: unknown;
  readonly headers?: Record<string, string>;
  readonly cache?: RequestCache;
  readonly signal?: AbortSignal | undefined;
  readonly onDispatch?: (() => void) | undefined;
  /**
   * Reads the body of a failed response within the transport's bounds (a
   * size limit, a stall limit, the signal) instead of whole.
   */
  readonly boundedErrorBody?: boolean;
  /** Per-operation streaming body limits; omitted limits retain existing policy. */
  readonly successBodyLimit?: number;
  readonly errorBodyLimit?: number;
  readonly redirect?: 'error';
  /**
   * The one success status the contract allows. Any other 2xx is an
   * `invalid_response` whose body is cancelled unread.
   */
  readonly expectStatus?: number;
}

type GrantState = 'settled' | 'pending' | 'unknown';
type GrantView = Pick<
  RuntimeView,
  'artifact_id' | 'revision' | 'head_revision'
>;

/** Keeps uncertain writes with the connection, even when a preview unmounts. */
class RuntimeGrantChanges {
  private readonly states = new Map<string, Exclude<GrantState, 'settled'>>();
  private readonly revisions = new Map<string, number>();
  private readonly listeners = new Set<() => void>();
  private epoch = 0;

  readonly subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };
  readonly snapshot = (): number => this.epoch;

  private key(view: GrantView): string {
    return JSON.stringify([
      view.artifact_id,
      view.revision,
      view.head_revision,
    ]);
  }

  private revisionKey(id: string, revision: number): string {
    return JSON.stringify([id, revision]);
  }

  revision(id: string, revision: number): number {
    return this.revisions.get(this.revisionKey(id, revision)) ?? 0;
  }

  status(view: GrantView): GrantState {
    return this.states.get(this.key(view)) ?? 'settled';
  }

  begin(view: GrantView, capabilities: readonly string[]): boolean {
    const previous = this.status(view);
    if (
      previous === 'pending' ||
      (previous === 'unknown' && capabilities.length !== 0)
    )
      throw new ArtifreeApiError(
        'Resolve the earlier permission change by clearing access first.',
        {
          status: 409,
          code: 'invalid_request',
        },
      );
    this.states.set(this.key(view), 'pending');
    this.notify();
    return previous === 'unknown';
  }

  finish(view: GrantView, unknown: boolean): void {
    const key = this.key(view);
    if (unknown) this.states.set(key, 'unknown');
    else this.states.delete(key);
    const revision = this.revisionKey(view.artifact_id, view.revision);
    this.revisions.set(revision, (this.revisions.get(revision) ?? 0) + 1);
    this.notify();
  }

  clear(): void {
    this.states.clear();
    this.revisions.clear();
    this.notify();
  }

  private notify(): void {
    this.epoch += 1;
    for (const listener of this.listeners) listener();
  }
}

function grantMayHaveChanged(error: unknown): boolean {
  return (
    !(error instanceof ArtifreeApiError) ||
    error.code === 'network_error' ||
    error.code === 'invalid_response' ||
    error.status >= 500
  );
}

function sessionExpired(error: unknown): boolean {
  return (
    error instanceof ArtifreeApiError &&
    error.status === 404 &&
    error.code === 'runtime_session_expired'
  );
}

/** The immutable proposal fields must not change between delivery states. */
function sameHandoff(left: Handoff, right: Handoff): boolean {
  const a = left.selection;
  const b = right.selection;
  return (
    left.id === right.id &&
    left.agent_session_id === right.agent_session_id &&
    left.agent_name === right.agent_name &&
    left.intent === right.intent &&
    a.artifact_id === b.artifact_id &&
    a.revision === b.revision &&
    a.path === b.path &&
    a.file_sha256 === b.file_sha256 &&
    a.start === b.start &&
    a.end === b.end &&
    a.text === b.text
  );
}

/**
 * How a failure message may name the server, and whether the original error
 * may travel with it. A runtime's network error can quote the URL it was given,
 * so an address that is not plain loses its errors' `cause`. The label is at
 * most origin plus path for an http(s) address (never credentials, query or
 * fragment), and a generic name for anything else.
 */
interface Address {
  readonly label: string;
  /** An http(s) address without credentials, query, or fragment. */
  readonly plain: boolean;
}

function describeAddress(baseUrl: string): Address {
  if (baseUrl === '') return { label: 'this origin', plain: true };
  // A path on the page's own origin has no authority to hold a credential. A
  // leading `//` would be one, so it is not a path.
  if (/^\/(?!\/)[^?#\\]*$/.test(baseUrl))
    return { label: baseUrl, plain: true };
  let url: URL;
  try {
    url = new URL(baseUrl);
  } catch {
    return { label: 'the configured server', plain: false };
  }
  if (url.protocol !== 'http:' && url.protocol !== 'https:')
    return { label: 'the configured server', plain: false };
  return {
    label: `${url.origin}${url.pathname}`.replace(/\/+$/, ''),
    plain:
      url.username === '' &&
      url.password === '' &&
      !baseUrl.includes('?') &&
      !baseUrl.includes('#'),
  };
}

/** The `cause` option for a failure: the error itself, or nothing. */
function causeOf(keep: boolean, error: unknown): { cause?: unknown } {
  return keep ? { cause: error } : {};
}

/** HTTP API v1 client. Every method rejects with `ArtifreeApiError`. */
export class ArtifreeClient {
  private runtimeSession: string | undefined;
  private runtimeOpenAttempt = 0;
  private runtimeCloseAttempt = 0;
  private runtimeConnectionEpoch = 0;
  private runtimeClosing = false;
  private runtimeRecovery:
    { readonly session: string; readonly promise: Promise<void> } | undefined;
  private readonly runtimeGrants = new RuntimeGrantChanges();
  readonly subscribeRuntimeGrants = this.runtimeGrants.subscribe;
  readonly runtimeGrantSnapshot = this.runtimeGrants.snapshot;
  private readonly baseUrl: string;
  private readonly address: Address;
  private readonly accessToken: string;
  private readonly fetchImpl: typeof fetch;
  private readonly clock: Clock;

  constructor(configuration: ClientConfiguration) {
    this.baseUrl = configuration.baseUrl.replace(/\/+$/, '');
    this.address = describeAddress(this.baseUrl);
    this.accessToken = configuration.accessToken;
    this.fetchImpl =
      configuration.fetch ?? ((input, init) => fetch(input, init));
    this.clock = configuration.clock ?? systemClock;
  }

  /** The active server-issued runtime identity. */
  get runtimeSessionId(): string {
    if (this.runtimeSession === undefined)
      throw new ArtifreeApiError('The runtime connection is not open.', {
        status: 0,
        code: 'invalid_request',
      });
    return this.runtimeSession;
  }

  /** Changes whenever a new connection replaces the previous one. */
  runtimeConnectionRevision(): number {
    return this.runtimeConnectionEpoch;
  }

  /** Signals permission changes for a preview's artifact and revision. */
  runtimeGrantRevision(id: string, revision: number): number {
    return this.runtimeGrants.revision(id, revision);
  }

  /** Whether this exact source binding has a pending or uncertain change. */
  runtimeGrantStatus(view: GrantView): GrantState {
    return this.runtimeGrants.status(view);
  }

  /**
   * One page of artifacts. Without a query it lists up to 500, most
   * recently updated first; pass `next_page_token` back as `page_token`,
   * with the same filters and order, for the next page.
   */
  async listArtifacts(
    query: ListArtifactsQuery = {},
    options: ReadOptions = {},
  ): Promise<ArtifactList> {
    const parameters = new URLSearchParams();
    for (const [name, value] of Object.entries(query))
      if (value !== undefined && value !== null)
        parameters.set(name, String(value));
    const search = parameters.toString();
    return this.json(
      'GET',
      search === '' ? '/v1/artifacts' : `/v1/artifacts?${search}`,
      parseArtifactList,
      { signal: options.signal },
    );
  }

  async getArtifact(id: string, options: ReadOptions = {}): Promise<Artifact> {
    return this.json(
      'GET',
      `/v1/artifacts/${encodeURIComponent(id)}`,
      parseArtifact,
      { signal: options.signal },
    );
  }

  /** Revision history, newest first. */
  async listRevisions(
    id: string,
    options: ReadOptions = {},
  ): Promise<Revision[]> {
    return this.json(
      'GET',
      `/v1/artifacts/${encodeURIComponent(id)}/revisions`,
      parseRevisionList,
      { signal: options.signal },
    );
  }

  async createArtifact(
    request: CreateArtifactRequest,
    options: MutationOptions,
  ): Promise<CommitResponse> {
    return this.json('POST', '/v1/artifacts', parseCommit, {
      mutation: { body: request, idempotencyKey: options.idempotencyKey },
    });
  }

  /**
   * Commits changes prepared against `request.base_revision`. Rejects with
   * code `revision_conflict` and `currentRevision` when another save won.
   */
  async saveRevision(
    id: string,
    request: SaveRevisionRequest,
    options: MutationOptions,
  ): Promise<CommitResponse> {
    return this.json(
      'POST',
      `/v1/artifacts/${encodeURIComponent(id)}/revisions`,
      parseCommit,
      {
        mutation: { body: request, idempotencyKey: options.idempotencyKey },
      },
    );
  }

  /**
   * Commits the files of an earlier `revision`, exactly as they were, as
   * the next revision, prepared against `request.base_revision`. Rejects
   * with code `revision_conflict` and `currentRevision` when another save
   * won.
   */
  async restoreRevision(
    id: string,
    revision: number,
    request: RestoreRevisionRequest,
    options: MutationOptions,
  ): Promise<CommitResponse> {
    return this.json(
      'POST',
      `/v1/artifacts/${encodeURIComponent(id)}/revisions/${revision}/restore`,
      parseCommit,
      {
        mutation: { body: request, idempotencyKey: options.idempotencyKey },
      },
    );
  }

  /**
   * Creates an artifact whose revision 1 has the files of `revision` of
   * `id`, with its own history: unpinned, and named `request.title` or the
   * source's title followed by " (Copy)". By default only the files are
   * copied. With `request.copy_data` true the copy also gets its own copy of
   * the source's application data. With `request.copy_assets` true it also
   * gets its own copy of the caller's entire uploaded asset space of the
   * source, with the same asset IDs; when those do not fit the asset limits
   * the whole duplicate rejects with code `asset_limit_exceeded` and nothing
   * is created. `copied` lists what the copy received; treat it as a set. A
   * receipt whose `copied` omits a requested part or names an unrequested
   * one rejects with `invalid_response`.
   */
  async duplicateArtifact(
    id: string,
    revision: number,
    request: DuplicateArtifactRequest,
    options: MutationOptions,
  ): Promise<DuplicateArtifactResponse> {
    // Captured before the first await, so the receipt is checked against
    // what was sent even if the caller changes `request` meanwhile.
    const requested = {
      data: request.copy_data === true,
      assets: request.copy_assets === true,
    };
    return this.json(
      'POST',
      `/v1/artifacts/${encodeURIComponent(id)}/revisions/${revision}/duplicate`,
      (value) => parseDuplicate(value, requested),
      {
        mutation: { body: request, idempotencyKey: options.idempotencyKey },
      },
    );
  }

  /**
   * Changes the title, pin, or trash state prepared against
   * `request.base_metadata_revision`, without committing a revision.
   * `deleted: true` moves the artifact to the trash, where every other
   * request answers `not_found`; `deleted: false` takes it out. Rejects
   * with code `metadata_conflict` and `currentMetadataRevision` when
   * another update won.
   */
  async updateArtifact(
    id: string,
    request: UpdateArtifactRequest,
    options: MutationOptions,
  ): Promise<UpdateArtifactResponse> {
    return this.json(
      'PATCH',
      `/v1/artifacts/${encodeURIComponent(id)}`,
      parseUpdate,
      { mutation: { body: request, idempotencyKey: options.idempotencyKey } },
    );
  }

  /**
   * Removes an artifact that is in the trash for good (decision 0029). It
   * cannot be undone. Rejects with `not_in_trash` for an artifact outside the
   * trash, with `metadata_conflict` and `currentMetadataRevision` when the
   * artifact's metadata is no longer at `base_metadata_revision`, and with
   * `not_found` for an artifact that is missing, belongs to someone else, or
   * is already purged. A retry with the same key and request resolves with
   * `replayed: true`. Needs the owner's access token or a token issued to a
   * user other than the owner profile (decision 0036); a token issued to the
   * owner profile rejects with `forbidden`.
   */
  async purgeArtifact(
    id: string,
    request: PurgeArtifactRequest,
    options: MutationOptions,
  ): Promise<PurgeArtifactResponse> {
    return this.json(
      'POST',
      `/v1/artifacts/${encodeURIComponent(id)}/purge`,
      parsePurge,
      { mutation: { body: request, idempotencyKey: options.idempotencyKey } },
    );
  }

  /**
   * Reads the value at `path` of your application data for the artifact
   * (decision 0031), with the data space's `data_version`, which is the
   * `base_data_version` of the next write. Without a `path`, or with the empty
   * path, it reads the root, which always exists. Nothing at the path is not
   * an error: the answer has `exists: false`. Rejects with `invalid_request`
   * for a path that breaks the grammar, and with `not_found` for an artifact
   * that is missing, in the trash, purged, or someone else's. Previews and
   * runtime frames cannot read this data.
   */
  async getData(
    artifactId: string,
    path: string | undefined = undefined,
    options: ReadOptions = {},
  ): Promise<AppData> {
    const query =
      path === undefined || path === ''
        ? ''
        : `?path=${encodeURIComponent(queryDataPath(path))}`;
    return this.json(
      'GET',
      `/v1/artifacts/${encodeURIComponent(artifactId)}/data${query}`,
      parseAppData,
      { signal: options.signal },
    );
  }

  /** Lists one page of names at a data path (decision 0040). */
  async listDataMembers(
    artifactId: string,
    options: DataMembersOptions = {},
  ): Promise<DataMembers> {
    const { signal } = options;
    signal?.throwIfAborted();
    const query = new URLSearchParams();
    if (options.path !== undefined)
      query.set('path', queryDataPath(options.path));
    if (options.limit !== undefined) query.set('limit', String(options.limit));
    if (options.pageToken !== undefined)
      query.set('page_token', options.pageToken);
    if (options.atDataVersion !== undefined)
      query.set('at_data_version', String(options.atDataVersion));
    const suffix = query.size === 0 ? '' : `?${query.toString()}`;
    const path = `/v1/artifacts/${encodeURIComponent(artifactId)}/data/members${suffix}`;
    try {
      const response = await this.send('GET', path, {
        signal,
        boundedErrorBody: true,
      });
      const body = await readSuccessJson(response, {
        clock: this.clock,
        signal,
        status: response.status,
        what: 'data members response',
        keepCause: this.address.plain,
      });
      signal?.throwIfAborted();
      const result = parseDataMembers(body);
      if (result === undefined || result.artifact_id !== artifactId)
        throw new ArtifreeApiError(
          'Data members response does not match the contract.',
          {
            status: response.status,
            code: 'invalid_response',
          },
        );
      return result;
    } catch (error: unknown) {
      signal?.throwIfAborted();
      throw error;
    }
  }

  /** Follows an application-data path until aborted or unavailable. */
  async followData(
    artifactId: string,
    options: FollowDataOptions,
  ): Promise<void> {
    if (options.signal.aborted) return;
    // Loading the optional follower may itself take time (for example while
    // the browser fetches its chunk). Cancellation must not wait for it.
    let onAbort = () => {};
    const aborted = new Promise<undefined>((resolve) => {
      onAbort = () => resolve(undefined);
      options.signal.addEventListener('abort', onAbort, { once: true });
    });
    let module: typeof import('./follow-data.js') | undefined;
    try {
      module = await Promise.race([import('./follow-data.js'), aborted]);
    } finally {
      options.signal.removeEventListener('abort', onAbort);
    }
    if (module === undefined || options.signal.aborted) return;
    const { followData } = module;
    return followData(
      {
        clock: this.clock,
        open: (signal) => this.openChanges({ signal }),
        read: (signal) =>
          this.readFollowedData(artifactId, options.path, signal),
      },
      artifactId,
      options,
    );
  }

  private async readFollowedData(
    artifactId: string,
    dataPath: string | undefined,
    signal: AbortSignal,
  ): Promise<AppData> {
    signal.throwIfAborted();
    const query =
      dataPath === undefined || dataPath === ''
        ? ''
        : `?path=${encodeURIComponent(queryDataPath(dataPath))}`;
    const path = `/v1/artifacts/${encodeURIComponent(artifactId)}/data${query}`;
    try {
      const response = await this.send('GET', path, {
        signal,
        boundedErrorBody: true,
      });
      const body = await readSuccessJson(response, {
        clock: this.clock,
        signal,
        status: response.status,
        what: 'data response',
        keepCause: this.address.plain,
      });
      signal.throwIfAborted();
      const result = parseFollowedAppData(body);
      if (result === undefined || result.artifact_id !== artifactId)
        throw new ArtifreeApiError(
          'Data response does not match the contract.',
          {
            status: response.status,
            code: 'invalid_response',
          },
        );
      return result;
    } catch (error: unknown) {
      signal.throwIfAborted();
      throw error;
    }
  }

  /**
   * Replaces the value at `request.path`, making the objects above it that
   * are missing, if the data is still at `request.base_data_version`. The
   * value can be `null`, which a merge patch cannot store. At the root it
   * must be an object. Rejects with `data_conflict` and `currentDataVersion`
   * when another write won, `data_path_conflict` when a value that is not an
   * object is in the way, `data_too_large`, `invalid_request` (path, JSON, or
   * a rule of the value), and `request_key_reused`. Writing what is already
   * there changes nothing and keeps the version (`changed: false`). A retry
   * with the same key and request resolves with the original answer and
   * `replayed: true`. Nothing keeps the value that is replaced.
   */
  async setData(
    artifactId: string,
    request: SetDataInput,
    options: MutationOptions,
  ): Promise<DataWriteResponse> {
    return this.dataWrite(
      'PUT',
      artifactId,
      dataBodyText(request, 'value', request.value),
      options,
    );
  }

  /**
   * Applies a JSON Merge Patch (RFC 7396) at `request.path`: objects merge
   * member by member, a member set to `null` is removed, and arrays and
   * scalars replace what is there. A `null` patch removes the value at the
   * path. The rules and rejections are those of {@link setData}.
   */
  async updateData(
    artifactId: string,
    request: UpdateDataInput,
    options: MutationOptions,
  ): Promise<DataWriteResponse> {
    return this.dataWrite(
      'PATCH',
      artifactId,
      dataBodyText(request, 'patch', request.patch),
      options,
    );
  }

  /**
   * Removes the value at `request.path`; the empty path empties the whole
   * space. Removing what is not there succeeds with `changed: false`. The
   * rules and rejections are those of {@link setData}.
   */
  async deleteData(
    artifactId: string,
    request: DeleteDataRequest,
    options: MutationOptions,
  ): Promise<DataWriteResponse> {
    checkBase(request.base_data_version);
    return this.dataWrite(
      'DELETE',
      artifactId,
      JSON.stringify(request),
      options,
    );
  }

  private async dataWrite(
    method: string,
    artifactId: string,
    text: string,
    options: MutationOptions,
  ): Promise<DataWriteResponse> {
    return this.json(
      method,
      `/v1/artifacts/${encodeURIComponent(artifactId)}/data`,
      parseDataWrite,
      {
        mutation: {
          body: undefined,
          text,
          idempotencyKey: options.idempotencyKey,
        },
      },
    );
  }

  /**
   * Queues a verification job (decision 0030) for `request.revision` of the
   * artifact: a worker process re-reads every file of that revision and
   * compares its digest and size with the stored listing. This only queues
   * it; no worker runs by itself, and nothing is pushed when it ends. Read
   * the job again with {@link getJob}. A retry with the same key and request
   * resolves with the job as it is now and `replayed: true`; the same key
   * with another request rejects with `request_key_reused`. Rejects with
   * `not_found` for an artifact that is missing, in the trash, purged, or
   * someone else's, and for a revision that does not exist, and with
   * `too_many_jobs` while 16 jobs wait or run.
   */
  async enqueueJob(
    artifactId: string,
    request: EnqueueJobRequest,
    options: MutationOptions,
  ): Promise<EnqueueJobResponse> {
    return this.json(
      'POST',
      `/v1/artifacts/${encodeURIComponent(artifactId)}/jobs`,
      parseEnqueueJob,
      { mutation: { body: request, idempotencyKey: options.idempotencyKey } },
    );
  }

  /** One job. Rejects with `not_found` unless it is yours and its artifact is neither in the trash nor purged. */
  async getJob(id: string, options: ReadOptions = {}): Promise<Job> {
    return this.json('GET', `/v1/jobs/${encodeURIComponent(id)}`, parseJob, {
      signal: options.signal,
    });
  }

  /**
   * One page of jobs, newest first: those of `artifactId`, or, without it,
   * those of every artifact that is not in the trash. Pass `next_page_token`
   * back as `page_token` for the next page.
   */
  async listJobs(
    query: ListJobsQuery = {},
    options: ReadOptions & { readonly artifactId?: string } = {},
  ): Promise<JobList> {
    const parameters = new URLSearchParams();
    for (const [name, value] of Object.entries(query))
      if (value !== undefined && value !== null)
        parameters.set(name, String(value));
    const search = parameters.toString();
    const path =
      options.artifactId === undefined
        ? '/v1/jobs'
        : `/v1/artifacts/${encodeURIComponent(options.artifactId)}/jobs`;
    return this.json(
      'GET',
      search === '' ? path : `${path}?${search}`,
      parseJobList,
      { signal: options.signal },
    );
  }

  /**
   * Asks to stop a job. One that has not started ends `cancelled` at once;
   * a running one stays `running` with `cancel_requested` until its worker
   * stops at its next checkpoint. Cancelling stops work and rolls nothing
   * back. Rejects with `job_ended` for a job that already ended.
   */
  async cancelJob(
    id: string,
    options: MutationOptions,
  ): Promise<CancelJobResponse> {
    const request: CancelJobRequest = {};
    return this.json(
      'POST',
      `/v1/jobs/${encodeURIComponent(id)}/cancel`,
      parseCancelJob,
      { mutation: { body: request, idempotencyKey: options.idempotencyKey } },
    );
  }

  /**
   * The dry run of {@link removeUnreferencedContent}: how many contents, and
   * how many bytes, no revision file of any artifact refers to, and the
   * `digest` that identifies this set. Removes nothing. Needs the owner's
   * access token: an issued agent token rejects with `forbidden`.
   */
  async getUnreferencedContent(
    options: ReadOptions = {},
  ): Promise<UnreferencedContent> {
    return this.json(
      'GET',
      '/v1/content/unreferenced',
      parseUnreferencedContent,
      { signal: options.signal },
    );
  }

  /**
   * Deletes the content no revision file refers to if it is still exactly the
   * set that a dry run reported (`base_digest`), and resolves with what was
   * deleted. Rejects with `content_conflict`, deleting nothing, when the set
   * has changed since the dry run; review it again. A retry with the same key
   * and `base_digest` resolves with the first run's numbers and
   * `replayed: true`, also when nothing is left to delete; the same key with
   * another `base_digest` rejects with `request_key_reused`. Needs the owner's
   * access token: an issued agent token rejects with `forbidden`.
   */
  async removeUnreferencedContent(
    request: RemoveUnreferencedContentRequest,
    options: MutationOptions,
  ): Promise<RemoveUnreferencedContentResponse> {
    return this.json(
      'DELETE',
      '/v1/content/unreferenced',
      parseRemoveUnreferencedContent,
      { mutation: { body: request, idempotencyKey: options.idempotencyKey } },
    );
  }

  /** Raw bytes of one file in a pinned revision. */
  async readFile(
    id: string,
    revision: number,
    path: string,
    options: ReadOptions = {},
  ): Promise<Uint8Array<ArrayBuffer>> {
    const response = await this.send(
      'GET',
      `/v1/artifacts/${encodeURIComponent(id)}/revisions/${revision}/files/${filePath(path)}`,
      { signal: options.signal },
    );
    return this.bytes(response, path);
  }

  /**
   * One byte range of a pinned file (decision 0016): `length` bytes from
   * `offset`, or to the end of the file without `length`, cut at bytes
   * whatever the file holds. A range that ends past the end of the file
   * stops there; one that starts at or past it rejects with
   * `range_not_satisfiable`, so an empty file has no parts. Resolves only
   * with exactly the bytes asked for and rejects with `invalid_response`
   * otherwise, also when the response is the whole file.
   */
  async readFilePart(
    id: string,
    revision: number,
    path: string,
    range: FileRange,
    options: ReadOptions = {},
  ): Promise<FilePart> {
    const { offset, length } = range;
    if (!rangeIsValid(range))
      throw new ArtifreeApiError(
        `Invalid range ${JSON.stringify(range)}: the offset and length must be whole numbers, the length at least 1, with the last position at most ${Number.MAX_SAFE_INTEGER}.`,
        { status: 0, code: 'invalid_request' },
      );
    const last = length === undefined ? '' : `${offset + length - 1}`;
    const response = await this.send(
      'GET',
      `/v1/artifacts/${encodeURIComponent(id)}/revisions/${revision}/files/${filePath(path)}`,
      { headers: { range: `bytes=${offset}-${last}` }, signal: options.signal },
    );
    const notThePart = () =>
      new ArtifreeApiError(
        `GET ${path} did not return bytes ${offset}-${last} of the file.`,
        { status: response.status, code: 'invalid_response' },
      );
    // Anything else, such as the whole file from a server or proxy that
    // ignored the range, is not read.
    const part =
      response.status === 206
        ? statedPart(range, response.headers.get('content-range'))
        : undefined;
    if (part === undefined) {
      await response.body?.cancel().catch(() => undefined);
      throw notThePart();
    }
    const content = await this.bytes(response, path);
    if (content.length !== part.length) throw notThePart();
    return { content, offset, size: part.size };
  }

  /**
   * The source export of a pinned revision (decision 0015): a ZIP archive
   * of its files at their package paths, with the manifest
   * `.artifree-export.json` first. Rejects with `invalid_response` when a
   * successful response is not a ZIP archive.
   */
  async exportRevision(
    id: string,
    revision: number,
    options: ReadOptions = {},
  ): Promise<Uint8Array<ArrayBuffer>> {
    const path = `/v1/artifacts/${encodeURIComponent(id)}/revisions/${revision}/export`;
    const response = await this.send('GET', path, { signal: options.signal });
    const type = response.headers.get('content-type')?.split(';')[0]?.trim();
    if (type?.toLowerCase() !== 'application/zip') {
      await response.body?.cancel().catch(() => undefined);
      throw new ArtifreeApiError(`GET ${path} did not return a ZIP archive.`, {
        status: response.status,
        code: 'invalid_response',
      });
    }
    return this.bytes(response, `the export of revision ${revision}`);
  }

  /**
   * Opens the change feed once. Resolves when the server accepted the
   * stream, with its events until the server ends it or `signal` aborts.
   * Without `after` the stream starts at the head with `ready`. Rejects with
   * `invalid_response` when a successful response is not an event stream.
   * Use {@link followChanges} to reconnect automatically.
   */
  async openChanges(
    options: { readonly after?: string; readonly signal?: AbortSignal } = {},
  ): Promise<AsyncGenerator<ChangeFeedEvent>> {
    const headers: Record<string, string> = { accept: 'text/event-stream' };
    if (options.after !== undefined) headers['last-event-id'] = options.after;
    const response = await this.send('GET', '/v1/changes', {
      headers,
      // Firefox serializes concurrent default-cache fetches of the same SSE
      // URL until the first response ends, which a live stream need not do.
      cache: 'no-store',
      signal: options.signal,
      boundedErrorBody: true,
    });
    const mediaType = response.headers
      .get('content-type')
      ?.split(';')[0]
      ?.trim()
      .toLowerCase();
    if (mediaType !== 'text/event-stream' || response.body === null) {
      if (response.body !== null) cancelQuietly(response.body);
      throw new ArtifreeApiError(
        `GET /v1/changes answered HTTP ${response.status} with ${mediaType ?? 'no content type'}, not an event stream.`,
        { status: response.status, code: 'invalid_response' },
      );
    }
    // The stall limit runs from the headers, whether or not the caller
    // reads soon; it and the abort listener are released when it passes.
    const body = new BodyReader(response.body, {
      clock: this.clock,
      signal: options.signal,
      status: 0,
      what: 'change feed',
      keepCause: this.address.plain,
    });
    return changeFeedEvents(decodeText(body), options.signal);
  }

  /**
   * Follows the change feed until `options.signal` aborts, reconnecting
   * from the last cursor after a lost connection, a 429, or a 5xx, so no
   * committed change is skipped. Rejects on a failure that reconnecting
   * cannot fix, such as a rejected token or a response that is not the
   * feed.
   */
  async followChanges(options: FollowOptions): Promise<void> {
    return follow(
      (after, signal) =>
        this.openChanges(after === undefined ? { signal } : { after, signal }),
      options,
      this.clock,
    );
  }

  /**
   * A short-lived URL rendering a pinned revision on the content origin.
   * Issuing one changes nothing else, so it is cancellable like a read.
   */
  async createPreview(
    id: string,
    revision: number,
    options: ReadOptions = {},
  ): Promise<Preview> {
    return this.json(
      'POST',
      `/v1/artifacts/${encodeURIComponent(id)}/revisions/${revision}/previews`,
      parsePreview,
      { signal: options.signal },
    );
  }

  /** Lists access tokens. Requires administrative privileges. */
  async listTokens(options: ReadOptions = {}): Promise<TokenList> {
    return this.json('GET', '/v1/tokens', parseTokenList, {
      signal: options.signal,
    });
  }

  /** Creates a new agent access token. Requires administrative privileges. */
  async createToken(
    request: CreateTokenRequest,
    options: ReadOptions = {},
  ): Promise<CreateTokenResponse> {
    return this.json('POST', '/v1/tokens', parseCreateToken, {
      body: request,
      signal: options.signal,
    });
  }

  /** Rotates an agent access token. Requires administrative privileges. */
  async rotateToken(
    id: string,
    options: ReadOptions = {},
  ): Promise<RotateTokenResponse> {
    return this.json(
      'POST',
      `/v1/tokens/${encodeURIComponent(id)}/rotate`,
      parseRotateToken,
      {
        signal: options.signal,
      },
    );
  }

  /** Revokes an agent access token. Requires administrative privileges. */
  async revokeToken(id: string, options: ReadOptions = {}): Promise<void> {
    const response = await this.send(
      'DELETE',
      `/v1/tokens/${encodeURIComponent(id)}`,
      { signal: options.signal },
    );
    if (response.status !== 204)
      throw new ArtifreeApiError('The server did not confirm the revocation.', {
        status: response.status,
        code: 'invalid_response',
      });
  }

  /** Lists registered agent inboxes for the current workspace connection. */
  async agentSessions(options: ReadOptions = {}): Promise<AgentSession[]> {
    if (this.runtimeClosing) throw new RuntimeConnectionReplacedError();
    options.signal?.throwIfAborted();
    const closeAttempt = this.runtimeCloseAttempt;
    const request = (session: string): Promise<AgentSession[]> =>
      this.json(
        'GET',
        `/v1/agent-sessions?workspace_session_id=${encodeURIComponent(session)}`,
        parseAgentSessions,
        { signal: options.signal },
      );
    let session = this.runtimeSessionId;
    let sessions: AgentSession[];
    try {
      sessions = await request(session);
    } catch (error: unknown) {
      options.signal?.throwIfAborted();
      if (this.runtimeClosing || this.runtimeCloseAttempt !== closeAttempt)
        throw new RuntimeConnectionReplacedError();
      if (!sessionExpired(error)) throw error;
      try {
        await this.recoverRuntimeSession(session);
        options.signal?.throwIfAborted();
        if (this.runtimeClosing || this.runtimeCloseAttempt !== closeAttempt)
          throw new RuntimeConnectionReplacedError();
        session = this.runtimeSessionId;
        sessions = await request(session);
      } catch (retryError: unknown) {
        options.signal?.throwIfAborted();
        throw retryError;
      }
    }
    options.signal?.throwIfAborted();
    if (
      this.runtimeClosing ||
      this.runtimeCloseAttempt !== closeAttempt ||
      this.runtimeSession !== session
    )
      throw new RuntimeConnectionReplacedError();
    return sessions;
  }

  /** Stages a saved source selection; this does not send it to the agent. */
  async stageHandoff(request: StageHandoffRequest): Promise<Handoff> {
    if (
      this.runtimeClosing ||
      this.runtimeSessionId !== request.workspace_session_id
    )
      throw new RuntimeConnectionReplacedError();
    const response = await this.send('POST', '/v1/handoffs', { body: request });
    const handoff = parseHandoff(await readJson(response, this.address.plain));
    const selection = handoff?.selection;
    if (
      response.status !== 201 ||
      handoff === undefined ||
      selection === undefined ||
      handoff.state !== 'staged' ||
      handoff.agent_session_id !== request.agent_session_id ||
      handoff.intent !== request.intent ||
      selection.artifact_id !== request.artifact_id ||
      selection.revision !== request.revision ||
      selection.path !== request.path ||
      selection.file_sha256 !== request.file_sha256 ||
      selection.start !== request.start ||
      selection.end !== request.end ||
      this.runtimeClosing ||
      this.runtimeSession !== request.workspace_session_id
    )
      throw new ArtifreeApiError('The staged handoff was not confirmed.', {
        status: response.status,
        code: 'invalid_response',
      });
    return handoff;
  }

  /** Reads a staged delivery's current status, including receipt. */
  async getHandoff(
    handoff: Handoff,
    workspace: string,
    options: ReadOptions = {},
  ): Promise<Handoff> {
    const result = await this.json(
      'GET',
      `/v1/handoffs/${encodeURIComponent(handoff.id)}?workspace_session_id=${encodeURIComponent(workspace)}`,
      parseHandoff,
      { signal: options.signal },
    );
    return this.confirmHandoff(handoff, result);
  }

  /** Sends the same staged identity on every retry; consent is not replayed. */
  async sendHandoff(handoff: Handoff, workspace: string): Promise<Handoff> {
    if (this.runtimeClosing || this.runtimeSessionId !== workspace)
      throw new RuntimeConnectionReplacedError();
    const result = await this.json(
      'POST',
      `/v1/handoffs/${encodeURIComponent(handoff.id)}/send`,
      parseHandoff,
      { body: { workspace_session_id: workspace } },
    );
    this.confirmHandoff(handoff, result);
    if (
      this.runtimeClosing ||
      this.runtimeSession !== workspace ||
      !['queued', 'offered', 'blocked', 'acknowledged'].includes(result.state)
    )
      throw new ArtifreeApiError('The handoff send was not confirmed.', {
        status: 200,
        code: 'invalid_response',
      });
    return result;
  }

  /** Cancels a proposal only if the server confirms it was not offered. */
  async cancelHandoff(handoff: Handoff, workspace: string): Promise<Handoff> {
    const result = await this.json(
      'POST',
      `/v1/handoffs/${encodeURIComponent(handoff.id)}/cancel`,
      parseHandoff,
      { body: { workspace_session_id: workspace } },
    );
    this.confirmHandoff(handoff, result);
    if (result.state !== 'cancelled')
      throw new ArtifreeApiError(
        'The handoff cancellation was not confirmed.',
        {
          status: 200,
          code: 'invalid_response',
        },
      );
    return result;
  }

  private confirmHandoff(expected: Handoff, result: Handoff): Handoff {
    if (!sameHandoff(expected, result))
      throw new ArtifreeApiError('The server returned a different handoff.', {
        status: 200,
        code: 'invalid_response',
      });
    return result;
  }

  /** Issues an ephemeral runtime view. The account token stays in this client. */
  async createRuntimeView(
    id: string,
    revision: number,
    viewId: string,
    generation: number,
    options: RuntimeViewOptions = {},
  ): Promise<RuntimeView> {
    if (this.runtimeClosing) throw new RuntimeConnectionReplacedError();
    const request = (session: string): Promise<RuntimeView> =>
      this.json(
        'POST',
        `/v1/artifacts/${encodeURIComponent(id)}/revisions/${revision}/runtime-views`,
        parseRuntimeView,
        {
          body: {
            session_id: session,
            view_id: viewId,
            generation,
          },
          signal: options.signal,
          onDispatch: options.onDispatch,
        },
      );
    let session = this.runtimeSessionId;
    let view: RuntimeView;
    try {
      view = await request(session);
    } catch (error: unknown) {
      if (!sessionExpired(error)) throw error;
      await this.recoverRuntimeSession(session);
      options.signal?.throwIfAborted();
      session = this.runtimeSessionId;
      view = await request(session);
    }
    if (
      view.session_id !== session ||
      this.runtimeSession !== session ||
      this.runtimeClosing ||
      view.artifact_id !== id ||
      view.revision !== revision ||
      view.view_id !== viewId ||
      view.generation !== generation
    )
      throw new ArtifreeApiError(
        'The server returned a runtime view for different inputs.',
        {
          status: 201,
          code: 'invalid_response',
        },
      );
    return view;
  }

  /** Opens the runtime connection before requesting previews. */
  async openRuntimeSession(options: ReadOptions = {}): Promise<void> {
    if (this.runtimeClosing) throw new RuntimeConnectionReplacedError();
    if (this.runtimeSession !== undefined) return;
    await this.establishRuntimeSession(undefined, options);
  }

  private async establishRuntimeSession(
    previous: string | undefined,
    options: ReadOptions,
  ): Promise<void> {
    const attempt = ++this.runtimeOpenAttempt;
    const connection = await this.json(
      'POST',
      '/v1/runtime-sessions',
      parseRuntimeConnection,
      { signal: options.signal },
    );
    options.signal?.throwIfAborted();
    if (
      attempt !== this.runtimeOpenAttempt ||
      this.runtimeSession !== previous ||
      this.runtimeClosing
    )
      throw new RuntimeConnectionReplacedError();
    if (previous !== undefined && connection.session_id === previous)
      throw new ArtifreeApiError(
        'The server returned the expired runtime connection again.',
        { status: 200, code: 'invalid_response' },
      );
    this.runtimeSession = connection.session_id;
    this.runtimeConnectionEpoch += 1;
    this.runtimeGrants.clear();
  }

  /** Replaces only a confirmed expired connection, once for all its views. */
  private async recoverRuntimeSession(previous: string): Promise<void> {
    if (this.runtimeClosing) throw new RuntimeConnectionReplacedError();
    if (this.runtimeSession !== previous) {
      if (this.runtimeSession === undefined)
        throw new RuntimeConnectionReplacedError();
      return;
    }
    if (this.runtimeRecovery?.session === previous)
      return this.runtimeRecovery.promise;
    const promise: Promise<void> = this.establishRuntimeSession(
      previous,
      {},
    ).finally(() => {
      if (this.runtimeRecovery?.promise === promise)
        this.runtimeRecovery = undefined;
    });
    this.runtimeRecovery = { session: previous, promise };
    return promise;
  }

  /** Closes the connection and its grants; retry until the server acknowledges. */
  async closeRuntimeSession(): Promise<void> {
    this.runtimeOpenAttempt += 1;
    this.runtimeCloseAttempt += 1;
    this.runtimeClosing = true;
    const session = this.runtimeSession;
    if (session !== undefined)
      await this.close(`/v1/runtime-sessions/${session}`);
    if (this.runtimeSession === session) {
      this.runtimeSession = undefined;
      this.runtimeGrants.clear();
    }
    this.runtimeClosing = false;
  }

  private requireViewSession(view: Pick<RuntimeView, 'session_id'>): string {
    const session = this.runtimeSessionId;
    if (this.runtimeClosing || view.session_id !== session)
      throw new RuntimeConnectionReplacedError();
    return session;
  }

  /** Replace a grant against the source and version the user observed. */
  async setRuntimeGrant(
    view: RuntimeView,
    capabilities: string[],
  ): Promise<RuntimeGrant> {
    const session = this.requireViewSession(view);
    const wasUnknown = this.runtimeGrants.begin(view, capabilities);
    try {
      const grant = await this.json(
        'PUT',
        `/v1/runtime-sessions/${session}/artifacts/${encodeURIComponent(view.artifact_id)}/revisions/${view.revision}/grant`,
        parseRuntimeGrant,
        {
          body: {
            head_revision: view.head_revision,
            expected_version: view.grant_version,
            capabilities,
          },
        },
      );
      if (grant.version <= view.grant_version)
        throw new ArtifreeApiError(
          'The server did not advance the runtime grant version.',
          {
            status: 200,
            code: 'invalid_response',
          },
        );
      this.requireViewSession(view);
      this.runtimeGrants.finish(view, false);
      return grant;
    } catch (error: unknown) {
      let failure = error;
      if (sessionExpired(error)) {
        try {
          await this.recoverRuntimeSession(session);
        } catch (recoveryError: unknown) {
          failure = recoveryError;
        }
      }
      if (this.runtimeSession === session)
        this.runtimeGrants.finish(
          view,
          wasUnknown || grantMayHaveChanged(error),
        );
      throw failure;
    }
  }

  /** Reads current metadata through the parent workspace, rechecking the binding. */
  async runtimeMetadata(
    view: RuntimeView,
    options: ReadOptions = {},
  ): Promise<RuntimeMetadata> {
    this.requireViewSession(view);
    const metadata = await this.json(
      'POST',
      `/v1/artifacts/${encodeURIComponent(view.artifact_id)}/revisions/${view.revision}/runtime-views/${view.view_id}/${view.generation}/metadata`,
      parseRuntimeMetadata,
      { body: { initialization: view.initialization }, signal: options.signal },
    );
    this.requireViewSession(view);
    if (metadata.artifact_id !== view.artifact_id)
      throw new ArtifreeApiError(
        'The server returned metadata for another artifact.',
        {
          status: 200,
          code: 'invalid_response',
        },
      );
    return metadata;
  }

  /**
   * Runs one source call of a runtime view. The calls and their checks are
   * loaded on first use, since only a page that shows a runtime view needs
   * them; a module that cannot be loaded is a failed request, like any other
   * the client reports. They are the calls a frame causes, sent here on its
   * behalf: the frame holds no account token.
   *
   * Public so that a call used by one caller only, such as the save of a
   * runtime host (decision 0033), can live in the on-demand module instead of
   * adding a method here: this class is in the main bundle, which has no room.
   */
  async runtimeCall<T>(
    view: Pick<RuntimeView, 'session_id'>,
    run: (
      source: typeof RuntimeSource,
      frame: RuntimeSource.FrameTransport,
    ) => Promise<T>,
  ): Promise<T> {
    this.requireViewSession(view);
    let source: typeof RuntimeSource;
    try {
      source = await import('./runtime-source.js');
    } catch (cause: unknown) {
      throw new ArtifreeApiError('The runtime checks could not be loaded.', {
        status: 0,
        code: 'network_error',
        cause,
      });
    }
    const check = (): void => {
      this.requireViewSession(view);
    };
    return run(source, {
      check,
      json: async (path, body, parse, signal) => {
        check();
        const result = await this.json('POST', path, parse, { body, signal });
        check();
        return result;
      },
      post: (path, body, signal) => this.send('POST', path, { body, signal }),
      bytes: (response, what) => this.bytes(response, what),
    });
  }

  /**
   * The files of the revision a view is pinned to (decision 0032). The
   * server checks the view, the `source.read` grant and the artifact's head
   * again; `403` is a missing grant, `409` a view the head has outgrown.
   */
  async runtimeSourceList(
    view: RuntimeView,
    options: ReadOptions = {},
  ): Promise<RuntimeSourceList> {
    return this.runtimeCall(view, (source, frame) =>
      source.listSource(frame, view, options.signal),
    );
  }

  /**
   * One part of a file of a view's pinned revision. It resolves only with a
   * part that echoes `request`, whose bytes number what its offsets say.
   */
  async runtimeSourceRead(
    view: RuntimeView,
    request: RuntimeSourceRequest,
    options: ReadOptions = {},
  ): Promise<RuntimeSourcePartBytes> {
    return this.runtimeCall(view, (source, frame) =>
      source.readSource(frame, view, request, options.signal),
    );
  }

  /**
   * Asks the server whether a download may be offered to the user and what
   * it would be, before any prompt is shown. Nothing is handed over.
   */
  async runtimeSourceOffer(
    view: RuntimeView,
    target: RuntimeDownloadTarget,
    options: ReadOptions = {},
  ): Promise<RuntimeDownloadOffer> {
    return this.runtimeCall(view, (source, frame) =>
      source.offerDownload(frame, view, target, options.signal),
    );
  }

  /**
   * The bytes of a download the user confirmed. Resolves only when the
   * response is the attachment `offer` named, with the closed content type
   * of its kind, and the bytes are what the offer promised.
   */
  async runtimeSourceDownload(
    view: RuntimeView,
    offer: RuntimeDownloadOffer,
    options: ReadOptions = {},
  ): Promise<Uint8Array<ArrayBuffer>> {
    return this.runtimeCall(view, (source, frame) =>
      source.fetchDownload(frame, view, offer, options.signal),
    );
  }

  /** Closes one view generation; an older close cannot revoke its replacement. */
  async closeRuntimeView(
    view: Pick<RuntimeView, 'view_id' | 'generation'>,
  ): Promise<void> {
    await this.close(`/v1/runtime-views/${view.view_id}/${view.generation}`);
  }

  /** A close is confirmed only by the contract's no-content response. */
  private async close(path: string): Promise<void> {
    const response = await this.send('DELETE', path);
    if (response.status !== 204)
      throw new ArtifreeApiError('The server did not confirm the close.', {
        status: response.status,
        code: 'invalid_response',
      });
  }

  /** The whole body; a failure while reading is a `network_error`. */
  private async bytes(
    response: Response,
    what: string,
  ): Promise<Uint8Array<ArrayBuffer>> {
    try {
      return new Uint8Array(await response.arrayBuffer());
    } catch (error: unknown) {
      throw new ArtifreeApiError(
        `The connection to ${this.address.label} ended while reading ${what}.`,
        {
          status: response.status,
          code: 'network_error',
          ...causeOf(this.address.plain, error),
        },
      );
    }
  }

  /**
   * Sends one request and parses its JSON answer, failing with the client's
   * errors. Public so that a call used by few callers can live in a module of
   * its own instead of adding a method here (see `editFiles`): this class is
   * in the main bundle, which has no room. Prefer the named methods.
   */
  async json<T>(
    method: string,
    path: string,
    parse: (value: unknown) => T | undefined,
    request: Omit<SendOptions, 'headers'> = {},
  ): Promise<T> {
    const response = await this.send(method, path, request);
    if (
      request.expectStatus !== undefined &&
      response.status !== request.expectStatus
    ) {
      if (response.body !== null) cancelQuietly(response.body);
      throw new ArtifreeApiError(
        `${method} ${path} answered HTTP ${response.status}, not ${request.expectStatus}.`,
        { status: response.status, code: 'invalid_response' },
      );
    }
    const value =
      request.successBodyLimit !== undefined
        ? await readSuccessJson(
            response,
            {
              clock: this.clock,
              signal: request.signal,
              status: response.status,
              what: 'API response',
              keepCause: this.address.plain,
            },
            request.successBodyLimit,
          )
        : await readJson(response, this.address.plain);
    const parsed = parse(value);
    if (parsed === undefined)
      throw new ArtifreeApiError(
        `${method} ${path} returned a body that does not match the contract.`,
        { status: response.status, code: 'invalid_response' },
      );
    return parsed;
  }

  /**
   * Reads an authenticated resource within `limit` bytes without redirects or
   * caching. Used by the separate asset adapter, which checks MIME and digest.
   * A lost, stalled or aborted body is a network failure; overflow is an
   * invalid response. Neither case is retried automatically.
   */
  async boundedBytes(
    path: string,
    limit: number,
    options: ReadOptions = {},
  ): Promise<{ bytes: Uint8Array<ArrayBuffer>; contentType: string | null }> {
    if (!Number.isSafeInteger(limit) || limit < 0)
      throw new RangeError(
        'The byte limit must be a safe nonnegative integer.',
      );
    const response = await this.send('GET', path, {
      cache: 'no-store',
      redirect: 'error',
      boundedErrorBody: true,
      errorBodyLimit: 4096,
      signal: options.signal,
    });
    const bytes = await readSuccessBytes(response, limit, {
      clock: this.clock,
      signal: options.signal,
      status: response.status,
      what: 'resource content',
      keepCause: this.address.plain,
    });
    return { bytes, contentType: response.headers.get('content-type') };
  }

  private async send(
    method: string,
    path: string,
    {
      mutation,
      body,
      headers: extraHeaders,
      cache,
      signal,
      onDispatch,
      boundedErrorBody,
      errorBodyLimit,
      redirect,
    }: SendOptions = {},
  ): Promise<Response> {
    const headers: Record<string, string> = {
      ...extraHeaders,
      authorization: `Bearer ${this.accessToken}`,
    };
    const init: DispatchRequestInit = { method, headers };
    if (redirect !== undefined) init.redirect = redirect;
    if (cache !== undefined) init.cache = cache;
    if (signal !== undefined) init.signal = signal;
    if (onDispatch !== undefined) init[REQUEST_DISPATCH] = onDispatch;
    if (mutation !== undefined) {
      headers['content-type'] = 'application/json';
      headers['idempotency-key'] = mutation.idempotencyKey;
      init.body = mutation.text ?? JSON.stringify(mutation.body);
    } else if (body !== undefined) {
      headers['content-type'] = 'application/json';
      init.body = JSON.stringify(body);
    }
    let response: Response;
    try {
      response = await this.fetchImpl(`${this.baseUrl}${path}`, init);
    } catch (error: unknown) {
      throw new ArtifreeApiError(
        `Could not reach the Artifree API at ${this.address.label}.`,
        {
          status: 0,
          code: 'network_error',
          ...causeOf(this.address.plain, error),
        },
      );
    }
    if (response.ok) return response;
    // Also a proxy's error page may say when to retry.
    const retryAfter = parseRetryAfter(response.headers.get('retry-after'));
    const answer =
      boundedErrorBody === true
        ? await readErrorJson(
            response,
            {
              clock: this.clock,
              signal,
              status: response.status,
              what: 'error response',
              keepCause: this.address.plain,
            },
            errorBodyLimit,
          )
        : await readJson(response, this.address.plain);
    const failure = parseError(answer);
    // HTTP 429 means too many requests whoever answered it, so a proxy's
    // refusal without an API body is a rate limit too.
    const limited = response.status === 429;
    if (failure === undefined)
      throw new ArtifreeApiError(
        `${method} ${path} failed with HTTP ${response.status}.`,
        {
          status: response.status,
          code: limited ? 'rate_limited' : 'invalid_response',
          ...(retryAfter === undefined
            ? {}
            : { retryAfterSeconds: retryAfter }),
        },
      );
    throw new ArtifreeApiError(failure.message, {
      status: response.status,
      code: limited ? 'rate_limited' : failure.code,
      body: answer,
      currentRevision: failure.current_revision ?? undefined,
      currentMetadataRevision: failure.current_metadata_revision ?? undefined,
      currentDataVersion: failure.current_data_version ?? undefined,
      ...(retryAfter === undefined ? {} : { retryAfterSeconds: retryAfter }),
    });
  }
}

/** Whole seconds of a `Retry-After` header; HTTP dates are not used here. */
/**
 * The delay a `Retry-After` header asks for, in whole seconds, or undefined
 * when it is absent or not a number of seconds.
 */
export function parseRetryAfter(value: string | null): number | undefined {
  if (value === null || !/^\d{1,6}$/.test(value)) return undefined;
  return Number(value);
}

async function readJson(
  response: Response,
  keepCause: boolean,
): Promise<unknown> {
  let text: string;
  try {
    text = await response.text();
  } catch (error: unknown) {
    // The connection ended mid-body: the outcome of a mutation is unknown.
    throw new ArtifreeApiError(
      'The connection ended while reading a response.',
      {
        status: response.status,
        code: 'network_error',
        ...causeOf(keepCause, error),
      },
    );
  }
  try {
    const value: unknown = JSON.parse(text);
    return value;
  } catch {
    // A non-JSON body fails the caller's contract check instead.
    return undefined;
  }
}
