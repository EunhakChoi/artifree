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
const VERSION = 1;
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

export interface IntentRow {
  readonly format: 1;
  readonly state: 'unprepared';
  readonly version: 1;
  readonly sequence: number;
  readonly key: string;
  readonly owner: string;
  readonly binding: BindingKey;
  readonly lane: LaneKey;
  readonly artifactId: string;
  readonly admittedGeneration: string;
  readonly operation: 'set' | 'update' | 'delete';
  readonly path: string;
  readonly text: string | null;
  readonly base: IntentBase;
  readonly bytes: number;
}

interface Checkpoint {
  readonly lane: LaneKey;
  readonly owner: string;
  readonly generation: string;
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

function account(row: Omit<IntentRow, 'bytes'>): number {
  const { text, ...metadata } = row;
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

function decodeRow(value: unknown): IntentRow {
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
      'operation',
      'path',
      'text',
      'base',
      'bytes',
    ]) ||
    value.format !== 1 ||
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
    typeof value.operation !== 'string' ||
    !['set', 'update', 'delete'].includes(value.operation) ||
    (value.binding[2] === 'local' && value.admittedGeneration !== '1') ||
    typeof value.path !== 'string' ||
    !validBase(value.base) ||
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
  const row: IntentRow = {
    format: 1,
    state: 'unprepared',
    version: 1,
    sequence: value.sequence,
    key: value.key,
    owner: value.owner,
    binding: value.binding,
    lane: value.lane,
    artifactId: value.lane[4],
    admittedGeneration: value.admittedGeneration,
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
    !record(value) ||
    !exact(value, ['lane', 'owner', 'generation', 'rows', 'bytes']) ||
    !validLane(value.lane) ||
    value.owner !== value.lane[0] ||
    !validGeneration(value.generation) ||
    (value.lane[2] === 'local' && value.generation !== '1') ||
    !natural(value.rows) ||
    value.rows === 0 ||
    value.rows > LANE_ROWS ||
    !natural(value.bytes) ||
    value.bytes > LANE_BYTES
  )
    throw new OutboxError('storage_corrupt');
  return {
    lane: value.lane,
    owner: value.lane[0],
    generation: value.generation,
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
      finished = true;
      resolve();
    };
    tx.onabort = () => {
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
        ('after' in row.base && values.at(-1)?.sequence !== row.base.after) ||
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
  if (
    checkpoint.rows !== values.length ||
    checkpoint.bytes !== values.reduce((sum, row) => sum + row.bytes, 0) ||
    values.some((row) => row.admittedGeneration !== checkpoint.generation)
  )
    throw new OutboxError('storage_corrupt');
  return values;
}

/** Dedicated first released schema; unknown/experimental formats stay untouched. */
export async function openStore(): Promise<LocalStore> {
  const database = await new Promise<IDBDatabase>((resolve, reject) => {
    const opening = indexedDB.open(DATABASE, VERSION);
    let refused = false;
    opening.onupgradeneeded = (event) => {
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
        if (
          !same(
            Array.from(database.objectStoreNames).sort(),
            [...STORES].sort(),
          )
        )
          throw new OutboxError('storage_corrupt');
        const tx = database.transaction([...STORES], 'readonly');
        for (const name of STORES) {
          const store = tx.objectStore(name);
          if (
            store.keyPath !==
              (name === 'intents'
                ? 'sequence'
                : name === 'usage'
                  ? 'id'
                  : 'lane') ||
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
        resolve(database);
      } catch {
        database.close();
        reject(new OutboxError('storage_corrupt'));
      }
    };
    opening.onerror = () => reject(new OutboxError('storage_failed'));
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
    for (const row of rows) {
      if (!same(row.binding.slice(0, 3), prefix))
        throw new OutboxError('storage_corrupt');
      const lane = JSON.stringify(row.lane);
      if (!targets.has(lane)) targets.set(lane, cleanupTarget(row));
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

  enqueue(input: Omit<IntentRow, 'sequence' | 'bytes'>): Promise<IntentRow> {
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
      const row = decodeRow({ ...body, bytes });
      await request(tx.objectStore('intents').add(row));
      await request(
        tx.objectStore('checkpoints').put({
          lane: row.lane,
          owner: row.owner,
          generation: row.admittedGeneration,
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
        if (!same(current, expected))
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
              lane: head.lane,
              owner: head.owner,
              generation: head.admittedGeneration,
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
