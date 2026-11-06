/** Opt-in browser-local outbox. Delivery/preparation/ACK are not implemented. */
import { ArtifreeApiError } from '../errors.js';
import { dataValueText, newRandomId } from '../client.js';
import { normalizeBase, readBinding, validId } from './binding.js';
import {
  bindingKey,
  cleanupTarget,
  openStore,
  type BindingKey,
  type IntentRow,
  type LocalStore,
} from './store.js';
import {
  OutboxError,
  type Binding,
  type EnqueuedIntent,
  type LocalOutboxState,
  type OutboxConfiguration,
  type OutboxIntent,
  type RecoverySelection,
  type UnsentDiscardSelection,
  type UnsentCleanupOptions,
  type UnsentCleanupTarget,
} from './types.js';

export { OutboxError } from './types.js';
export type {
  Binding,
  EnqueuedIntent,
  IntentBase,
  LocalOutboxState,
  OutboxConfiguration,
  OutboxFailure,
  OutboxIntent,
  RecoverySelection,
  UnsentDiscardSelection,
  UnsentCleanupOptions,
  UnsentCleanupTarget,
} from './types.js';

const ACTIVE = 'artifree-data-outbox-owner-v1';
const CANDIDATE = 'artifree-data-outbox-candidate-v1';
interface HeldOwner {
  readonly release: () => void;
  readonly settled: Promise<void>;
}
interface RecoveryContext {
  readonly owner: string;
  readonly rows: IntentRow[];
  readonly authority: symbol;
}
interface DiscardContext {
  readonly owner: string;
  readonly rows: IntentRow[];
  readonly authority: symbol;
  readonly mode: 'online' | 'offline';
}
const PAGE_REGISTRY = Symbol.for('artifree.data-outbox.page-coordinator.v1');
interface PageRegistry {
  readonly base: string;
  readonly token: string;
  readonly opening: Promise<BrowserOutbox>;
  coordinator?: BrowserOutbox;
}
declare global {
  interface Window {
    [PAGE_REGISTRY]?: PageRegistry;
  }
}

/** Public handle excludes coordinator construction and startup internals. */
export type BrowserOutbox = Pick<
  LocalCoordinator,
  | 'state'
  | 'enqueue'
  | 'retryRecoveryCandidate'
  | 'confirmRecovery'
  | 'clearRecoveryCandidate'
  | 'selectUnsent'
  | 'listUnsentTargets'
  | 'invalidateAuthentication'
  | 'discardUnsent'
  | 'close'
>;

function saved(storage: Storage, key: string): string | null {
  const value = storage.getItem(key);
  if (value !== null && !validId(value))
    throw new OutboxError('storage_failed');
  return value;
}

function persistReference(
  storage: Storage,
  key: string,
  value: string | null,
): void {
  try {
    if (value === null) storage.removeItem(key);
    else storage.setItem(key, value);
  } catch {
    throw new OutboxError('storage_failed');
  }
}

function cleanupOptions(options: UnsentCleanupOptions): {
  mode: 'online' | 'offline';
  storageEpoch?: string;
} {
  if (
    typeof options !== 'object' ||
    options === null ||
    Array.isArray(options) ||
    Object.keys(options).some((key) => key !== 'mode' && key !== 'storageEpoch')
  )
    throw new OutboxError('invalid_selection');
  const mode = options.mode ?? 'online';
  if (mode !== 'online' && mode !== 'offline')
    throw new OutboxError('invalid_selection');
  const storageEpoch = options.storageEpoch;
  if (storageEpoch === undefined) return { mode };
  if (!validId(storageEpoch)) throw new OutboxError('invalid_selection');
  return { mode, storageEpoch };
}

/** Nonblocking exclusive lock; rejection cannot escape as an unhandled promise. */
async function lockOwner(owner: string): Promise<HeldOwner | undefined> {
  let answer: (held: HeldOwner | undefined) => void = () => undefined;
  let fail: (error: Error) => void = () => undefined;
  const acquisition = new Promise<HeldOwner | undefined>((resolve, reject) => {
    answer = resolve;
    fail = reject;
  });
  let release: () => void = () => undefined;
  const lifetime = new Promise<void>((resolve) => {
    release = resolve;
  });
  const settled = navigator.locks.request(
    `artifree-data-outbox:${owner}`,
    { ifAvailable: true },
    async (lock) => {
      if (lock === null) {
        answer(undefined);
        return;
      }
      answer({ release, settled });
      await lifetime;
    },
  );
  void settled.catch(() => {
    fail(new OutboxError('outbox_unavailable'));
  });
  return acquisition;
}

/**
 * One document coordinator, independent of React lifetimes. Repeated opens with
 * the same immutable connection share it. Close before changing credentials.
 * No native/process recovery or network delivery is claimed by this entry point.
 */
export async function openBrowserOutbox(
  configuration: OutboxConfiguration,
): Promise<BrowserOutbox> {
  const base = normalizeBase(
    configuration.baseUrl,
    globalThis.location?.origin ?? 'http://localhost',
  );
  if ('fetch' in configuration)
    throw new OutboxError('invalid_outbox_configuration');
  if (globalThis.location === undefined || globalThis.window === undefined)
    throw new OutboxError('outbox_unavailable');
  const registry = window[PAGE_REGISTRY];
  if (registry !== undefined) {
    if (registry.base !== base || registry.token !== configuration.accessToken)
      throw new OutboxError('owner_not_ready');
    return registry.opening;
  }
  const opening = initialize(base, configuration.accessToken);
  window[PAGE_REGISTRY] = { base, token: configuration.accessToken, opening };
  try {
    return await opening;
  } catch (error: unknown) {
    if (window[PAGE_REGISTRY]?.opening === opening)
      delete window[PAGE_REGISTRY];
    throw error;
  }
}

async function initialize(base: string, token: string): Promise<BrowserOutbox> {
  let storage: Storage;
  let active: string | null;
  let candidate: string | null;
  try {
    if (navigator.locks === undefined || globalThis.indexedDB === undefined)
      throw new OutboxError('outbox_unavailable');
    storage = globalThis.sessionStorage;
    active = saved(storage, ACTIVE);
    candidate = saved(storage, CANDIDATE);
  } catch {
    throw new OutboxError('outbox_unavailable');
  }
  const binding = await readBinding(base, token);
  const store = await openStore();
  const outbox = new LocalCoordinator(base, token, binding, store, storage);
  try {
    await outbox.initializeOwner(active, candidate);
    const registry = window[PAGE_REGISTRY];
    if (registry !== undefined) registry.coordinator = outbox;
    return outbox;
  } catch (error: unknown) {
    await outbox.close();
    throw error instanceof OutboxError
      ? error
      : new OutboxError('storage_failed');
  }
}

/** Local-only coordinator. There is intentionally no sender or resumeDelivery. */
class LocalCoordinator {
  private readonly base: string;
  private readonly token: string;
  private readonly binding: Binding;
  private readonly store: LocalStore;
  private readonly storage: Storage;
  private readonly locks = new Map<string, HeldOwner>();
  private readonly recovered = new Set<string>();
  private readonly pausedLanes = new Set<string>();
  private readonly recoverySelections = new WeakMap<
    RecoverySelection,
    RecoveryContext
  >();
  private readonly discardSelections = new WeakMap<
    UnsentDiscardSelection,
    DiscardContext
  >();
  private owner = '';
  private candidate: string | null = null;
  private current: LocalOutboxState = { kind: 'closed' };
  private closing = false;
  private detached = false;
  private locallyAuthenticated = true;
  private readonly stop = new AbortController();
  private cleanupAuthorization = new AbortController();
  private authority = Symbol();
  private tail: Promise<void> = Promise.resolve();

  /** Open through openBrowserOutbox to preserve the page registry/owner gate. */
  constructor(
    base: string,
    token: string,
    binding: Binding,
    store: LocalStore,
    storage: Storage,
  ) {
    this.base = base;
    this.token = token;
    this.binding = binding;
    this.store = store;
    this.storage = storage;
  }

  get state(): LocalOutboxState {
    return this.current;
  }

  private ensureOpen(): void {
    if (this.closing) throw new OutboxError('outbox_closed');
    if (this.detached) throw new OutboxError('owner_not_ready');
  }

  /** Revoke a logged-out/replaced profile without releasing its owner locks. */
  invalidateAuthentication(): void {
    this.detached = true;
    this.revokeCleanupProof();
    if (!this.closing) this.current = { kind: 'binding_paused' };
    this.stop.abort();
  }

  private revokeCleanupProof(): void {
    this.locallyAuthenticated = false;
    this.authority = Symbol();
    this.recovered.clear();
    const pending = this.cleanupAuthorization;
    this.cleanupAuthorization = new AbortController();
    pending.abort();
  }

  private requireCleanupProof(): void {
    this.ensureOpen();
    if (!this.locallyAuthenticated) throw new OutboxError('owner_not_ready');
  }

  private async authorizeCleanup(mode: 'online' | 'offline'): Promise<void> {
    if (mode === 'online') await this.revalidate();
    this.requireCleanupProof();
  }

  private serialize<T>(
    action: () => Promise<T>,
    staleCode: 'owner_not_ready' | 'invalid_selection' = 'owner_not_ready',
  ): Promise<T> {
    if (this.closing) return Promise.reject(new OutboxError('outbox_closed'));
    const authority = this.authority;
    const result = this.tail.then(async () => {
      this.ensureOpen();
      if (authority !== this.authority) throw new OutboxError(staleCode);
      try {
        if (
          saved(this.storage, ACTIVE) !== this.owner ||
          saved(this.storage, CANDIDATE) !== this.candidate
        )
          throw new OutboxError('storage_failed');
      } catch {
        throw new OutboxError('storage_failed');
      }
      return action();
    });
    this.tail = result.then(
      () => undefined,
      () => undefined,
    );
    return result;
  }

  private async hold(owner: string): Promise<boolean> {
    this.ensureOpen();
    if (this.locks.has(owner)) return true;
    if (this.locks.size === 2) throw new OutboxError('recovery_reference_full');
    const held = await lockOwner(owner);
    if (held === undefined) {
      this.ensureOpen();
      return false;
    }
    if (this.closing) {
      held.release();
      await held.settled;
      throw new OutboxError('outbox_closed');
    }
    this.locks.set(owner, held);
    return true;
  }

  private async fresh(): Promise<void> {
    const owner = newRandomId();
    if (owner === this.owner || owner === this.candidate)
      throw new OutboxError('storage_failed');
    if (!(await this.hold(owner))) throw new OutboxError('owner_not_ready');
    this.ensureOpen();
    if (await this.store.occupied(owner))
      throw new OutboxError('storage_failed');
    this.ensureOpen();
    persistReference(this.storage, ACTIVE, owner);
    this.owner = owner;
    this.recovered.add(owner);
    this.current = { kind: 'ready' };
  }

  /** Internal startup, called once before the coordinator is exposed. */
  async initializeOwner(
    active: string | null,
    candidate: string | null,
  ): Promise<void> {
    this.candidate = candidate;
    if (active === null) {
      await this.fresh();
      return;
    }
    this.owner = active;
    if (!(await this.hold(active))) {
      if (candidate !== null && candidate !== active)
        throw new OutboxError('recovery_reference_full');
      persistReference(this.storage, CANDIDATE, active);
      this.candidate = active;
      await this.fresh();
      this.current = { kind: 'owner_busy' };
      return;
    }
    if (!(await this.store.occupied(active))) {
      // Rotation is safe only after the complete owner-wide existence probe.
      await this.fresh();
      const old = this.locks.get(active);
      old?.release();
      await old?.settled;
      this.locks.delete(active);
      return;
    }
    this.current = await this.classify(active);
  }

  private key(owner: string, epoch = this.binding.storage_epoch): BindingKey {
    const key = bindingKey(this.base, this.binding, owner);
    key[3] = epoch;
    return key;
  }

  private async classify(owner: string): Promise<LocalOutboxState> {
    this.ensureOpen();
    const occupied = await this.store.occupied(owner);
    this.ensureOpen();
    if (!occupied) return { kind: 'recovery_empty' };
    const matches = await this.store.matches(this.key(owner));
    this.ensureOpen();
    if (!matches) return { kind: 'owner_binding_quarantined' };
    const rows = await this.store.recover(this.key(owner));
    this.ensureOpen();
    if (rows.length === 0) return { kind: 'owner_binding_quarantined' };
    if (
      rows.some(
        (row) => row.admittedGeneration !== this.binding.actor_generation,
      )
    )
      return { kind: 'generation_changed' };
    // The private type brand prevents TS interchange; WeakMap identity enforces it at runtime.
    const selection = Object.freeze({}) as RecoverySelection;
    this.recoverySelections.set(selection, {
      owner,
      rows,
      authority: this.authority,
    });
    return { kind: 'recovery_required', selection };
  }

  private async revalidate(): Promise<void> {
    let fresh: Binding;
    try {
      fresh = await readBinding(this.base, this.token, this.stop.signal);
    } catch (error: unknown) {
      this.ensureOpen();
      if (error instanceof ArtifreeApiError && error.status === 401) {
        this.invalidateAuthentication();
      } else {
        this.revokeCleanupProof();
        this.current = { kind: 'binding_paused' };
      }
      throw error;
    }
    this.ensureOpen();
    if (JSON.stringify(fresh) !== JSON.stringify(this.binding)) {
      this.revokeCleanupProof();
      this.current =
        fresh.actor_id === this.binding.actor_id &&
        fresh.storage_epoch === this.binding.storage_epoch
          ? { kind: 'generation_changed' }
          : { kind: 'owner_binding_quarantined' };
      throw new OutboxError('owner_not_ready');
    }
    this.locallyAuthenticated = true;
  }

  /** Explicit retry only; saved candidates are never automatically inspected. */
  retryRecoveryCandidate(): Promise<LocalOutboxState> {
    return this.serialize(async () => {
      const candidate = this.candidate;
      if (candidate === null || saved(this.storage, CANDIDATE) !== candidate)
        throw new OutboxError('invalid_selection');
      await this.revalidate();
      const held = await this.hold(candidate);
      this.ensureOpen();
      if (!held) return { kind: 'owner_busy' };
      return this.classify(candidate);
    });
  }

  /** Authorizes only local access to this logical queue, never physical origin/delivery. */
  confirmRecovery(selection: RecoverySelection): Promise<void> {
    return this.serialize(async () => {
      const context = this.recoverySelections.get(selection);
      if (
        context === undefined ||
        context.authority !== this.authority ||
        !this.locks.has(context.owner)
      )
        throw new OutboxError('invalid_selection');
      this.recoverySelections.delete(selection);
      await this.revalidate();
      this.ensureOpen();
      const rows = await this.store.recover(this.key(context.owner));
      this.ensureOpen();
      if (JSON.stringify(rows) !== JSON.stringify(context.rows))
        throw new OutboxError('invalid_selection');
      this.recovered.add(context.owner);
      if (context.owner === this.owner) this.current = { kind: 'ready' };
    }, 'invalid_selection');
  }

  /** A saved reference can be cleared only explicitly under its lock and global emptiness. */
  clearRecoveryCandidate(): Promise<void> {
    return this.serialize(async () => {
      const candidate = this.candidate;
      if (
        candidate === null ||
        !this.locks.has(candidate) ||
        saved(this.storage, CANDIDATE) !== candidate
      )
        throw new OutboxError('invalid_selection');
      await this.revalidate();
      this.ensureOpen();
      const occupied = await this.store.occupied(candidate);
      this.ensureOpen();
      if (occupied) throw new OutboxError('owner_not_ready');
      persistReference(this.storage, CANDIDATE, null);
      this.candidate = null;
      if (candidate === this.owner) return;
      const held = this.locks.get(candidate);
      held?.release();
      await held?.settled;
      this.locks.delete(candidate);
      this.recovered.delete(candidate);
    });
  }

  /** Snapshot synchronously, then atomically admit; resolves only after IDB complete. */
  enqueue(intent: OutboxIntent): Promise<EnqueuedIntent> {
    let text: string | null;
    try {
      text =
        intent.operation === 'delete'
          ? null
          : dataValueText(
              intent.operation === 'set' ? intent.value : intent.patch,
              intent.operation === 'set' ? 'value' : 'patch',
            );
    } catch (error: unknown) {
      return Promise.reject(error);
    }
    const snapshot = {
      artifactId: intent.artifactId,
      path: intent.path,
      operation: intent.operation,
      base: { ...intent.base },
      text,
    };
    return this.serialize(async () => {
      if (!this.recovered.has(this.owner) || !this.locks.has(this.owner))
        throw new OutboxError('owner_not_ready');
      if (
        !/^art_[0-9a-f]{32}$/.test(snapshot.artifactId) ||
        typeof snapshot.path !== 'string' ||
        (snapshot.operation !== 'set' &&
          snapshot.operation !== 'update' &&
          snapshot.operation !== 'delete') ||
        (snapshot.text !== null &&
          new TextEncoder().encode(snapshot.text).byteLength > 262144)
      )
        throw new OutboxError('not_enqueued');
      const binding = this.key(this.owner);
      if (
        this.pausedLanes.has(JSON.stringify([...binding, snapshot.artifactId]))
      )
        throw new OutboxError('owner_not_ready');
      this.ensureOpen();
      try {
        const row = await this.store.enqueue({
          format: 1,
          state: 'unprepared',
          version: 1,
          key: newRandomId(),
          owner: this.owner,
          binding,
          lane: [...binding, snapshot.artifactId],
          artifactId: snapshot.artifactId,
          admittedGeneration: this.binding.actor_generation,
          operation: snapshot.operation,
          path: snapshot.path,
          text: snapshot.text,
          base: snapshot.base,
        });
        return {
          kind: 'durably_enqueued',
          sequence: row.sequence,
          idempotencyKey: row.key,
        };
      } catch (error: unknown) {
        if (error instanceof OutboxError && error.code === 'capacity_exceeded')
          throw error;
        if (error instanceof OutboxError && error.code === 'storage_corrupt')
          throw error;
        throw new OutboxError('not_enqueued');
      }
    });
  }

  /** List bounded original targets for this authenticated actor/URL, without payloads. */
  async listUnsentTargets(
    recoveredCandidate = false,
    options: UnsentCleanupOptions = {},
  ): Promise<UnsentCleanupTarget[]> {
    const { mode, storageEpoch } = cleanupOptions(options);
    if (typeof recoveredCandidate !== 'boolean')
      throw new OutboxError('invalid_selection');
    return this.serialize(async () => {
      const owner = recoveredCandidate ? this.candidate : this.owner;
      if (owner === null || !this.locks.has(owner))
        throw new OutboxError('owner_not_ready');
      await this.authorizeCleanup(mode);
      this.requireCleanupProof();
      const targets = await this.store.cleanupTargets(
        owner,
        this.base,
        this.binding.actor_id,
      );
      this.requireCleanupProof();
      return storageEpoch === undefined
        ? targets
        : targets.filter((target) => target.storageEpoch === storageEpoch);
    });
  }

  /** Select one original head/closure for explicit cleanup, never delivery authority. */
  async selectUnsent(
    artifactId: string,
    recoveredCandidate = false,
    options: UnsentCleanupOptions = {},
  ): Promise<UnsentDiscardSelection> {
    const { mode, storageEpoch = this.binding.storage_epoch } =
      cleanupOptions(options);
    if (typeof recoveredCandidate !== 'boolean')
      throw new OutboxError('invalid_selection');
    return this.serialize(async () => {
      const owner = recoveredCandidate ? this.candidate : this.owner;
      if (
        owner === null ||
        !this.locks.has(owner) ||
        !/^art_[0-9a-f]{32}$/.test(artifactId)
      )
        throw new OutboxError('owner_not_ready');
      await this.authorizeCleanup(mode);
      this.requireCleanupProof();
      const rows = await this.store.lane([
        ...this.key(owner, storageEpoch),
        artifactId,
      ]);
      this.requireCleanupProof();
      if (rows.length === 0) throw new OutboxError('invalid_selection');
      const head = rows[0];
      if (head === undefined) throw new OutboxError('invalid_selection');
      const selection = Object.freeze({
        target: cleanupTarget(head),
        mode,
      }) as UnsentDiscardSelection;
      this.discardSelections.set(selection, {
        owner,
        rows,
        authority: this.authority,
        mode,
      });
      return selection;
    });
  }

  /** Revalidate, fence the complete lane snapshot, then delete/account in one commit. */
  discardUnsent(
    selection: UnsentDiscardSelection,
  ): Promise<{ readonly kind: 'discarded_unsent' }> {
    return this.serialize(async () => {
      const context = this.discardSelections.get(selection);
      if (
        context === undefined ||
        context.authority !== this.authority ||
        !this.locks.has(context.owner)
      )
        throw new OutboxError('invalid_selection');
      this.discardSelections.delete(selection);
      await this.authorizeCleanup(context.mode);
      this.requireCleanupProof();
      const authorization = this.cleanupAuthorization.signal;
      try {
        await this.store.discard(
          context.rows,
          () => {
            if (authorization.aborted || this.detached)
              throw new OutboxError('owner_not_ready');
            // Deliberate close settles an existing transaction; auth revocation aborts it.
            if (!this.closing) this.requireCleanupProof();
            if (
              (!this.closing && context.authority !== this.authority) ||
              !this.locks.has(context.owner)
            )
              throw new OutboxError('invalid_selection');
          },
          authorization,
        );
      } catch (error: unknown) {
        if (error instanceof OutboxError && error.code === 'storage_failed') {
          const head = context.rows[0];
          if (head !== undefined)
            this.pausedLanes.add(JSON.stringify(head.lane));
          this.current = { kind: 'storage_paused' };
        }
        throw error;
      }
      return { kind: 'discarded_unsent' };
    }, 'invalid_selection');
  }

  /** Fence new actions, settle accepted local operations, then release lifetime locks. */
  async close(): Promise<void> {
    this.closing = true;
    this.authority = Symbol();
    this.locallyAuthenticated = false;
    this.stop.abort();
    await this.tail;
    this.current = { kind: 'closed' };
    this.store.close();
    for (const held of this.locks.values()) held.release();
    await Promise.all([...this.locks.values()].map((held) => held.settled));
    this.locks.clear();
    this.recovered.clear();
    if (window[PAGE_REGISTRY]?.coordinator === this)
      delete window[PAGE_REGISTRY];
  }
}
