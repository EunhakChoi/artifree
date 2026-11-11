import type { ChangeFeedEvent, FeedChange } from '@artifree/client';

/** Receives each change of one artifact's feed, as the feed delivered it. */
export type ArtifactChangeListener = (change: FeedChange) => void;

/**
 * Where the workspace reports every change of its change feed, per artifact,
 * for a view that follows one artifact (decision 0055, rule 15). The
 * workspace's own state is not changed by this.
 */
export interface ArtifactChanges {
  /** Registers `listener` for `artifactId`; the returned function removes this registration. */
  readonly subscribe: (
    artifactId: string,
    listener: ArtifactChangeListener,
  ) => () => void;
  /**
   * How many `reset` events of the feed the workspace has followed. A reset
   * means the server's data may be another database's, so a follower drops
   * what it loaded (rule 15). A `ready` does not count: the data is the same.
   */
  readonly resetCount: () => number;
}

/** The page's one `ArtifactChanges`, fed by its change feed handler. */
export class ArtifactChangeHub implements ArtifactChanges {
  private readonly listeners = new Map<
    string,
    Set<{ readonly listener: ArtifactChangeListener }>
  >();
  private resets = 0;

  readonly subscribe = (
    artifactId: string,
    listener: ArtifactChangeListener,
  ): (() => void) => {
    const entry = { listener };
    let entries = this.listeners.get(artifactId);
    if (entries === undefined) {
      entries = new Set();
      this.listeners.set(artifactId, entries);
    }
    entries.add(entry);
    return () => {
      const current = this.listeners.get(artifactId);
      if (current === undefined) return;
      current.delete(entry);
      if (current.size === 0) this.listeners.delete(artifactId);
    };
  };

  /** How many `reset` events the page has followed (see `ArtifactChanges`). */
  readonly resetCount = (): number => this.resets;

  /**
   * Passes the change that `event` names to the listeners of its artifact. A
   * reset is counted; any other event that is not a change is ignored.
   */
  readonly follow = (event: ChangeFeedEvent): void => {
    if (event.type === 'reset') {
      this.resets += 1;
      return;
    }
    if (event.type !== 'change') return;
    const entries = this.listeners.get(event.change.artifact_id);
    if (entries === undefined) return;
    for (const { listener } of Array.from(entries)) {
      // A listener must not stop the page's change feed, whose handler this
      // runs in, or the listeners after it.
      try {
        listener(event.change);
      } catch (error: unknown) {
        console.error('An artifact change listener failed.', error);
      }
    }
  };
}
