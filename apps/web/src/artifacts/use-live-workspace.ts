import { useEffect, useEffectEvent, useReducer, useRef, useState } from 'react';
import { ArtifreeApiError, cursorOf } from '@artifree/client';
import type { ArtifreeClient, ChangeFeedEvent } from '@artifree/client';
import { usePageVisible } from './page-visibility';
import { useReadScheduler } from './use-request';

/** Whether commits made elsewhere reach this page. */
export type FeedState =
  'connecting' | 'live' | 'reconnecting' | 'paused' | 'stopped';

/** A revision this page knows an artifact has. */
export interface KnownRevision {
  readonly id: string;
  readonly revision: number;
}

/** What the page knows from the change feed, and when it may read. */
export interface LiveWorkspace {
  readonly feed: FeedState;
  /** False until the page may read for the first time. */
  readonly canRead: boolean;
  /**
   * Advances whenever everything must be read again, file content and
   * previews included.
   */
  readonly epoch: number;
  /**
   * Advances when the server's data may have been replaced since the page
   * read it, so a revision number read before may now name other content.
   */
  readonly resets: number;
  /** Advances for each revision newer than the page knew. */
  readonly news: number;
  /** The newest revision known per artifact id, in this epoch. */
  readonly known: Readonly<Record<string, number>>;
  /**
   * Records revisions the page read or saved itself, which are not news.
   * It may be called while rendering.
   */
  readonly learn: (revisions: readonly KnownRevision[]) => void;
}

/** How long a visible page waits for the stream before its first read. */
const FIRST_READ_WAIT_MS = 2000;

interface Knowledge {
  readonly canRead: boolean;
  readonly epoch: number;
  readonly resets: number;
  readonly news: number;
  readonly known: Readonly<Record<string, number>>;
}

type Learned =
  | { readonly type: 'event'; readonly event: ChangeFeedEvent }
  | { readonly type: 'read'; readonly revisions: readonly KnownRevision[] }
  // The stream did not start in time, or cannot start now.
  | { readonly type: 'read anyway' };

const START: Knowledge = {
  canRead: false,
  epoch: 0,
  resets: 0,
  news: 0,
  known: {},
};

/** `state` with the revisions newer than it knows; the same object if none. */
function withRevisions(
  state: Knowledge,
  revisions: readonly KnownRevision[],
  news: boolean,
): Knowledge {
  const newer = revisions.filter(
    ({ id, revision }) => revision > (state.known[id] ?? 0),
  );
  if (newer.length === 0) return state;
  const known = { ...state.known };
  for (const { id, revision } of newer) known[id] = revision;
  return { ...state, known, news: state.news + (news ? 1 : 0) };
}

function learnFrom(state: Knowledge, learned: Learned): Knowledge {
  switch (learned.type) {
    case 'read':
      return withRevisions(state, learned.revisions, false);
    case 'read anyway':
      return state.canRead ? state : { ...state, canRead: true };
    case 'event': {
      const { event } = learned;
      if (event.type === 'change')
        return withRevisions(
          state,
          [{ id: event.change.artifact_id, revision: event.change.revision }],
          true,
        );
      // `ready` and `reset` drop what earlier reads established. A `ready`
      // after the page already read: nothing ties those reads to the feed,
      // so the data they showed may have been replaced as well.
      const replaced = event.type === 'reset' || state.canRead;
      return {
        canRead: true,
        epoch: state.epoch + 1,
        resets: state.resets + (replaced ? 1 : 0),
        news: state.news,
        known: {},
      };
    }
  }
}

/**
 * The read policy of a connected workspace (decision 0007). While the page
 * is visible it follows the change feed, resuming from its last cursor; a
 * hidden page gives up its stream. The first read waits for the stream's
 * `ready`, at most two seconds of visibility, and not at all once the
 * stream has failed. A stream refused with `429` holds the page's reads for
 * its `Retry-After`, as a refused read does: the limit is the actor's.
 */
export function useLiveWorkspace(client: ArtifreeClient): LiveWorkspace {
  const visible = usePageVisible();
  const scheduler = useReadScheduler();
  const [state, learn] = useReducer(learnFrom, START);
  const feed = useChangeFeed(client, visible, {
    onEvent: (event) => learn({ type: 'event', event }),
    onTrouble: (error) => {
      if (error?.code === 'rate_limited')
        scheduler.holdFor(error.retryAfterSeconds ?? 1);
      learn({ type: 'read anyway' });
    },
  });
  // Counts visible time only: a background page has no reason to read.
  const { canRead } = state;
  useEffect(() => {
    if (!visible || canRead) return undefined;
    const timer = setTimeout(
      () => learn({ type: 'read anyway' }),
      FIRST_READ_WAIT_MS,
    );
    return () => clearTimeout(timer);
  }, [visible, canRead]);
  return {
    feed,
    ...state,
    learn(revisions) {
      if (withRevisions(state, revisions, false) !== state)
        learn({ type: 'read', revisions });
    },
  };
}

interface FeedHandlers {
  readonly onEvent: (event: ChangeFeedEvent) => void;
  /**
   * The stream was lost or refused, with the server's answer if it gave
   * one; it may or may not come back.
   */
  readonly onTrouble: (error: ArtifreeApiError | undefined) => void;
}

/**
 * Follows the change feed while `active`. Becoming inactive aborts the
 * stream; becoming active again resumes after the last cursor. Another
 * `client` starts over, and the old subscription delivers nothing more. A
 * failure that retrying cannot fix, such as a rejected token, stops
 * following until the page connects again.
 */
function useChangeFeed(
  client: ArtifreeClient,
  active: boolean,
  handlers: FeedHandlers,
): FeedState {
  const [connection, setConnection] = useState<
    'connecting' | 'live' | 'reconnecting' | 'stopped'
  >('connecting');
  // A resumed stream is connecting again until the server accepts it.
  const [wasActive, setWasActive] = useState(active);
  if (wasActive !== active) {
    setWasActive(active);
    if (active && connection !== 'stopped') setConnection('connecting');
  }
  const resumeAfter = useRef<{ client: ArtifreeClient; cursor: string }>(
    undefined,
  );
  const onEvent = useEffectEvent(handlers.onEvent);
  const onTrouble = useEffectEvent(handlers.onTrouble);
  const stopped = connection === 'stopped';
  useEffect(() => {
    if (!active || stopped) return undefined;
    const after =
      resumeAfter.current?.client === client
        ? resumeAfter.current.cursor
        : undefined;
    const controller = new AbortController();
    const { signal } = controller;
    client
      .followChanges({
        signal,
        ...(after === undefined ? {} : { after }),
        onConnect() {
          if (!signal.aborted) setConnection('live');
        },
        onEvent(event) {
          if (signal.aborted) return;
          resumeAfter.current = { client, cursor: cursorOf(event) };
          onEvent(event);
        },
        onDisconnect(error) {
          if (signal.aborted) return;
          setConnection('reconnecting');
          onTrouble(error);
        },
      })
      .catch((error: unknown) => {
        if (signal.aborted) return;
        setConnection('stopped');
        onTrouble(error instanceof ArtifreeApiError ? error : undefined);
      });
    return () => controller.abort();
  }, [client, active, stopped]);
  if (stopped) return 'stopped';
  return active ? connection : 'paused';
}
