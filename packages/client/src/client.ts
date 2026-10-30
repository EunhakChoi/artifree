import type {
  Artifact,
  ArtifactList,
  CommitResponse,
  CreateArtifactRequest,
  DuplicateArtifactRequest,
  DuplicateArtifactResponse,
  FileUpload,
  ListArtifactsQuery,
  Preview,
  RestoreRevisionRequest,
  Revision,
  RuntimeMetadata,
  RuntimeView,
  SaveRevisionRequest,
  UpdateArtifactRequest,
  UpdateArtifactResponse,
} from '@artifree/contracts';
import {
  type ChangeFeedEvent,
  type FollowOptions,
  changeFeedEvents,
  follow,
} from './change-feed.js';
import { ArtifreeApiError } from './errors.js';
import {
  parseArtifact,
  parseArtifactList,
  parseCommit,
  parseDuplicate,
  parseError,
  parsePreview,
  parseRevisionList,
  parseRuntimeMetadata,
  parseRuntimeView,
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
}

/** Options for requests that only read. */
export interface ReadOptions {
  /** Cancels the request, also while a `fetch` wrapper holds it back. */
  readonly signal?: AbortSignal;
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

/** A random key suitable for `Idempotency-Key`. */
export function newIdempotencyKey(): string {
  return `key-${crypto.randomUUID()}`;
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

function toBase64(bytes: Uint8Array): string {
  let binary = '';
  const chunk = 0x8000;
  for (let start = 0; start < bytes.length; start += chunk) {
    binary += String.fromCharCode(...bytes.subarray(start, start + chunk));
  }
  return btoa(binary);
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

interface SendOptions {
  readonly mutation?: { body: unknown; idempotencyKey: string } | undefined;
  readonly body?: unknown;
  readonly headers?: Record<string, string>;
  readonly signal?: AbortSignal | undefined;
}

/** HTTP API v1 client. Every method rejects with `ArtifreeApiError`. */
export class ArtifreeClient {
  private readonly baseUrl: string;
  private readonly accessToken: string;
  private readonly fetchImpl: typeof fetch;

  constructor(configuration: ClientConfiguration) {
    this.baseUrl = configuration.baseUrl.replace(/\/+$/, '');
    this.accessToken = configuration.accessToken;
    this.fetchImpl =
      configuration.fetch ?? ((input, init) => fetch(input, init));
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
   * source's title followed by " (Copy)". Only files are copied
   * (`copied`).
   */
  async duplicateArtifact(
    id: string,
    revision: number,
    request: DuplicateArtifactRequest,
    options: MutationOptions,
  ): Promise<DuplicateArtifactResponse> {
    return this.json(
      'POST',
      `/v1/artifacts/${encodeURIComponent(id)}/revisions/${revision}/duplicate`,
      parseDuplicate,
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
        `GET /v1/changes answered HTTP ${response.status} with ${mediaType ?? 'no content type'}, not an event stream.`,
        { status: response.status, code: 'invalid_response' },
      );
    }
    return changeFeedEvents(
      decodeText(response.body, options.signal),
      options.signal,
    );
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

  /** Issues an ephemeral runtime view. The account token stays in this client. */
  async createRuntimeView(
    id: string,
    revision: number,
    viewId: string,
    generation: number,
    options: ReadOptions = {},
  ): Promise<RuntimeView> {
    const view = await this.json(
      'POST',
      `/v1/artifacts/${encodeURIComponent(id)}/revisions/${revision}/runtime-views`,
      parseRuntimeView,
      { body: { view_id: viewId, generation }, signal: options.signal },
    );
    if (
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

  /** Reads current metadata through the parent workspace, rechecking the binding. */
  async runtimeMetadata(
    view: RuntimeView,
    options: ReadOptions = {},
  ): Promise<RuntimeMetadata> {
    const metadata = await this.json(
      'POST',
      `/v1/artifacts/${encodeURIComponent(view.artifact_id)}/revisions/${view.revision}/runtime-views/${view.view_id}/${view.generation}/metadata`,
      parseRuntimeMetadata,
      { body: { initialization: view.initialization }, signal: options.signal },
    );
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

  /** Closes one view generation; an older close cannot revoke its replacement. */
  async closeRuntimeView(view: RuntimeView): Promise<void> {
    await this.send(
      'DELETE',
      `/v1/runtime-views/${view.view_id}/${view.generation}`,
    );
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
        `The connection to ${this.baseUrl || 'this origin'} ended while reading ${what}.`,
        { status: response.status, code: 'network_error', cause: error },
      );
    }
  }

  private async json<T>(
    method: string,
    path: string,
    parse: (value: unknown) => T | undefined,
    request: Omit<SendOptions, 'headers'> = {},
  ): Promise<T> {
    const response = await this.send(method, path, request);
    const parsed = parse(await readJson(response));
    if (parsed === undefined)
      throw new ArtifreeApiError(
        `${method} ${path} returned a body that does not match the contract.`,
        { status: response.status, code: 'invalid_response' },
      );
    return parsed;
  }

  private async send(
    method: string,
    path: string,
    { mutation, body, headers: extraHeaders, signal }: SendOptions = {},
  ): Promise<Response> {
    const headers: Record<string, string> = {
      ...extraHeaders,
      authorization: `Bearer ${this.accessToken}`,
    };
    const init: RequestInit = { method, headers };
    if (signal !== undefined) init.signal = signal;
    if (mutation !== undefined) {
      headers['content-type'] = 'application/json';
      headers['idempotency-key'] = mutation.idempotencyKey;
      init.body = JSON.stringify(mutation.body);
    } else if (body !== undefined) {
      headers['content-type'] = 'application/json';
      init.body = JSON.stringify(body);
    }
    let response: Response;
    try {
      response = await this.fetchImpl(`${this.baseUrl}${path}`, init);
    } catch (error: unknown) {
      throw new ArtifreeApiError(
        `Could not reach the Artifree API at ${this.baseUrl || 'this origin'}.`,
        { status: 0, code: 'network_error', cause: error },
      );
    }
    if (response.ok) return response;
    // Also a proxy's error page may say when to retry.
    const retryAfter = parseRetryAfter(response.headers.get('retry-after'));
    const failure = parseError(await readJson(response));
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
      ...(typeof failure.current_revision === 'number'
        ? { currentRevision: failure.current_revision }
        : {}),
      ...(typeof failure.current_metadata_revision === 'number'
        ? { currentMetadataRevision: failure.current_metadata_revision }
        : {}),
      ...(retryAfter === undefined ? {} : { retryAfterSeconds: retryAfter }),
    });
  }
}

/**
 * The text of a streamed body. A failure while reading, such as a lost
 * connection, is a `network_error`. Stopping early or aborting `signal`
 * cancels the body, which closes its connection, also with a `fetch` that
 * ignores the signal once the response has arrived.
 */
async function* decodeText(
  body: ReadableStream<Uint8Array>,
  signal: AbortSignal | undefined,
): AsyncGenerator<string> {
  const reader = body.getReader();
  const cancel = (): void => {
    reader.cancel().catch(() => undefined);
  };
  signal?.addEventListener('abort', cancel);
  // Reading starts on the first iteration, possibly after an abort.
  if (signal?.aborted === true) cancel();
  // Removes a leading byte order mark, as event streams require.
  const decoder = new TextDecoder();
  try {
    for (;;) {
      let chunk: ReadableStreamReadResult<Uint8Array>;
      try {
        chunk = await reader.read();
      } catch (error: unknown) {
        throw new ArtifreeApiError('The change feed connection was lost.', {
          status: 0,
          code: 'network_error',
          cause: error,
        });
      }
      if (chunk.done) {
        yield decoder.decode();
        return;
      }
      yield decoder.decode(chunk.value, { stream: true });
    }
  } finally {
    signal?.removeEventListener('abort', cancel);
    await reader.cancel().catch(() => undefined);
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

async function readJson(response: Response): Promise<unknown> {
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
        cause: error,
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
