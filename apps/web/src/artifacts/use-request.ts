import {
  createContext,
  useContext,
  useEffect,
  useEffectEvent,
  useState,
} from 'react';
import { ArtifreeApiError } from '@artifree/client';
import type { ReadScheduler } from './read-scheduler';

/**
 * The state of one read. `ready` with `refreshing` is a value read for an
 * earlier key, kept while the read for the current key waits or runs. An
 * error keeps the last value read, if any, as `previous`.
 */
export type RequestState<T> =
  | { readonly status: 'loading' }
  | {
      readonly status: 'ready';
      readonly value: T;
      readonly refreshing: boolean;
    }
  | {
      readonly status: 'error';
      readonly message: string;
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

/** The scheduler every read of the workspace goes through. */
export const ReadSchedulerContext = createContext<ReadScheduler | undefined>(
  undefined,
);

/** The page's read scheduler. */
export function useReadScheduler(): ReadScheduler {
  const scheduler = useContext(ReadSchedulerContext);
  if (scheduler === undefined)
    throw new Error('Reads need a ReadSchedulerContext provider.');
  return scheduler;
}

interface Settled<T> {
  readonly key: string;
  readonly result: RequestState<T>;
  readonly last: T | undefined;
}

/**
 * Reads with `load` whenever `key` changes, through the page's
 * {@link ReadScheduler}; an undefined key reads nothing. `key` must
 * identify every input `load` reads. A result for a replaced key is
 * ignored. A read answered `429` holds the page's reads for `Retry-After`
 * and is then sent again, while the state stays as it was. With
 * `keepPrevious`, the last value stays visible, marked `refreshing`, while
 * a new key is read; use it only where a briefly stale value cannot mislead.
 */
export function useRequest<T>(
  key: string | undefined,
  load: () => Promise<T>,
  options: { readonly keepPrevious?: boolean } = {},
): RequestState<T> {
  const scheduler = useReadScheduler();
  const [settled, setSettled] = useState<Settled<T>>();
  const start = useEffectEvent(load);
  useEffect(() => {
    if (key === undefined) return undefined;
    let wanted = true;
    let cancel = (): void => {};
    const read = async (): Promise<void> => {
      try {
        const value = await start();
        if (wanted)
          setSettled({
            key,
            result: { status: 'ready', value, refreshing: false },
            last: value,
          });
      } catch (error: unknown) {
        if (!wanted) return;
        if (
          error instanceof ArtifreeApiError &&
          error.code === 'rate_limited'
        ) {
          // This attempt was not processed; nothing else changes.
          scheduler.holdFor(error.retryAfterSeconds ?? 1);
          cancel = scheduler.request(read);
          return;
        }
        const message = describeFailure(error);
        setSettled((previous) => ({
          key,
          result:
            previous?.last === undefined
              ? { status: 'error', message }
              : { status: 'error', message, previous: previous.last },
          last: previous?.last,
        }));
      }
    };
    cancel = scheduler.request(read);
    return () => {
      wanted = false;
      cancel();
    };
  }, [key, scheduler]);
  if (key !== undefined && settled?.key === key) return settled.result;
  if (options.keepPrevious === true && settled?.last !== undefined)
    return { status: 'ready', value: settled.last, refreshing: true };
  return { status: 'loading' };
}
