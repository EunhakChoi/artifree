import type { Clock } from './transport.js';

/** Most seconds a server's `Retry-After` can make a client wait. */
export const MAX_RETRY_AFTER_SECONDS = 3600;

/**
 * How long a connection stays open, after it was accepted, before the next
 * failure counts as the first again. A fixed length, not the longest delay.
 */
export const RETRY_RESET_MS = 30_000;

/** Reconnection delays in milliseconds: the first, and the longest. */
export interface RetryDelays {
  readonly min: number;
  readonly max: number;
}

/** The delays used when a caller gives none. */
export const DEFAULT_RETRY_DELAYS: RetryDelays = { min: 500, max: 30_000 };

/**
 * The wait before retry number `attempt` (0 for the first since the last
 * reset), in milliseconds. The base doubles from `min` up to `max`; the wait
 * is a random point of its upper half, [base/2, base), so that many clients
 * that lost one server do not return together. A server's `Retry-After` is a
 * floor, at most {@link MAX_RETRY_AFTER_SECONDS}. `random` yields [0, 1).
 */
export function retryDelayMs(
  attempt: number,
  { min, max }: RetryDelays,
  random: () => number,
  retryAfterSeconds?: number,
): number {
  // Past 2^64 the base is `max` anyway; this keeps `0 * Infinity` out.
  const base = Math.min(max, min * 2 ** Math.min(attempt, 64));
  const jittered = base * (0.5 + 0.5 * random());
  if (retryAfterSeconds === undefined) return jittered;
  const floor = Math.min(retryAfterSeconds, MAX_RETRY_AFTER_SECONDS) * 1000;
  return Math.max(floor, jittered);
}

/** Waits `milliseconds`, or less when `signal` aborts or has aborted. */
export function wait(
  clock: Clock,
  milliseconds: number,
  signal: AbortSignal,
): Promise<void> {
  return new Promise((resolve) => {
    if (signal.aborted) {
      resolve();
      return;
    }
    // A clock may run `done` before `after` returns, or after the abort.
    let cancel: (() => void) | undefined;
    let finished = false;
    const done = (): void => {
      if (finished) return;
      finished = true;
      cancel?.();
      signal.removeEventListener('abort', done);
      resolve();
    };
    signal.addEventListener('abort', done);
    cancel = clock.after(milliseconds, done);
    // The clock ran `done` before it returned what cancels it.
    if (finished) cancel();
  });
}
