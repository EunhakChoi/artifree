import type { DataWriteResponse } from '@artifree/contracts';
import {
  normalizeBase,
  validActor,
  validGeneration,
  validId,
} from './binding.js';
import {
  OutboxError,
  type Binding,
  type IntentBase,
  type UnsentCleanupTarget,
} from './types.js';

const DATABASE = 'artifree-data-outbox';
const VERSION = 2;
const STORES = ['intents', 'receipts', 'checkpoints', 'usage'] as const;
const MAX_ROWS = 256;
const MAX_BYTES = 8 * 1024 * 1024;
const LANE_ROWS = 64;
const LANE_BYTES = 2 * 1024 * 1024;
const ROW_BYTES = 272 * 1024;
const RESERVE = 4096;
const encoder = new TextEncoder();

export type BindingKey = [string, string, string, string];
export type LaneKey = [...BindingKey, string];

interface RowIdentity {
  readonly format: 2;
  readonly version: number;
  readonly sequence: number;
  readonly key: string;
  readonly owner: string;
  readonly binding: BindingKey;
  readonly lane: LaneKey;
  readonly artifactId: string;
  readonly admittedGeneration: string;
  readonly authorizedGeneration: string;
  readonly operation: 'set' | 'update' | 'delete';
  readonly path: string;
  readonly base: IntentBase;
  readonly bytes: number;
}

/** Accepted intent, with no frozen request and no delivery observation. */
export interface UnpreparedRow extends RowIdentity {
  readonly state: 'unprepared';
  readonly text: string | null;
}
/** Exact original request. A missing durable ACK always leaves delivery unknown. */
export interface PreparedRequest {
  readonly method: 'PUT' | 'PATCH' | 'DELETE';
  readonly body: string;
  readonly base: number;
}
export interface PreparedRow extends RowIdentity {
  readonly state: 'prepared';
  readonly request: PreparedRequest;
}
export interface AcknowledgedRow extends RowIdentity {
  readonly state: 'acknowledged';
  readonly request: PreparedRequest;
  readonly receipt: Readonly<DataWriteResponse>;
}
export type IntentRow = UnpreparedRow | PreparedRow | AcknowledgedRow;
interface Predecessor {
  readonly sequence: number;
  readonly dataVersion: number;
}
interface Checkpoint {
  readonly format: 2;
  readonly lane: LaneKey;
  readonly owner: string;
  readonly generation: string;
  readonly predecessor: Predecessor | null;
  readonly rows: number;
  readonly bytes: number;
}

interface Usage {
  readonly id: 'global';
  readonly next: number;
  readonly rows: number;
  readonly bytes: number;
}

export function bindingKey(
  base: string,
  binding: Binding,
  owner: string,
): BindingKey {
  return [owner, base, binding.actor_id, binding.storage_epoch];
}

export function cleanupTarget(row: IntentRow): UnsentCleanupTarget {
  return Object.freeze({
    apiBaseUrl: row.binding[1],
    actorId: row.binding[2],
    artifactId: row.artifactId,
    storageEpoch: row.binding[3],
    admittedGeneration: row.admittedGeneration,
    headSequence: row.sequence,
    recordVersion: row.version,
  });
}

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function natural(value: unknown): value is number {
  return Number.isSafeInteger(value) && typeof value === 'number' && value >= 0;
}

function same(a: unknown, b: unknown): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

function exact(value: Record<string, unknown>, names: string[]): boolean {
  return Object.keys(value).sort().join(',') === names.sort().join(',');
}

function validBinding(value: unknown): value is BindingKey {
  if (!Array.isArray(value) || value.length !== 4) return false;
  const [owner, base, actor, epoch]: unknown[] = value;
  if (
    !validId(owner) ||
    typeof base !== 'string' ||
    !validActor(actor) ||
    !validId(epoch)
  )
    return false;
  try {
    return normalizeBase(base, 'http://localhost') === base;
  } catch {
    return false;
  }
}

function validLane(value: unknown): value is LaneKey {
  return (
    Array.isArray(value) &&
    value.length === 5 &&
    validBinding(value.slice(0, 4)) &&
    typeof value[4] === 'string' &&
    /^art_[0-9a-f]{32}$/.test(value[4])
  );
}

function validBase(value: unknown): value is IntentBase {
  return (
    record(value) &&
    ((exact(value, ['version']) && natural(value.version)) ||
      (exact(value, ['after']) && natural(value.after) && value.after > 0))
  );
}

function account(row: Omit<UnpreparedRow, 'bytes'>): number {
  const { text, authorizedGeneration, ...fields } = row;
  // v1 already reserved generation/checkpoint metadata. Preserve that allocation
  // through migration and phase transitions rather than charging the payload twice.
  void authorizedGeneration;
  const metadata = { ...fields, format: 1, version: 1 };
  // IndexedDB stores the payload string directly. Counting a JSON-escaped second
  // copy would reject valid maximum-size raw text. Preparation and a full
  // receipt each repeat the path; receipt/checkpoint each repeat lane identity.
  // Reserve those copies explicitly, even when their strings are unusually long.
  // The fixed 4 KiB remains available for wrapper/keys/generation/receipt fields.
  const preparedPath = encoder.encode(JSON.stringify(row.path)).byteLength;
  const laneAnchor = encoder.encode(
    JSON.stringify({ lane: row.lane, owner: row.owner }),
  ).byteLength;
  return (
    encoder.encode(JSON.stringify(metadata)).byteLength +
    (text === null ? 0 : encoder.encode(text).byteLength) +
    2 * preparedPath +
    2 * laneAnchor +
    RESERVE
  );
}

function decodeUnprepared(value: unknown): UnpreparedRow {
  if (
    record(value) &&
    value.format === 1 &&
    !Object.hasOwn(value, 'authorizedGeneration')
  ) {
    // Called only by owner-held binding/lane reads, never while opening storage.
    // Exact validation below rejects every unrecognized legacy field or phase.
    return decodeUnprepared({
      ...value,
      format: 2,
      authorizedGeneration: value.admittedGeneration,
    });
  }
  if (
    !record(value) ||
    !exact(value, [
      'format',
      'state',
      'version',
      'sequence',
      'key',
      'owner',
      'binding',
      'lane',
      'artifactId',
      'admittedGeneration',
      'authorizedGeneration',
      'operation',
      'path',
      'text',
      'base',
      'bytes',
    ]) ||
    value.format !== 2 ||
    value.state !== 'unprepared' ||
    value.version !== 1 ||
    !natural(value.sequence) ||
    value.sequence === 0 ||
    !validId(value.key) ||
    !validId(value.owner) ||
    !validBinding(value.binding) ||
    !validLane(value.lane) ||
    value.artifactId !== value.lane[4] ||
    !same(value.binding, value.lane.slice(0, 4)) ||
    value.owner !== value.binding[0] ||
    !validGeneration(value.admittedGeneration) ||
    !validGeneration(value.authorizedGeneration) ||
    typeof value.operation !== 'string' ||
    !['set', 'update', 'delete'].includes(value.operation) ||
    (value.binding[2] === 'local' &&
      (value.admittedGeneration !== '1' ||
        value.authorizedGeneration !== '1')) ||
    typeof value.path !== 'string' ||
    !validBase(value.base) ||
    ('after' in value.base && value.base.after >= value.sequence) ||
    !natural(value.bytes) ||
    value.bytes > ROW_BYTES ||
    (value.operation === 'delete'
      ? value.text !== null
      : typeof value.text !== 'string') ||
    (typeof value.text === 'string' &&
      encoder.encode(value.text).byteLength > 262144)
  )
    throw new OutboxError('storage_corrupt');
  // All fields have runtime checks above; payload text is intentionally not JSON-decoded.
  const row: UnpreparedRow = {
    format: 2,
    state: 'unprepared',
    version: 1,
    sequence: value.sequence,
    key: value.key,
    owner: value.owner,
    binding: value.binding,
    lane: value.lane,
    artifactId: value.lane[4],
    admittedGeneration: value.admittedGeneration,
    authorizedGeneration: value.authorizedGeneration,
    operation:
      value.operation === 'delete'
        ? 'delete'
        : value.operation === 'set'
          ? 'set'
          : 'update',
    path: value.path,
    text: typeof value.text === 'string' ? value.text : null,
    base: value.base,
    bytes: value.bytes,
  };
  const { bytes, ...body } = row;
  if (account(body) !== bytes) throw new OutboxError('storage_corrupt');
  return row;
}

function method(
  operation: RowIdentity['operation'],
): PreparedRequest['method'] {
  return operation === 'set'
    ? 'PUT'
    : operation === 'update'
      ? 'PATCH'
      : 'DELETE';
}
function bodyPrefix(path: string, base: number): string {
  return `{"base_data_version":${base},"path":${JSON.stringify(path)}`;
}
function frozenRequest(row: UnpreparedRow, base: number): PreparedRequest {
  const prefix = bodyPrefix(row.path, base);
  const body =
    row.operation === 'delete'
      ? `${prefix}}`
      : `${prefix},"${row.operation === 'set' ? 'value' : 'patch'}":${row.text}}`;
  return { method: method(row.operation), body, base };
}
function decodeReceipt(value: unknown, row: PreparedRow): DataWriteResponse {
  if (
    !record(value) ||
    !exact(value, [
      'artifact_id',
      'path',
      'data_version',
      'changed',
      'replayed',
    ]) ||
    value.artifact_id !== row.artifactId ||
    value.path !== row.path ||
    !natural(value.data_version) ||
    typeof value.changed !== 'boolean' ||
    typeof value.replayed !== 'boolean' ||
    value.data_version !== row.request.base + (value.changed ? 1 : 0)
  ) {
    throw new OutboxError('storage_corrupt');
  }
  return {
    artifact_id: row.artifactId,
    path: row.path,
    data_version: value.data_version,
    changed: value.changed,
    replayed: value.replayed,
  };
}
function decodeRow(value: unknown): IntentRow {
  if (!record(value)) throw new OutboxError('storage_corrupt');
  if (value.state === 'unprepared') return decodeUnprepared(value);
  if (
    (value.state !== 'prepared' && value.state !== 'acknowledged') ||
    value.format !== 2 ||
    !validGeneration(value.authorizedGeneration) ||
    !natural(value.version) ||
    value.version !== (value.state === 'prepared' ? 2 : 3) ||
    !record(value.request) ||
    !exact(value.request, ['method', 'body', 'base']) ||
    !natural(value.request.base) ||
    typeof value.request.body !== 'string' ||
    typeof value.path !== 'string' ||
    (value.state === 'prepared' && Object.hasOwn(value, 'receipt')) ||
    Object.hasOwn(value, 'text')
  )
    throw new OutboxError('storage_corrupt');
  const { request: storedRequest, receipt, ...identity } = value;
  const prefix = bodyPrefix(value.path, value.request.base);
  let text: string | null = null;
  if (value.operation !== 'delete') {
    const opening = `${prefix},"${value.operation === 'set' ? 'value' : 'patch'}":`;
    if (
      !value.request.body.startsWith(opening) ||
      !value.request.body.endsWith('}')
    )
      throw new OutboxError('storage_corrupt');
    text = value.request.body.slice(opening.length, -1);
  }
  const original = decodeUnprepared({
    ...identity,
    state: 'unprepared',
    version: 1,
    text,
  });
  const requestValue = frozenRequest(original, value.request.base);
  if (
    !same(requestValue, storedRequest) ||
    ('version' in original.base && original.base.version !== requestValue.base)
  )
    throw new OutboxError('storage_corrupt');
  const { text: ignoredText, ...common } = original;
  // Destructuring removes the full enqueue payload; only the frozen body survives.
  void ignoredText;
  const prepared: PreparedRow = {
    ...common,
    state: 'prepared',
    version: value.version,
    request: requestValue,
  };
  return value.state === 'prepared'
    ? prepared
    : {
        ...prepared,
        state: 'acknowledged',
        receipt: decodeReceipt(receipt, prepared),
      };
}

function decodeUsage(value: unknown): Usage {
  if (
    !record(value) ||
    !exact(value, ['id', 'next', 'rows', 'bytes']) ||
    value.id !== 'global' ||
    !natural(value.next) ||
    value.next === 0 ||
    !natural(value.rows) ||
    value.rows > MAX_ROWS ||
    !natural(value.bytes) ||
    value.bytes > MAX_BYTES
  )
    throw new OutboxError('storage_corrupt');
  return {
    id: 'global',
    next: value.next,
    rows: value.rows,
    bytes: value.bytes,
  };
}

function decodeCheckpoint(value: unknown): Checkpoint {
  if (
    record(value) &&
    !Object.hasOwn(value, 'format') &&
    !Object.hasOwn(value, 'predecessor')
  ) {
    return decodeCheckpoint({ ...value, format: 2, predecessor: null });
  }
  if (
    !record(value) ||
    !exact(value, [
      'format',
      'lane',
      'owner',
      'generation',
      'predecessor',
      'rows',
      'bytes',
    ]) ||
    value.format !== 2 ||
    !validLane(value.lane) ||
    value.owner !== value.lane[0] ||
    !validGeneration(value.generation) ||
    !(
      value.predecessor === null ||
      (record(value.predecessor) &&
        exact(value.predecessor, ['sequence', 'dataVersion']) &&
        natural(value.predecessor.sequence) &&
        value.predecessor.sequence > 0 &&
        natural(value.predecessor.dataVersion))
    ) ||
    (value.lane[2] === 'local' && value.generation !== '1') ||
    !natural(value.rows) ||
    value.rows === 0 ||
    value.rows > LANE_ROWS ||
    !natural(value.bytes) ||
    value.bytes > LANE_BYTES
  )
    throw new OutboxError('storage_corrupt');
  return {
    format: 2,
    lane: value.lane,
    owner: value.lane[0],
    generation: value.generation,
    predecessor:
      value.predecessor === null
        ? null
        : {
            sequence: Number(value.predecessor.sequence),
            dataVersion: Number(value.predecessor.dataVersion),
          },
    rows: value.rows,
    bytes: value.bytes,
  };
}

function request<T>(pending: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    pending.onsuccess = () => resolve(pending.result);
    pending.onerror = () => reject(new OutboxError('storage_failed'));
  });
}

/** Terminal events, never individual request success, determine durability. */
async function transaction<T>(
  database: IDBDatabase,
  mode: IDBTransactionMode,
  action: (tx: IDBTransaction) => Promise<T>,
  signal?: AbortSignal,
): Promise<T> {
  if (signal?.aborted) throw new OutboxError('owner_not_ready');
  let tx: IDBTransaction;
  try {
    tx = database.transaction([...STORES], mode);
  } catch {
    throw new OutboxError('storage_failed');
  }
  let finished = false;
  let invalidated = false;
  const invalidate = (): void => {
    if (finished) return;
    try {
      tx.abort();
      invalidated = true;
    } catch {
      // A transaction already committing settles through its terminal event.
    }
  };
  const terminal = new Promise<void>((resolve, reject) => {
    tx.oncomplete = () => {
      if (finished) return;
      finished = true;
      resolve();
    };
    tx.onabort = () => {
      if (finished) return;
      finished = true;
      reject(
        new OutboxError(invalidated ? 'owner_not_ready' : 'storage_failed'),
      );
    };
    tx.onerror = () => {
      /* Abort's terminal event supplies the failure. */
    };
  });
  signal?.addEventListener('abort', invalidate, { once: true });
  if (signal?.aborted) invalidate();
  try {
    const value = await action(tx);
    await terminal;
    return value;
  } catch (error: unknown) {
    try {
      tx.abort();
    } catch {
      /* It may already have terminated. */
    }
    await terminal.catch(() => undefined);
    if (invalidated) throw new OutboxError('owner_not_ready');
    throw error instanceof OutboxError
      ? error
      : new OutboxError('storage_failed');
  } finally {
    signal?.removeEventListener('abort', invalidate);
  }
}

/** A cursor page never materializes more than 32 records. */
async function page(
  store: IDBObjectStore | IDBIndex,
  range?: IDBKeyRange,
): Promise<unknown[]> {
  return request(store.getAll(range, 32));
}

/** Origin-wide quota ledger is metadata-only and bounded by the row cap. */
async function usage(tx: IDBTransaction): Promise<Usage> {
  const global = decodeUsage(
    await request(tx.objectStore('usage').get('global')),
  );
  const store = tx.objectStore('checkpoints');
  let last: IDBValidKey | undefined;
  let rows = 0;
  let bytes = 0;
  for (;;) {
    const values = await page(
      store,
      last === undefined ? undefined : IDBKeyRange.lowerBound(last, true),
    );
    for (const value of values) {
      const checkpoint = decodeCheckpoint(value);
      rows += checkpoint.rows;
      bytes += checkpoint.bytes;
      last = checkpoint.lane;
    }
    if (rows > MAX_ROWS || bytes > MAX_BYTES)
      throw new OutboxError('storage_corrupt');
    if (values.length < 32) break;
  }
  const newest = await request(
    tx.objectStore('intents').openKeyCursor(null, 'prev'),
  );
  if (
    (newest !== null &&
      (!natural(newest.primaryKey) || newest.primaryKey >= global.next)) ||
    rows !== global.rows ||
    bytes !== global.bytes ||
    (await request(tx.objectStore('intents').count())) !== rows
  )
    throw new OutboxError('storage_corrupt');
  return global;
}

async function laneRows(
  tx: IDBTransaction,
  lane: LaneKey,
): Promise<IntentRow[]> {
  const values: IntentRow[] = [];
  const index = tx.objectStore('intents').index('laneSequence');
  let after = 0;
  for (;;) {
    const batch = await page(
      index,
      IDBKeyRange.bound([lane, after], [lane, Number.MAX_SAFE_INTEGER], true),
    );
    for (const value of batch) {
      const row = decodeRow(value);
      if (
        !same(row.lane, lane) ||
        row.sequence <= after ||
        values.some((previous) => previous.key === row.key)
      )
        throw new OutboxError('storage_corrupt');
      values.push(row);
      after = row.sequence;
      if (values.length > LANE_ROWS) throw new OutboxError('storage_corrupt');
    }
    if (batch.length < 32) break;
  }
  const stored: unknown = await request(
    tx.objectStore('checkpoints').get(lane),
  );
  if (values.length === 0) {
    if (stored !== undefined) throw new OutboxError('storage_corrupt');
    return values;
  }
  const checkpoint = decodeCheckpoint(stored);
  let pending = false;
  let lastAck: AcknowledgedRow | undefined;
  for (const row of values) {
    if (row.state === 'acknowledged') {
      if (pending) throw new OutboxError('storage_corrupt');
      lastAck = row;
    } else {
      if (pending && row.state === 'prepared')
        throw new OutboxError('storage_corrupt');
      pending = true;
    }
  }
  if (
    lastAck !== undefined
      ? !same(checkpoint.predecessor, {
          sequence: lastAck.sequence,
          dataVersion: lastAck.receipt.data_version,
        })
      : checkpoint.predecessor !== null &&
        checkpoint.predecessor.sequence >= (values[0]?.sequence ?? 0)
  )
    throw new OutboxError('storage_corrupt');
  if (
    checkpoint.rows !== values.length ||
    checkpoint.bytes !== values.reduce((sum, row) => sum + row.bytes, 0) ||
    values.some((row) => row.authorizedGeneration !== checkpoint.generation) ||
    values.some((row, index) => {
      if (!('after' in row.base)) return false;
      const previous = values[index - 1];
      if (previous !== undefined) {
        return (
          previous.sequence !== row.base.after ||
          (row.state !== 'unprepared' &&
            (previous.state !== 'acknowledged' ||
              row.request.base !== previous.receipt.data_version))
        );
      }
      if (
        row.state === 'acknowledged' &&
        checkpoint.predecessor !== null &&
        checkpoint.predecessor.sequence >= row.sequence
      )
        return false;
      return (
        checkpoint.predecessor?.sequence !== row.base.after ||
        (row.state !== 'unprepared' &&
          row.request.base !== checkpoint.predecessor.dataVersion)
      );
    })
  )
    throw new OutboxError('storage_corrupt');
  return values;
}

function validateLayout(database: IDBDatabase, tx: IDBTransaction): void {
  if (!same(Array.from(database.objectStoreNames).sort(), [...STORES].sort()))
    throw new OutboxError('storage_corrupt');
  for (const name of STORES) {
    const store = tx.objectStore(name);
    if (
      store.keyPath !==
        (name === 'intents' ? 'sequence' : name === 'usage' ? 'id' : 'lane') ||
      store.autoIncrement
    )
      throw new OutboxError('storage_corrupt');
    const indexes: Array<[string, string | string[], boolean]> =
      name === 'intents'
        ? [
            ['owner', 'owner', false],
            ['binding', 'binding', false],
            ['laneSequence', ['lane', 'sequence'], false],
            ['key', 'key', true],
            ['artifact', 'artifactId', false],
          ]
        : name === 'usage'
          ? []
          : [['owner', 'owner', false]];
    if (
      !same(
        Array.from(store.indexNames).sort(),
        indexes.map(([index]) => index).sort(),
      )
    )
      throw new OutboxError('storage_corrupt');
    for (const [name, keyPath, unique] of indexes) {
      const index = store.index(name);
      if (
        !same(index.keyPath, keyPath) ||
        index.unique !== unique ||
        index.multiEntry
      )
        throw new OutboxError('storage_corrupt');
    }
  }
}

/** Dedicated versioned schema. Failed upgrades leave the complete old database intact. */
export async function openStore(): Promise<LocalStore> {
  const database = await new Promise<IDBDatabase>((resolve, reject) => {
    const opening = indexedDB.open(DATABASE, VERSION);
    let refused = false;
    let migrationFailure: OutboxError | undefined;
    opening.onupgradeneeded = (event) => {
      if (refused) {
        opening.transaction?.abort();
        return;
      }
      if (event.oldVersion === 1 && opening.transaction !== null) {
        const tx = opening.transaction;
        try {
          // Versionchange checks layout metadata only. Busy/foreign owner payloads
          // remain untouched; exact v1 rows normalize lazily under their owner lock.
          validateLayout(tx.db, tx);
        } catch (error: unknown) {
          migrationFailure =
            error instanceof OutboxError
              ? error
              : new OutboxError('storage_failed');
          tx.abort();
        }
        return;
      }
      if (event.oldVersion !== 0) {
        opening.transaction?.abort();
        return;
      }
      for (const name of ['intents', 'receipts', 'checkpoints']) {
        const store = opening.result.createObjectStore(name, {
          keyPath: name === 'intents' ? 'sequence' : 'lane',
        });
        store.createIndex('owner', 'owner');
        if (name === 'intents') {
          store.createIndex('binding', 'binding');
          store.createIndex('laneSequence', ['lane', 'sequence']);
          store.createIndex('key', 'key', { unique: true });
          store.createIndex('artifact', 'artifactId');
        }
      }
      const totals = opening.result.createObjectStore('usage', {
        keyPath: 'id',
      });
      totals.add({ id: 'global', next: 1, rows: 0, bytes: 0 });
    };
    opening.onsuccess = () => {
      const database = opening.result;
      if (refused) {
        database.close();
        return;
      }
      try {
        validateLayout(database, database.transaction([...STORES], 'readonly'));
        resolve(database);
      } catch {
        database.close();
        reject(new OutboxError('storage_corrupt'));
      }
    };
    opening.onerror = () =>
      reject(migrationFailure ?? new OutboxError('storage_failed'));
    opening.onblocked = () => {
      refused = true;
      reject(new OutboxError('storage_failed'));
    };
  });
  const store = new LocalStore(database);
  database.onversionchange = () => database.close();
  return store;
}

export class LocalStore {
  private readonly database: IDBDatabase;
  constructor(database: IDBDatabase) {
    this.database = database;
  }
  close(): void {
    this.database.close();
  }

  /** Existence-only probes: never decode foreign rows or expose counts. */
  occupied(owner: string): Promise<boolean> {
    return transaction(this.database, 'readonly', async (tx) => {
      for (const name of ['intents', 'receipts', 'checkpoints']) {
        if (
          (await request(tx.objectStore(name).index('owner').getKey(owner))) !==
          undefined
        )
          return true;
      }
      return false;
    });
  }

  matches(binding: BindingKey): Promise<boolean> {
    return transaction(
      this.database,
      'readonly',
      async (tx) =>
        (await request(
          tx.objectStore('intents').index('binding').getKey(binding),
        )) !== undefined,
    );
  }

  recover(binding: BindingKey): Promise<IntentRow[]> {
    return this.readRange(IDBKeyRange.only(binding));
  }

  async cleanupTargets(
    owner: string,
    base: string,
    actor: string,
  ): Promise<UnsentCleanupTarget[]> {
    const prefix = [owner, base, actor];
    const rows = await this.readRange(
      IDBKeyRange.bound(prefix, [...prefix, []], false, true),
    );
    const targets = new Map<string, UnsentCleanupTarget>();
    const visited = new Set<string>();
    for (const row of rows) {
      if (!same(row.binding.slice(0, 3), prefix))
        throw new OutboxError('storage_corrupt');
      const lane = JSON.stringify(row.lane);
      if (!visited.has(lane) && row.state === 'unprepared')
        targets.set(lane, cleanupTarget(row));
      visited.add(lane);
    }
    return [...targets.values()];
  }

  private readRange(range: IDBKeyRange): Promise<IntentRow[]> {
    return transaction(this.database, 'readonly', async (tx) => {
      await usage(tx);
      const index = tx.objectStore('intents').index('binding');
      const rows: IntentRow[] = [];
      // The key cursor enumerates only this binding; each payload page is bounded.
      let cursor = await request(index.openKeyCursor(range));
      const lanes = new Map<string, LaneKey>();
      while (cursor !== null) {
        const value: unknown = await request(
          tx.objectStore('intents').get(cursor.primaryKey),
        );
        const row = decodeRow(value);
        if (rows.some((previous) => previous.key === row.key))
          throw new OutboxError('storage_corrupt');
        lanes.set(JSON.stringify(row.lane), row.lane);
        rows.push(row);
        if (rows.length > MAX_ROWS) throw new OutboxError('storage_corrupt');
        cursor = await new Promise((resolve, reject) => {
          cursor?.continue();
          const pending = cursor?.request;
          if (pending === undefined) {
            reject(new OutboxError('storage_failed'));
            return;
          }
          pending.onsuccess = () => resolve(pending.result);
          pending.onerror = () => reject(new OutboxError('storage_failed'));
        });
      }
      for (const lane of lanes.values()) await laneRows(tx, lane);
      return rows;
    });
  }

  enqueue(
    input: Omit<UnpreparedRow, 'sequence' | 'bytes'>,
  ): Promise<UnpreparedRow> {
    return transaction(this.database, 'readwrite', async (tx) => {
      if (!validBase(input.base)) throw new OutboxError('not_enqueued');
      const global = await usage(tx);
      const rows = await laneRows(tx, input.lane);
      if (
        rows.some(
          (row) => row.admittedGeneration !== input.admittedGeneration,
        ) ||
        ('after' in input.base && rows.at(-1)?.sequence !== input.base.after)
      )
        throw new OutboxError('not_enqueued');
      if (!Number.isSafeInteger(global.next + 1))
        throw new OutboxError('capacity_exceeded');
      const body = { ...input, sequence: global.next };
      const bytes = account(body);
      const laneBytes = rows.reduce((sum, item) => sum + item.bytes, 0) + bytes;
      if (
        bytes > ROW_BYTES ||
        global.rows + 1 > MAX_ROWS ||
        global.bytes + bytes > MAX_BYTES ||
        rows.length + 1 > LANE_ROWS ||
        laneBytes > LANE_BYTES
      )
        throw new OutboxError('capacity_exceeded');
      const row = decodeUnprepared({ ...body, bytes });
      await request(tx.objectStore('intents').add(row));
      await request(
        tx.objectStore('checkpoints').put({
          format: 2,
          lane: row.lane,
          owner: row.owner,
          generation: row.authorizedGeneration,
          predecessor:
            rows.length === 0
              ? null
              : decodeCheckpoint(
                  await request(tx.objectStore('checkpoints').get(row.lane)),
                ).predecessor,
          rows: rows.length + 1,
          bytes: laneBytes,
        }),
      );
      await request(
        tx.objectStore('usage').put({
          id: 'global',
          next: global.next + 1,
          rows: global.rows + 1,
          bytes: global.bytes + bytes,
        }),
      );
      return row;
    });
  }

  lane(lane: LaneKey): Promise<IntentRow[]> {
    return transaction(this.database, 'readonly', async (tx) => {
      await usage(tx);
      return laneRows(tx, lane);
    });
  }

  /** Commit the frozen request before any caller may dispatch it. */
  prepare(
    expected: UnpreparedRow,
    authorize: () => void,
    signal: AbortSignal,
  ): Promise<PreparedRow> {
    return transaction(
      this.database,
      'readwrite',
      async (tx) => {
        await usage(tx);
        const rows = await laneRows(tx, expected.lane);
        const head = rows.find((row) => row.state !== 'acknowledged');
        if (!same(head, expected) || head?.state !== 'unprepared')
          throw new OutboxError('invalid_selection');
        const checkpoint = decodeCheckpoint(
          await request(tx.objectStore('checkpoints').get(expected.lane)),
        );
        const predecessor = rows.find(
          (row) => 'after' in head.base && row.sequence === head.base.after,
        );
        const base =
          'version' in head.base
            ? head.base.version
            : predecessor?.state === 'acknowledged'
              ? predecessor.receipt.data_version
              : checkpoint.predecessor?.sequence === head.base.after
                ? checkpoint.predecessor.dataVersion
                : undefined;
        if (base === undefined) throw new OutboxError('invalid_selection');
        authorize();
        const { text, ...identity } = head;
        void text;
        const prepared: PreparedRow = {
          ...identity,
          state: 'prepared',
          version: head.version + 1,
          request: frozenRequest(head, base),
        };
        const checked = decodeRow(prepared);
        if (checked.state !== 'prepared')
          throw new OutboxError('storage_corrupt');
        await request(tx.objectStore('intents').put(checked));
        // Persist legacy checkpoint normalization in the same owner-held transition.
        await request(tx.objectStore('checkpoints').put(checkpoint));
        return checked;
      },
      signal,
    );
  }

  /** Receipt and predecessor checkpoint share one terminal commit event. */
  acknowledge(
    expected: PreparedRow,
    receipt: DataWriteResponse,
    authorize: () => void,
    signal: AbortSignal,
  ): Promise<AcknowledgedRow> {
    return transaction(
      this.database,
      'readwrite',
      async (tx) => {
        await usage(tx);
        const rows = await laneRows(tx, expected.lane);
        const head = rows.find((row) => row.state !== 'acknowledged');
        if (!same(head, expected) || head?.state !== 'prepared')
          throw new OutboxError('invalid_selection');
        const validated = decodeReceipt(receipt, head);
        authorize();
        const acknowledged: AcknowledgedRow = {
          ...head,
          state: 'acknowledged',
          version: head.version + 1,
          receipt: validated,
        };
        const checkpoint = decodeCheckpoint(
          await request(tx.objectStore('checkpoints').get(head.lane)),
        );
        await request(tx.objectStore('intents').put(acknowledged));
        await request(
          tx.objectStore('checkpoints').put({
            ...checkpoint,
            predecessor: {
              sequence: head.sequence,
              dataVersion: validated.data_version,
            },
          }),
        );
        return acknowledged;
      },
      signal,
    );
  }

  /** Local cleanup only: remove the complete durable-ACK prefix, preserving its checkpoint. */
  trim(
    expected: IntentRow[],
    authorize: () => void,
    signal: AbortSignal,
  ): Promise<void> {
    return transaction(
      this.database,
      'readwrite',
      async (tx) => {
        const first = expected[0];
        if (first === undefined) throw new OutboxError('invalid_selection');
        const global = await usage(tx);
        const rows = await laneRows(tx, first.lane);
        if (!same(rows, expected)) throw new OutboxError('invalid_selection');
        const count = rows.findIndex((row) => row.state !== 'acknowledged');
        const removed = rows.slice(0, count === -1 ? rows.length : count);
        if (removed.length === 0) throw new OutboxError('invalid_selection');
        authorize();
        const bytes = removed.reduce((sum, row) => sum + row.bytes, 0);
        for (const row of removed)
          await request(tx.objectStore('intents').delete(row.sequence));
        const checkpoint = decodeCheckpoint(
          await request(tx.objectStore('checkpoints').get(first.lane)),
        );
        if (removed.length === rows.length)
          await request(tx.objectStore('checkpoints').delete(first.lane));
        else
          await request(
            tx.objectStore('checkpoints').put({
              ...checkpoint,
              rows: rows.length - removed.length,
              bytes: checkpoint.bytes - bytes,
            }),
          );
        await request(
          tx.objectStore('usage').put({
            ...global,
            rows: global.rows - removed.length,
            bytes: global.bytes - bytes,
          }),
        );
      },
      signal,
    );
  }

  discard(
    expected: IntentRow[],
    authorize: () => void,
    signal: AbortSignal,
  ): Promise<void> {
    return transaction(
      this.database,
      'readwrite',
      async (tx) => {
        const head = expected[0];
        if (head === undefined) throw new OutboxError('invalid_selection');
        const global = await usage(tx);
        const current = await laneRows(tx, head.lane);
        if (!same(current, expected) || current[0]?.state !== 'unprepared')
          throw new OutboxError('invalid_selection');
        authorize();
        const removed = new Set([head.sequence]);
        for (const row of current.slice(1)) {
          if ('after' in row.base && removed.has(row.base.after))
            removed.add(row.sequence);
        }
        let bytes = 0;
        for (const row of current) {
          if (removed.has(row.sequence)) {
            await request(tx.objectStore('intents').delete(row.sequence));
            bytes += row.bytes;
          }
        }
        const survivors = current.filter((row) => !removed.has(row.sequence));
        if (survivors.length === 0)
          await request(tx.objectStore('checkpoints').delete(head.lane));
        else
          await request(
            tx.objectStore('checkpoints').put({
              format: 2,
              lane: head.lane,
              owner: head.owner,
              generation: head.authorizedGeneration,
              predecessor: decodeCheckpoint(
                await request(tx.objectStore('checkpoints').get(head.lane)),
              ).predecessor,
              rows: survivors.length,
              bytes: survivors.reduce((sum, row) => sum + row.bytes, 0),
            }),
          );
        await request(
          tx.objectStore('usage').put({
            ...global,
            rows: global.rows - removed.size,
            bytes: global.bytes - bytes,
          }),
        );
      },
      signal,
    );
  }
}
