import type { RuntimeView } from '@artifree/contracts';
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
import type { Offer, Outcome } from './offer-desk';

export type { FailureCode, FrameReply } from './frame-calls/call';

/** Why the workspace adopts a frame's view (decision 0054, rule 10). */
export type AdoptionReason = 'permissions' | 'saved' | 'head' | 'lease';

/**
 * How long a pause waits for a call on its way that is not a write (decision
 * 0054, rule 9).
 */
export const ADOPTION_WAIT_MS = 5_000;

/** One call of the frame that has not ended. */
interface Pending {
  readonly controller: AbortController;
  /** A write is waited for until its request ends (rule 9). */
  readonly write: boolean;
  /** The generation it is served under; undefined while it is held. */
  generation: number | undefined;
  /** Waiting for the user's answer to a prompt: not on its way (rule 9). */
  prompting: boolean;
  /** Called once when the call ends or starts to wait for the user. */
  readonly wakes: Array<() => void>;
}

/** A call received during a pause, sent when the pause ends. */
interface Held {
  readonly id: number;
  readonly parsed: ParsedCall<ServedCall>;
  readonly entry: Pending;
}

/**
 * open: calls are served. paused: they are held. refusing: they are answered
 * `unavailable` at once.
 */
type Mode = 'open' | 'paused' | 'refusing';

/**
 * Serves the calls a runtime frame makes (decisions 0020, 0032, 0033, 0045,
 * 0053 and 0054). The frame is untrusted script, so nothing it sends is
 * believed: a message is parsed strictly, charged against a budget before
 * anything else is done, answered from a closed vocabulary of failures, and
 * never echoed. The workspace asks the server on every call, whatever it has
 * seen before. While the workspace adopts a new view, the calls are held and
 * sent under it (decision 0054). This module has no DOM and no React; the
 * component supplies the port and the prompt. Each call type is described
 * once, in `frame-calls/` (decision 0052, rule 6); this module is the envelope
 * around them.
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
 * for a source part, 256 KiB for an asset part. A file write carries at most
 * 1 MiB of bytes, a data write at most 256 KiB of text. A message that is not
 * an object, has another key, or has a value of another type is not a call.
 */
export function parseCall(message: unknown): FrameCall | undefined {
  const parsed = parseMessage(message);
  if (parsed === undefined) return undefined;
  return 'serve' in parsed ? parsed.call : parsed;
}

/**
 * The frame's word for a failure. The frame learns the kind of refusal and
 * nothing else: no server message, status or detail is passed on, except the
 * version a conflict names (see `failureReply`).
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
      // A conflict is one the frame can act on only when it names the version
      // the server holds; without it the frame is told unavailable.
      if (
        code === 'asset_conflict' &&
        conflictVersion(error, 'current_asset_version') !== undefined
      )
        return 'conflict';
      if (
        code === 'data_conflict' &&
        conflictVersion(error, 'current_data_version') !== undefined
      )
        return 'conflict';
      // Decision 0053, rule 14: a path through a value that is not an object
      // is a bad request.
      if (code === 'data_path_conflict') return 'invalid_request';
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
 * The version a conflict names under `field`: the asset version of an
 * `asset_conflict` or the data version of a `data_conflict`. The server
 * answers with `{ error: { code, current_asset_version } }` (or with
 * `current_data_version`); the client keeps that whole answer as the error's
 * `body`, so the version sits one level down. It is taken only when it is a
 * count.
 */
function conflictVersion(
  error: unknown,
  field: 'current_asset_version' | 'current_data_version',
): number | undefined {
  if (!isRecord(error) && !(error instanceof Error)) return undefined;
  if (!('body' in error)) return undefined;
  const { body } = error;
  if (!isRecord(body) || !isRecord(body.error)) return undefined;
  const version = body.error[field];
  return isCount(version) ? version : undefined;
}

/** The server's error code of a failed request, when it has one. */
function serverCode(error: unknown): unknown {
  if (!isRecord(error) && !(error instanceof Error)) return undefined;
  return 'code' in error ? error.code : undefined;
}

/**
 * The reply that ends a call which failed: its word and, for a conflict, the
 * version the server holds, which the frame bases its next attempt on. A data
 * conflict names the data version; any other conflict names the asset version.
 */
function failureReply(id: number, error: unknown): FrameReply {
  const code = failureCode(error);
  const dataConflict = serverCode(error) === 'data_conflict';
  const version =
    code === 'conflict'
      ? conflictVersion(
          error,
          dataConflict ? 'current_data_version' : 'current_asset_version',
        )
      : undefined;
  if (version === undefined)
    return { message: { id, ok: false, code }, transfer: [] };
  return {
    message: dataConflict
      ? { id, ok: false, code, currentDataVersion: version }
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
  readonly #pending = new Map<number, Pending>();
  /** Calls received during a pause, sent when it ends (decision 0054). */
  #held: Held[] = [];
  /** Answers given during a pause, released when it ends (rule 13). */
  readonly #opened: Array<() => void> = [];
  #view: RuntimeView;
  #deadline: number;
  #mode: Mode = 'open';
  #reason: AdoptionReason | undefined;
  #closed = false;

  constructor(deps: FrameRpcDeps) {
    this.#deps = deps;
    this.#view = deps.view;
    this.#deadline = deps.deadline;
  }

  /** Calls waiting for the server, the user or an adoption, held ones too. */
  get pending(): number {
    return this.#pending.size;
  }

  /** The view the calls are served under; an adoption replaces it. */
  get view(): RuntimeView {
    return this.#view;
  }

  /** When the view's lease ends, on the same clock as `now`. */
  get deadline(): number {
    return this.#deadline;
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
    for (const entry of this.#pending.values()) entry.controller.abort();
    this.#pending.clear();
    this.#held = [];
    this.#wakeOpened();
  }

  /**
   * Holds the frame's calls for an adoption and resolves once the calls on
   * their way have ended or been abandoned (decision 0054, rule 9). A prompt
   * stays open only through a `lease` adoption (rule 13).
   */
  async pause(reason: AdoptionReason): Promise<void> {
    if (this.#closed) return;
    this.#mode = 'paused';
    this.#reason = reason;
    const waits: Array<Promise<void>> = [];
    for (const [id, entry] of Array.from(this.#pending)) {
      if (entry.prompting) {
        if (reason !== 'lease') this.#abandon(id, entry, 'unavailable');
        continue;
      }
      if (entry.generation !== undefined) waits.push(this.#ended(id, entry));
    }
    await Promise.all(waits);
  }

  /**
   * Ends a pause without an adoption: the held calls go out under the current
   * view (decision 0054, rule 8).
   */
  resume(): void {
    if (this.#closed) return;
    this.#mode = 'open';
    this.#reason = undefined;
    this.#release();
  }

  /**
   * The grant's outcome is not known: held calls are answered `unavailable`,
   * and so are later ones, until the next pause, resume or adoption (decision
   * 0054, rule 8).
   */
  refuse(): void {
    if (this.#closed) return;
    this.#mode = 'refusing';
    this.#reason = undefined;
    const held = this.#held;
    this.#held = [];
    for (const { id, entry } of held)
      if (this.#pending.get(id) === entry)
        this.#abandon(id, entry, 'unavailable');
  }

  /**
   * Replaces the view and the end of its lease. The held calls go out under the
   * new generation, and a prompt kept through a lease moves with it (decision
   * 0054, rules 9 and 13). The budgets are not refilled.
   */
  adopt(view: RuntimeView, deadline: number): void {
    if (this.#closed) return;
    this.#view = view;
    this.#deadline = deadline;
    for (const entry of this.#pending.values())
      if (entry.prompting) entry.generation = view.generation;
    this.#mode = 'open';
    this.#reason = undefined;
    this.#release();
  }

  /**
   * The adoption failed: every call that has not ended is answered `expired`
   * and the rpc closes (decision 0054, rule 9).
   */
  fail(): void {
    if (this.#closed) return;
    for (const id of this.#pending.keys()) this.#fail(id, 'expired');
    this.close();
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
      if (id !== undefined && this.#deps.now() < this.#deadline)
        this.#fail(id, taken ? 'invalid_request' : 'rate_limited');
      return;
    }
    if (!('serve' in parsed)) {
      this.#pending.get(parsed.id)?.controller.abort();
      this.#pending.delete(parsed.id);
      return;
    }
    const { call } = parsed;
    // After the lease the frame is told nothing: it keeps the same deadline
    // and refuses on its own, and a late reply must not reach it.
    if (this.#deps.now() >= this.#deadline) return;
    if (!this.#deps.take()) return this.#fail(call.id, 'rate_limited');
    // The grant's outcome is unknown (decision 0054, rule 8): answered at
    // once, never held.
    if (this.#mode === 'refusing') return this.#fail(call.id, 'unavailable');
    if (this.#pending.has(call.id)) return;
    if (this.#pending.size >= FRAME_PENDING_CAP)
      return this.#fail(call.id, 'rate_limited');
    const entry: Pending = {
      controller: new AbortController(),
      write: parsed.write,
      generation: undefined,
      prompting: false,
      wakes: [],
    };
    this.#pending.set(call.id, entry);
    // During an adoption the call is held and sent under the new generation
    // (decision 0054, rule 9).
    if (this.#mode === 'paused') {
      this.#held.push({ id: call.id, parsed, entry });
      return;
    }
    this.#serve(call.id, parsed, entry);
  }

  /**
   * Serves a call now, under the view the workspace holds, and releases its
   * slot when it ends. The call keeps the generation it is served under.
   */
  #serve(id: number, parsed: ParsedCall<ServedCall>, entry: Pending): void {
    entry.generation = this.#view.generation;
    const current = (): RuntimeView => this.#view;
    const deps: CallDeps = {
      client: this.#deps.client,
      // Read when used: a download kept through an adoption uses the new view.
      get view() {
        return current();
      },
      ask: (offer, signal) => this.#ask(id, entry, offer, signal),
      save: this.#deps.save,
      takeDataWrite: this.#deps.takeDataWrite,
    };
    // A serve that throws before it returns a promise fails the call like a
    // rejection, so the pending slot is always released below.
    let served: Promise<FrameReply | undefined>;
    try {
      served = parsed.serve(deps, entry.controller.signal);
    } catch (error: unknown) {
      served = Promise.reject(error);
    }
    void served
      .then(
        (reply) => {
          if (reply !== undefined) this.#reply(id, entry, reply);
        },
        (error: unknown) => {
          this.#reply(id, entry, failureReply(id, error));
        },
      )
      .finally(() => {
        if (this.#pending.get(id) === entry) this.#pending.delete(id);
        this.#wake(entry);
      })
      .catch(() => {
        // A reply that cannot be delivered means the port is unusable.
        this.close();
      });
  }

  /**
   * The prompt of a call. A prompt that would open during a pause, or while the
   * grant is unresolved, is withdrawn at once unless the pause is a lease
   * (decision 0054, rule 13).
   */
  async #ask(
    id: number,
    entry: Pending,
    offer: Offer,
    signal: AbortSignal,
  ): Promise<Outcome> {
    if (this.#mode !== 'open' && this.#reason !== 'lease') {
      this.#abandon(id, entry, 'unavailable');
      return 'withdrawn';
    }
    entry.prompting = true;
    this.#wake(entry);
    try {
      const outcome = await this.#deps.ask(offer, signal);
      // An answer given during a pause is acted on when the pause ends.
      if (this.#mode !== 'open' && !signal.aborted && !this.#closed)
        await new Promise<void>((resolve) => {
          this.#opened.push(resolve);
        });
      return outcome;
    } finally {
      entry.prompting = false;
    }
  }

  /**
   * Resolves when the call ends or starts to wait for the user. A call that is
   * not a write is abandoned after `ADOPTION_WAIT_MS` (decision 0054, rule 9).
   */
  #ended(id: number, entry: Pending): Promise<void> {
    return new Promise<void>((resolve) => {
      let timer: ReturnType<typeof setTimeout> | undefined;
      entry.wakes.push(() => {
        clearTimeout(timer);
        resolve();
      });
      if (!entry.write)
        timer = setTimeout(() => {
          if (this.#pending.get(id) === entry && !entry.prompting)
            this.#abandon(id, entry, 'unavailable');
          this.#wake(entry);
        }, ADOPTION_WAIT_MS);
    });
  }

  /** Wakes the pauses that wait on a call. */
  #wake(entry: Pending): void {
    for (const wake of entry.wakes.splice(0)) wake();
  }

  /** Releases the answers that waited for a pause to end. */
  #wakeOpened(): void {
    for (const resolve of this.#opened.splice(0)) resolve();
  }

  /** Ends a call now and tells the frame `code`. */
  #abandon(id: number, entry: Pending, code: FailureCode): void {
    entry.controller.abort();
    if (this.#pending.get(id) === entry) this.#pending.delete(id);
    this.#fail(id, code);
    this.#wake(entry);
  }

  /**
   * Serves the held calls under the view the workspace holds now. A call whose
   * lease has ended is dropped unanswered, as a call received then is.
   */
  #release(): void {
    const held = this.#held;
    this.#held = [];
    for (const { id, parsed, entry } of held) {
      if (this.#pending.get(id) !== entry || entry.controller.signal.aborted)
        continue;
      if (this.#deps.now() >= this.#deadline) {
        this.#pending.delete(id);
        continue;
      }
      this.#serve(id, parsed, entry);
    }
    this.#wakeOpened();
  }

  /**
   * Sends a reply unless the call was cancelled or the rpc closed, the lease
   * has ended, or the call is of a generation the view no longer has.
   */
  #reply(id: number, entry: Pending, reply: FrameReply): void {
    if (
      this.#closed ||
      entry.controller.signal.aborted ||
      this.#pending.get(id) !== entry
    )
      return;
    // An answer to a call of a generation the view no longer has is dropped
    // (decision 0054, rule 13).
    if (entry.generation !== this.#view.generation) return;
    if (this.#deps.now() >= this.#deadline) return;
    this.#deps.send(reply.message, reply.transfer);
  }

  #fail(id: number, code: FailureCode): void {
    this.#deps.send({ id, ok: false, code }, []);
  }
}
