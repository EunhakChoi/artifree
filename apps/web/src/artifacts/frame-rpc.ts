import type { ArtifreeClient } from '@artifree/client';
import type { RuntimeView } from '@artifree/contracts';
import { FRAME_PENDING_CAP } from './frame-budget';
import type { Offer, Outcome } from './offer-desk';
import { pathError } from './package-path';

/**
 * Serves the calls a runtime frame makes (decisions 0020, 0032, 0033 and
 * 0045). The frame is untrusted script, so nothing it sends is believed: a
 * message is parsed strictly, charged against a budget before anything else is
 * done, answered from a closed vocabulary of failures, and never echoed. The
 * workspace asks the server on every call, whatever it has seen before. This
 * module has no DOM and no React; the component supplies the port and the
 * prompt.
 */

/** Why a call failed, as the frame sees it. */
export type FailureCode =
  | 'invalid_request'
  | 'denied'
  | 'stale_view'
  | 'not_found'
  | 'rate_limited'
  | 'declined'
  | 'unavailable'
  | 'expired';

/**
 * What a retry identity looks like: the client and the server judge the same
 * rule. It is written out here, not imported, because importing a value from
 * the client would put a module shared by two on-demand chunks into the
 * entry bundle's preload list, which has no room (decision 0033). A test
 * holds this and the client's check to the same answers.
 */
const RETRY_ID = /^[A-Za-z0-9_-]{8,36}$/;

/** The most bytes a frame may ask for in one part. */
const MAX_PART = 1_048_576;

/** The most bytes of one file a frame may save in one call. */
const MAX_WRITE = 1_048_576;

/** The most bytes of an asset a frame may ask for in one part: 256 KiB. */
const MAX_ASSET_PART = 262_144;

/**
 * What an asset ID looks like: 64 lowercase hexadecimal digits. It is written
 * out here, not imported, for the reason RETRY_ID gives (decision 0033).
 */
const ASSET_ID = /^[0-9a-f]{64}$/;

/** A parsed, bounded call. */
export type FrameCall =
  | { readonly type: 'metadata'; readonly id: number }
  | { readonly type: 'source.list'; readonly id: number }
  | {
      readonly type: 'source.read';
      readonly id: number;
      readonly path: string;
      readonly offset: number;
      readonly length: number;
    }
  | {
      readonly type: 'source.download';
      readonly id: number;
      readonly path: string | null;
    }
  | {
      readonly type: 'source.write';
      readonly id: number;
      readonly retryId: string;
      readonly path: string;
      /** The frame's own copy, transferred to the page. */
      readonly bytes: ArrayBuffer;
    }
  | { readonly type: 'assets.list'; readonly id: number }
  | {
      readonly type: 'assets.read';
      readonly id: number;
      readonly assetId: string;
      readonly offset: number;
      readonly length: number;
    }
  | { readonly type: 'cancel'; readonly id: number };

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function isCount(value: unknown): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0;
}

/** The keys a message of each type carries, and no others. */
const SHAPES: Record<string, readonly string[]> = {
  metadata: ['type', 'id'],
  'source.list': ['type', 'id'],
  'source.read': ['type', 'id', 'path', 'offset', 'length'],
  'source.download': ['type', 'id', 'kind', 'path'],
  'source.write': ['type', 'id', 'retryId', 'path', 'bytes'],
  'assets.list': ['type', 'id'],
  'assets.read': ['type', 'id', 'assetId', 'offset', 'length'],
  cancel: ['type', 'id'],
};

/**
 * The id a message asks to be answered under: a positive safe integer on an
 * object of a known type, whatever else is wrong with it. A message without
 * one cannot be answered and is dropped.
 */
export function callId(message: unknown): number | undefined {
  if (!isRecord(message)) return undefined;
  const { type, id } = message;
  if (typeof type !== 'string' || !Object.hasOwn(SHAPES, type))
    return undefined;
  return isCount(id) && id >= 1 ? id : undefined;
}

/**
 * The call a message is, or undefined. Exactly the keys of its type, a
 * positive safe-integer id, a path the package rules accept, and a part
 * length within its bound: 1 MiB for a source part, 256 KiB for an asset
 * part. A message that is not an object, has another key, or has a value of
 * another type is not a call.
 */
export function parseCall(message: unknown): FrameCall | undefined {
  if (!isRecord(message)) return undefined;
  const { type, id } = message;
  if (typeof type !== 'string' || !Object.hasOwn(SHAPES, type))
    return undefined;
  const keys = Object.keys(message);
  const shape = SHAPES[type] ?? [];
  if (keys.length !== shape.length || !keys.every((key) => shape.includes(key)))
    return undefined;
  if (!isCount(id) || id < 1) return undefined;
  switch (type) {
    case 'metadata':
    case 'source.list':
    case 'assets.list':
    case 'cancel':
      return { type, id };
    case 'source.read': {
      const { path, offset, length } = message;
      if (
        typeof path !== 'string' ||
        pathError(path) !== undefined ||
        !isCount(offset) ||
        !isCount(length) ||
        length < 1 ||
        length > MAX_PART
      )
        return undefined;
      return { type, id, path, offset, length };
    }
    case 'source.download': {
      const { kind, path } = message;
      if (kind === 'archive' && path === null) return { type, id, path: null };
      if (
        kind === 'file' &&
        typeof path === 'string' &&
        pathError(path) === undefined
      )
        return { type, id, path };
      return undefined;
    }
    case 'source.write': {
      const { retryId, path, bytes } = message;
      if (
        typeof retryId !== 'string' ||
        !RETRY_ID.test(retryId) ||
        typeof path !== 'string' ||
        pathError(path) !== undefined ||
        !(bytes instanceof ArrayBuffer) ||
        bytes.byteLength > MAX_WRITE
      )
        return undefined;
      return { type, id, retryId, path, bytes };
    }
    case 'assets.read': {
      const { assetId, offset, length } = message;
      if (
        typeof assetId !== 'string' ||
        !ASSET_ID.test(assetId) ||
        !isCount(offset) ||
        !isCount(length) ||
        length < 1 ||
        length > MAX_ASSET_PART
      )
        return undefined;
      return { type, id, assetId, offset, length };
    }
    default:
      return undefined;
  }
}

/**
 * The frame's word for a failure. The frame learns the kind of refusal and
 * nothing else: no server message, status or detail is passed on.
 */
export function failureCode(error: unknown): FailureCode {
  if (!isRecord(error) && !(error instanceof Error)) return 'unavailable';
  const { name, status, code, connectionReplaced } = error as {
    name?: unknown;
    status?: unknown;
    code?: unknown;
    connectionReplaced?: unknown;
  };
  if (name !== 'ArtifreeApiError' || typeof status !== 'number')
    return 'unavailable';
  // The client's own marker: the call was never sent because the runtime
  // connection ended. No other status 0 failure says the view is over.
  if (connectionReplaced === true) return 'expired';
  if (status === 0)
    return code === 'invalid_request' ? 'invalid_request' : 'unavailable';
  switch (status) {
    case 404:
      return 'not_found';
    case 403:
      return 'denied';
    case 409:
      return code === 'revision_conflict' ? 'stale_view' : 'unavailable';
    case 429:
      return 'rate_limited';
    case 400:
    case 413:
    case 416:
    case 422:
      return 'invalid_request';
    default:
      return 'unavailable';
  }
}

/** A message to the frame. */
export interface FrameReply {
  readonly message: Record<string, unknown>;
  readonly transfer: Transferable[];
}

/** What the rpc needs; the component supplies the port and the prompt. */
export interface FrameRpcDeps {
  readonly client: Pick<
    ArtifreeClient,
    | 'runtimeMetadata'
    | 'runtimeSourceList'
    | 'runtimeSourceRead'
    | 'runtimeSourceOffer'
    | 'runtimeSourceDownload'
    | 'runtimeCall'
  >;
  readonly view: RuntimeView;
  /** Shows a prompt; resolves with the user's answer. */
  readonly ask: (offer: Offer, signal: AbortSignal) => Promise<Outcome>;
  /** Hands the confirmed bytes to the browser. */
  readonly save: (
    bytes: Uint8Array<ArrayBuffer>,
    fileName: string,
    type: string,
  ) => void;
  /** Takes one token of the frame's budget. */
  readonly take: () => boolean;
  /** Monotonic milliseconds. */
  readonly now: () => number;
  /** When the view's lease ends, on the same clock. */
  readonly deadline: number;
  readonly send: (
    message: Record<string, unknown>,
    transfer: Transferable[],
  ) => void;
}

const OUTCOME_CODE: Record<Exclude<Outcome, 'accepted'>, FailureCode | null> = {
  declined: 'declined',
  // The frame is told nothing about a withdrawn offer: it is gone or gave up.
  withdrawn: null,
  busy: 'rate_limited',
  cooling: 'rate_limited',
};

/** One frame's calls. */
export class FrameRpc {
  readonly #deps: FrameRpcDeps;
  readonly #pending = new Map<number, AbortController>();
  #closed = false;

  constructor(deps: FrameRpcDeps) {
    this.#deps = deps;
  }

  /** Calls waiting for the server or the user. */
  get pending(): number {
    return this.#pending.size;
  }

  /** Handles one message from the frame. It never throws. */
  handle(message: unknown): void {
    try {
      this.#handle(message);
    } catch {
      // Nothing the frame sent may break the workspace. The call, if it had
      // an id, times out in the frame.
    }
  }

  /** Ends every call. The frame is gone, the view ended, or it expired. */
  close(): void {
    this.#closed = true;
    for (const controller of this.#pending.values()) controller.abort();
    this.#pending.clear();
  }

  #handle(message: unknown): void {
    if (this.#closed) return;
    const call = parseCall(message);
    if (call === undefined) {
      // Not a call. It still costs a token, so a flood of noise is bounded.
      const taken = this.#deps.take();
      // A message the frame can wait on is answered at once, so a bad one
      // never holds one of its waiting slots until it times out. After the
      // lease nothing is sent. A message without a usable id is dropped.
      const id = callId(message);
      if (id !== undefined && this.#deps.now() < this.#deps.deadline)
        this.#fail(id, taken ? 'invalid_request' : 'rate_limited');
      return;
    }
    if (call.type === 'cancel') {
      this.#pending.get(call.id)?.abort();
      this.#pending.delete(call.id);
      return;
    }
    // After the lease the frame is told nothing: it keeps the same deadline
    // and refuses on its own, and a late reply must not reach it.
    if (this.#deps.now() >= this.#deps.deadline) return;
    if (!this.#deps.take()) return this.#fail(call.id, 'rate_limited');
    if (this.#pending.has(call.id)) return;
    if (this.#pending.size >= FRAME_PENDING_CAP)
      return this.#fail(call.id, 'rate_limited');
    const controller = new AbortController();
    this.#pending.set(call.id, controller);
    void this.#serve(call, controller.signal)
      .then(
        (reply) => {
          if (reply !== undefined) this.#reply(call.id, controller, reply);
        },
        (error: unknown) => {
          this.#reply(call.id, controller, {
            message: { id: call.id, ok: false, code: failureCode(error) },
            transfer: [],
          });
        },
      )
      .finally(() => {
        if (this.#pending.get(call.id) === controller)
          this.#pending.delete(call.id);
      })
      .catch(() => {
        // A reply that cannot be delivered means the port is unusable.
        this.close();
      });
  }

  /** Sends a reply unless the call was cancelled or the rpc closed. */
  #reply(id: number, controller: AbortController, reply: FrameReply): void {
    if (
      this.#closed ||
      controller.signal.aborted ||
      this.#pending.get(id) !== controller
    )
      return;
    if (this.#deps.now() >= this.#deps.deadline) return;
    this.#deps.send(reply.message, reply.transfer);
  }

  #fail(id: number, code: FailureCode): void {
    this.#deps.send({ id, ok: false, code }, []);
  }

  async #serve(
    call: Exclude<FrameCall, { type: 'cancel' }>,
    signal: AbortSignal,
  ): Promise<FrameReply | undefined> {
    const { client, view } = this.#deps;
    const options = { signal };
    switch (call.type) {
      case 'metadata': {
        const result = await client.runtimeMetadata(view, options);
        return ok(call.id, {
          metadata: {
            artifactId: result.artifact_id,
            title: result.title,
            currentRevision: result.current_revision,
            metadataRevision: result.metadata_revision,
          },
        });
      }
      case 'source.list': {
        const result = await client.runtimeSourceList(view, options);
        return ok(call.id, {
          listing: {
            revision: result.revision,
            files: result.files.map((file) => ({
              path: file.path,
              size: file.size,
              sha256: file.sha256,
              mediaType: file.media_type,
            })),
          },
        });
      }
      case 'source.read': {
        const part = await client.runtimeSourceRead(
          view,
          { path: call.path, offset: call.offset, length: call.length },
          options,
        );
        const bytes = part.bytes.buffer;
        return {
          message: {
            id: call.id,
            ok: true,
            part: {
              path: part.path,
              offset: part.offset,
              nextOffset: part.nextOffset,
              size: part.size,
              sha256: part.sha256,
              bytes,
            },
          },
          transfer: [bytes],
        };
      }
      case 'source.download':
        return this.#download(call.id, call.path, signal);
      case 'source.write': {
        // A frame's bytes are copied once, here; the client reads the copy.
        const input = {
          path: call.path,
          retryId: call.retryId,
          bytes: new Uint8Array(call.bytes.slice(0)),
        };
        const receipt = await client.runtimeCall(view, (source, frame) =>
          source.writeSource(frame, view, input, signal),
        );
        return ok(call.id, {
          written: {
            path: receipt.path,
            revision: receipt.revision,
            size: receipt.size,
            sha256: receipt.sha256,
          },
        });
      }
      case 'assets.list': {
        const result = await client.runtimeCall(view, (source, frame) =>
          source.listRuntimeAssets(frame, view, signal),
        );
        return ok(call.id, {
          listing: {
            assetVersion: result.asset_version,
            assets: result.assets.map((asset) => ({
              id: asset.id,
              contentType: asset.content_type,
              size: asset.size_bytes,
              sha256: asset.sha256,
              createdAtMs: asset.created_at_ms,
            })),
          },
        });
      }
      case 'assets.read': {
        const request = {
          assetId: call.assetId,
          offset: call.offset,
          length: call.length,
        };
        const part = await client.runtimeCall(view, (source, frame) =>
          source.readRuntimeAsset(frame, view, request, signal),
        );
        const bytes = part.bytes.buffer;
        return {
          message: {
            id: call.id,
            ok: true,
            part: {
              assetId: part.assetId,
              contentType: part.contentType,
              offset: part.offset,
              nextOffset: part.nextOffset,
              size: part.size,
              sha256: part.sha256,
              bytes,
            },
          },
          transfer: [bytes],
        };
      }
    }
  }

  /**
   * The server is asked first, so a prompt never shows an offer the server
   * would refuse: no grant, an old head, a missing file. Only the user's
   * accept makes the workspace fetch the bytes, and the server checks again.
   */
  async #download(
    id: number,
    path: string | null,
    signal: AbortSignal,
  ): Promise<FrameReply | undefined> {
    const { client, view } = this.#deps;
    const target =
      path === null
        ? ({ kind: 'archive' } as const)
        : ({ kind: 'file', path } as const);
    const offer = await client.runtimeSourceOffer(view, target, { signal });
    const outcome = await this.#deps.ask(
      {
        kind: offer.kind,
        path: offer.path,
        fileName: offer.file_name,
        size: offer.size,
      },
      signal,
    );
    if (signal.aborted) return undefined;
    if (outcome !== 'accepted') {
      const code = OUTCOME_CODE[outcome];
      return code === null
        ? undefined
        : { message: { id, ok: false, code }, transfer: [] };
    }
    const bytes = await client.runtimeSourceDownload(view, offer, { signal });
    if (signal.aborted) return undefined;
    this.#deps.save(
      bytes,
      offer.file_name,
      offer.kind === 'file' ? 'application/octet-stream' : 'application/zip',
    );
    return ok(id, {
      receipt: { started: true, kind: offer.kind, bytes: bytes.length },
    });
  }
}

function ok(id: number, body: Record<string, unknown>): FrameReply {
  return { message: { id, ok: true, ...body }, transfer: [] };
}
