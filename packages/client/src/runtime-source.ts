import type {
  RuntimeDownloadKind,
  RuntimeDownloadOffer,
  RuntimeSourceList,
  RuntimeSourceWriteReceipt,
  RuntimeView,
} from '@artifree/contracts';
import { toBase64 } from './base64.js';
import { ArtifreeApiError } from './errors.js';
import { isCount, isRecord, list, revisionFile } from './responses.js';
import {
  MAX_SOURCE_WRITE_BYTES,
  decodeBase64,
  isDownloadName,
  isRetryId,
  partNumbersFit,
} from './source-part.js';
export {
  listRuntimeAssets,
  readRuntimeAsset,
  parseRuntimeAssetPart,
  uploadRuntimeAsset,
  deleteRuntimeAsset,
} from './runtime-assets.js';

/**
 * Checks of what a runtime view's source calls return (decision 0032). They
 * live apart from the other response checks because only a page that shows a
 * runtime view needs them: the client loads this module on the first such
 * call, so a page that never does carries none of it.
 */

const MAX_LISTED_FILES = 256;

/** Validates the files a runtime view lists: each path once. */
export function parseRuntimeSourceList(
  value: unknown,
  revision: number,
): RuntimeSourceList | undefined {
  if (!isRecord(value) || !Array.isArray(value.files)) return undefined;
  if (value.revision !== revision || value.files.length > MAX_LISTED_FILES)
    return undefined;
  const files = list(value.files, revisionFile);
  if (
    files === undefined ||
    new Set(files.map((file) => file.path)).size !== files.length
  )
    return undefined;
  return { revision, files };
}

/** One part of a file, with its bytes decoded and its numbers verified. */
export interface RuntimeSourcePartBytes {
  readonly path: string;
  readonly offset: number;
  /** Where the next part starts; null once the bytes reach the end. */
  readonly nextOffset: number | null;
  readonly size: number;
  readonly sha256: string;
  readonly bytes: Uint8Array<ArrayBuffer>;
}

/** What a part was asked for; a part must answer exactly that. */
export interface RuntimeSourceRequest {
  readonly path: string;
  readonly offset: number;
  readonly length: number;
}

/**
 * Validates a part against the request it answers: it echoes the path and
 * offset, and the bytes received number what its offsets say.
 */
export function parseRuntimeSourcePart(
  value: unknown,
  revision: number,
  request: RuntimeSourceRequest,
): RuntimeSourcePartBytes | undefined {
  if (!isRecord(value)) return undefined;
  const { path, offset, next_offset, size, sha256, content } = value;
  if (
    value.revision !== revision ||
    path !== request.path ||
    offset !== request.offset ||
    !isCount(size) ||
    typeof sha256 !== 'string' ||
    !/^[0-9a-f]{64}$/.test(sha256) ||
    typeof content !== 'string' ||
    !(next_offset === null || isCount(next_offset))
  )
    return undefined;
  const bytes = decodeBase64(content);
  if (
    bytes === undefined ||
    !partNumbersFit(
      { offset, nextOffset: next_offset, size, received: bytes.length },
      request.length,
    )
  )
    return undefined;
  return { path, offset, nextOffset: next_offset, size, sha256, bytes };
}

/** What a download was asked for. */
export type RuntimeDownloadTarget =
  | { readonly kind: 'file'; readonly path: string }
  | { readonly kind: 'archive' };

const DOWNLOAD_KINDS: Record<RuntimeDownloadKind, true> = {
  file: true,
  archive: true,
};

/** Validates an offer against the target it answers. */
export function parseRuntimeDownloadOffer(
  value: unknown,
  target: RuntimeDownloadTarget,
): RuntimeDownloadOffer | undefined {
  if (!isRecord(value)) return undefined;
  const { kind, path, file_name, size, sha256 } = value;
  if (
    typeof kind !== 'string' ||
    !Object.hasOwn(DOWNLOAD_KINDS, kind) ||
    kind !== target.kind ||
    !isDownloadName(file_name)
  )
    return undefined;
  if (target.kind === 'archive')
    return path === null && size === null && sha256 === null
      ? { kind: 'archive', path: null, file_name, size: null, sha256: null }
      : undefined;
  return path === target.path &&
    isCount(size) &&
    typeof sha256 === 'string' &&
    /^[0-9a-f]{64}$/.test(sha256)
    ? { kind: 'file', path, file_name, size, sha256 }
    : undefined;
}

/**
 * The SHA-256 of `bytes` in lowercase hex, or `undefined` when the platform
 * cannot compute it (no `crypto.subtle`, as on a page that is not a secure
 * context, or a digest that fails). Callers treat `undefined` as a mismatch.
 */
async function sha256Hex(
  bytes: Uint8Array<ArrayBuffer>,
): Promise<string | undefined> {
  try {
    const digest = await globalThis.crypto.subtle.digest('SHA-256', bytes);
    return Array.from(new Uint8Array(digest), (byte) =>
      byte.toString(16).padStart(2, '0'),
    ).join('');
  } catch {
    return undefined;
  }
}

/**
 * Whether downloaded bytes are what the offer promised. A file is the size
 * and the digest the offer advertised; an archive is built on demand and has
 * neither, so it is accepted at any length (decision 0032). A digest that
 * cannot be computed is a mismatch, never a pass.
 */
export async function downloadMatches(
  bytes: Uint8Array<ArrayBuffer>,
  offer: RuntimeDownloadOffer,
): Promise<boolean> {
  if (offer.size === null) return true;
  if (bytes.length !== offer.size) return false;
  return offer.sha256 === null || (await sha256Hex(bytes)) === offer.sha256;
}

/**
 * What the calls below need from the client. Every request is sent as the
 * workspace on the frame's behalf; `check` fails with the client's own error
 * once the view's connection ended, and `json` checks before and after.
 */
export interface FrameTransport {
  /** Fails if the runtime connection of the view was closed or replaced. */
  readonly check: () => void;
  readonly json: <T>(
    path: string,
    body: Record<string, unknown>,
    parse: (value: unknown) => T | undefined,
    signal: AbortSignal | undefined,
  ) => Promise<T>;
  readonly post: (
    path: string,
    body: Record<string, unknown>,
    signal: AbortSignal | undefined,
  ) => Promise<Response>;
  readonly bytes: (
    response: Response,
    what: string,
  ) => Promise<Uint8Array<ArrayBuffer>>;
}

/** The path of one of a view's frame routes. */
function frameRoute(view: RuntimeView, tail: string): string {
  return `/v1/artifacts/${encodeURIComponent(view.artifact_id)}/revisions/${view.revision}/runtime-views/${view.view_id}/${view.generation}/${tail}`;
}

/** A request body: the view's secret, then what the call adds. */
function frameBody(
  view: RuntimeView,
  rest: Record<string, unknown>,
): Record<string, unknown> {
  return { initialization: view.initialization, ...rest };
}

/** The files of the revision a view is pinned to. */
export function listSource(
  frame: FrameTransport,
  view: RuntimeView,
  signal: AbortSignal | undefined,
): Promise<RuntimeSourceList> {
  return frame.json(
    frameRoute(view, 'source/list'),
    frameBody(view, {}),
    (value) => parseRuntimeSourceList(value, view.revision),
    signal,
  );
}

/** One part of a file, answering exactly `request`. */
export function readSource(
  frame: FrameTransport,
  view: RuntimeView,
  request: RuntimeSourceRequest,
  signal: AbortSignal | undefined,
): Promise<RuntimeSourcePartBytes> {
  return frame.json(
    frameRoute(view, 'source/read'),
    frameBody(view, {
      path: request.path,
      offset: request.offset,
      length: request.length,
    }),
    (value) => parseRuntimeSourcePart(value, view.revision, request),
    signal,
  );
}

/** What a download of `target` would be, before anything is handed over. */
export function offerDownload(
  frame: FrameTransport,
  view: RuntimeView,
  target: RuntimeDownloadTarget,
  signal: AbortSignal | undefined,
): Promise<RuntimeDownloadOffer> {
  return frame.json(
    frameRoute(view, 'source/offer'),
    frameBody(view, {
      kind: target.kind,
      path: target.kind === 'file' ? target.path : null,
    }),
    (value) => parseRuntimeDownloadOffer(value, target),
    signal,
  );
}

/**
 * The bytes of a download the user confirmed. Resolves only when the
 * response is the attachment `offer` named, with the closed content type of
 * its kind, and the bytes are what the offer promised.
 */
export async function fetchDownload(
  frame: FrameTransport,
  view: RuntimeView,
  offer: RuntimeDownloadOffer,
  signal: AbortSignal | undefined,
): Promise<Uint8Array<ArrayBuffer>> {
  const path = frameRoute(view, 'source/download');
  const response = await frame.post(
    path,
    frameBody(view, { kind: offer.kind, path: offer.path }),
    signal,
  );
  const type = response.headers.get('content-type')?.trim().toLowerCase();
  const disposition = response.headers.get('content-disposition');
  const refuse = async (): Promise<never> => {
    await response.body?.cancel().catch(() => undefined);
    throw new ArtifreeApiError(
      `POST ${path} did not return the attachment that was offered.`,
      { status: response.status, code: 'invalid_response' },
    );
  };
  if (
    type !==
      (offer.kind === 'file'
        ? 'application/octet-stream'
        : 'application/zip') ||
    disposition !== `attachment; filename="${offer.file_name}"`
  )
    return refuse();
  const bytes = await frame.bytes(response, offer.file_name);
  const matches = await downloadMatches(bytes, offer);
  frame.check();
  if (!matches) return refuse();
  return bytes;
}

/** What a frame asked to save: one file, under the frame's retry identity. */
export interface SourceWriteInput {
  readonly path: string;
  /** 8 to 36 ASCII letters, digits, `-` or `_`; the server scopes it. */
  readonly retryId: string;
  readonly bytes: Uint8Array<ArrayBuffer>;
}

/**
 * Validates a save receipt against what was sent and the view it was sent
 * from. The server states the path, the revision, the size and the digest;
 * none is believed: the path is the one asked for, the revision is the one
 * after the view's, the size is the number of bytes sent, and the digest is
 * the SHA-256 of the bytes sent, which the caller computed itself.
 */
export function parseRuntimeSourceWriteReceipt(
  value: unknown,
  revision: number,
  input: SourceWriteInput,
  sha256: string,
): RuntimeSourceWriteReceipt | undefined {
  if (!isRecord(value)) return undefined;
  const { path, revision: made, size, sha256: digest } = value;
  return path === input.path &&
    made === revision + 1 &&
    size === input.bytes.length &&
    digest === sha256
    ? { path, revision: made, size, sha256: digest }
    : undefined;
}

/**
 * Saves one file for the frame and resolves only with a receipt that matches
 * the bytes sent. The arguments are judged here as well as by the server, and
 * the digest of the bytes is computed before anything is sent: a platform
 * that cannot compute one refuses the write, because it could not tell
 * whether the revision holds what the frame wrote (decision 0033).
 */
export async function writeSource(
  frame: FrameTransport,
  view: RuntimeView,
  input: SourceWriteInput,
  signal: AbortSignal | undefined,
): Promise<RuntimeSourceWriteReceipt> {
  if (
    typeof input.path !== 'string' ||
    !isRetryId(input.retryId) ||
    !(input.bytes instanceof Uint8Array) ||
    input.bytes.length > MAX_SOURCE_WRITE_BYTES
  )
    throw new ArtifreeApiError('The save was not valid.', {
      status: 0,
      code: 'invalid_request',
    });
  const digest = await sha256Hex(input.bytes);
  if (digest === undefined)
    throw new ArtifreeApiError(
      'This page cannot compute the digest that checks a save.',
      { status: 0, code: 'unavailable' },
    );
  return frame.json(
    frameRoute(view, 'source/write'),
    frameBody(view, {
      retry_id: input.retryId,
      path: input.path,
      content: toBase64(input.bytes),
    }),
    (value) =>
      parseRuntimeSourceWriteReceipt(value, view.revision, input, digest),
    signal,
  );
}
