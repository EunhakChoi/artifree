/**
 * The workspace's own limit on what one frame may ask of it (decision 0032).
 * A frame is untrusted script: it can send messages as fast as it likes. Each
 * one costs the workspace a token here before anything is sent to the server,
 * and the server keeps a separate per-actor limit for frame traffic, so a
 * frame that floods spends neither the owner's budget nor the page's.
 */

/** Frame calls allowed in a burst. Below the server's, so it seldom answers. */
export const FRAME_BURST = 30;
/** Frame calls refilled each minute. */
export const FRAME_PER_MINUTE = 600;
/** Frame calls waiting for the server at once. */
export const FRAME_PENDING_CAP = 16;

/** A token bucket over an injectable clock, in milliseconds. */
export class TokenBucket {
  readonly #burst: number;
  readonly #perMs: number;
  readonly #now: () => number;
  #tokens: number;
  #last: number;

  constructor(
    now: () => number,
    burst: number = FRAME_BURST,
    perMinute: number = FRAME_PER_MINUTE,
  ) {
    this.#now = now;
    this.#burst = burst;
    this.#perMs = perMinute / 60_000;
    this.#tokens = burst;
    this.#last = now();
  }

  /** Spends one token, or returns false when none is left. */
  take(): boolean {
    const now = this.#now();
    // A clock that steps back refills nothing.
    const elapsed = Math.max(0, now - this.#last);
    this.#last = Math.max(this.#last, now);
    this.#tokens = Math.min(this.#burst, this.#tokens + elapsed * this.#perMs);
    if (this.#tokens < 1) return false;
    this.#tokens -= 1;
    return true;
  }
}
