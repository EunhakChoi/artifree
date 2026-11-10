import type { ChangeFeedEvent } from '@artifree/client';

/** Receives the `data_version` a data write left an artifact's space at. */
export type DataChangeListener = (dataVersion: number) => void;

/**
 * Where the workspace reports the `data_changed` changes of its change feed,
 * per artifact (decision 0053, rule 16). The workspace's own state ignores
 * them (0040, rule 6); a preview forwards them to its frame as a hint.
 */
export interface DataChanges {
  /** Registers `listener` for `artifactId`; the returned function removes this registration. */
  readonly subscribe: (
    artifactId: string,
    listener: DataChangeListener,
  ) => () => void;
}

/** The page's one `DataChanges`, fed by its change feed handler. */
export class DataChangeHub implements DataChanges {
  readonly #listeners = new Map<
    string,
    Set<{ readonly listener: DataChangeListener }>
  >();

  readonly subscribe = (
    artifactId: string,
    listener: DataChangeListener,
  ): (() => void) => {
    const entry = { listener };
    let entries = this.#listeners.get(artifactId);
    if (entries === undefined) {
      entries = new Set();
      this.#listeners.set(artifactId, entries);
    }
    entries.add(entry);
    return () => {
      const current = this.#listeners.get(artifactId);
      if (current === undefined) return;
      current.delete(entry);
      if (current.size === 0) this.#listeners.delete(artifactId);
    };
  };

  /**
   * Tells the listeners of the artifact that `event` names, when it is a
   * `data_changed` change with its version; every other event is ignored.
   */
  readonly follow = (event: ChangeFeedEvent): void => {
    if (event.type !== 'change') return;
    const { artifact_id: id, change, data_version: dataVersion } = event.change;
    if (change !== 'data_changed' || dataVersion === undefined) return;
    const entries = this.#listeners.get(id);
    if (entries === undefined) return;
    for (const { listener } of Array.from(entries)) listener(dataVersion);
  };
}
