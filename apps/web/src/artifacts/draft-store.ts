import type { SourceDraft } from './source-draft';

/**
 * A source draft as a tab keeps it in this browser, so that it outlives the
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
  readonly sent:
    | {
        readonly key: string;
        readonly version: number;
        /** Digest of the exact request body, absent on older records. */
        readonly requestDigest?: string;
      }
    | undefined;
  /** Request keys retained while the draft exists, keyed by body digest. */
  readonly attempts?: readonly {
    readonly key: string;
    readonly requestDigest: string;
  }[];
  /** Whether a save of this draft may have committed. */
  readonly uncertain: boolean;
}

/** A kept draft this tab may continue. */
export interface KeptDraft extends DraftRecord {
  /** The owner id of the tab that wrote it. */
  readonly owner: string;
  /** Whether that tab is this one. */
  readonly mine: boolean;
  /** The stored version that was listed, absent on older records. */
  readonly storageVersion?: string;
}

/**
 * The browser's store of source drafts, apart from any other stored data.
 * Every call first waits until this page holds its owner id.
 */
export interface DraftStore {
  /**
   * The drafts this tab may continue: its own, and those of tabs no longer
   * open or whose state the browser cannot tell when Web Locks are absent.
   * A draft of an open tab is left to that tab when locks are available.
   */
  list(): Promise<KeptDraft[]>;
  /**
   * Keeps `record` as this tab's draft of its artifact. `replacing`, a
   * listed draft of another tab that this one continues, is removed in the
   * same transaction, so it stays when the write fails.
   */
  put(record: DraftRecord, replacing?: KeptDraft): Promise<void>;
  /** Removes this tab's draft of an artifact, and `replacing` with it. */
  forget(artifactId: string, replacing?: KeptDraft): Promise<void>;
  /** Removes a listed draft of another tab. */
  remove(record: KeptDraft): Promise<void>;
}

const DATABASE = 'artifree-source-drafts';
const DRAFTS = 'drafts';
const OWNER_KEY = 'artifree.draftOwner';
const LOCK_PREFIX = 'artifree.draft-owner:';

/** A record as the object store holds it, under its owner and artifact. */
interface Stored extends DraftRecord {
  readonly owner: string;
  readonly storageVersion?: string;
}

/**
 * Opens the store and claims this page's owner id at once, so the page
 * holds it from its start, whatever it shows.
 */
export function openDraftStore(): DraftStore {
  let claimed: Promise<string> | undefined;
  const owner = (): Promise<string> => {
    if (claimed === undefined) {
      const attempt = claimOwner();
      claimed = attempt;
      // A transient failure is reported to the current caller and the next
      // lookup may try again, while the initial claim remains handled.
      attempt.catch(() => {
        if (claimed === attempt) claimed = undefined;
      });
    }
    return claimed;
  };
  void owner();
  let database: Promise<IDBDatabase> | undefined;
  const open = (): Promise<IDBDatabase> => {
    database ??= openDatabase();
    // A failed open is tried again by the next call.
    database.catch(() => {
      database = undefined;
    });
    return database;
  };
  /** Runs `change` in one read-write transaction, once the owner is held. */
  const write = async (
    replacing: KeptDraft | undefined,
    change: (
      store: IDBObjectStore,
      owner: string,
      refuse: (reason: Error) => void,
    ) => void,
  ): Promise<void> => {
    const heldOwner = await owner();
    const perform = async (): Promise<void> => {
      const transaction = (await open()).transaction(DRAFTS, 'readwrite');
      let refusal: Error | undefined;
      const refuse = (reason: Error): void => {
        refusal = reason;
        transaction.abort();
      };
      await committed(
        transaction,
        () => change(transaction.objectStore(DRAFTS), heldOwner, refuse),
        () => refusal,
      );
    };
    const locks = navigator.locks as LockManager | undefined;
    if (
      replacing === undefined ||
      replacing.owner === heldOwner ||
      locks === undefined
    )
      return perform();
    // A list is only a snapshot. Hold the other owner's lock through the
    // transaction so a tab that has returned cannot lose its draft.
    const failure = await locks.request(
      `${LOCK_PREFIX}${replacing.owner}`,
      { ifAvailable: true },
      async (lock): Promise<Error | undefined> => {
        // Firefox reports a rejected lock callback as a page error even
        // when the request's rejection is handled. Return the failure while
        // holding the lock, then reject the caller outside the callback.
        if (lock === null) return new Error('Another tab is using this draft.');
        try {
          await perform();
          return undefined;
        } catch (error: unknown) {
          return error instanceof Error ? error : new Error(String(error));
        }
      },
    );
    if (failure !== undefined) throw failure;
  };
  const removeReplaced = (
    store: IDBObjectStore,
    owner: string,
    replacing: KeptDraft | undefined,
    refuse: (reason: Error) => void,
  ) => {
    if (replacing === undefined || replacing.owner === owner) return;
    const key = [replacing.owner, replacing.artifactId];
    const current = store.get(key);
    current.onsuccess = () => {
      const stored = current.result as Stored | undefined;
      if (
        stored === undefined ||
        stored.storageVersion !== replacing.storageVersion ||
        (stored.storageVersion === undefined &&
          (stored.keptAt !== replacing.keptAt ||
            stored.version !== replacing.version))
      ) {
        refuse(
          new Error('This draft changed. Reopen the artifact to review it.'),
        );
        return;
      }
      try {
        store.delete(key);
      } catch (error: unknown) {
        refuse(error instanceof Error ? error : new Error(String(error)));
      }
    };
  };
  return {
    async list() {
      const heldOwner = await owner();
      const db = await open();
      const all = await new Promise<Stored[]>((resolve, reject) => {
        const asked = db.transaction(DRAFTS).objectStore(DRAFTS).getAll();
        asked.onsuccess = () => resolve(asked.result as Stored[]);
        asked.onerror = () =>
          reject(asked.error ?? new Error('The drafts could not be read.'));
      });
      const others = await openOwners();
      const records: KeptDraft[] = [];
      for (const record of all) {
        const mine = record.owner === heldOwner;
        if (!mine && others?.has(record.owner) === true) continue;
        records.push({ ...record, mine });
      }
      return records;
    },
    put(record, replacing) {
      return write(replacing, (store, owner, refuse) => {
        const stored: Stored = {
          owner,
          storageVersion: crypto.randomUUID(),
          artifactId: record.artifactId,
          title: record.title,
          keptAt: record.keptAt,
          draft: record.draft,
          version: record.version,
          sent: record.sent,
          attempts: record.attempts ?? [],
          uncertain: record.uncertain,
        };
        store.put(stored);
        removeReplaced(store, owner, replacing, refuse);
      });
    },
    forget(artifactId, replacing) {
      return write(replacing, (store, owner, refuse) => {
        store.delete([owner, artifactId]);
        removeReplaced(store, owner, replacing, refuse);
      });
    },
    remove(record) {
      return write(record, (store, owner, refuse) =>
        removeReplaced(store, owner, record, refuse),
      );
    },
  };
}

/**
 * The page's owner id. It lives in session storage, so a reload of the tab
 * keeps it, and the page holds a Web Lock named after it until it closes.
 * A tab duplicated with its session storage finds the lock held and takes
 * a new id; so does a reload that finds the previous page still holding
 * it, whose drafts it may continue once that page is gone.
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

/** The owners whose pages are open now, or none where the browser cannot tell. */
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
    opening.onerror = () =>
      reject(opening.error ?? new Error('The drafts could not be opened.'));
    opening.onblocked = () =>
      reject(new Error('Another tab holds an older draft store open.'));
  });
}

/** Runs `change` and resolves once `transaction` committed. */
function committed(
  transaction: IDBTransaction,
  change: () => void,
  refusal: () => Error | undefined,
): Promise<void> {
  return new Promise((resolve, reject) => {
    transaction.oncomplete = () => resolve();
    transaction.onerror = () =>
      reject(transaction.error ?? new Error('The draft was not kept.'));
    transaction.onabort = () =>
      reject(
        refusal() ?? transaction.error ?? new Error('The draft was not kept.'),
      );
    try {
      change();
    } catch (error: unknown) {
      transaction.abort();
      reject(error instanceof Error ? error : new Error(String(error)));
    }
  });
}
