import { startTimer, systemClock, type Clock } from '../transport.js';
import { ArtifreeApiError, type FailureCode } from '../errors.js';
import { isErrorCode, isRecord } from '../responses.js';
import {
  DeliveryAttemptError,
  DeliveryKernel,
  dispatchedRow,
  type DeliveryAttempt,
  type DeliveryContext,
  type DeliveryFailure,
} from './delivery.js';
import type {
  CleanupSnapshot,
  IntentRow,
  LaneKey,
  LocalStore,
  PreparedRow,
} from './store.js';
import { OutboxError } from './types.js';
import { LocalOperationGate } from './local-operation-gate.js';

const preparationAction = Symbol();
const deliveryAction = Symbol();
const cleanupAction = Symbol();
const unsentAction = Symbol();
const emptyGateAction = Symbol();
const rejectedAction = Symbol();
const reconciliation = Symbol();
const MAX_LANES = 256;

/** Internal one-action token; cannot authorize HTTP or renew a delivery epoch. */
export interface PreparationRetry {
  readonly [preparationAction]: true;
}
/** Internal one-action token; only a successful host revalidation permits renewal. */
export interface DeliveryResume {
  readonly [deliveryAction]: true;
}
/** Internal local-only retry for a retained durable ACK; never grants HTTP budget. */
export interface CleanupRetry {
  readonly [cleanupAction]: true;
}

/** Internal explicit never-prepared disposal; distinct from the public local handle. */
export interface SchedulerUnsentDiscardSelection {
  readonly [unsentAction]: true;
}

/** Internal explicit choice to retire one proven-empty preparation gate. */
export interface EmptyPreparationGateSelection {
  readonly [emptyGateAction]: true;
}

export interface UnsentDiscardResult {
  readonly kind: 'discarded_unsent';
}

export interface EmptyPreparationGateResult {
  readonly kind: 'retired_empty_preparation_gate';
}

/** Internal explicit choice to discard one rejected head; caller-initiated only. */
export interface RejectedDiscardSelection {
  readonly [rejectedAction]: true;
}

/** Committed rejected discard; `removedDependents` excludes the discarded head. */
export interface RejectedDiscardResult {
  readonly kind: 'discarded_rejected';
  readonly removedDependents: number;
}

/** Captured local authentication/lock proof; a delivery read is not this proof. */
export interface SchedulerCleanupAuthority {
  /** Stable page/profile/credential generation identity, never a caller label. */
  readonly identity: object;
  /** Abort synchronously on auth/profile/credential/lock loss. */
  readonly signal: AbortSignal;
  /** Check the still-open locally authenticated handle and exclusive owner lock. */
  readonly check: () => void;
}

/** Internal host contract; every durable admission must share this local gate. */
export interface SchedulerCleanupHost {
  readonly gate: LocalOperationGate;
  /**
   * Online uses one bound read with the existing deadline and cancellation.
   * Revoke captured proof/context at 401 headers before any body wait.
   * Offline must be explicit and makes no binding read or network fallback.
   */
  readonly authorize: (
    mode: 'online' | 'offline',
    signal: AbortSignal,
  ) => Promise<SchedulerCleanupAuthority>;
}

type RetryReason =
  | DeliveryFailure['reason']
  | 'ack_storage_failed'
  | 'aggregate_exhausted'
  | 'unresolved_after_rejection'
  | 'survivors_after_disposal';

type Pause =
  | {
      readonly kind: 'preparation_paused';
      readonly selection: PreparationRetry;
    }
  | {
      readonly kind: 'retry_paused';
      readonly selection: DeliveryResume;
      readonly reason: RetryReason;
      readonly scope: 'lane' | 'coordinator';
      readonly revalidation?: RevalidationFailure;
    }
  | {
      readonly kind: 'cleanup_paused';
      readonly selection: CleanupRetry;
      readonly ackDurable: true;
    }
  | { readonly kind: 'blocked'; readonly failure: DeliveryFailure };

/** Delivery refusals that prove the head was rejected, not merely delayed. */
type DefinitiveRejection = Extract<
  DeliveryFailure,
  { reason: 'data_conflict' | 'rejected' }
>;
/** The blocked pause, the only pause a rejection proof may name. */
type BlockedPause = Extract<Pause, { kind: 'blocked' }>;

/** Memory-only proof that a prepared head was refused; never persisted or caller-supplied. */
interface RejectionProof {
  readonly lane: Lane;
  readonly pause: BlockedPause;
  readonly failure: DefinitiveRejection;
  readonly row: Readonly<PreparedRow>;
}

/** Scheduler observations never replace canonical application-data snapshots. */
export type SchedulerEvent =
  | {
      readonly kind: 'attempt';
      readonly lane: LaneKey;
      readonly result: DeliveryAttempt;
    }
  | {
      readonly kind: 'delivery_retrying';
      readonly lane: LaneKey;
      readonly failure: DeliveryFailure;
      readonly delayMilliseconds: number;
      readonly laneRemaining: number;
      readonly aggregateRemaining: number;
    }
  | { readonly kind: 'paused'; readonly lane: LaneKey; readonly pause: Pause }
  | {
      readonly kind: 'empty_preparation_gate';
      readonly lane: LaneKey;
      readonly selection: EmptyPreparationGateSelection;
    }
  | {
      readonly kind: 'reconciled';
      readonly queued: number;
      readonly pending: boolean;
    }
  | { readonly kind: 'stopped'; readonly code: 'owner_not_ready' };

interface Lane {
  readonly key: LaneKey;
  failures: number;
  queued: boolean;
  wakeMark: object;
  /** Current-instance terminal preparation proof; consumed before the first run. */
  preparedWithoutDispatch?: PreparedRow;
  pause?: Pause;
  cancelTimer?: () => void;
  unsentSelection?: SchedulerUnsentDiscardSelection;
  emptySelection?: EmptyPreparationGateSelection;
  rejection?: RejectionProof;
  rejectedSelection?: RejectedDiscardSelection;
}

interface Action {
  readonly lane: Lane;
  readonly snapshot: string;
  readonly epoch: number;
}

interface Actions<T extends object, Context = Action, Result = void> {
  readonly tokens: WeakMap<T, Context>;
  readonly pending: WeakMap<T, Promise<Result>>;
}

interface CleanupActionContext extends Action {
  readonly mode: 'online' | 'offline';
  readonly authority: SchedulerCleanupAuthority;
  readonly snapshotState: CleanupSnapshot;
  readonly gatePause: Pause | undefined;
  readonly wakeMark: object;
}

interface RejectedActionContext extends Action {
  readonly mode: 'online' | 'offline';
  readonly authority: SchedulerCleanupAuthority;
  readonly snapshotState: CleanupSnapshot;
  readonly gatePause: BlockedPause;
  readonly wakeMark: object;
  readonly proof: RejectionProof;
  readonly failures: number;
}

/** The fields every cleanup action context binds; see checkActionBinding. */
type CleanupBinding = Pick<
  CleanupActionContext,
  'authority' | 'epoch' | 'gatePause' | 'lane' | 'wakeMark'
>;

function pendingChoice<T extends object, Context, Result>(
  selection: T,
  registry: Actions<T, Context, Result>,
  brand: symbol,
  operation: (selection: T) => Promise<Result>,
  closed?: () => boolean,
): Promise<Result> {
  if (typeof selection !== 'object' || selection === null)
    return Promise.reject(new OutboxError('invalid_selection'));
  try {
    if (Object.getOwnPropertyDescriptor(selection, brand)?.value !== true)
      return Promise.reject(new OutboxError('invalid_selection'));
  } catch {
    return Promise.reject(new OutboxError('invalid_selection'));
  }
  const pending = registry.pending.get(selection);
  if (pending !== undefined) return pending;
  // ADR 0049 rule 8 checks the scheduler before the token. While the scheduler
  // is closed, an unregistered token reaches the operation, whose check answers.
  if (!registry.tokens.has(selection) && closed?.() !== true)
    return Promise.reject(new OutboxError('invalid_selection'));
  const attempt = new Promise<Result>((resolve, reject) => {
    queueMicrotask(() => {
      void Promise.resolve()
        .then(() => operation(selection))
        .then(
          (result) => {
            resolve(result);
            registry.pending.delete(selection);
          },
          (error: unknown) => {
            reject(error);
            registry.pending.delete(selection);
          },
        )
        .catch((error: unknown) => {
          reject(error);
          registry.pending.delete(selection);
        });
    });
  });
  registry.pending.set(selection, attempt);
  return attempt;
}

type ActionKind = 'preparation' | 'delivery' | 'cleanup';

interface RefreshedPause {
  readonly lane: Lane;
  readonly pause: Extract<Pause, { kind: 'retry_paused' }>;
  readonly selection: DeliveryResume;
  readonly action: Action;
}

/** Bounded metadata only; excludes raw bodies, causes, messages and destinations. */
export interface RevalidationFailure {
  readonly code: FailureCode;
  readonly status: number;
  readonly retryAfterSeconds?: number | undefined;
}

/** A read failure grants no authority; proven identity/auth loss is terminal. */
export type RevalidationResult =
  | { readonly kind: 'authorized' }
  | {
      readonly kind: 'authority_lost';
      readonly reason:
        | 'binding_changed'
        | 'actor_generation_changed'
        | 'authentication_invalid';
    }
  | { readonly kind: 'read_failed'; readonly failure: RevalidationFailure };

function revalidationResult(value: unknown): RevalidationResult {
  if (!isRecord(value)) throw new OutboxError('owner_not_ready');
  if (value.kind === 'authorized' && Object.keys(value).join(',') === 'kind')
    return { kind: 'authorized' };
  if (
    value.kind === 'authority_lost' &&
    Object.keys(value).sort().join(',') === 'kind,reason' &&
    (value.reason === 'binding_changed' ||
      value.reason === 'actor_generation_changed' ||
      value.reason === 'authentication_invalid')
  )
    return { kind: 'authority_lost', reason: value.reason };
  if (
    value.kind !== 'read_failed' ||
    Object.keys(value).sort().join(',') !== 'failure,kind' ||
    !isRecord(value.failure)
  )
    throw new OutboxError('owner_not_ready');
  const { code, status, retryAfterSeconds } = value.failure;
  if (
    typeof code !== 'string' ||
    (code !== 'network_error' &&
      code !== 'invalid_response' &&
      !isErrorCode(code)) ||
    typeof status !== 'number' ||
    !Number.isInteger(status) ||
    (status !== 0 && (status < 100 || status > 599)) ||
    (retryAfterSeconds !== undefined &&
      (typeof retryAfterSeconds !== 'number' ||
        !Number.isFinite(retryAfterSeconds) ||
        retryAfterSeconds < 0))
  )
    throw new OutboxError('owner_not_ready');
  return {
    kind: 'read_failed',
    failure: {
      code,
      status,
      retryAfterSeconds:
        retryAfterSeconds === undefined
          ? undefined
          : Math.min(retryAfterSeconds, 3600),
    },
  };
}

/**
 * A host-only scheduler for a single fixed, already authorized owner/binding.
 * It neither obtains locks nor authorizes recovery, access or actor-generation
 * changes. The page registry must own its single instance before public use.
 */
export interface SchedulerHost {
  readonly context: DeliveryContext;
  /**
   * Exactly one bound read per explicit action; never follows redirects.
   * Abort context authority synchronously on proven loss, and at 401 headers
   * before waiting for any response body. Unexpected results/exceptions fail closed.
   */
  readonly revalidate: (
    lane: LaneKey,
    signal: AbortSignal,
  ) => Promise<RevalidationResult>;
  readonly observe: (event: SchedulerEvent) => void;
  readonly random?: () => number;
  /** Optional internal-only cleanup integration; absence leaves cleanup unavailable. */
  readonly cleanup?: SchedulerCleanupHost;
}

/**
 * Internal fresh-owner scheduling and explicitly configured local cleanup.
 * Not a browser entry point or inherited-owner recovery API. Call wake only
 * after durable admission under this host and its shared cleanup gate, if set.
 */
export class DeliveryScheduler {
  private readonly store: LocalStore;
  private readonly host: SchedulerHost;
  private readonly clock: Clock;
  private readonly kernel: DeliveryKernel;
  private readonly fence = new AbortController();
  private readonly publications = new AbortController();
  private readonly lanes = new Map<string, Lane>();
  private readonly ready: Array<Lane | typeof reconciliation> = [];
  private readonly preparations: Actions<PreparationRetry> = {
    tokens: new WeakMap(),
    pending: new WeakMap(),
  };
  private readonly deliveries: Actions<DeliveryResume> = {
    tokens: new WeakMap(),
    pending: new WeakMap(),
  };
  private readonly cleanups: Actions<CleanupRetry> = {
    tokens: new WeakMap(),
    pending: new WeakMap(),
  };
  private readonly discards: Actions<
    SchedulerUnsentDiscardSelection,
    CleanupActionContext,
    UnsentDiscardResult
  > = { tokens: new WeakMap(), pending: new WeakMap() };
  private readonly emptyGates: Actions<
    EmptyPreparationGateSelection,
    CleanupActionContext,
    EmptyPreparationGateResult
  > = { tokens: new WeakMap(), pending: new WeakMap() };
  private readonly rejections: Actions<
    RejectedDiscardSelection,
    RejectedActionContext,
    RejectedDiscardResult
  > = { tokens: new WeakMap(), pending: new WeakMap() };
  private aggregateFailures = 0;
  private epoch = 0;
  private pumping = false;
  private activeTask?: Promise<void>;
  private activeLane?: Lane;
  private activeWake = false;
  private acting = false;
  private reconciliationQueued = false;
  private reconciling = false;
  private reconcileAgain = false;
  private capacityWaiting = false;

  constructor(store: LocalStore, host: SchedulerHost) {
    this.store = store;
    this.host = {
      ...host,
      context: { ...host.context, binding: { ...host.context.binding } },
      ...(host.cleanup === undefined
        ? {}
        : {
            cleanup: Object.freeze({
              gate: host.cleanup.gate,
              authorize: host.cleanup.authorize.bind(host.cleanup),
            }),
          }),
    };
    this.clock = this.host.context.clock ?? systemClock;
    const abort = (): void => this.close();
    const signals = [this.host.context.signal];
    if (this.host.cleanup !== undefined)
      signals.push(this.host.cleanup.gate.signal);
    for (const signal of signals)
      signal.addEventListener('abort', abort, { once: true });
    this.fence.signal.addEventListener(
      'abort',
      () => {
        for (const signal of signals)
          signal.removeEventListener('abort', abort);
      },
      { once: true },
    );
    this.kernel = new DeliveryKernel(store, {
      ...host.context,
      signal: this.fence.signal,
    });
    if (signals.some((signal) => signal.aborted)) this.close();
  }

  /** Coalesces wakes. No wake clears a pause, timer or failure budget. */
  wake(key: LaneKey): void {
    this.check(key);
    const id = JSON.stringify(key);
    let lane = this.lanes.get(id);
    if (lane === undefined) {
      if (this.lanes.size >= MAX_LANES) {
        // Keys are hints, not proof of persistence. One marker cannot be filled
        // by stale/nonexistent keys and will discover only scoped durable rows.
        this.requestReconciliation();
        return;
      }
      lane = { key: [...key], failures: 0, queued: false, wakeMark: {} };
      this.lanes.set(id, lane);
    }
    this.retireCleanupChoices(lane);
    lane.wakeMark = {};
    this.queue(lane);
  }

  /** Synchronously aborts native requests/transactions and cancels retry timers. */
  close(): void {
    this.publications.abort();
    this.stopDelivery();
    this.host.cleanup?.gate.close();
  }

  private stopDelivery(): void {
    if (this.fence.signal.aborted) return;
    this.fence.abort();
    for (const lane of this.lanes.values()) {
      lane.cancelTimer?.();
      // The proof ends with the fence (ADR 0049 rule 2), and the lane stops
      // referencing its selection, so a closed scheduler keeps no rows. A
      // retired selection also leaves the token map (rule 16). The blocked
      // pause stays.
      if (lane.rejectedSelection !== undefined)
        this.rejections.tokens.delete(lane.rejectedSelection);
      delete lane.rejection;
      delete lane.rejectedSelection;
    }
    this.ready.length = 0;
    this.reconciliationQueued = false;
    this.reconcileAgain = false;
    this.capacityWaiting = false;
  }

  /** One deliberate local preparation attempt; does not revalidate or refill budgets. */
  retryPreparation(selection: PreparationRetry): Promise<void> {
    return this.action(selection, this.preparations, 'preparation');
  }

  /** One deliberate epoch renewal after fresh host binding/access validation. */
  resumeDelivery(selection: DeliveryResume): Promise<void> {
    return this.action(selection, this.deliveries, 'delivery');
  }

  /** One deliberate trim-only transaction; no HTTP, binding read or budget renewal. */
  retryCleanup(selection: CleanupRetry): Promise<void> {
    return this.action(selection, this.cleanups, 'cleanup');
  }

  /** Select a retained zero-failure preparation gate; never deletes or enables delivery. */
  selectUnsent(
    key: LaneKey,
    mode: 'online' | 'offline' = 'online',
  ): Promise<SchedulerUnsentDiscardSelection> {
    return this.selectCleanup(key, mode, false).then((selection) => {
      if (!(unsentAction in selection))
        throw new OutboxError('invalid_selection');
      return selection;
    });
  }

  /** Select one retained empty preparation gate; never inferred from wake/scan. */
  selectEmptyPreparationGate(
    key: LaneKey,
    mode: 'online' | 'offline' = 'online',
  ): Promise<EmptyPreparationGateSelection> {
    return this.selectCleanup(key, mode, true).then((selection) => {
      if (!(emptyGateAction in selection))
        throw new OutboxError('invalid_selection');
      return selection;
    });
  }

  /**
   * Selects the proven rejected head of a settled, blocked lane. Caller-initiated
   * only: throws invalid_selection and changes nothing unless the stored closure
   * still matches the rejection proof.
   */
  async selectRejected(
    key: LaneKey,
    mode: 'online' | 'offline' = 'online',
  ): Promise<RejectedDiscardSelection> {
    const selectedKey = this.selectedLaneKey(key);
    return this.cleanupBoundary(async (host) => {
      const { authority, state } = await this.captureSelection(
        host,
        selectedKey,
        mode,
      );
      const lane = this.lanes.get(JSON.stringify(selectedKey));
      const proof = lane?.rejection;
      if (
        lane === undefined ||
        proof === undefined ||
        proof.lane !== lane ||
        lane.pause !== proof.pause ||
        !this.settled(lane) ||
        state.kind !== 'intents' ||
        state.rows[0]?.state !== 'prepared' ||
        JSON.stringify(state.rows[0]) !== JSON.stringify(proof.row) ||
        !state.rows.slice(1).every((row) => row.state === 'unprepared')
      )
        throw new OutboxError('invalid_selection');
      this.retireCleanupChoices(lane);
      const context: RejectedActionContext = {
        lane,
        snapshot: state.signature,
        snapshotState: state,
        epoch: this.epoch,
        mode,
        authority,
        gatePause: lane.pause,
        wakeMark: lane.wakeMark,
        proof,
        failures: lane.failures,
      };
      const selection = Object.freeze<RejectedDiscardSelection>({
        [rejectedAction]: true,
      });
      this.rejections.tokens.set(selection, context);
      lane.rejectedSelection = selection;
      return selection;
    });
  }

  /** Delete one selected never-prepared closure, preserving terminal commit meaning. */
  discardUnsent(
    selection: SchedulerUnsentDiscardSelection,
  ): Promise<UnsentDiscardResult> {
    return pendingChoice(
      selection,
      this.discards,
      unsentAction,
      async (chosen) => {
        const context = this.discards.tokens.get(chosen);
        this.discards.tokens.delete(chosen);
        if (context === undefined) throw new OutboxError('invalid_selection');
        return this.cleanupBoundary(async (host) => {
          this.checkCleanupContext(context);
          const authority = await this.cleanupAuthority(host, context.mode);
          this.sameCleanupAuthority(context, authority);
          this.checkCleanupContext(context);
          const snapshot = context.snapshotState;
          if (snapshot.kind !== 'intents')
            throw new OutboxError('invalid_selection');
          const empty = await this.cleanupSignal(authority, (signal) =>
            this.store.discardSnapshot(
              snapshot,
              () => this.checkCleanupContext(context),
              signal,
            ),
          );
          const result: UnsentDiscardResult = { kind: 'discarded_unsent' };
          // The native transaction has completed. Losing authority or a wake
          // after that point suppresses publication, never the committed result.
          try {
            this.checkCleanupContext(context);
          } catch {
            return result;
          }
          if (empty) this.retireCleanupLane(context.lane);
          else this.retireCleanupChoices(context.lane);
          return result;
        });
      },
    );
  }

  /** Explicit metadata retirement; a successful empty probe grants no delivery. */
  retireEmptyPreparationGate(
    selection: EmptyPreparationGateSelection,
  ): Promise<EmptyPreparationGateResult> {
    return pendingChoice(
      selection,
      this.emptyGates,
      emptyGateAction,
      async (chosen) => {
        const context = this.emptyGates.tokens.get(chosen);
        this.emptyGates.tokens.delete(chosen);
        if (context === undefined) throw new OutboxError('invalid_selection');
        return this.cleanupBoundary(async (host) => {
          this.checkCleanupContext(context);
          const authority = await this.cleanupAuthority(host, context.mode);
          this.sameCleanupAuthority(context, authority);
          this.checkCleanupContext(context);
          if (context.snapshotState.kind !== 'empty')
            throw new OutboxError('invalid_selection');
          await this.cleanupSignal(authority, (signal) =>
            this.store.checkEmptyCleanup(
              context.lane.key,
              () => this.checkCleanupContext(context),
              signal,
            ),
          );
          this.checkCleanupContext(context);
          this.retireCleanupLane(context.lane);
          const result: EmptyPreparationGateResult = {
            kind: 'retired_empty_preparation_gate',
          };
          return result;
        });
      },
    );
  }

  /**
   * Discards a selected rejected head and its unprepared dependents in one
   * local transaction. Rejects, with no stored change, if the selection is
   * stale or authority is lost before commit. A committed result is returned
   * even if publication is suppressed afterwards.
   */
  discardRejected(
    selection: RejectedDiscardSelection,
  ): Promise<RejectedDiscardResult> {
    return pendingChoice(
      selection,
      this.rejections,
      rejectedAction,
      async (chosen) => {
        const context = this.rejections.tokens.get(chosen);
        this.rejections.tokens.delete(chosen);
        this.check();
        if (context === undefined) throw new OutboxError('invalid_selection');
        return this.cleanupBoundary(async (host) => {
          this.checkRejectedContext(context);
          const authority = await this.cleanupAuthority(host, context.mode);
          this.sameCleanupAuthority(context, authority);
          this.checkRejectedContext(context);
          const snapshot = context.snapshotState;
          if (snapshot.kind !== 'intents')
            throw new OutboxError('invalid_selection');
          const disposal = await this.cleanupSignal(authority, (signal) =>
            this.store.discardRejectedSnapshot(
              snapshot,
              context.proof.row,
              () => this.checkRejectedContext(context),
              signal,
            ),
          );
          const result = Object.freeze<RejectedDiscardResult>({
            kind: 'discarded_rejected',
            removedDependents: disposal.removedDependents,
          });
          const lane = context.lane;
          // The transaction's terminal event has decided. Losing authority or
          // the lane identity now suppresses publication, never the result.
          try {
            this.checkCleanupProof(authority, lane.key);
            if (this.lanes.get(JSON.stringify(lane.key)) !== lane)
              throw new OutboxError('invalid_selection');
          } catch {
            return result;
          }
          if (disposal.survivors.length === 0) {
            // A wake after complete does not block retirement; a reconciliation
            // rediscovers any rows that the wake enqueued.
            const woken = lane.wakeMark !== context.wakeMark;
            this.retireCleanupLane(lane);
            if (woken && !this.capacityWaiting) this.requestReconciliation();
          } else {
            this.publishSurvivors(lane, disposal.survivors);
          }
          return result;
        });
      },
      () => this.fence.signal.aborted,
    );
  }

  private sameCleanupAuthority(
    context: CleanupActionContext,
    authority: SchedulerCleanupAuthority,
  ): void {
    if (
      authority.identity !== context.authority.identity ||
      authority.signal !== context.authority.signal
    )
      throw new OutboxError('owner_not_ready');
  }

  /** Checks what every cleanup action binds; each kind adds its own rules. */
  private checkActionBinding(context: CleanupBinding): Lane {
    const lane = context.lane;
    this.checkCleanupProof(context.authority, lane.key);
    if (
      this.lanes.get(JSON.stringify(lane.key)) !== lane ||
      this.epoch !== context.epoch ||
      lane.pause !== context.gatePause ||
      lane.wakeMark !== context.wakeMark
    )
      throw new OutboxError('invalid_selection');
    return lane;
  }

  private checkCleanupContext(context: CleanupActionContext): void {
    const lane = this.checkActionBinding(context);
    if (
      lane.pause?.kind !== 'preparation_paused' ||
      lane.failures !== 0 ||
      lane.cancelTimer !== undefined ||
      this.activeLane === lane
    )
      throw new OutboxError('invalid_selection');
  }

  private checkRejectedContext(context: RejectedActionContext): void {
    const lane = this.checkActionBinding(context);
    if (
      lane.rejection !== context.proof ||
      lane.failures !== context.failures ||
      !this.settled(lane)
    )
      throw new OutboxError('invalid_selection');
  }

  private retireCleanupLane(lane: Lane): void {
    this.retirePause(lane);
    this.retireCleanupChoices(lane);
    delete lane.rejection;
    lane.queued = false;
    this.lanes.delete(JSON.stringify(lane.key));
    if (this.capacityWaiting) this.requestReconciliation();
  }

  /**
   * Publishes the survivor resume choice from the rows that the committed
   * transaction returned. Reads nothing, so it cannot fail after commit.
   */
  private publishSurvivors(lane: Lane, survivors: readonly IntentRow[]): void {
    delete lane.rejection;
    this.publishRetryPause(lane, 'survivors_after_disposal', 'lane', {
      lane,
      snapshot: JSON.stringify(survivors),
      epoch: this.epoch,
    });
  }

  /** Publishes a retry pause whose resume token replays the captured action. */
  private publishRetryPause(
    lane: Lane,
    reason: RetryReason,
    scope: 'lane' | 'coordinator',
    captured: Action,
  ): void {
    const selection = Object.freeze<DeliveryResume>({ [deliveryAction]: true });
    this.retirePause(lane);
    this.deliveries.tokens.set(selection, captured);
    lane.pause = { kind: 'retry_paused', reason, scope, selection };
    this.emit({ kind: 'paused', lane: lane.key, pause: lane.pause });
  }

  private cleanupBoundary<Result>(
    operation: (host: SchedulerCleanupHost) => Promise<Result>,
  ): Promise<Result> {
    const host = this.host.cleanup;
    if (host === undefined || !(host.gate instanceof LocalOperationGate))
      return Promise.reject(new OutboxError('owner_not_ready'));
    return host.gate.run(async () => {
      this.check();
      if (this.acting) throw new OutboxError('invalid_selection');
      this.acting = true;
      try {
        await this.activeTask;
        this.check();
        return await operation(host);
      } finally {
        this.acting = false;
        this.kick();
      }
    });
  }

  private async cleanupAuthority(
    host: SchedulerCleanupHost,
    mode: 'online' | 'offline',
  ): Promise<SchedulerCleanupAuthority> {
    if (mode !== 'online' && mode !== 'offline')
      throw new OutboxError('invalid_selection');
    try {
      const proof = await host.authorize(mode, this.fence.signal);
      if (
        !isRecord(proof) ||
        typeof proof.identity !== 'object' ||
        proof.identity === null ||
        !(proof.signal instanceof AbortSignal) ||
        typeof proof.check !== 'function'
      )
        throw new OutboxError('owner_not_ready');
      const captured = Object.freeze({
        identity: proof.identity,
        signal: proof.signal,
        check: proof.check.bind(proof),
      });
      captured.check();
      if (captured.signal.aborted) {
        this.close();
        throw new OutboxError('owner_not_ready');
      }
      this.check();
      return captured;
    } catch (error: unknown) {
      if (error instanceof ArtifreeApiError) {
        let failure: RevalidationFailure;
        try {
          const result = revalidationResult({
            kind: 'read_failed',
            failure: {
              code: error.code,
              status: error.status,
              retryAfterSeconds: error.retryAfterSeconds,
            },
          });
          if (result.kind !== 'read_failed')
            throw new OutboxError('owner_not_ready');
          failure = result.failure;
        } catch {
          throw new OutboxError('owner_not_ready');
        }
        if (
          failure.status === 401 ||
          (failure.status === 409 && failure.code === 'outbox_binding_changed')
        ) {
          this.close();
          if (failure.status === 401) {
            try {
              this.host.context.invalidateAuthentication();
            } catch {
              throw new OutboxError('owner_not_ready');
            }
          }
        }
        throw new ArtifreeApiError(failure.code, failure);
      }
      throw new OutboxError('owner_not_ready');
    }
  }

  private checkCleanupProof(
    proof: SchedulerCleanupAuthority,
    key: LaneKey,
  ): void {
    this.check(key);
    try {
      if (proof.signal.aborted) {
        this.close();
        throw new OutboxError('owner_not_ready');
      }
      proof.check();
    } catch {
      throw new OutboxError('owner_not_ready');
    }
  }

  private async cleanupSignal<Result>(
    proof: SchedulerCleanupAuthority,
    operation: (signal: AbortSignal) => Promise<Result>,
  ): Promise<Result> {
    const linked = new AbortController();
    const sources = [this.fence.signal, proof.signal];
    const abort = (): void => {
      if (proof.signal.aborted) this.close();
      linked.abort();
    };
    for (const source of sources) {
      if (source.aborted) abort();
      else source.addEventListener('abort', abort, { once: true });
    }
    try {
      return await operation(linked.signal);
    } finally {
      for (const source of sources) source.removeEventListener('abort', abort);
    }
  }

  private retireCleanupChoices(lane: Lane): void {
    if (lane.unsentSelection !== undefined)
      this.discards.tokens.delete(lane.unsentSelection);
    if (lane.emptySelection !== undefined)
      this.emptyGates.tokens.delete(lane.emptySelection);
    if (lane.rejectedSelection !== undefined)
      this.rejections.tokens.delete(lane.rejectedSelection);
    delete lane.unsentSelection;
    delete lane.emptySelection;
    delete lane.rejectedSelection;
  }

  // A blocked lane is settled only when no delivery, timer or queue entry can
  // still change its pause or rows.
  private settled(lane: Lane): boolean {
    return (
      lane.pause !== undefined &&
      this.activeLane !== lane &&
      lane.cancelTimer === undefined &&
      !lane.queued
    );
  }

  private selectedLaneKey(key: LaneKey): LaneKey {
    try {
      if (!Array.isArray(key) || key.length !== 5)
        throw new OutboxError('owner_not_ready');
      const selectedKey: LaneKey = [key[0], key[1], key[2], key[3], key[4]];
      this.check(selectedKey);
      return selectedKey;
    } catch {
      throw new OutboxError('owner_not_ready');
    }
  }

  /**
   * Captures a lane's cleanup snapshot between the cleanup authority and proof
   * checks. Callers still validate the snapshot against their own rules.
   */
  private async captureSelection(
    host: SchedulerCleanupHost,
    key: LaneKey,
    mode: 'online' | 'offline',
  ): Promise<{
    readonly authority: SchedulerCleanupAuthority;
    readonly state: CleanupSnapshot;
  }> {
    const authority = await this.cleanupAuthority(host, mode);
    const state = await this.cleanupSignal(authority, (signal) =>
      this.store.captureCleanup(
        key,
        () => this.checkCleanupProof(authority, key),
        signal,
      ),
    );
    this.checkCleanupProof(authority, key);
    return { authority, state };
  }

  private async selectCleanup(
    key: LaneKey,
    mode: 'online' | 'offline',
    empty: boolean,
  ): Promise<SchedulerUnsentDiscardSelection | EmptyPreparationGateSelection> {
    const selectedKey = this.selectedLaneKey(key);
    return this.cleanupBoundary(async (host) => {
      const { authority, state } = await this.captureSelection(
        host,
        selectedKey,
        mode,
      );
      const id = JSON.stringify(selectedKey);
      const lane = this.lanes.get(id);
      if (
        lane?.pause?.kind !== 'preparation_paused' ||
        lane.failures !== 0 ||
        lane.cancelTimer !== undefined ||
        this.activeLane === lane ||
        (empty
          ? state.kind !== 'empty'
          : state.kind !== 'intents' || state.rows[0]?.state !== 'unprepared')
      )
        throw new OutboxError('invalid_selection');
      this.retireCleanupChoices(lane);
      const context: CleanupActionContext = {
        lane,
        snapshot: state.kind === 'intents' ? state.signature : 'empty',
        snapshotState: state,
        epoch: this.epoch,
        mode,
        authority,
        gatePause: lane.pause,
        wakeMark: lane.wakeMark,
      };
      if (empty) {
        const selection = Object.freeze<EmptyPreparationGateSelection>({
          [emptyGateAction]: true,
        });
        this.emptyGates.tokens.set(selection, context);
        lane.emptySelection = selection;
        this.emit({
          kind: 'empty_preparation_gate',
          lane: [...selectedKey],
          selection,
        });
        return selection;
      }
      const selection = Object.freeze<SchedulerUnsentDiscardSelection>({
        [unsentAction]: true,
      });
      this.discards.tokens.set(selection, context);
      lane.unsentSelection = selection;
      return selection;
    });
  }

  private check(lane?: LaneKey): void {
    if (this.fence.signal.aborted) throw new OutboxError('owner_not_ready');
    try {
      this.host.context.authorize();
      const context = this.host.context;
      if (
        lane !== undefined &&
        (!Array.isArray(lane) ||
          lane.length !== 5 ||
          lane[0] !== context.owner ||
          lane[1] !== context.base ||
          lane[2] !== context.binding.actor_id ||
          lane[3] !== context.binding.storage_epoch ||
          typeof lane[4] !== 'string' ||
          !/^art_[0-9a-f]{32}$/.test(lane[4]))
      )
        throw new OutboxError('owner_not_ready');
    } catch {
      this.close();
      throw new OutboxError('owner_not_ready');
    }
  }

  private emit(event: SchedulerEvent): void {
    if (this.fence.signal.aborted) return;
    this.observe(event);
  }

  private observe(event: SchedulerEvent): boolean {
    if (this.publications.signal.aborted) return false;
    try {
      // Consumers may be JavaScript or deliberately mutate readonly objects.
      // Never give a callback the state that the pump still needs to classify.
      if (event.kind === 'attempt') {
        this.host.observe({
          ...event,
          lane: [...event.lane],
          result: structuredClone(event.result),
        });
      } else if (event.kind === 'paused') {
        this.host.observe({
          ...event,
          lane: [...event.lane],
          pause:
            event.pause.kind === 'blocked'
              ? { ...event.pause, failure: { ...event.pause.failure } }
              : event.pause.kind === 'retry_paused' &&
                  event.pause.revalidation !== undefined
                ? {
                    ...event.pause,
                    revalidation: { ...event.pause.revalidation },
                  }
                : { ...event.pause },
        });
      } else if (event.kind === 'delivery_retrying') {
        this.host.observe({
          ...event,
          lane: [...event.lane],
          failure: { ...event.failure },
        });
      } else this.host.observe(event);
      return true;
    } catch {
      // A consumer exception must not escape into delivery or leak diagnostics.
      this.close();
      return false;
    }
  }

  private queue(lane: Lane): void {
    if (this.lanes.get(JSON.stringify(lane.key)) !== lane) return;
    if (lane === this.activeLane) {
      this.activeWake = true;
      return;
    }
    if (
      lane.queued ||
      lane.pause !== undefined ||
      lane.cancelTimer !== undefined
    )
      return;
    lane.queued = true;
    this.ready.push(lane);
    this.kick();
  }

  private requestReconciliation(): void {
    if (this.fence.signal.aborted) return;
    if (this.reconciling) {
      this.reconcileAgain = true;
      return;
    }
    if (this.reconciliationQueued) return;
    this.reconciliationQueued = true;
    this.ready.push(reconciliation);
    this.kick();
  }

  private retireIdle(lane: Lane): void {
    const id = JSON.stringify(lane.key);
    if (this.lanes.get(id) !== lane) return;
    this.retireCleanupChoices(lane);
    this.lanes.delete(id);
    if (this.capacityWaiting) this.requestReconciliation();
  }

  private async reconcile(): Promise<void> {
    this.reconciliationQueued = false;
    this.reconciling = true;
    this.reconcileAgain = false;
    try {
      this.check();
      const eligible = [...this.lanes.values()]
        .filter(
          (lane) =>
            lane.failures === 0 &&
            lane.pause === undefined &&
            lane.cancelTimer === undefined,
        )
        .map((lane) => ({ lane, mark: lane.wakeMark }));
      const context = this.host.context;
      const rows = await this.store.recover(
        [
          context.owner,
          context.base,
          context.binding.actor_id,
          context.binding.storage_epoch,
        ],
        this.fence.signal,
      );
      this.check();
      if (rows.length > MAX_LANES) throw new OutboxError('storage_corrupt');
      const heads = new Map<string, IntentRow>();
      for (const row of rows) {
        this.check(row.lane);
        if (row.authorizedGeneration !== context.binding.actor_generation)
          throw new OutboxError('owner_not_ready');
        const id = JSON.stringify(row.lane);
        const prior = heads.get(id);
        if (prior === undefined || prior.sequence > row.sequence)
          heads.set(id, row);
      }
      const ordered = [...heads.values()].sort(
        (left, right) => left.sequence - right.sequence,
      );
      this.check();
      // No state changes precede fallible reads/validation. A wake during the
      // snapshot protects its lane even if that snapshot still showed no row.
      for (const { lane, mark } of eligible) {
        const id = JSON.stringify(lane.key);
        if (
          !heads.has(id) &&
          this.lanes.get(id) === lane &&
          lane.wakeMark === mark &&
          lane.failures === 0 &&
          lane.pause === undefined &&
          lane.cancelTimer === undefined
        ) {
          this.lanes.delete(id);
          lane.queued = false;
        }
      }
      const scheduled: Lane[] = [];
      this.capacityWaiting = false;
      for (const head of ordered) {
        const id = JSON.stringify(head.lane);
        if (this.lanes.has(id)) continue;
        if (this.lanes.size >= MAX_LANES) {
          this.capacityWaiting = true;
          continue;
        }
        const lane: Lane = {
          key: [...head.lane],
          failures: 0,
          queued: true,
          wakeMark: {},
        };
        this.lanes.set(id, lane);
        scheduled.push(lane);
      }
      // These heads occupy the first overflow hint's FIFO position. Existing
      // queued work before the marker has already had its round-robin turn.
      this.ready.unshift(...scheduled);
      this.emit({
        kind: 'reconciled',
        queued: scheduled.length,
        pending: this.capacityWaiting || this.reconcileAgain,
      });
    } finally {
      this.reconciling = false;
    }
    if (this.reconcileAgain) this.requestReconciliation();
  }

  private kick(): void {
    if (this.pumping || this.acting || this.fence.signal.aborted) return;
    this.pumping = true;
    this.activeTask = this.pump()
      .catch(() => {
        this.emit({ kind: 'stopped', code: 'owner_not_ready' });
        this.close();
      })
      .finally(() => {
        this.pumping = false;
        if (this.ready.length > 0 && !this.fence.signal.aborted) this.kick();
      });
  }

  private async pump(): Promise<void> {
    while (this.ready.length > 0 && !this.acting) {
      this.check();
      const lane = this.ready.shift();
      if (lane === undefined) return;
      if (lane === reconciliation) {
        await this.reconcile();
        continue;
      }
      lane.queued = false;
      if (this.lanes.get(JSON.stringify(lane.key)) !== lane) continue;
      // wake() can have queued this lane while its previous HTTP was active.
      // Eligibility may have changed after that wake, before this dequeue.
      if (lane.pause !== undefined || lane.cancelTimer !== undefined) continue;
      if (this.aggregateFailures >= 32) {
        await this.pause(lane, 'aggregate_exhausted', 'coordinator');
        continue;
      }
      let result: DeliveryAttempt;
      let woken = false;
      this.activeLane = lane;
      this.activeWake = false;
      try {
        const locallyPrepared = lane.preparedWithoutDispatch;
        delete lane.preparedWithoutDispatch;
        result = await this.kernel.run(lane.key, locallyPrepared);
        woken = this.activeWake;
      } catch (error: unknown) {
        if (error instanceof DeliveryAttemptError && error.dispatched) {
          lane.failures += 1;
          this.aggregateFailures += 1;
        }
        throw error;
      } finally {
        delete this.activeLane;
      }
      if (this.fence.signal.aborted) return;
      this.check();
      if (
        result.kind === 'delivery_failed' &&
        result.failure.reason === 'binding_changed'
      ) {
        this.failed(lane);
        const pause: Pause = { kind: 'blocked', failure: result.failure };
        lane.pause = pause;
        // Fence the whole captured binding before either external callback.
        this.stopDelivery();
        const observations: SchedulerEvent[] = [
          { kind: 'attempt', lane: lane.key, result },
          { kind: 'paused', lane: lane.key, pause },
        ];
        for (const event of observations) {
          // This synchronous terminal refusal is the only publication after
          // our own close. External auth/lock invalidation still suppresses it.
          if (
            this.publications.signal.aborted ||
            this.host.context.signal.aborted
          )
            return;
          try {
            this.host.context.authorize();
          } catch {
            this.close();
            return;
          }
          if (!this.observe(event)) return;
        }
        return;
      }
      this.emit({ kind: 'attempt', lane: lane.key, result });
      this.check();
      switch (result.kind) {
        case 'idle':
          // Reclaim only genuinely idle healthy lanes. Paused/failed lanes must
          // retain their history until an explicit epoch renewal.
          if (woken) this.queue(lane);
          else if (lane.failures === 0 && !lane.queued) this.retireIdle(lane);
          break;
        case 'cleanup_complete':
          this.queue(lane);
          break;
        case 'server_acknowledged':
          lane.failures = 0;
          this.queue(lane);
          break;
        case 'acknowledged_cleanup_pending':
          lane.failures = 0;
          await this.pauseCleanup(lane);
          break;
        case 'preparation_storage_failed': {
          await this.pausePreparation(lane);
          break;
        }
        case 'ack_storage_failed':
          this.failed(lane);
          await this.pause(lane, 'ack_storage_failed', 'lane');
          break;
        case 'delivery_failed': {
          this.failed(lane);
          const row = dispatchedRow(result);
          await this.failure(lane, result.failure, result.priorDelivery, row);
          break;
        }
        default: {
          const exhaustive: never = result;
          return exhaustive;
        }
      }
    }
  }

  private failed(lane: Lane): void {
    lane.failures += 1;
    this.aggregateFailures += 1;
    if (this.aggregateFailures >= 32) {
      for (const waiting of this.lanes.values()) {
        waiting.cancelTimer?.();
        delete waiting.cancelTimer;
        if (waiting !== lane) this.queue(waiting);
      }
    }
  }

  private async capture(lane: Lane): Promise<Action> {
    this.check(lane.key);
    const rows = await this.store.lane(lane.key);
    this.check(lane.key);
    return { lane, snapshot: JSON.stringify(rows), epoch: this.epoch };
  }

  private async pausePreparation(lane: Lane): Promise<void> {
    const selection = Object.freeze<PreparationRetry>({
      [preparationAction]: true,
    });
    const captured = await this.capture(lane);
    this.retirePause(lane);
    this.preparations.tokens.set(selection, captured);
    lane.pause = { kind: 'preparation_paused', selection };
    this.emit({ kind: 'paused', lane: lane.key, pause: lane.pause });
  }

  private async pauseCleanup(lane: Lane): Promise<void> {
    const selection = Object.freeze<CleanupRetry>({ [cleanupAction]: true });
    const captured = await this.capture(lane);
    this.retirePause(lane);
    this.cleanups.tokens.set(selection, captured);
    lane.pause = { kind: 'cleanup_paused', selection, ackDurable: true };
    this.emit({ kind: 'paused', lane: lane.key, pause: lane.pause });
  }

  private retirePause(lane: Lane): void {
    this.retireCleanupChoices(lane);
    if (lane.pause?.kind === 'preparation_paused')
      this.preparations.tokens.delete(lane.pause.selection);
    if (lane.pause?.kind === 'retry_paused')
      this.deliveries.tokens.delete(lane.pause.selection);
    if (lane.pause?.kind === 'cleanup_paused')
      this.cleanups.tokens.delete(lane.pause.selection);
  }

  private async validateSnapshot(action: Action): Promise<void> {
    if ((await this.capture(action.lane)).snapshot === action.snapshot) return;
    const pause = action.lane.pause;
    // A new enqueue may legitimately stale a complete-lane snapshot. Refresh
    // only the metadata, then require another deliberate action; no gate or
    // budget is cleared by this failed selection.
    if (pause?.kind === 'preparation_paused')
      await this.pausePreparation(action.lane);
    if (pause?.kind === 'retry_paused')
      await this.pause(action.lane, pause.reason, pause.scope);
    if (pause?.kind === 'cleanup_paused') await this.pauseCleanup(action.lane);
    throw new OutboxError('invalid_selection');
  }

  private async retryLocalPreparation(
    action: Action,
    beginMutation: () => void,
  ): Promise<boolean> {
    const { lane } = action;
    this.check(lane.key);
    const first = (await this.store.lane(lane.key))[0];
    this.check(lane.key);
    if (first?.state !== 'unprepared')
      throw new OutboxError('invalid_selection');
    beginMutation();
    let prepared: PreparedRow;
    try {
      prepared = await this.store.prepare(
        first,
        () => this.check(lane.key),
        this.fence.signal,
        action.snapshot,
      );
    } catch (error: unknown) {
      this.check();
      if (
        !(error instanceof OutboxError) ||
        (error.code !== 'storage_failed' && error.code !== 'invalid_selection')
      )
        throw error;
      await this.pausePreparation(lane);
      if (error.code === 'invalid_selection') throw error;
      return false;
    }
    this.check();
    // This proof comes only from this instance's completed local transaction.
    // It survives delivery gates, but never a run, head replacement or reload.
    lane.preparedWithoutDispatch = prepared;
    return true;
  }

  private async retryLocalCleanup(
    action: Action,
    beginMutation: () => void,
  ): Promise<boolean> {
    const { lane } = action;
    this.check(lane.key);
    const rows = await this.store.lane(lane.key);
    this.check(lane.key);
    if (rows[0]?.state !== 'acknowledged')
      throw new OutboxError('invalid_selection');
    beginMutation();
    try {
      await this.store.trim(
        rows,
        () => this.check(lane.key),
        this.fence.signal,
        action.snapshot,
      );
    } catch (error: unknown) {
      this.check(lane.key);
      if (
        !(error instanceof OutboxError) ||
        (error.code !== 'storage_failed' && error.code !== 'invalid_selection')
      )
        throw error;
      await this.pauseCleanup(lane);
      if (error.code === 'invalid_selection') throw error;
      return false;
    }
    this.check(lane.key);
    return true;
  }

  private async pause(
    lane: Lane,
    reason: RetryReason,
    scope: 'lane' | 'coordinator',
  ): Promise<void> {
    const captured = await this.capture(lane);
    if (
      reason === 'aggregate_exhausted' &&
      scope === 'coordinator' &&
      lane.failures === 0 &&
      lane.pause === undefined &&
      captured.snapshot === '[]'
    ) {
      // A completed trim can leave no work despite a globally exhausted epoch.
      // Reclaim only a proven healthy idle lane; a concurrent wake stays queued.
      if (!lane.queued) this.retireIdle(lane);
      return;
    }
    this.publishRetryPause(lane, reason, scope, captured);
  }

  private async failure(
    lane: Lane,
    failure: DeliveryFailure,
    priorDelivery: 'none' | 'unknown',
    row?: Readonly<PreparedRow>,
  ): Promise<void> {
    if (failure.reason === 'binding_changed') {
      this.close();
      return;
    }
    if (
      priorDelivery === 'unknown' &&
      (failure.reason === 'data_conflict' || failure.reason === 'rejected')
    ) {
      // A refusal of this attempt cannot resolve the retained prepared head.
      // Keep its explicit lane gate even if another lane renews the epoch.
      await this.pause(lane, 'unresolved_after_rejection', 'lane');
      return;
    }
    const retryable =
      failure.reason === 'network_error' ||
      failure.reason === 'invalid_response' ||
      failure.reason === 'rate_limited' ||
      failure.reason === 'server_error';
    if (!retryable) {
      // Access/generation recovery and unresolved abandonment (R08c-3b) are not
      // implemented by this fixed-authority host. Only the rejection proof below
      // reaches discardRejected; no token here may bypass those future gates.
      if (failure.reason === 'unclassified_error_response') {
        await this.pause(lane, failure.reason, 'lane');
      } else {
        const pause: BlockedPause = { kind: 'blocked', failure };
        lane.pause = pause;
        // Observers run inside emit, so the proof must exist before it.
        // Only a refusal with no earlier delivery proves the head was refused.
        if (
          priorDelivery === 'none' &&
          (failure.reason === 'data_conflict' ||
            failure.reason === 'rejected') &&
          row !== undefined &&
          this.lanes.get(JSON.stringify(lane.key)) === lane &&
          !this.fence.signal.aborted
        ) {
          lane.rejection = Object.freeze({ lane, pause, failure, row });
        }
        this.emit({ kind: 'paused', lane: lane.key, pause });
      }
      return;
    }
    if (lane.failures >= 8 || this.aggregateFailures >= 32) {
      await this.pause(
        lane,
        failure.reason,
        this.aggregateFailures >= 32 ? 'coordinator' : 'lane',
      );
      return;
    }
    const random = (this.host.random ?? Math.random)();
    if (!Number.isFinite(random) || random < 0 || random >= 1)
      throw new OutboxError('owner_not_ready');
    const retryAfter =
      'retryAfterSeconds' in failure
        ? Math.min(failure.retryAfterSeconds ?? 0, 3600) * 1000
        : 0;
    const delayMilliseconds = Math.max(
      retryAfter,
      Math.min(500 * 2 ** (lane.failures - 1), 30_000) * (0.5 + random / 2),
    );
    this.emit({
      kind: 'delivery_retrying',
      lane: lane.key,
      failure,
      delayMilliseconds,
      laneRemaining: 8 - lane.failures,
      aggregateRemaining: 32 - this.aggregateFailures,
    });
    this.check();
    // Install the sentinel first: injected clocks may invoke their callback
    // synchronously, including while the previous attempt is still unwinding.
    let fired = false;
    lane.cancelTimer = () => undefined;
    const cancel = startTimer(this.clock, delayMilliseconds, () => {
      fired = true;
      delete lane.cancelTimer;
      if (!this.fence.signal.aborted) this.queue(lane);
    });
    if (!fired) lane.cancelTimer = cancel;
  }

  private action<T extends object>(
    selection: T,
    registry: Actions<T>,
    kind: ActionKind,
  ): Promise<void> {
    const brand =
      kind === 'preparation'
        ? preparationAction
        : kind === 'delivery'
          ? deliveryAction
          : cleanupAction;
    return pendingChoice(selection, registry, brand, (chosen) =>
      this.applyAction(chosen, registry.tokens, kind),
    );
  }

  private async applyAction<T extends object>(
    selection: T,
    tokens: WeakMap<T, Action>,
    kind: ActionKind,
  ): Promise<void> {
    this.check();
    const action = tokens.get(selection);
    if (
      action === undefined ||
      this.acting ||
      action.lane.pause === undefined ||
      action.lane.pause.kind === 'blocked' ||
      action.lane.pause.selection !== selection ||
      (kind === 'delivery' && action.epoch !== this.epoch)
    )
      throw new OutboxError('invalid_selection');
    const initialPause = action.lane.pause;
    let bindingReadStarted = false;
    this.acting = true;
    try {
      await this.activeTask;
      this.check();
      if (kind === 'delivery') {
        // One choice permits one binding read, even when later local staging
        // fails before an epoch can be renewed. Pending calls still coalesce.
        tokens.delete(selection);
        bindingReadStarted = true;
        let result: RevalidationResult;
        try {
          result = revalidationResult(
            await this.host.revalidate([...action.lane.key], this.fence.signal),
          );
        } catch {
          this.close();
          throw new OutboxError('owner_not_ready');
        }
        if (
          result.kind === 'authority_lost' ||
          (result.kind === 'read_failed' &&
            (result.failure.status === 401 ||
              (result.failure.status === 409 &&
                result.failure.code === 'outbox_binding_changed')))
        ) {
          // Fence before any host callback, including a throwing invalidator.
          this.close();
          if (
            (result.kind === 'authority_lost' &&
              result.reason === 'authentication_invalid') ||
            (result.kind === 'read_failed' && result.failure.status === 401)
          ) {
            try {
              if (!this.host.context.signal.aborted)
                this.host.context.invalidateAuthentication();
            } catch {
              throw new OutboxError('owner_not_ready');
            }
          }
          if (result.kind === 'authority_lost')
            throw new OutboxError('owner_not_ready');
          throw new ArtifreeApiError(result.failure.code, result.failure);
        }
        this.check();
        if (result.kind === 'read_failed') {
          const pause = action.lane.pause;
          if (pause?.kind !== 'retry_paused')
            throw new OutboxError('invalid_selection');
          const replacement = Object.freeze<DeliveryResume>({
            [deliveryAction]: true,
          });
          tokens.delete(selection);
          this.deliveries.tokens.set(replacement, action);
          action.lane.pause = {
            ...pause,
            selection: replacement,
            revalidation: result.failure,
          };
          this.emit({
            kind: 'paused',
            lane: action.lane.key,
            pause: action.lane.pause,
          });
          throw new ArtifreeApiError(result.failure.code, result.failure);
        }
      }
      this.check();
      await this.validateSnapshot(action);
      if (kind === 'delivery') {
        const nextEpoch = this.epoch + 1;
        const refreshed: RefreshedPause[] = [];
        for (const lane of this.lanes.values()) {
          if (
            lane === action.lane ||
            lane.pause?.kind !== 'retry_paused' ||
            lane.pause.scope !== 'lane'
          )
            continue;
          const captured = await this.capture(lane);
          refreshed.push({
            lane,
            pause: lane.pause,
            selection: Object.freeze<DeliveryResume>({
              [deliveryAction]: true,
            }),
            action: { ...captured, epoch: nextEpoch },
          });
        }
        // All fallible reads finish before changing epoch/gates/counters or
        // other-lane tokens. The selected one-read choice is already consumed.
        // The selected lane is checked last; callbacks see the complete epoch.
        await this.validateSnapshot(action);
        this.check();
        tokens.delete(selection);
        delete action.lane.pause;
        this.epoch = nextEpoch;
        this.aggregateFailures = 0;
        for (const lane of this.lanes.values()) {
          lane.failures = 0;
          lane.cancelTimer?.();
          delete lane.cancelTimer;
          // Choices name the epoch that issued them, so renewal retires them.
          this.retireCleanupChoices(lane);
          if (
            lane.pause?.kind === 'retry_paused' &&
            lane.pause.scope === 'coordinator'
          )
            delete lane.pause;
        }
        for (const current of refreshed) {
          this.deliveries.tokens.delete(current.pause.selection);
          this.deliveries.tokens.set(current.selection, current.action);
          current.lane.pause = {
            ...current.pause,
            selection: current.selection,
          };
        }
        for (const lane of this.lanes.values()) {
          this.queue(lane);
        }
        for (const current of refreshed) {
          this.emit({
            kind: 'paused',
            lane: current.lane.key,
            pause: { ...current.pause, selection: current.selection },
          });
        }
      } else {
        // Preserve the old gate through preliminary reads and until a terminal
        // preparation outcome. A failed read cannot silently enable wake().
        const priorPause = action.lane.pause;
        let started = false;
        const beginMutation = (): void => {
          // One deliberate choice permits one transaction, even if capturing
          // its replacement pause subsequently fails. Pending calls still
          // coalesce through their separate per-kind promise registry.
          tokens.delete(selection);
          this.retireCleanupChoices(action.lane);
          started = true;
        };
        try {
          const prepared =
            kind === 'preparation'
              ? await this.retryLocalPreparation(action, beginMutation)
              : await this.retryLocalCleanup(action, beginMutation);
          if (prepared) {
            delete action.lane.pause;
            this.queue(action.lane);
          }
        } catch (error: unknown) {
          // A spent choice without a successfully captured replacement gate
          // cannot leave a live scheduler with an unactionable/stale pause.
          if (started && action.lane.pause === priorPause) this.close();
          throw error;
        }
      }
    } catch (error: unknown) {
      // A consumed binding choice with no replacement pause must never leave
      // the old gate live after failed local renewal staging.
      if (bindingReadStarted && action.lane.pause === initialPause)
        this.close();
      throw error;
    } finally {
      this.acting = false;
      this.kick();
    }
  }
}
