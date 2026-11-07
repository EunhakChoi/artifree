import type { DataWriteResponse } from '@artifree/contracts';
import { parseRetryAfter } from '../client.js';
import { ArtifreeApiError } from '../errors.js';
import { parseDataWrite, parseError } from '../responses.js';
import {
  BodyReader,
  ERROR_BODY_LIMIT,
  RETRY_AFTER_CAP_SECONDS,
  SUCCESS_BODY_LIMIT,
  cancelQuietly,
  startTimer,
  systemClock,
  type Clock,
} from '../transport.js';
import {
  normalizeBase,
  validActor,
  validGeneration,
  validId,
} from './binding.js';
import type {
  AcknowledgedRow,
  LaneKey,
  LocalStore,
  PreparedRow,
} from './store.js';
import { OutboxError, type Binding } from './types.js';

/** Internal host authority. This does not acquire or establish an owner lock. */
export interface DeliveryContext {
  readonly base: string;
  readonly owner: string;
  readonly accessToken: string;
  readonly binding: Binding;
  /** Abort synchronously before releasing the lock or invalidating captured authority. */
  readonly signal: AbortSignal;
  /**
   * Throws unless captured page/owner/binding/credentials and the held lock remain
   * current. Exceptions become owner_not_ready without host diagnostics.
   */
  readonly authorize: () => void;
  /** Revokes host proof after 401; exceptions remain fenced and become owner_not_ready. */
  readonly invalidateAuthentication: () => void;
  readonly clock?: Clock;
}

/** A fenced local failure records whether this invocation called native Fetch. */
export class DeliveryAttemptError extends OutboxError {
  readonly dispatched: boolean;
  constructor(error: OutboxError, dispatched: boolean) {
    super(error.code);
    this.name = 'DeliveryAttemptError';
    this.dispatched = dispatched;
  }
}

/** Bounded observations only; no response body, message, destination or credentials. */
export type DeliveryFailure =
  | {
      readonly reason:
        'network_error' | 'invalid_response' | 'rate_limited' | 'server_error';
      readonly status: number;
      readonly retryAfterSeconds?: number;
    }
  | { readonly reason: 'access_unavailable'; readonly status: 401 | 403 | 404 }
  | { readonly reason: 'binding_changed'; readonly status: 409 }
  | {
      readonly reason: 'data_conflict';
      readonly status: 409;
      readonly currentDataVersion: number;
    }
  | {
      readonly reason: 'rejected';
      readonly status: number;
      readonly code:
        | 'data_path_conflict'
        | 'invalid_request'
        | 'data_too_large'
        | 'request_key_reused'
        | 'payload_too_large';
    }
  | { readonly reason: 'unclassified_error_response'; readonly status: number };

type HttpOutcome =
  | { readonly kind: 'receipt'; readonly receipt: DataWriteResponse }
  | { readonly kind: 'failed'; readonly failure: DeliveryFailure };

/** One attempt never authorizes another attempt, rejection disposal or budget renewal. */
export type DeliveryAttempt =
  | { readonly kind: 'idle' | 'cleanup_complete' }
  | {
      readonly kind: 'preparation_storage_failed';
      readonly durablyEnqueued: true;
      readonly prepared: false;
    }
  | {
      readonly kind: 'delivery_failed';
      readonly failure: DeliveryFailure;
      readonly outcome: 'unknown';
      /** Durable phase at attempt entry, independently of local dispatch history. */
      readonly initialState: 'unprepared' | 'prepared';
      /** Earlier delivery is unknown unless this instance proves no dispatch. */
      readonly priorDelivery: 'none' | 'unknown';
    }
  | {
      readonly kind: 'ack_storage_failed';
      readonly observedReceipt: Readonly<DataWriteResponse>;
      readonly ackDurable: false;
    }
  | {
      readonly kind: 'acknowledged_cleanup_pending';
      readonly receipt: Readonly<DataWriteResponse>;
      readonly ackDurable: true;
    }
  | {
      readonly kind: 'server_acknowledged';
      readonly receipt: Readonly<DataWriteResponse>;
      readonly ackDurable: true;
    };

/**
 * Kernel-internal channel for the frozen decoded row of a refused dispatch. It
 * is kept beside DeliveryAttempt rather than on it, so the row never reaches an
 * observation: a structured clone of an attempt does not carry the entry.
 */
const dispatchedRows = new WeakMap<DeliveryAttempt, Readonly<PreparedRow>>();

/**
 * Returns the frozen row this kernel dispatched for a refused `delivery_failed`
 * attempt (`data_conflict` or `rejected`). Returns undefined for any other
 * attempt, including a retryable failure and a clone of one.
 */
export function dispatchedRow(
  attempt: DeliveryAttempt,
): Readonly<PreparedRow> | undefined {
  return dispatchedRows.get(attempt);
}

function deepFreeze(value: unknown): void {
  if (typeof value !== 'object' || value === null || Object.isFrozen(value))
    return;
  Object.freeze(value);
  const children: unknown[] = Object.values(value);
  for (const child of children) deepFreeze(child);
}

/** Copies the row so the registered form cannot change after the attempt returns. */
function freezeRow(row: PreparedRow): Readonly<PreparedRow> {
  const copy = structuredClone(row);
  deepFreeze(copy);
  return copy;
}

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}
function validErrorEnvelope(value: unknown): boolean {
  if (
    !record(value) ||
    Object.keys(value).join(',') !== 'error' ||
    !record(value.error)
  )
    return false;
  const error = value.error;
  const fields = [
    'code',
    'message',
    'current_revision',
    'current_metadata_revision',
    'current_data_version',
    'conflicting_files',
    'conflicting_files_total',
  ];
  if (Object.keys(error).some((key) => !fields.includes(key))) return false;
  const total = error.conflicting_files_total;
  if (
    total !== undefined &&
    total !== null &&
    (typeof total !== 'number' || !Number.isSafeInteger(total) || total < 0)
  )
    return false;
  const files = error.conflicting_files;
  if (
    files !== undefined &&
    files !== null &&
    (!Array.isArray(files) ||
      files.length > 32 ||
      files.some(
        (file: unknown) =>
          !record(file) ||
          Object.keys(file).sort().join(',') !== 'current_sha256,path' ||
          typeof file.path !== 'string' ||
          !(
            file.current_sha256 === null ||
            (typeof file.current_sha256 === 'string' &&
              /^[0-9a-f]{64}$/.test(file.current_sha256))
          ),
      ))
  )
    return false;
  return true;
}

function responseFailure(
  status: number,
  value: unknown,
  retryAfterSeconds?: number,
): DeliveryFailure {
  if (status === 401 || status === 403 || status === 404)
    return { reason: 'access_unavailable', status };
  if (status === 429 || (status >= 500 && status <= 599)) {
    const reason = status === 429 ? 'rate_limited' : 'server_error';
    return retryAfterSeconds === undefined
      ? { reason, status }
      : { reason, status, retryAfterSeconds };
  }
  const error = validErrorEnvelope(value) ? parseError(value) : undefined;
  if (status === 409 && error?.code === 'outbox_binding_changed')
    return { reason: 'binding_changed', status };
  if (
    status === 409 &&
    error?.code === 'data_conflict' &&
    typeof error.current_data_version === 'number'
  )
    return {
      reason: 'data_conflict',
      status,
      currentDataVersion: error.current_data_version,
    };
  if (
    error !== undefined &&
    ((status === 409 && error.code === 'data_path_conflict') ||
      ((status === 400 || status === 422) &&
        error.code === 'invalid_request') ||
      (status === 422 &&
        (error.code === 'data_too_large' ||
          error.code === 'request_key_reused')) ||
      (status === 413 && error.code === 'payload_too_large'))
  ) {
    const code = error.code;
    if (
      code === 'data_path_conflict' ||
      code === 'invalid_request' ||
      code === 'data_too_large' ||
      code === 'request_key_reused' ||
      code === 'payload_too_large'
    )
      return { reason: 'rejected', status, code };
  }
  return { reason: 'unclassified_error_response', status };
}

async function boundedJson(
  response: Response,
  signal: AbortSignal,
  clock: Clock,
): Promise<unknown> {
  if (response.body === null) return undefined;
  const reader = new BodyReader(response.body, {
    clock,
    signal,
    status: response.status,
    what: 'outbox receipt',
    keepCause: false,
  });
  const limit = response.status === 200 ? SUCCESS_BODY_LIMIT : ERROR_BODY_LIMIT;
  const chunks: Uint8Array[] = [];
  let size = 0;
  try {
    for (;;) {
      const chunk = await reader.read();
      if (chunk === 'end') break;
      if (chunk === 'aborted')
        throw new ArtifreeApiError('network_error', {
          status: response.status,
          code: 'network_error',
        });
      size += chunk.byteLength;
      if (size > limit)
        throw new ArtifreeApiError('invalid_response', {
          status: response.status,
          code: 'invalid_response',
        });
      chunks.push(chunk);
    }
  } finally {
    reader.close();
  }
  const bytes = new Uint8Array(size);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  try {
    const value: unknown = JSON.parse(
      new TextDecoder('utf-8', { fatal: true }).decode(bytes),
    );
    return value;
  } catch {
    // Body syntax never supplies authority, error classification or an ACK.
    return undefined;
  }
}

async function dispatch(
  row: PreparedRow,
  context: DeliveryContext,
  clock: Clock,
  check: () => void,
  onDispatch: () => void,
): Promise<HttpOutcome> {
  check();
  const controller = new AbortController();
  const abort = (): void => controller.abort();
  context.signal.addEventListener('abort', abort, { once: true });
  if (context.signal.aborted) controller.abort();
  const stopHeaders = startTimer(clock, 15_000, abort);
  const stopWhole = startTimer(clock, 60_000, abort);
  let status = 0;
  let retryAfterSeconds: number | undefined;
  let response: Response | undefined;
  try {
    check();
    // Clock.after may run synchronously. No request starts after an expired gate.
    if (controller.signal.aborted) throw new OutboxError('owner_not_ready');
    onDispatch();
    response = await globalThis.fetch(
      `${context.base}/v1/artifacts/${encodeURIComponent(row.artifactId)}/data`,
      {
        method: row.request.method,
        body: row.request.body,
        headers: {
          Authorization: `Bearer ${context.accessToken}`,
          'Content-Type': 'application/json',
          'Idempotency-Key': row.key,
          'Artifree-Outbox-Actor': row.binding[2],
          'Artifree-Outbox-Epoch': row.binding[3],
          'Artifree-Outbox-Actor-Generation': row.authorizedGeneration,
        },
        credentials: 'omit',
        cache: 'no-store',
        redirect: 'error',
        signal: controller.signal,
      },
    );
    stopHeaders();
    status = response.status;
    const retryAfter = parseRetryAfter(response.headers.get('retry-after'));
    retryAfterSeconds =
      retryAfter === undefined
        ? undefined
        : Math.min(retryAfter, RETRY_AFTER_CAP_SECONDS);
    check();
    if (
      status === 401 ||
      status === 403 ||
      status === 404 ||
      status === 429 ||
      (status >= 500 && status <= 599)
    )
      return {
        kind: 'failed',
        failure: responseFailure(status, undefined, retryAfterSeconds),
      };
    if (status >= 200 && status <= 299 && status !== 200)
      return {
        kind: 'failed',
        failure: { reason: 'invalid_response', status },
      };
    const value = await boundedJson(response, controller.signal, clock);
    check();
    if (controller.signal.aborted)
      return {
        kind: 'failed',
        failure:
          status === 200
            ? { reason: 'network_error', status }
            : responseFailure(status, undefined, retryAfterSeconds),
      };
    if (status !== 200)
      return {
        kind: 'failed',
        failure: responseFailure(status, value, retryAfterSeconds),
      };
    const receipt = parseDataWrite(value);
    if (
      typeof value !== 'object' ||
      value === null ||
      Object.keys(value).length !== 5 ||
      receipt === undefined ||
      receipt.artifact_id !== row.artifactId ||
      receipt.path !== row.path ||
      receipt.data_version !== row.request.base + (receipt.changed ? 1 : 0)
    )
      return {
        kind: 'failed',
        failure: { reason: 'invalid_response', status },
      };
    return { kind: 'receipt', receipt };
  } catch (error: unknown) {
    check();
    if (error instanceof OutboxError) throw error;
    const failure: DeliveryFailure =
      status === 0 || status === 200
        ? {
            reason:
              error instanceof ArtifreeApiError &&
              error.code === 'invalid_response'
                ? 'invalid_response'
                : 'network_error',
            status,
          }
        : responseFailure(status, undefined, retryAfterSeconds);
    return { kind: 'failed', failure };
  } finally {
    stopHeaders();
    stopWhole();
    context.signal.removeEventListener('abort', abort);
    if (response?.body !== null && response?.body !== undefined)
      cancelQuietly(response.body);
  }
}

/**
 * Internal, instance-serialized one-attempt kernel. The owner coordinator must hold
 * its native lock and supply captured authority checks. Not exported by the
 * public browser entry point; no retry, resume, recovery or delivery activation.
 */
export class DeliveryKernel {
  private readonly store: LocalStore;
  private readonly context: DeliveryContext;
  private readonly clock: Clock;
  private readonly fence = new AbortController();
  private active = false;
  private dispatched = false;

  constructor(store: LocalStore, context: DeliveryContext) {
    this.store = store;
    const invalidate = (): void => this.fence.abort();
    context.signal.addEventListener('abort', invalidate, { once: true });
    this.fence.signal.addEventListener(
      'abort',
      () => {
        context.signal.removeEventListener('abort', invalidate);
      },
      { once: true },
    );
    if (context.signal.aborted) invalidate();
    this.context = {
      ...context,
      binding: { ...context.binding },
      signal: this.fence.signal,
    };
    this.clock = context.clock ?? systemClock;
  }

  /** The coordinator may pass only its own just-committed, never-dispatched head. */
  async run(
    lane: LaneKey,
    locallyPrepared?: PreparedRow,
  ): Promise<DeliveryAttempt> {
    if (this.active)
      throw new DeliveryAttemptError(new OutboxError('owner_not_ready'), false);
    this.active = true;
    this.dispatched = false;
    try {
      return await this.attempt(lane, locallyPrepared);
    } catch (error: unknown) {
      if (error instanceof OutboxError)
        throw new DeliveryAttemptError(error, this.dispatched);
      throw error;
    } finally {
      this.active = false;
    }
  }

  private check(lane: LaneKey): void {
    const context = this.context;
    if (context.signal.aborted) throw new OutboxError('owner_not_ready');
    try {
      context.authorize();
    } catch {
      // Host exception data may contain credentials or captured payloads.
      this.fence.abort();
      throw new OutboxError('owner_not_ready');
    }
    try {
      if (
        normalizeBase(
          context.base,
          globalThis.location?.origin ?? 'http://localhost',
        ) !== context.base ||
        !validId(context.owner) ||
        !validActor(context.binding.actor_id) ||
        !validId(context.binding.storage_epoch) ||
        !validGeneration(context.binding.actor_generation) ||
        (context.binding.actor_id === 'local' &&
          context.binding.actor_generation !== '1') ||
        lane[0] !== context.owner ||
        lane[1] !== context.base ||
        lane[2] !== context.binding.actor_id ||
        lane[3] !== context.binding.storage_epoch
      )
        throw new OutboxError('owner_not_ready');
    } catch (error: unknown) {
      // A failed observation permanently fences this captured context, including
      // native transactions already waiting on its signal. It cannot be revived.
      this.fence.abort();
      throw error;
    }
  }

  private async cleanup(
    row: AcknowledgedRow,
    lane: LaneKey,
    source: 'received' | 'retained',
  ): Promise<DeliveryAttempt> {
    const check = (): void => this.check(lane);
    const current = await this.store.lane(lane);
    check();
    try {
      await this.store.trim(current, check, this.context.signal);
    } catch (error: unknown) {
      check();
      if (!(error instanceof OutboxError) || error.code !== 'storage_failed')
        throw error;
      return {
        kind: 'acknowledged_cleanup_pending',
        receipt: row.receipt,
        ackDurable: true,
      };
    }
    check();
    return source === 'retained'
      ? { kind: 'cleanup_complete' }
      : {
          kind: 'server_acknowledged',
          receipt: row.receipt,
          ackDurable: true,
        };
  }

  private async attempt(
    lane: LaneKey,
    locallyPrepared: PreparedRow | undefined,
  ): Promise<DeliveryAttempt> {
    const check = (): void => this.check(lane);
    check();
    const rows = await this.store.lane(lane);
    check();
    const first = rows[0];
    if (first === undefined) return { kind: 'idle' };
    if (first.authorizedGeneration !== this.context.binding.actor_generation)
      throw new OutboxError('owner_not_ready');
    if (first.state === 'acknowledged')
      return this.cleanup(first, lane, 'retained');
    const initialState = first.state;
    const priorDelivery =
      initialState === 'unprepared' ||
      (locallyPrepared !== undefined &&
        JSON.stringify(first) === JSON.stringify(locallyPrepared))
        ? 'none'
        : 'unknown';
    let prepared: PreparedRow;
    if (first.state === 'unprepared') {
      try {
        prepared = await this.store.prepare(first, check, this.context.signal);
      } catch (error: unknown) {
        check();
        if (!(error instanceof OutboxError) || error.code !== 'storage_failed')
          throw error;
        return {
          kind: 'preparation_storage_failed',
          durablyEnqueued: true,
          prepared: false,
        };
      }
    } else prepared = first;
    check();
    const retained = await this.store.lane(lane);
    check();
    if (JSON.stringify(retained[0]) !== JSON.stringify(prepared))
      throw new OutboxError('invalid_selection');
    const outcome = await dispatch(
      prepared,
      this.context,
      this.clock,
      check,
      () => {
        this.dispatched = true;
      },
    );
    check();
    if (outcome.kind === 'failed') {
      if (
        outcome.failure.reason === 'access_unavailable' &&
        outcome.failure.status === 401
      ) {
        this.fence.abort();
        try {
          this.context.invalidateAuthentication();
        } catch {
          throw new OutboxError('owner_not_ready');
        }
      }
      const failedAttempt: DeliveryAttempt = {
        kind: 'delivery_failed',
        failure: outcome.failure,
        outcome: 'unknown',
        initialState,
        priorDelivery,
      };
      // Only a refusal proves that the head was refused, so a retryable failure
      // keeps no row.
      if (
        outcome.failure.reason === 'data_conflict' ||
        outcome.failure.reason === 'rejected'
      ) {
        dispatchedRows.set(failedAttempt, freezeRow(prepared));
      }
      return failedAttempt;
    }
    let acknowledged: AcknowledgedRow;
    try {
      acknowledged = await this.store.acknowledge(
        prepared,
        outcome.receipt,
        check,
        this.context.signal,
      );
    } catch (error: unknown) {
      check();
      if (!(error instanceof OutboxError) || error.code !== 'storage_failed')
        throw error;
      return {
        kind: 'ack_storage_failed',
        observedReceipt: outcome.receipt,
        ackDurable: false,
      };
    }
    check();
    return this.cleanup(acknowledged, lane, 'received');
  }
}
