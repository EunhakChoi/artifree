import type {
  Artifact,
  CommitResponse,
  CreateArtifactRequest,
  ErrorCode,
  FileUpload,
  Preview,
  Revision,
  SaveRevisionRequest,
} from '@artifree/contracts';
import {
  parseArtifact,
  parseArtifactList,
  parseCommit,
  parseError,
  parsePreview,
  parseRevisionList,
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

/** Why a request failed: a server error code or a client-side failure. */
export type FailureCode = ErrorCode | 'network_error' | 'invalid_response';

/** A failed API request. */
export class ArtifreeApiError extends Error {
  /** HTTP status, or 0 when no response arrived. */
  readonly status: number;
  readonly code: FailureCode;
  /** The current revision when `code` is `revision_conflict`. */
  readonly currentRevision: number | undefined;

  constructor(
    message: string,
    details: {
      status: number;
      code: FailureCode;
      currentRevision?: number;
      cause?: unknown;
    },
  ) {
    super(message, { cause: details.cause });
    this.name = 'ArtifreeApiError';
    this.status = details.status;
    this.code = details.code;
    this.currentRevision = details.currentRevision;
  }
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

  async listArtifacts(): Promise<Artifact[]> {
    return this.json('GET', '/v1/artifacts', parseArtifactList);
  }

  async getArtifact(id: string): Promise<Artifact> {
    return this.json(
      'GET',
      `/v1/artifacts/${encodeURIComponent(id)}`,
      parseArtifact,
    );
  }

  /** Revision history, newest first. */
  async listRevisions(id: string): Promise<Revision[]> {
    return this.json(
      'GET',
      `/v1/artifacts/${encodeURIComponent(id)}/revisions`,
      parseRevisionList,
    );
  }

  async createArtifact(
    request: CreateArtifactRequest,
    options: MutationOptions,
  ): Promise<CommitResponse> {
    return this.json('POST', '/v1/artifacts', parseCommit, {
      body: request,
      idempotencyKey: options.idempotencyKey,
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
      { body: request, idempotencyKey: options.idempotencyKey },
    );
  }

  /** Raw bytes of one file in a pinned revision. */
  async readFile(
    id: string,
    revision: number,
    path: string,
  ): Promise<Uint8Array> {
    const response = await this.send(
      'GET',
      `/v1/artifacts/${encodeURIComponent(id)}/revisions/${revision}/files/${filePath(path)}`,
    );
    try {
      return new Uint8Array(await response.arrayBuffer());
    } catch (error: unknown) {
      throw new ArtifreeApiError(
        `The connection to ${this.baseUrl || 'this origin'} ended while reading ${path}.`,
        { status: response.status, code: 'network_error', cause: error },
      );
    }
  }

  /** A short-lived URL rendering a pinned revision on the content origin. */
  async createPreview(id: string, revision: number): Promise<Preview> {
    return this.json(
      'POST',
      `/v1/artifacts/${encodeURIComponent(id)}/revisions/${revision}/previews`,
      parsePreview,
    );
  }

  private async json<T>(
    method: string,
    path: string,
    parse: (value: unknown) => T | undefined,
    mutation?: { body: unknown; idempotencyKey: string },
  ): Promise<T> {
    const response = await this.send(method, path, mutation);
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
    mutation?: { body: unknown; idempotencyKey: string },
  ): Promise<Response> {
    const headers: Record<string, string> = {
      authorization: `Bearer ${this.accessToken}`,
    };
    const init: RequestInit = { method, headers };
    if (mutation !== undefined) {
      headers['content-type'] = 'application/json';
      headers['idempotency-key'] = mutation.idempotencyKey;
      init.body = JSON.stringify(mutation.body);
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
    const failure = parseError(await readJson(response));
    if (failure === undefined)
      throw new ArtifreeApiError(
        `${method} ${path} failed with HTTP ${response.status}.`,
        { status: response.status, code: 'invalid_response' },
      );
    throw new ArtifreeApiError(failure.message, {
      status: response.status,
      code: failure.code,
      ...(typeof failure.current_revision === 'number'
        ? { currentRevision: failure.current_revision }
        : {}),
    });
  }
}

async function readJson(response: Response): Promise<unknown> {
  try {
    const value: unknown = await response.json();
    return value;
  } catch {
    // A non-JSON body fails the caller's contract check instead.
    return undefined;
  }
}
