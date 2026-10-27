import { useEffect, useEffectEvent, useState } from 'react';
import { ArtifreeApiError } from '@artifree/client';

/**
 * The state of one asynchronous read. `refreshing` marks an earlier value
 * shown while its replacement loads; `previous` is the last value read
 * before a failure, which a view may keep showing.
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

interface Settled<T> {
  readonly key: string;
  readonly result: RequestState<T>;
  /** The last value read, for any key. */
  readonly last: T | undefined;
}

/**
 * Reads with `load` whenever `key` changes; an undefined key reads nothing.
 * `key` must identify every input `load` reads. A read whose key was
 * replaced, or whose view went away, is aborted through the signal given
 * to `load`, and its result is ignored. With `keepPrevious`, the last value
 * stays visible, marked `refreshing`, while a new key is read; use it only
 * where a briefly stale value cannot mislead.
 */
export function useRequest<T>(
  key: string | undefined,
  load: (signal: AbortSignal) => Promise<T>,
  options: { readonly keepPrevious?: boolean } = {},
): RequestState<T> {
  const [settled, setSettled] = useState<Settled<T>>();
  const start = useEffectEvent(load);
  useEffect(() => {
    if (key === undefined) return undefined;
    const controller = new AbortController();
    const { signal } = controller;
    start(signal).then(
      (value) => {
        if (!signal.aborted)
          setSettled({
            key,
            result: { status: 'ready', value, refreshing: false },
            last: value,
          });
      },
      (error: unknown) => {
        if (signal.aborted) return;
        const message = describeFailure(error);
        setSettled((previous) => ({
          key,
          result:
            previous?.last === undefined
              ? { status: 'error', message }
              : { status: 'error', message, previous: previous.last },
          last: previous?.last,
        }));
      },
    );
    return () => controller.abort();
  }, [key]);
  if (settled !== undefined && settled.key === key) return settled.result;
  if (options.keepPrevious === true && settled?.last !== undefined)
    return { status: 'ready', value: settled.last, refreshing: true };
  return { status: 'loading' };
}
