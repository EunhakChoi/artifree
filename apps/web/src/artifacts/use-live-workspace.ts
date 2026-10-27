import {
  useEffect,
  useEffectEvent,
  useRef,
  useState,
  useSyncExternalStore,
} from 'react';
import { cursorOf } from '@artifree/client';
import type { ArtifreeClient, ChangeFeedEvent } from '@artifree/client';

/** Whether commits made elsewhere reach this page. */
export type FeedState =
  'connecting' | 'live' | 'reconnecting' | 'paused' | 'stopped';

/** A revision this page knows an artifact has. */
export interface KnownRevision {
  readonly id: string;
  readonly revision: number;
}

/** What the page may read and what it knows, from the change feed. */
export interface LiveWorkspace {
  readonly feed: FeedState;
  /** False until the page may read for the first time. */
  readonly canRead: boolean;
  /** Advances whenever everything must be read again. */
  readonly epoch: number;
  /** Advances for each revision newer than the page knew. */
  readonly news: number;
  /** The newest revision known per artifact id. */
  readonly known: Readonly<Record<string, number>>;
  /**
   * Records revisions the page read or saved itself, which are not news.
   * It may be called while rendering.
   */
  readonly learn: (revisions: readonly KnownRevision[]) => void;
}

/** How long a visible page waits for the stream before its first read. */
const FIRST_READ_WAIT_MS = 2000;

interface Announced {
  readonly epoch: number;
  readonly news: number;
  readonly known: Readonly<Record<string, number>>;
}

/** `announced` with the revisions in `revisions` that are newer than known. */
function withKnown(
  announced: Announced,
  revisions: readonly KnownRevision[],
  news: boolean,
): Announced {
  const newer = revisions.filter(
    ({ id, revision }) => revision > (announced.known[id] ?? 0),
  );
  if (newer.length === 0) return announced;
  const known = { ...announced.known };
  for (const { id, revision } of newer) known[id] = revision;
  return { ...announced, known, news: announced.news + (news ? 1 : 0) };
}

function subscribeVisibility(onChange: () => void): () => void {
  document.addEventListener('visibilitychange', onChange);
  return () => document.removeEventListener('visibilitychange', onChange);
}

function pageVisible(): boolean {
  return document.visibilityState !== 'hidden';
}

/**
 * The read policy of a connected workspace (decision 0007). While the page
 * is visible it follows the change feed, resuming from its last cursor; a
 * hidden page gives up its stream and starts no reads. The first read waits
 * for the stream's `ready`, at most two seconds of visibility, unless the
 * stream already failed. `ready` and `reset` advance `epoch` and forget
 * known revisions; a change newer than known advances `news`.
 */
export function useLiveWorkspace(client: ArtifreeClient): LiveWorkspace {
  const visible = useSyncExternalStore(subscribeVisibility, pageVisible);
  const [announced, setAnnounced] = useState<Announced>({
    epoch: 0,
    news: 0,
    known: {},
  });
  const feed = useChangeFeed(client, visible, (event) => {
    if (event.type === 'change') {
      const { artifact_id: id, revision } = event.change;
      setAnnounced((value) => withKnown(value, [{ id, revision }], true));
    } else
      setAnnounced((value) => ({
        epoch: value.epoch + 1,
        news: value.news,
        known: {},
      }));
  });

  const [waited, setWaited] = useState(false);
  useEffect(() => {
    if (!visible || waited) return undefined;
    const timer = setTimeout(() => setWaited(true), FIRST_READ_WAIT_MS);
    return () => clearTimeout(timer);
  }, [visible, waited]);
  // Once the page may read it keeps reading; later reads follow notices,
  // which stop while it is hidden. A hidden page's feed is paused and its
  // wait does not run, so it cannot start reading.
  const [canRead, setCanRead] = useState(false);
  if (
    !canRead &&
    (announced.epoch > 0 ||
      waited ||
      feed === 'reconnecting' ||
      feed === 'stopped')
  )
    setCanRead(true);

  function learn(revisions: readonly KnownRevision[]): void {
    if (withKnown(announced, revisions, false) !== announced)
      setAnnounced((value) => withKnown(value, revisions, false));
  }
  return { feed, canRead, ...announced, learn };
}

/**
 * Follows the change feed while `active`, handing each event to `onEvent`.
 * Becoming inactive aborts the stream; becoming active again resumes after
 * the last cursor. A new `client` starts over; the old subscription
 * delivers nothing more. A failure that retrying cannot fix stops
 * following for good.
 */
function useChangeFeed(
  client: ArtifreeClient,
  active: boolean,
  onEvent: (event: ChangeFeedEvent) => void,
): FeedState {
  const [state, setState] = useState<'connecting' | 'live' | 'reconnecting'>(
    'connecting',
  );
  const [stopped, setStopped] = useState(false);
  // A stream that starts again is connecting until the server accepts it.
  const [wasActive, setWasActive] = useState(active);
  if (wasActive !== active) {
    setWasActive(active);
    if (active) setState('connecting');
  }
  const position = useRef<{ client: ArtifreeClient; cursor?: string }>({
    client,
  });
  const deliver = useEffectEvent(onEvent);
  useEffect(() => {
    if (!active || stopped) return undefined;
    if (position.current.client !== client) position.current = { client };
    const { cursor } = position.current;
    const controller = new AbortController();
    const { signal } = controller;
    client
      .followChanges({
        signal,
        ...(cursor === undefined ? {} : { after: cursor }),
        onConnect() {
          if (!signal.aborted) setState('live');
        },
        onEvent(event) {
          if (signal.aborted) return;
          position.current = { client, cursor: cursorOf(event) };
          deliver(event);
        },
        onDisconnect() {
          if (!signal.aborted) setState('reconnecting');
        },
      })
      // A failure retrying cannot fix, such as a rejected token, which the
      // page's other requests report as well.
      .catch(() => {
        if (!signal.aborted) setStopped(true);
      });
    return () => controller.abort();
  }, [client, active, stopped]);
  if (stopped) return 'stopped';
  return active ? state : 'paused';
}
