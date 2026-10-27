import { useEffect, useEffectEvent, useState } from 'react';
import { cursorOf } from '@artifree/client';
import type { ArtifreeClient, ChangeFeedEvent } from '@artifree/client';

/** Whether commits made elsewhere reach this page. */
export type FeedStatus =
  | { readonly state: 'connecting' }
  | { readonly state: 'live' }
  | { readonly state: 'reconnecting' }
  | { readonly state: 'paused' }
  | { readonly state: 'stopped' };

/**
 * Follows the server's change feed while mounted and hands each event to
 * `onEvent`, resuming from the last cursor after a lost connection. While
 * the page is hidden it pauses, freeing its connection, and resumes from
 * the last cursor when shown, so nothing is missed. A new `client` replaces
 * the subscription; the old one delivers nothing more.
 */
export function useChangeFeed(
  client: ArtifreeClient,
  onEvent: (event: ChangeFeedEvent) => void,
): FeedStatus {
  const [status, setStatus] = useState<FeedStatus>(() =>
    document.visibilityState === 'hidden'
      ? { state: 'paused' }
      : { state: 'connecting' },
  );
  const deliver = useEffectEvent(onEvent);
  useEffect(() => {
    let cursor: string | undefined;
    let following: AbortController | undefined;
    let stopped = false;
    function start(): void {
      if (following !== undefined || stopped) return;
      const controller = new AbortController();
      following = controller;
      const { signal } = controller;
      setStatus({ state: 'connecting' });
      client
        .followChanges({
          signal,
          ...(cursor === undefined ? {} : { after: cursor }),
          onConnect() {
            if (!signal.aborted) setStatus({ state: 'live' });
          },
          onEvent(event) {
            if (signal.aborted) return;
            cursor = cursorOf(event);
            deliver(event);
          },
          onDisconnect() {
            if (!signal.aborted) setStatus({ state: 'reconnecting' });
          },
        })
        // A failure retrying cannot fix, such as a rejected token, which the
        // page's other requests report as well.
        .catch(() => {
          if (signal.aborted) return;
          stopped = true;
          setStatus({ state: 'stopped' });
        });
    }
    function pause(): void {
      following?.abort();
      following = undefined;
      if (!stopped) setStatus({ state: 'paused' });
    }
    // Each open stream holds one of the few connections a browser allows per
    // server, shared by all its tabs; hidden tabs give theirs up.
    function onVisibility(): void {
      if (document.visibilityState === 'hidden') pause();
      else start();
    }
    if (document.visibilityState !== 'hidden') start();
    document.addEventListener('visibilitychange', onVisibility);
    return () => {
      document.removeEventListener('visibilitychange', onVisibility);
      following?.abort();
    };
  }, [client]);
  return status;
}
