import { HostRefusal } from './runtime-requests';

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
  #confirmed = false;
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
    this.#confirmed = false;
    return 'opened';
  }

  /**
   * Whether a press now counts: open, active, not yet expired and not yet
   * pressed. This is the only authority on all of it; what the button shows
   * is derived from it.
   */
  canConfirm(now: number): boolean {
    if (this.#openedAt === undefined || this.#confirmed) return false;
    const age = now - this.#openedAt;
    return age >= this.#timing.activationMs && age < this.#timing.lifetimeMs;
  }

  /** Takes a press: true for the first one that counts, false for any other. */
  confirm(now: number): boolean {
    if (!this.canConfirm(now)) return false;
    this.#confirmed = true;
    return true;
  }

  /** The question ended without a download: the next one has to wait. */
  decline(now: number): void {
    this.#openedAt = undefined;
    this.#confirmed = false;
    this.#coolUntil = now + this.#timing.cooldownMs;
  }

  /** The question ended in a download, or with its frame: no pause. */
  close(): void {
    this.#openedAt = undefined;
    this.#confirmed = false;
  }
}

/** Time as the desk sees it; a test supplies its own. */
export interface Clock {
  now(): number;
  after(ms: number, run: () => void): unknown;
  cancel(timer: unknown): void;
}

/** The browser's clock. */
export const browserClock: Clock = {
  now: () => performance.now(),
  after: (ms, run) => globalThis.setTimeout(run, ms),
  cancel: (timer) => {
    globalThis.clearTimeout(timer as ReturnType<typeof setTimeout>);
  },
};

/**
 * Calls `listener` once if `signal` aborts. The returned function takes the
 * listener off again, so an answered question leaves nothing on a signal
 * that outlives it.
 */
export function onAbort(signal: AbortSignal, listener: () => void): () => void {
  signal.addEventListener('abort', listener, { once: true });
  return () => signal.removeEventListener('abort', listener);
}

/** What the page shows of the desk. */
export type OfferView =
  | {
      readonly phase: 'asking' | 'saving';
      readonly path: string | undefined;
      /** Whether a press counts now; the gate's answer when this was made. */
      readonly active: boolean;
    }
  | { readonly phase: 'failed'; readonly error: unknown };

/** A question that is open, and everything that has to end with it. */
interface Open<Receipt> {
  readonly path: string | undefined;
  readonly controller: AbortController;
  readonly resolve: (receipt: Receipt) => void;
  readonly reject: (reason: unknown) => void;
  readonly timers: { handle: unknown }[];
  detach: () => void;
  saving: boolean;
}

/**
 * The download question of one frame (decision 0032): at most one open, its
 * button active only when the {@link OfferGate} says a press counts, its
 * end in exactly one of four ways (saved, declined, withdrawn unanswered, or
 * the frame went away), each of which settles the frame's request.
 *
 * The page shows {@link OfferDesk.getView}. The timers only make the desk
 * look again at the gate: a timer that fires early is set again for the
 * rest, so what the button shows is never ahead of what a press does.
 */
export class OfferDesk<Receipt> {
  readonly #clock: Clock;
  readonly #timing: typeof OFFER_TIMING;
  readonly #gate: OfferGate;
  readonly #listeners = new Set<() => void>();
  #open: Open<Receipt> | undefined;
  #failed: { readonly error: unknown } | undefined;
  #view: OfferView | undefined;

  constructor(
    clock: Clock = browserClock,
    timing: typeof OFFER_TIMING = OFFER_TIMING,
  ) {
    this.#clock = clock;
    this.#timing = timing;
    this.#gate = new OfferGate(timing);
  }

  /** For `useSyncExternalStore`. */
  readonly subscribe = (listener: () => void): (() => void) => {
    this.#listeners.add(listener);
    return () => {
      this.#listeners.delete(listener);
    };
  };

  /** For `useSyncExternalStore`: the same object until something changes. */
  readonly getView = (): OfferView | undefined => this.#view;

  /**
   * A frame asks to download. Resolves with the receipt of the download the
   * user confirmed, and rejects with a {@link HostRefusal} otherwise. A
   * request whose signal has already aborted opens nothing and touches
   * nothing.
   */
  readonly ask = ({
    path,
    signal,
  }: {
    readonly path: string | undefined;
    readonly signal: AbortSignal;
  }): Promise<Receipt> => {
    if (signal.aborted)
      return Promise.reject(new HostRefusal('view_unavailable'));
    const now = this.#clock.now();
    const verdict = this.#gate.open(now);
    // A frame that asks again at once after a "no" is told "no" again.
    if (verdict === 'cooling')
      return Promise.reject(new HostRefusal('declined'));
    if (verdict === 'busy') return Promise.reject(new HostRefusal('busy'));
    return new Promise<Receipt>((resolve, reject) => {
      const offer: Open<Receipt> = {
        path,
        controller: new AbortController(),
        resolve,
        reject,
        timers: [],
        detach: () => undefined,
        saving: false,
      };
      this.#open = offer;
      this.#failed = undefined;
      offer.detach = onAbort(signal, () => this.#frameGone(offer));
      this.#publish();
      this.#at(offer, now + this.#timing.activationMs, () => this.#publish());
      // Unanswered for too long: the frame stops waiting before long, and a
      // press after that would save a file nobody asked for.
      this.#at(offer, now + this.#timing.lifetimeMs, () =>
        this.#withdraw(offer),
      );
    });
  };

  /**
   * The user pressed Download. A press that does not count (too early, too
   * late, or a second one) does nothing; the first that counts takes the
   * question at once, before anything is awaited, so a double click starts
   * one download. `fetch` gets the bytes, `deliver` hands them to the
   * browser; both are the caller's effects.
   */
  async confirm<Fetched>(
    fetch: (request: {
      readonly path: string | undefined;
      readonly signal: AbortSignal;
    }) => Promise<Fetched>,
    deliver: (fetched: Fetched) => Receipt,
  ): Promise<void> {
    const offer = this.#open;
    if (offer === undefined || offer.saving) return;
    if (!this.#gate.confirm(this.#clock.now())) return;
    offer.saving = true;
    this.#stopTimers(offer);
    this.#publish();
    try {
      const fetched = await fetch({
        path: offer.path,
        signal: offer.controller.signal,
      });
      // The frame went away while the bytes were on their way.
      if (this.#open !== offer) return;
      const receipt = deliver(fetched);
      this.#finish(offer);
      offer.resolve(receipt);
    } catch (error: unknown) {
      if (this.#open !== offer) return;
      this.#finish(offer);
      this.#failed = { error };
      this.#publish();
      offer.reject(error);
    }
  }

  /** The user pressed Decline. */
  decline(): void {
    const offer = this.#open;
    if (offer !== undefined && !offer.saving) this.#withdraw(offer);
  }

  /** The user dismissed the message about a download that failed. */
  dismiss(): void {
    if (this.#failed === undefined) return;
    this.#failed = undefined;
    this.#publish();
  }

  /** The frame is gone: an open question ends with it. */
  dispose(): void {
    if (this.#open !== undefined) this.#frameGone(this.#open);
  }

  /** Ends the question with a "no" and starts the pause. */
  #withdraw(offer: Open<Receipt>): void {
    if (this.#open !== offer) return;
    this.#gate.decline(this.#clock.now());
    this.#end(offer);
    offer.reject(new HostRefusal('declined'));
  }

  #frameGone(offer: Open<Receipt>): void {
    if (this.#open !== offer) return;
    this.#gate.close();
    this.#end(offer);
    offer.reject(new HostRefusal('view_unavailable'));
  }

  /** Ends a question without a download: nothing of it is left running. */
  #end(offer: Open<Receipt>): void {
    this.#open = undefined;
    this.#stopTimers(offer);
    offer.detach();
    offer.controller.abort();
    this.#publish();
  }

  /** Ends a question that was answered by a download or a failure of it. */
  #finish(offer: Open<Receipt>): void {
    this.#open = undefined;
    this.#gate.close();
    this.#stopTimers(offer);
    offer.detach();
    this.#publish();
  }

  #stopTimers(offer: Open<Receipt>): void {
    for (const timer of offer.timers) this.#clock.cancel(timer.handle);
    offer.timers.length = 0;
  }

  /** Runs `run` at `deadline` on the desk's clock, even if the timer is early. */
  #at(offer: Open<Receipt>, deadline: number, run: () => void): void {
    const slot: { handle: unknown } = { handle: undefined };
    const fire = (): void => {
      if (this.#open !== offer || offer.saving) return;
      const remaining = deadline - this.#clock.now();
      if (remaining > 0) slot.handle = this.#clock.after(remaining, fire);
      else run();
    };
    slot.handle = this.#clock.after(
      Math.max(0, deadline - this.#clock.now()),
      fire,
    );
    offer.timers.push(slot);
  }

  #publish(): void {
    const offer = this.#open;
    this.#view =
      offer !== undefined
        ? {
            phase: offer.saving ? 'saving' : 'asking',
            path: offer.path,
            active: !offer.saving && this.#gate.canConfirm(this.#clock.now()),
          }
        : this.#failed === undefined
          ? undefined
          : { phase: 'failed', error: this.#failed.error };
    for (const listener of this.#listeners) listener();
  }
}

/** How long a saved file stays addressable after the browser was handed it. */
export const SAVED_URL_LINGER_MS = 60_000;

/**
 * The address of the file just handed to the browser stays valid for a
 * while, whatever happens to the page meanwhile: the browser reads a large
 * file from it after the click, and a frame that was told `started` must not
 * find the file cut because the stage was rebuilt. A newer file replaces it
 * at once; otherwise it is revoked after {@link SAVED_URL_LINGER_MS}.
 */
export class SavedFileUrls {
  readonly #revoke: (url: string) => void;
  readonly #clock: Clock;
  readonly #lingerMs: number;
  #held: { readonly url: string; readonly timer: unknown } | undefined;

  constructor(
    revoke: (url: string) => void,
    clock: Clock = browserClock,
    lingerMs: number = SAVED_URL_LINGER_MS,
  ) {
    this.#revoke = revoke;
    this.#clock = clock;
    this.#lingerMs = lingerMs;
  }

  hold(url: string): void {
    this.#drop();
    const timer = this.#clock.after(this.#lingerMs, () => this.#drop());
    this.#held = { url, timer };
  }

  #drop(): void {
    if (this.#held === undefined) return;
    const { url, timer } = this.#held;
    this.#held = undefined;
    this.#clock.cancel(timer);
    this.#revoke(url);
  }
}
