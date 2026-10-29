import type { DraftRecord, DraftStore, KeptDraft } from './draft-store';

/** Keeps one artifact's draft of this tab in a draft store. */
export interface DraftKeeper {
  /**
   * Asks for `record` to be kept, or for the draft to be forgotten. Only
   * the last request counts; it resolves once that, or a later one, was
   * written or failed, with why the last write failed, if it did.
   * `continued`, a kept draft of another tab that this draft continues, is
   * removed with the first write that succeeds.
   */
  keep(
    record: DraftRecord | undefined,
    continued?: KeptDraft,
  ): Promise<string | undefined>;
  /** Resolves once every write asked for so far was made or failed. */
  settled(): Promise<void>;
}

const keepers = new WeakMap<DraftStore, Map<string, DraftKeeper>>();

/**
 * The keeper of one artifact's draft in `store`. There is one per store
 * and artifact, so the writes of every view of that draft, also of a view
 * that replaced an earlier one, run in the order asked (decision 0018).
 */
export function draftKeeper(
  store: DraftStore,
  artifactId: string,
): DraftKeeper {
  let byArtifact = keepers.get(store);
  if (byArtifact === undefined) {
    byArtifact = new Map();
    keepers.set(store, byArtifact);
  }
  let keeper = byArtifact.get(artifactId);
  if (keeper === undefined) {
    keeper = createDraftKeeper(store, artifactId);
    byArtifact.set(artifactId, keeper);
  }
  return keeper;
}

/**
 * Discards a listed draft: this tab's through its keeper, after the writes
 * asked for before; another tab's once this tab's writes settled.
 */
export async function discardKept(
  store: DraftStore,
  record: KeptDraft,
): Promise<void> {
  const keeper = draftKeeper(store, record.artifactId);
  if (record.mine) {
    const failure = await keeper.keep(undefined);
    if (failure !== undefined) throw new Error(failure);
    return;
  }
  await keeper.settled();
  await store.remove(record);
}

/**
 * Writes the last requested state of one draft, one write at a time, so a
 * write that finishes late never undoes a later one.
 */
function createDraftKeeper(store: DraftStore, artifactId: string): DraftKeeper {
  let wanted: { readonly record: DraftRecord | undefined } | undefined;
  let running: Promise<string | undefined> | undefined;
  // A closed tab's draft this one continues, until a write removes it.
  let replacing: KeptDraft | undefined;

  async function drain(): Promise<string | undefined> {
    let failure: string | undefined;
    while (wanted !== undefined) {
      const { record } = wanted;
      wanted = undefined;
      const replaced = replacing;
      try {
        if (record === undefined) await store.forget(artifactId, replaced);
        else await store.put(record, replaced);
        if (replacing === replaced) replacing = undefined;
        failure = undefined;
      } catch (error: unknown) {
        failure = error instanceof Error ? error.message : String(error);
      }
    }
    running = undefined;
    return failure;
  }

  return {
    keep(record, continued) {
      if (continued !== undefined) replacing = continued;
      wanted = { record };
      running ??= drain();
      return running;
    },
    async settled() {
      await running;
    },
  };
}
