import { useEffect, useEffectEvent, useState } from 'react';
import { ArtifreeApiError } from '@artifree/client';

/** The state of one asynchronous read. */
export type RequestState<T> =
  | { readonly status: 'loading' }
  | { readonly status: 'ready'; readonly value: T }
  | { readonly status: 'error'; readonly message: string };

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
 * newer request started. `key` must identify every input `load` reads.
 * With `keepPrevious`, a ready value stays visible while its replacement
 * loads; use it only where a briefly stale value cannot mislead.
 */
export function useRequest<T>(
  key: string,
  load: () => Promise<T>,
  options: { keepPrevious?: boolean } = {},
): RequestState<T> {
  const [state, setState] = useState<{
    key: string;
    result: RequestState<T>;
  }>({ key, result: { status: 'loading' } });
  const start = useEffectEvent(load);
  useEffect(() => {
    let current = true;
    start().then(
      (value) => {
        if (current) setState({ key, result: { status: 'ready', value } });
      },
      (error: unknown) => {
        if (current)
          setState({
            key,
            result: { status: 'error', message: describeFailure(error) },
          });
      },
    );
    return () => {
      current = false;
    };
  }, [key]);
  if (state.key === key) return state.result;
  if (options.keepPrevious === true && state.result.status === 'ready')
    return state.result;
  return { status: 'loading' };
}
