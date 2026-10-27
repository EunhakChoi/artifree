import { useEffect, useEffectEvent, useRef, useState } from 'react';
import { ArtifreeApiError } from '@artifree/client';
import { pageVisible, usePageVisible } from './use-page-visible';

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

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => {
    setTimeout(resolve, ms);
  });
}

/** Settles when `previous` does, or after `ms`, whichever is first. */
function within(previous: Promise<void>, ms: number): Promise<void> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  return Promise.race([
    previous,
    new Promise<void>((resolve) => {
      timer = setTimeout(resolve, ms);
    }),
  ]).finally(() => clearTimeout(timer));
}

/** The reads of one view, in the order they were asked for. */
export type ReadQueue = { current: Promise<void> };

/**
 * Runs `load` whenever `key` changes and ignores results of reads that a
 * newer key replaced. `key` must identify every input `load` reads; while
 * it is undefined nothing is read. With `keepPrevious`, the last value read
 * stays visible while a newer read is pending (`refreshing`) and after one
 * fails (`previous`); use it only where a briefly stale value cannot
 * mislead.
 *
 * Reads follow the page's request budget (decision 0007). While the page
 * is hidden no read starts; the latest key is read when it is shown. One
 * view sends one read at a time: a new read waits for the one before it,
 * at most ten seconds, and a key replaced while waiting is never read. A
 * `429` was not processed, so the read is sent again after `Retry-After`,
 * and the view's next read waits for that too. `queue` shares the order
 * across components that replace each other, such as an editor remounted
 * for another revision.
 */
export function useRequest<T>(
  key: string | undefined,
  load: () => Promise<T>,
  options: { keepPrevious?: boolean; queue?: ReadQueue } = {},
): RequestState<T> {
  // The key being read: the latest one while the page is visible; while it
  // is hidden, the last one asked for while it was visible.
  const visible = usePageVisible();
  const [asked, setAsked] = useState(() => (pageVisible() ? key : undefined));
  if (visible && asked !== key) setAsked(key);
  const current = visible ? key : asked;
  const [state, setState] = useState<{
    key: string | undefined;
    result: RequestState<T>;
    last?: T;
  }>({ key: undefined, result: { status: 'loading' } });
  const start = useEffectEvent(load);
  const ownQueue = useRef<Promise<void>>(Promise.resolve());
  const queue = options.queue ?? ownQueue;
  useEffect(() => {
    if (current === undefined) return undefined;
    let wanted = true;
    const read = (): Promise<void> =>
      start().then(
        (value) => {
          if (wanted)
            setState({
              key: current,
              result: { status: 'ready', value },
              last: value,
            });
        },
        (error: unknown) => {
          const wait = retryDelayMs(error);
          // Kept in the queue, so that no read of this view goes out
          // before the server's delay has passed.
          if (wait !== undefined)
            return delay(wait).then(() => (wanted ? read() : undefined));
          if (wanted)
            setState((previous) => ({
              ...previous,
              key: current,
              result: { status: 'error', message: describeFailure(error) },
            }));
          return undefined;
        },
      );
    queue.current = within(queue.current, QUEUE_WAIT_MS).then(() =>
      wanted ? read() : undefined,
    );
    return () => {
      wanted = false;
    };
  }, [current, queue]);
  const keep = options.keepPrevious === true;
  if (current !== undefined && state.key === current) {
    if (keep && state.result.status === 'error' && state.last !== undefined)
      return { ...state.result, previous: state.last };
    return state.result;
  }
  if (keep && state.last !== undefined)
    return { status: 'ready', value: state.last, refreshing: true };
  return { status: 'loading' };
}
