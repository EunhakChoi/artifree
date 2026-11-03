/**
 * The desk that decides when a frame's download request becomes a prompt for
 * the user, and when the user's answer counts (decision 0032). It holds no DOM
 * and no React: a component only shows `snapshot()` and forwards presses, and
 * every rule is tested here with an injected clock.
 *
 * A frame is untrusted script that can ask at any moment, possibly while the
 * user is about to click something else. So: one offer at a time; the accept
 * button counts only after an activation delay and only for a press the
 * browser itself made; declining starts a cooldown during which the frame is
 * refused; an offer is withdrawn when the frame gives up, goes away or was
 * never wanted. The prompt is a modal that does not move the frame.
 */

/** Milliseconds after an offer appears before accepting can count. */
export const ACTIVATION_MS = 800;
/** Milliseconds after a decline during which no new offer is shown. */
export const DECLINE_COOLDOWN_MS = 5_000;

/** The time the desk reads, injectable for tests. */
export interface DeskClock {
  now(): number;
  /** Runs `run` after `ms`; the returned function cancels it. */
  after(ms: number, run: () => void): () => void;
}

/** The page's clock. */
export const systemClock: DeskClock = {
  now: () => performance.now(),
  after: (ms, run) => {
    const handle = window.setTimeout(run, ms);
    return () => window.clearTimeout(handle);
  },
};

/** What the server said it would hand over; the desk shows exactly this. */
export interface Offer {
  readonly kind: 'file' | 'archive';
  readonly path: string | null;
  readonly fileName: string;
  readonly size: number | null;
}

/**
 * `accepted`: the user pressed accept after the delay. `declined`: the user
 * said no. `withdrawn`: the frame gave up, left, or was already gone.
 * `busy`: another offer is open. `cooling`: the user declined a moment ago.
 */
export type Outcome =
  'accepted' | 'declined' | 'withdrawn' | 'busy' | 'cooling';

/** What a prompt shows. */
export interface DeskSnapshot {
  /** The open offer, if any. */
  readonly offer: Offer | undefined;
  /** Whether a press on accept would count now. */
  readonly armed: boolean;
}

interface Open {
  readonly offer: Offer;
  readonly shownAt: number;
  readonly resolve: (outcome: Outcome) => void;
  readonly signal: AbortSignal;
  readonly onAbort: () => void;
  readonly cancelArming: () => void;
}

const NONE: DeskSnapshot = { offer: undefined, armed: false };

/** See the module comment. */
export class OfferDesk {
  readonly #clock: DeskClock;
  readonly #listeners = new Set<() => void>();
  #open: Open | undefined;
  #coolUntil = Number.NEGATIVE_INFINITY;
  #view: DeskSnapshot = NONE;

  constructor(clock: DeskClock = systemClock) {
    this.#clock = clock;
  }

  /** Subscribes to changes of the snapshot; the result unsubscribes. */
  readonly subscribe = (listener: () => void): (() => void) => {
    this.#listeners.add(listener);
    return () => {
      this.#listeners.delete(listener);
    };
  };

  /** The state a prompt shows; the same object until it changes. */
  readonly snapshot = (): DeskSnapshot => this.#view;

  /**
   * Offers `offer` to the user and waits for the answer. Aborting `signal`
   * withdraws the offer. Nothing is shown when the signal is already
   * aborted, when an offer is open, or during the cooldown.
   */
  ask(offer: Offer, signal: AbortSignal): Promise<Outcome> {
    if (signal.aborted) return Promise.resolve('withdrawn');
    if (this.#open !== undefined) return Promise.resolve('busy');
    if (this.#clock.now() < this.#coolUntil) return Promise.resolve('cooling');
    return new Promise<Outcome>((resolve) => {
      const onAbort = (): void => this.#finish('withdrawn');
      signal.addEventListener('abort', onAbort, { once: true });
      const cancelArming = this.#clock.after(ACTIVATION_MS, () => {
        if (this.#open?.signal === signal) this.#show(offer, true);
      });
      this.#open = {
        offer,
        shownAt: this.#clock.now(),
        resolve,
        signal,
        onAbort,
        cancelArming,
      };
      this.#show(offer, false);
    });
  }

  /**
   * The user pressed accept. It counts only for a press the browser made
   * (`trusted`), once the activation delay has passed, and once: the offer is
   * closed before this returns, so a second press finds nothing open.
   */
  accept(trusted: boolean): boolean {
    const open = this.#open;
    if (open === undefined || !trusted) return false;
    if (this.#clock.now() - open.shownAt < ACTIVATION_MS) return false;
    this.#finish('accepted');
    return true;
  }

  /** The user pressed decline or dismissed the prompt. */
  decline(): boolean {
    if (this.#open === undefined) return false;
    this.#coolUntil = this.#clock.now() + DECLINE_COOLDOWN_MS;
    this.#finish('declined');
    return true;
  }

  /** Withdraws the open offer, if any: the frame went away. */
  withdraw(): void {
    this.#finish('withdrawn');
  }

  #show(offer: Offer, armed: boolean): void {
    this.#view = { offer, armed };
    for (const listener of this.#listeners) listener();
  }

  #finish(outcome: Outcome): void {
    const open = this.#open;
    if (open === undefined) return;
    // Closed before anything else can run, so a second call finds nothing.
    this.#open = undefined;
    open.cancelArming();
    open.signal.removeEventListener('abort', open.onAbort);
    this.#view = NONE;
    for (const listener of this.#listeners) listener();
    open.resolve(outcome);
  }
}
