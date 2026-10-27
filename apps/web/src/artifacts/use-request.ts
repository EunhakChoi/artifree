import { useEffect, useEffectEvent, useState } from 'react';
import { ArtifreeApiError } from '@artifree/client';

/** The state of one asynchronous read. */
export type RequestState<T> =
  | { readonly status: 'loading' }
  | { readonly status: 'ready'; readonly value: T }
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

/**
 * Runs `load` whenever `key` changes and ignores results that arrive after a
 * newer request started. `key` must identify every input `load` reads;
 * without a key nothing is read yet and the state is loading. With
 * `keepPrevious`, a ready value stays visible while its replacement loads,
 * and a failure carries it as `previous`; use it only where a briefly stale
 * value cannot mislead.
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
  useEffect(() => {
    if (key === undefined) return undefined;
    let current = true;
    start().then(
      (value) => {
        if (current)
          setState({ key, result: { status: 'ready', value }, last: value });
      },
      (error: unknown) => {
        if (current)
          setState((previous) => ({
            ...previous,
            key,
            result: { status: 'error', message: describeFailure(error) },
          }));
      },
    );
    return () => {
      current = false;
    };
  }, [key]);
  const keep = options.keepPrevious === true;
  if (key !== undefined && state.key === key) {
    if (keep && state.result.status === 'error' && state.last !== undefined)
      return { ...state.result, previous: state.last };
    return state.result;
  }
  if (keep && state.last !== undefined)
    return { status: 'ready', value: state.last };
  return { status: 'loading' };
}
