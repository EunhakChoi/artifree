import { useEffect, useEffectEvent, useRef, useState } from 'react';
import { ArtifreeApiError } from '@artifree/client';

/** The state of one asynchronous read. */
export type RequestState<T> =
  | { readonly status: 'loading' }
  | {
      readonly status: 'ready';
      readonly value: T;
      /** With `keepPrevious`, the value is an earlier read; a newer one is pending. */
      readonly refreshing?: true;
    }
  | {
      readonly status: 'error';
      readonly message: string;
      /** With `keepPrevious`, the last value read before this failure. */
      readonly previous?: T;
    };

/** A readable message for a failed API call. */
export function describeFailure(error: unknown): string {
  if (error instanceof ArtifreeApiError) {
    if (error.code === 'unauthenticated')
      return 'The access token was rejected. Reconnect with the token from the server data directory.';
    if (error.code === 'network_error')
      return 'The Artifree server is not reachable. Start artifree-server serve and try again.';
    return error.message;
  }
  return error instanceof Error ? error.message : 'Unexpected failure.';
}

/** The longest a read waits for the one before it in the same view. */
const MAX_WAIT_MS = 10_000;

/** How long to wait before reading again, when the server said so. */
function rateLimitDelay(error: unknown): number | undefined {
  if (!(error instanceof ArtifreeApiError) || error.status !== 429)
    return undefined;
  return (error.retryAfterSeconds ?? 1) * 1000;
}

/**
 * Runs `load` whenever `key` changes and ignores results that arrive after a
 * newer request started. `key` must identify every input `load` reads;
 * without a key nothing is read yet and the state is loading. With
 * `keepPrevious`, a ready value stays visible while its replacement loads,
 * and a failure carries it as `previous`; use it only where a briefly stale
 * value cannot mislead.
 *
 * Each hook sends one request at a time, so changes that arrive in a burst
 * share the actor's request budget: a newer key waits for the request in
 * flight, at most ten seconds, and keys replaced meanwhile are never read.
 * A `429` is not processed by the server, so the same request is sent again
 * after its `Retry-After` delay.
 */
export function useRequest<T>(
  key: string | undefined,
  load: () => Promise<T>,
  options: { keepPrevious?: boolean } = {},
): RequestState<T> {
  const [state, setState] = useState<{
    key: string | undefined;
    result: RequestState<T>;
    last?: T;
  }>({ key: undefined, result: { status: 'loading' } });
  const start = useEffectEvent(load);
  const inFlight = useRef<Promise<void>>(Promise.resolve());
  useEffect(() => {
    if (key === undefined) return undefined;
    let current = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const run = (): Promise<void> =>
      start().then(
        (value) => {
          if (current)
            setState({ key, result: { status: 'ready', value }, last: value });
        },
        (error: unknown) => {
          if (!current) return;
          const delay = rateLimitDelay(error);
          if (delay !== undefined) {
            timer = setTimeout(() => {
              inFlight.current = run();
            }, delay);
            return;
          }
          setState((previous) => ({
            ...previous,
            key,
            result: { status: 'error', message: describeFailure(error) },
          }));
        },
      );
    // A request that never settles must not block this view for good.
    const previous = Promise.race([
      inFlight.current,
      new Promise<void>((resolve) => {
        setTimeout(resolve, MAX_WAIT_MS);
      }),
    ]);
    inFlight.current = previous.then(() => (current ? run() : undefined));
    return () => {
      current = false;
      clearTimeout(timer);
    };
  }, [key]);
  const keep = options.keepPrevious === true;
  if (key !== undefined && state.key === key) {
    if (keep && state.result.status === 'error' && state.last !== undefined)
      return { ...state.result, previous: state.last };
    return state.result;
  }
  if (keep && state.last !== undefined)
    return { status: 'ready', value: state.last, refreshing: true };
  return { status: 'loading' };
}
