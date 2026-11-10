import { FRAME_PENDING_CAP } from './frame-budget';
import { ASSET_CALLS } from './frame-calls/assets';
import {
  type CallDeps,
  type CallEntry,
  type CallOf,
  type FailureCode,
  type FrameReply,
  type ParsedCall,
  isCount,
  isRecord,
} from './frame-calls/call';
import { DATA_CALLS } from './frame-calls/data';
import { METADATA_CALLS } from './frame-calls/metadata';
import { SOURCE_CALLS } from './frame-calls/source';

export type { FailureCode, FrameReply } from './frame-calls/call';

/**
 * Serves the calls a runtime frame makes (decisions 0020, 0032, 0033, 0045
 * and 0053). The frame is untrusted script, so nothing it sends is believed: a
 * message is parsed strictly, charged against a budget before anything else is
 * done, answered from a closed vocabulary of failures, and never echoed. The
 * workspace asks the server on every call, whatever it has seen before. This
 * module has no DOM and no React; the component supplies the port and the
 * prompt. Each call type is described once, in `frame-calls/` (decision 0052,
 * rule 6); this module is the envelope around them.
 */

/** Every call the frame may make, in the order the families are listed. */
const CALLS = [
  ...METADATA_CALLS,
  ...SOURCE_CALLS,
  ...ASSET_CALLS,
  ...DATA_CALLS,
];

/** A call the registry serves. */
type ServedCall = CallOf<(typeof CALLS)[number]>;

/** The envelope's own message: it ends a waiting call and is never served. */
interface CancelCall {
  readonly type: 'cancel';
  readonly id: number;
}

/** A parsed, bounded call. */
export type FrameCall = ServedCall | CancelCall;

/** The registry of calls by type (decision 0052, rule 6). */
const REGISTRY: ReadonlyMap<string, CallEntry<ServedCall>> = new Map(
  CALLS.map((entry): [string, CallEntry<ServedCall>] => [entry.type, entry]),
);

/** The call types the registry serves, in its order. */
export const FRAME_CALL_TYPES: readonly string[] = CALLS.map(
  (entry) => entry.type,
);

/** The keys a cancel carries, and no others. */
const CANCEL_KEYS: readonly string[] = ['type', 'id'];

/** The keys a message of `type` carries, or undefined for an unknown type. */
function keysOf(type: string): readonly string[] | undefined {
  return type === 'cancel' ? CANCEL_KEYS : REGISTRY.get(type)?.keys;
}

/**
 * The id a message asks to be answered under: a positive safe integer on an
 * object of a known type, whatever else is wrong with it. A message without
 * one cannot be answered and is dropped.
 */
export function callId(message: unknown): number | undefined {
  if (!isRecord(message)) return undefined;
  const { type, id } = message;
  if (typeof type !== 'string' || keysOf(type) === undefined) return undefined;
  return isCount(id) && id >= 1 ? id : undefined;
}

/** A message as a served call and what serves it, a cancel, or undefined. */
function parseMessage(
  message: unknown,
): ParsedCall<ServedCall> | CancelCall | undefined {
  if (!isRecord(message)) return undefined;
  const { type, id } = message;
  if (typeof type !== 'string') return undefined;
  const shape = keysOf(type);
  if (shape === undefined) return undefined;
  const keys = Object.keys(message);
  if (keys.length !== shape.length || !keys.every((key) => shape.includes(key)))
    return undefined;
  if (!isCount(id) || id < 1) return undefined;
  if (type === 'cancel') return { type, id };
  return REGISTRY.get(type)?.parse(message, id);
}

/**
 * The call a message is, or undefined. Exactly the keys of its type, a
 * positive safe-integer id, a path its type's parser accepts (a package path,
 * or a data path within its bound), and a part length within its bound: 1 MiB
 * for a source part, 256 KiB for an asset part. A write carries at most 1 MiB
 * of bytes. A message that is not an object, has another key, or has a value
 * of another type is not a call.
 */
export function parseCall(message: unknown): FrameCall | undefined {
  const parsed = parseMessage(message);
  if (parsed === undefined) return undefined;
  return 'serve' in parsed ? parsed.call : parsed;
}

/**
 * The frame's word for a failure. The frame learns the kind of refusal and
 * nothing else: no server message, status or detail is passed on, except the
 * asset version a conflict names (see `failureReply`).
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
      if (code === 'revision_conflict') return 'stale_view';
      // An asset conflict is one the frame can act on only when it names the
      // version the server holds; without it the frame is told unavailable.
      if (code === 'asset_conflict' && conflictVersion(error) !== undefined)
        return 'conflict';
      return 'unavailable';
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

/**
 * The asset version an `asset_conflict` names. The server answers with
 * `{ error: { code, current_asset_version } }`; the client keeps that whole
 * answer as the error's `body`, so the version sits one level down. It is
 * taken only when it is a count.
 */
function conflictVersion(error: unknown): number | undefined {
  if (!isRecord(error) && !(error instanceof Error)) return undefined;
  if (!('body' in error)) return undefined;
  const { body } = error;
  if (!isRecord(body) || !isRecord(body.error)) return undefined;
  const version = body.error.current_asset_version;
  return isCount(version) ? version : undefined;
}

/**
 * The reply that ends a call which failed: its word and, for a conflict, the
 * asset version the server holds, which the frame bases its next attempt on.
 */
function failureReply(id: number, error: unknown): FrameReply {
  const code = failureCode(error);
  const version = code === 'conflict' ? conflictVersion(error) : undefined;
  return {
    message:
      version === undefined
        ? { id, ok: false, code }
        : { id, ok: false, code, currentAssetVersion: version },
    transfer: [],
  };
}

/** What the rpc needs; the component supplies the port and the prompt. */
export interface FrameRpcDeps extends CallDeps {
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
    const parsed = parseMessage(message);
    if (parsed === undefined) {
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
    if (!('serve' in parsed)) {
      this.#pending.get(parsed.id)?.abort();
      this.#pending.delete(parsed.id);
      return;
    }
    const { call } = parsed;
    // After the lease the frame is told nothing: it keeps the same deadline
    // and refuses on its own, and a late reply must not reach it.
    if (this.#deps.now() >= this.#deps.deadline) return;
    if (!this.#deps.take()) return this.#fail(call.id, 'rate_limited');
    if (this.#pending.has(call.id)) return;
    if (this.#pending.size >= FRAME_PENDING_CAP)
      return this.#fail(call.id, 'rate_limited');
    const controller = new AbortController();
    this.#pending.set(call.id, controller);
    // A serve that throws before it returns a promise fails the call like a
    // rejection, so the pending slot is always released below.
    let served: Promise<FrameReply | undefined>;
    try {
      served = parsed.serve(this.#deps, controller.signal);
    } catch (error: unknown) {
      served = Promise.reject(error);
    }
    void served
      .then(
        (reply) => {
          if (reply !== undefined) this.#reply(call.id, controller, reply);
        },
        (error: unknown) => {
          this.#reply(call.id, controller, failureReply(call.id, error));
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
}
