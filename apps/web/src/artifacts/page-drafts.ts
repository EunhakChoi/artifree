import type { DraftRecord, DraftStore, KeptDraft } from './draft-store';

/**
 * This page's source drafts in the browser's store (decision 0018). The
 * page creates it once, before anything renders; views and connections
 * come and go, but they all write through it, so the writes for one
 * artifact run in the order asked whichever view asked.
 */
export interface PageDrafts {
  /**
   * The drafts this page may continue, once its writes asked for so far
   * settled: its own first, since continuing another tab's would replace
   * it, then the newest.
   */
  list(): Promise<KeptDraft[]>;
  /**
   * Asks for `record` to be kept as this page's draft of `artifactId`, or
   * for that draft to be forgotten. Consecutive keeps can coalesce while a
   * write is pending; a discard of another tab's record separates the keeps
   * before and after it. A keep resolves after its batch settles, with why
   * its last write failed, if it did. `continued`, a draft of another tab
   * that this one continues, is removed with the first write that succeeds.
   */
  keep(
    artifactId: string,
    record: DraftRecord | undefined,
    continued?: KeptDraft,
  ): Promise<string | undefined>;
  /** Discards a listed draft in order with the artifact's other writes. */
  discard(record: KeptDraft): Promise<void>;
}

/** Consecutive keeps, coalesced until another owner's discard separates them. */
interface Batch {
  wanted: { readonly record: DraftRecord | undefined } | undefined;
  continued: KeptDraft | undefined;
  readonly running: Promise<string | undefined>;
}

/** One artifact's ordered writes and the current coalescible keeps. */
interface Queue {
  batch: Batch | undefined;
  tail: Promise<void> | undefined;
  /** A draft of another tab this one continues, until a write removes it. */
  replacing: KeptDraft | undefined;
}

/** The page's drafts in `store`, whose owner id the page holds. */
export function createPageDrafts(store: DraftStore): PageDrafts {
  const queues = new Map<string, Queue>();
  const queueOf = (artifactId: string): Queue => {
    let queue = queues.get(artifactId);
    if (queue === undefined) {
      queue = { batch: undefined, tail: undefined, replacing: undefined };
      queues.set(artifactId, queue);
    }
    return queue;
  };

  /** Orders one operation after earlier ones, without poisoning later work. */
  function append<T>(queue: Queue, work: () => Promise<T>): Promise<T> {
    const result = (queue.tail ?? Promise.resolve()).then(work);
    const tail = result.then(
      () => undefined,
      () => undefined,
    );
    queue.tail = tail;
    void tail.then(() => {
      if (queue.tail === tail) queue.tail = undefined;
    });
    return result;
  }

  /** Writes a batch's last state until nothing newer is asked for. */
  async function drain(
    artifactId: string,
    queue: Queue,
    batch: Batch,
  ): Promise<string | undefined> {
    let failure: string | undefined;
    while (batch.wanted !== undefined) {
      const { record } = batch.wanted;
      batch.wanted = undefined;
      if (batch.continued !== undefined) {
        queue.replacing = batch.continued;
        batch.continued = undefined;
      }
      const replaced = queue.replacing;
      try {
        if (record === undefined) await store.forget(artifactId, replaced);
        else await store.put(record, replaced);
        if (queue.replacing === replaced) queue.replacing = undefined;
        failure = undefined;
      } catch (error: unknown) {
        failure = error instanceof Error ? error.message : String(error);
      }
    }
    if (queue.batch === batch) queue.batch = undefined;
    return failure;
  }

  function keep(
    artifactId: string,
    record: DraftRecord | undefined,
    continued?: KeptDraft,
  ): Promise<string | undefined> {
    const queue = queueOf(artifactId);
    if (queue.batch !== undefined) {
      queue.batch.wanted = { record };
      if (continued !== undefined) queue.batch.continued = continued;
      return queue.batch.running;
    }
    // append starts in a microtask, after this batch has been assigned.
    const running = append(queue, () => drain(artifactId, queue, batch));
    const batch: Batch = { wanted: { record }, continued, running };
    queue.batch = batch;
    return running;
  }

  return {
    async list() {
      await Promise.all([...queues.values()].map((queue) => queue.tail));
      return (await store.list()).sort(
        (left, right) =>
          Number(right.mine) - Number(left.mine) || right.keptAt - left.keptAt,
      );
    },
    keep,
    async discard(record) {
      if (record.mine) {
        const failure = await keep(record.artifactId, undefined);
        if (failure !== undefined) throw new Error(failure);
        return;
      }
      const queue = queueOf(record.artifactId);
      // Keeps asked for after this discard belong behind it, even if an
      // earlier keep is still in flight.
      queue.batch = undefined;
      const remove = async (): Promise<void> => {
        await store.remove(record);
        // A failed adoption may still be waiting to replace this record.
        // Once the user discards it, a new draft must not try again.
        if (
          queue.replacing?.owner === record.owner &&
          queue.replacing.artifactId === record.artifactId
        )
          queue.replacing = undefined;
      };
      await append(queue, remove);
    },
  };
}
