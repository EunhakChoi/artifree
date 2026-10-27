/** Whether the page is visible, and a way to hear when that changes. */
export interface Visibility {
  readonly visible: () => boolean;
  /** Calls `onChange` after each change; returns a function that stops. */
  readonly subscribe: (onChange: () => void) => () => void;
}

/** How long a running read holds back the next one. */
export const STALL_MS = 10_000;

/**
 * Decides when the workspace's reads start (decision 0007). It owns every
 * condition for starting one, so that no combination of them is decided
 * elsewhere:
 *
 * - the page is visible at the moment the read would start;
 * - no `Retry-After` delay is pending. The delay holds every read of the
 *   page, because request limits apply to the actor, not to one view;
 * - no other read is running, unless that one has run for {@link STALL_MS},
 *   so a read that never answers cannot stop the page.
 *
 * Reads start one at a time in the order they were requested. Writes do not
 * pass through it: the user starts each save and sees its outcome.
 */
export class ReadScheduler {
  readonly #visibility: Visibility;
  // A set keeps insertion order and lets a cancelled entry leave in place.
  readonly #waiting = new Set<() => Promise<void>>();
  #running: object | undefined;
  #notBefore = 0;
  #holdTimer: ReturnType<typeof setTimeout> | undefined;
  #stopWatching: (() => void) | undefined;

  constructor(visibility: Visibility) {
    this.#visibility = visibility;
  }

  /**
   * Starts `read` when the conditions allow. The returned function cancels
   * it if it has not started; a read that started runs to its end, and its
   * caller ignores the result.
   */
  request(read: () => Promise<void>): () => void {
    // A wrapper, so that requesting the same function twice queues it twice.
    const entry = (): Promise<void> => read();
    this.#waiting.add(entry);
    this.#watch();
    this.#pump();
    return () => {
      this.#waiting.delete(entry);
      this.#watch();
    };
  }

  /** Holds every read that has not started until `seconds` from now. */
  holdFor(seconds: number): void {
    this.#notBefore = Math.max(this.#notBefore, Date.now() + seconds * 1000);
    this.#pump();
  }

  // Visibility matters only while reads wait, so the scheduler listens only
  // then and holds no listener once its page is gone.
  #watch(): void {
    if (this.#waiting.size > 0 && this.#stopWatching === undefined)
      this.#stopWatching = this.#visibility.subscribe(() => this.#pump());
    else if (this.#waiting.size === 0 && this.#stopWatching !== undefined) {
      this.#stopWatching();
      this.#stopWatching = undefined;
    }
  }

  #pump(): void {
    clearTimeout(this.#holdTimer);
    this.#holdTimer = undefined;
    if (this.#running !== undefined || !this.#visibility.visible()) return;
    const [next] = this.#waiting;
    if (next === undefined) return;
    const wait = this.#notBefore - Date.now();
    if (wait > 0) {
      this.#holdTimer = setTimeout(() => this.#pump(), wait);
      return;
    }
    this.#waiting.delete(next);
    this.#watch();
    const running = {};
    this.#running = running;
    const release = (): void => {
      if (this.#running !== running) return;
      this.#running = undefined;
      this.#pump();
    };
    const stalled = setTimeout(release, STALL_MS);
    // Each read reports its own failure; here it only frees the slot.
    void next()
      .catch(() => undefined)
      .finally(() => {
        clearTimeout(stalled);
        release();
      });
  }
}
