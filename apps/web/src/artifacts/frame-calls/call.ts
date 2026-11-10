import type { ArtifreeClient } from '@artifree/client';
import type { RuntimeView } from '@artifree/contracts';
import type { Offer, Outcome } from '../offer-desk';

/**
 * What every frame call shares (decision 0052, rule 6): the descriptor that
 * names a call type once, the reply, the bounds the parsers apply and the
 * helpers they use. Only `frame-rpc.ts` and the call families import this
 * module, so it is loaded on demand with them.
 */

/** Why a call failed, as the frame sees it. */
export type FailureCode =
  | 'invalid_request'
  | 'denied'
  | 'stale_view'
  | 'conflict'
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
export const RETRY_ID = /^[A-Za-z0-9_-]{8,36}$/;

/** The most bytes a frame may ask for in one part. */
export const MAX_PART = 1_048_576;

/** The most bytes of one file a frame may save or upload in one call. */
export const MAX_WRITE = 1_048_576;

/** The most bytes of an asset a frame may ask for in one part: 256 KiB. */
export const MAX_ASSET_PART = 262_144;

/**
 * What an asset ID looks like: 64 lowercase hexadecimal digits. It is written
 * out here, not imported, for the reason RETRY_ID gives (decision 0033).
 */
export const ASSET_ID = /^[0-9a-f]{64}$/;

/**
 * What a media type spelling looks like: `type/subtype` in lowercase letters,
 * digits and the punctuation RFC 6838 allows in a name. Only the client knows
 * which types an asset may have; this keeps the frame's value a spelling. It
 * is written out here for the reason RETRY_ID gives.
 */
export const MEDIA_TYPE =
  /^[a-z0-9][a-z0-9!#$&^_.+-]*\/[a-z0-9][a-z0-9!#$&^_.+-]*$/;

/** The most characters a frame may spell a media type with. */
export const MAX_MEDIA_TYPE_LENGTH = 127;

/** Whether `value` is a plain object, not null and not an array. */
export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/** Whether `value` is a safe integer of zero or more. */
export function isCount(value: unknown): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0;
}

/** A message to the frame. */
export interface FrameReply {
  readonly message: Record<string, unknown>;
  readonly transfer: Transferable[];
}

/** The reply that ends a call which succeeded. */
export function ok(id: number, body: Record<string, unknown>): FrameReply {
  return { message: { id, ok: true, ...body }, transfer: [] };
}

/** What serving a call needs from the rpc; the component supplies the prompt. */
export interface CallDeps {
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
}

/** A message from the frame, already known to be a plain object. */
export type Message = Readonly<Record<string, unknown>>;

/** What every call carries. */
interface CallHead<T extends string> {
  readonly type: T;
  readonly id: number;
}

/** One parsed, bounded call of type `T` with the fields `F` its parser kept. */
export type Call<T extends string, F extends object> = CallHead<T> &
  Readonly<F>;

/**
 * How one call type is told apart and served. The type is named here and
 * nowhere else on the path from a message to a reply.
 */
export interface CallDescriptor<T extends string, F extends object> {
  readonly type: T;
  /** The keys a message of this type carries, and no others. */
  readonly keys: readonly string[];
  /**
   * The bounded fields beyond `type` and `id`, or undefined when a value is
   * wrong. The keys and the id were checked before.
   */
  readonly parse: (message: Message) => F | undefined;
  /** Serves the call; declared `async`, so a throw becomes a failed call. */
  readonly serve: (
    call: Call<T, F>,
    deps: CallDeps,
    signal: AbortSignal,
  ) => Promise<FrameReply | undefined>;
}

/** A parsed call and what serves it. */
export interface ParsedCall<C> {
  readonly call: C;
  readonly serve: (
    deps: CallDeps,
    signal: AbortSignal,
  ) => Promise<FrameReply | undefined>;
}

/** A descriptor as the registry holds it, whatever its call's fields. */
export interface CallEntry<C> {
  readonly type: string;
  readonly keys: readonly string[];
  readonly parse: (message: Message, id: number) => ParsedCall<C> | undefined;
}

/** The call an entry parses. */
export type CallOf<E> = E extends CallEntry<infer C> ? C : never;

/** The registry entry of one descriptor. */
export function defineCall<T extends string, F extends object>(
  descriptor: CallDescriptor<T, F>,
): CallEntry<Call<T, F>> {
  return {
    type: descriptor.type,
    keys: descriptor.keys,
    parse(message, id) {
      const fields = descriptor.parse(message);
      if (fields === undefined) return undefined;
      // The envelope's type and id go last, so a parser's field cannot
      // replace either.
      const call: Call<T, F> = { ...fields, type: descriptor.type, id };
      return {
        call,
        serve: (deps, signal) => descriptor.serve(call, deps, signal),
      };
    },
  };
}
