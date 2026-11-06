import type { DataOutboxBinding } from '@artifree/contracts';
import type { DataInput } from '../client.js';

/** Fixed diagnostics never include stored identity or payload information. */
export type OutboxFailure =
  | 'invalid_outbox_configuration'
  | 'outbox_unavailable'
  | 'storage_failed'
  | 'storage_corrupt'
  | 'not_enqueued'
  | 'capacity_exceeded'
  | 'recovery_reference_full'
  | 'invalid_selection'
  | 'outbox_closed'
  | 'owner_not_ready';

/** Expected local failure; enqueue errors never imply durable acceptance. */
export class OutboxError extends Error {
  readonly code: OutboxFailure;
  constructor(code: OutboxFailure) {
    super(code);
    this.name = 'OutboxError';
    this.code = code;
  }
}

/** Server-authenticated delivery identity, held in memory without credentials. */
export type Binding = Readonly<DataOutboxBinding>;

/** Numeric canonical base or explicit immediately preceding local intent. */
export type IntentBase =
  { readonly version: number } | { readonly after: number };

/** Local admission deliberately leaves path/JSON domain validation to Rust. */
export type OutboxIntent = {
  readonly artifactId: string;
  readonly path: string;
  readonly base: IntentBase;
} & (
  | { readonly operation: 'set'; readonly value: DataInput }
  | { readonly operation: 'update'; readonly patch: DataInput }
  | { readonly operation: 'delete' }
);

declare const recoveryBrand: unique symbol;
declare const discardBrand: unique symbol;
/** Opaque, page-local explicit logical-owner recovery choice. */
export interface RecoverySelection {
  readonly [recoveryBrand]: true;
}
/** Opaque single-use choice for a never-prepared head and its dependent closure. */
export interface UnsentDiscardSelection {
  readonly [discardBrand]: true;
  readonly target: UnsentCleanupTarget;
  readonly mode: 'online' | 'offline';
}

/** Original target metadata, restricted to this handle's authenticated actor/URL. */
export interface UnsentCleanupTarget {
  readonly apiBaseUrl: string;
  readonly actorId: string;
  readonly artifactId: string;
  readonly storageEpoch: string;
  readonly admittedGeneration: string;
  readonly headSequence: number;
  readonly recordVersion: number;
}

/** Offline is a deliberate choice, never a failed-network fallback. */
export interface UnsentCleanupOptions {
  readonly mode?: 'online' | 'offline';
  readonly storageEpoch?: string;
}

/** No state in this local-only entry point authorizes network mutation. */
export type LocalOutboxState =
  | { readonly kind: 'ready' }
  | { readonly kind: 'owner_busy' }
  | { readonly kind: 'owner_binding_quarantined' }
  | { readonly kind: 'recovery_empty' }
  | { readonly kind: 'generation_changed' }
  | { readonly kind: 'storage_paused' }
  | { readonly kind: 'binding_paused' }
  | {
      readonly kind: 'recovery_required';
      readonly selection: RecoverySelection;
    }
  | { readonly kind: 'closed' };

/** Durably accepted locally; this is never a server acknowledgement. */
export interface EnqueuedIntent {
  readonly kind: 'durably_enqueued';
  readonly sequence: number;
  readonly idempotencyKey: string;
}

/** Browser-native Fetch is required; caller transports cannot weaken redirects. */
export interface OutboxConfiguration {
  readonly baseUrl: string;
  readonly accessToken: string;
}
