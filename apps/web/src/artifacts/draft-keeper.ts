import type { DraftRecord, DraftStore, KeptDraft } from './draft-store';

/** Keeps one artifact's draft of this tab in a draft store. */
export interface DraftKeeper {
  /**
   * Asks for `record` to be kept, or for the draft to be forgotten. Only
   * the last request counts; it resolves once that, or a later one, was
   * written or failed. `continued`, a kept draft of another tab that this
   * draft continues, is removed with the first write that succeeds.
   */
  keep(record: DraftRecord | undefined, continued?: KeptDraft): Promise<void>;
  /** Resolves once every write asked for so far was made or failed. */
  settled(): Promise<void>;
}

/**
 * Writes the last requested state of one draft, one write at a time, so a
 * write that finishes late never undoes a later one (decision 0018).
 * `onResult` hears why the last write failed, or `undefined` once one
 * succeeded.
 */
export function createDraftKeeper(
  store: DraftStore,
  artifactId: string,
  onResult: (failure: string | undefined) => void,
): DraftKeeper {
  let wanted: { readonly record: DraftRecord | undefined } | undefined;
  let running: Promise<void> | undefined;
  // A closed tab's draft this one continues, until a write removes it.
  let replacing: KeptDraft | undefined;

  async function drain(): Promise<void> {
    while (wanted !== undefined) {
      const { record } = wanted;
      wanted = undefined;
      const replaced = replacing;
      try {
        if (record === undefined) await store.forget(artifactId, replaced);
        else await store.put(record, replaced);
        if (replacing === replaced) replacing = undefined;
        onResult(undefined);
      } catch (error: unknown) {
        onResult(error instanceof Error ? error.message : String(error));
      }
    }
    running = undefined;
  }

  return {
    keep(record, continued) {
      if (continued !== undefined) replacing = continued;
      wanted = { record };
      running ??= drain();
      return running;
    },
    settled() {
      return running ?? Promise.resolve();
    },
  };
}
