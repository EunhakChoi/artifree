/**
 * When the workspace asks the user to confirm a download that a frame asked
 * for (decision 0032). Only timing and counting: no DOM and no React, so the
 * rules can be tested with a clock the test supplies.
 *
 * A frame chooses when its question appears, so it can try to have the
 * button under the pointer at that moment, or ask again as soon as it is
 * declined. The gate therefore ignores a press for a short time after the
 * question opens, withdraws a question nobody answered before the frame gives
 * up, and refuses a new one for a while after a "no".
 */
export const OFFER_TIMING = {
  /** A press earlier than this after the question opened is ignored. */
  activationMs: 700,
  /**
   * An unanswered question is withdrawn after this long. It is shorter than
   * the frame's own wait (120 s in the runtime script), so the frame is told
   * `declined` before it gives up, and a later press cannot save a file
   * nobody is waiting for.
   */
  lifetimeMs: 90_000,
  /** After a decline or a withdrawal the frame is told `declined` at once. */
  cooldownMs: 3_000,
} as const;

/** How many characters of a frame-supplied path the question shows. */
export const DISPLAY_PATH_CHARS = 80;

/** A frame-supplied path as shown to the user: cut, with an ellipsis. */
export function displayPath(path: string): string {
  const characters = Array.from(path);
  return characters.length <= DISPLAY_PATH_CHARS
    ? path
    : `${characters.slice(0, DISPLAY_PATH_CHARS).join('')}…`;
}

/** What happened to a frame's request for a question. */
export type OfferVerdict = 'opened' | 'busy' | 'cooling';

/** The one question that can be open, and the pause after a "no". */
export class OfferGate {
  readonly #timing: typeof OFFER_TIMING;
  #openedAt: number | undefined;
  #coolUntil = Number.NEGATIVE_INFINITY;

  constructor(timing: typeof OFFER_TIMING = OFFER_TIMING) {
    this.#timing = timing;
  }

  get isOpen(): boolean {
    return this.#openedAt !== undefined;
  }

  /** Opens a question unless one is open or the last "no" is too recent. */
  open(now: number): OfferVerdict {
    if (this.#openedAt !== undefined) return 'busy';
    if (now < this.#coolUntil) return 'cooling';
    this.#openedAt = now;
    return 'opened';
  }

  /** Whether a press now counts: open, active, and not yet expired. */
  canConfirm(now: number): boolean {
    if (this.#openedAt === undefined) return false;
    const age = now - this.#openedAt;
    return age >= this.#timing.activationMs && age < this.#timing.lifetimeMs;
  }

  /** The question ended without a download: the next one has to wait. */
  decline(now: number): void {
    this.#openedAt = undefined;
    this.#coolUntil = now + this.#timing.cooldownMs;
  }

  /** The question ended in a download, or with its frame: no pause. */
  close(): void {
    this.#openedAt = undefined;
  }
}
