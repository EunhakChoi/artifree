import type { SourceDraft } from './source-draft';

/**
 * A source draft as a tab keeps it in this browser so that it outlives the
 * page (decision 0018). A tab keeps at most one per artifact.
 */
export interface DraftRecord {
  readonly artifactId: string;
  /** The artifact's title when the draft was kept, to name it. */
  readonly title: string;
  /** When it was last written, in Unix milliseconds. */
  readonly keptAt: number;
  readonly draft: SourceDraft;
  /** Counts the draft's changes, so a kept save can name what it sent. */
  readonly version: number;
  /** The last save sent: its key, and the version of the draft it carried. */
  readonly sent: { readonly key: string; readonly version: number } | undefined;
  /** Whether a save of this draft may have committed. */
  readonly uncertain: boolean;
}

/** A kept draft this tab may continue. */
export interface KeptDraft extends DraftRecord {
  /** The tab that wrote it. */
  readonly owner: string;
  /** Whether that tab is this one. */
  readonly mine: boolean;
}

/** Source drafts kept in this browser, apart from any other stored data. */
export interface DraftStore {
  /**
   * The drafts this tab may continue, newest first: its own, and those of
   * tabs no longer open or whose state the browser cannot tell (no Web
   * Locks outside a secure context). A draft of an open tab stays with it.
   */
  list(): Promise<KeptDraft[]>;
  /** Keeps `record` as this tab's draft of its artifact. */
  put(record: DraftRecord): Promise<void>;
  /** Removes this tab's draft of an artifact. */
  forget(artifactId: string): Promise<void>;
  /** Removes a listed draft, of this tab or another. */
  remove(record: KeptDraft): Promise<void>;
}

const DATABASE = 'artifree-source-drafts';
const DRAFTS = 'drafts';
const OWNER_KEY = 'artifree.draftOwner';
const LOCK_PREFIX = 'artifree.draft-owner:';

/** What the object store holds: a draft and the tab that wrote it. */
interface Stored extends DraftRecord {
  readonly owner: string;
}

/**
 * The store for this tab. Each call waits until the tab holds its owner
 * id, so no draft is written or listed under an id another tab holds.
 */
export function openDraftStore(): DraftStore {
  const owner = claimOwner();
  // Each call reports a failed claim; none is left unhandled meanwhile.
  owner.catch(() => {});
  let database: Promise<IDBDatabase> | undefined;
  const open = (): Promise<IDBDatabase> => {
    database ??= openDatabase();
    // A failed open is tried again on the next call.
    database.catch(() => {
      database = undefined;
    });
    return database;
  };
  const remove = async (recordOwner: string, artifactId: string) => {
    const db = await open();
    await done(db, 'readwrite', (store) =>
      store.delete([recordOwner, artifactId]),
    );
  };
  return {
    async list() {
      const me = await owner;
      const db = await open();
      const all = await request<Stored[]>(
        db.transaction(DRAFTS).objectStore(DRAFTS).getAll(),
      );
      const others = await openOwners();
      const records: KeptDraft[] = [];
      for (const record of all) {
        const mine = record.owner === me;
        if (!mine && others?.has(record.owner) === true) continue;
        records.push({ ...record, mine });
      }
      return records.sort((left, right) => right.keptAt - left.keptAt);
    },
    async put(record) {
      const stored: Stored = {
        owner: await owner,
        artifactId: record.artifactId,
        title: record.title,
        keptAt: record.keptAt,
        draft: record.draft,
        version: record.version,
        sent: record.sent,
        uncertain: record.uncertain,
      };
      const db = await open();
      await done(db, 'readwrite', (store) => store.put(stored));
    },
    async forget(artifactId) {
      await remove(await owner, artifactId);
    },
    async remove(record) {
      await owner;
      await remove(record.owner, record.artifactId);
    },
  };
}

/**
 * The tab's owner id. It lives in session storage, so a reload keeps it,
 * and the tab holds a Web Lock named after it until it closes. A tab
 * duplicated with its session storage finds the lock held and takes a new
 * id; so does a reload that finds the previous page still holding it, whose
 * drafts it may then continue once that page is gone.
 */
async function claimOwner(): Promise<string> {
  let owner: string | undefined;
  try {
    owner = sessionStorage.getItem(OWNER_KEY) ?? undefined;
  } catch {
    // Without session storage the id lasts as long as the page.
  }
  owner ??= crypto.randomUUID();
  const locks = navigator.locks as LockManager | undefined;
  if (locks !== undefined)
    while (!(await hold(locks, owner))) owner = crypto.randomUUID();
  try {
    sessionStorage.setItem(OWNER_KEY, owner);
  } catch {
    // As above.
  }
  return owner;
}

/** Takes the lock of `owner` for the life of the page, if it is free. */
function hold(locks: LockManager, owner: string): Promise<boolean> {
  return new Promise((resolve, reject) => {
    locks
      .request(`${LOCK_PREFIX}${owner}`, { ifAvailable: true }, (lock) => {
        resolve(lock !== null);
        return lock === null ? undefined : new Promise<never>(() => {});
      })
      .catch(reject);
  });
}

/** The owners whose tabs are open now, or none where the browser cannot tell. */
async function openOwners(): Promise<Set<string> | undefined> {
  const locks = navigator.locks as LockManager | undefined;
  if (locks === undefined) return undefined;
  const { held = [] } = await locks.query();
  const owners = new Set<string>();
  for (const lock of held)
    if (lock.name?.startsWith(LOCK_PREFIX) === true)
      owners.add(lock.name.slice(LOCK_PREFIX.length));
  return owners;
}

function openDatabase(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const opening = indexedDB.open(DATABASE, 1);
    opening.onupgradeneeded = () => {
      opening.result.createObjectStore(DRAFTS, {
        keyPath: ['owner', 'artifactId'],
      });
    };
    opening.onsuccess = () => resolve(opening.result);
    opening.onerror = () => reject(opening.error ?? new Error('IndexedDB'));
    opening.onblocked = () =>
      reject(new Error('Another tab holds an older draft store open.'));
  });
}

function request<T>(asked: IDBRequest): Promise<T> {
  return new Promise((resolve, reject) => {
    asked.onsuccess = () => resolve(asked.result as T);
    asked.onerror = () => reject(asked.error ?? new Error('IndexedDB'));
  });
}

/** Runs `write` and resolves once its transaction committed. */
function done(
  db: IDBDatabase,
  mode: IDBTransactionMode,
  write: (store: IDBObjectStore) => void,
): Promise<void> {
  return new Promise((resolve, reject) => {
    const transaction = db.transaction(DRAFTS, mode);
    transaction.oncomplete = () => resolve();
    transaction.onerror = () =>
      reject(transaction.error ?? new Error('IndexedDB'));
    transaction.onabort = () =>
      reject(transaction.error ?? new Error('The draft was not kept.'));
    try {
      write(transaction.objectStore(DRAFTS));
    } catch (error: unknown) {
      transaction.abort();
      reject(error instanceof Error ? error : new Error(String(error)));
    }
  });
}
