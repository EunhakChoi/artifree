import { useEffect, useEffectEvent, useRef, useState } from 'react';
import { ArtifreeApiError } from '@artifree/client';

/** The state of one asynchronous read. */
export type RequestState<T> =
  | { readonly status: 'loading' }
  | {
      readonly status: 'ready';
      readonly value: T;
      /** Set when `value` is an earlier read kept while a newer one is pending. */
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

/** The longest a read waits for the previous read of the same view. */
const QUEUE_WAIT_MS = 10_000;

/** The delay before sending a read again that the server refused for now. */
function retryDelayMs(error: unknown): number | undefined {
  if (!(error instanceof ArtifreeApiError) || error.status !== 429)
    return undefined;
  return (error.retryAfterSeconds ?? 1) * 1000;
}

/**
 * Runs `load` whenever `key` changes and ignores results of reads that a
 * newer key replaced. `key` must identify every input `load` reads; while
 * it is undefined nothing is read. With `keepPrevious`, the last value read
 * stays visible while a newer read is pending (`refreshing`) and after one
 * fails (`previous`); use it only where a briefly stale value cannot
 * mislead.
 *
 * One view sends one read at a time, so a burst of key changes costs the
 * actor's request budget once: a new read waits for the one in flight, at
 * most ten seconds, and a key replaced while waiting is never read. A `429`
 * was not processed, so the same read is sent again after `Retry-After`.
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
  const queue = useRef<Promise<void>>(Promise.resolve());
  useEffect(() => {
    if (key === undefined) return undefined;
    let wanted = true;
    let retry: ReturnType<typeof setTimeout> | undefined;
    const read = (): Promise<void> =>
      start().then(
        (value) => {
          if (wanted)
            setState({ key, result: { status: 'ready', value }, last: value });
        },
        (error: unknown) => {
          if (!wanted) return;
          const delay = retryDelayMs(error);
          if (delay === undefined)
            setState((previous) => ({
              ...previous,
              key,
              result: { status: 'error', message: describeFailure(error) },
            }));
          else
            retry = setTimeout(() => {
              queue.current = read();
            }, delay);
        },
      );
    const turn = Promise.race([
      queue.current,
      new Promise<void>((resolve) => {
        setTimeout(resolve, QUEUE_WAIT_MS);
      }),
    ]);
    queue.current = turn.then(() => (wanted ? read() : undefined));
    return () => {
      wanted = false;
      clearTimeout(retry);
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
