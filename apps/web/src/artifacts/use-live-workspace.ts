import {
  useCallback,
  useEffect,
  useEffectEvent,
  useReducer,
  useRef,
  useState,
} from 'react';
import { cursorOf } from '@artifree/client';
import type { ArtifreeClient, ChangeFeedEvent } from '@artifree/client';
import type { Artifact } from '@artifree/contracts';
import { usePageVisible } from './page-visibility';

/** Whether commits made elsewhere reach this page. */
export type FeedState =
  'connecting' | 'live' | 'reconnecting' | 'paused' | 'stopped';

/**
 * How far an artifact has changed: its content revision and its metadata
 * revision, which advance independently (decision 0008).
 */
export interface Version {
  readonly revision: number;
  readonly metadata: number;
}

/** Whether `version` has a change that `than` does not. */
export function isNewer(version: Version, than: Version | undefined): boolean {
  return (
    than === undefined ||
    version.revision > than.revision ||
    version.metadata > than.metadata
  );
}

/** The version an artifact's metadata names. */
export function versionOf(artifact: Artifact): Version {
  return {
    revision: artifact.current_revision,
    metadata: artifact.metadata_revision,
  };
}

function latest(a: Version | undefined, b: Version): Version {
  if (a === undefined) return b;
  return {
    revision: Math.max(a.revision, b.revision),
    metadata: Math.max(a.metadata, b.metadata),
  };
}

/** An artifact's version as a list or a save reported it. */
export interface ListedVersion {
  readonly id: string;
  readonly version: Version;
}

/** What the page knows from the change feed. */
interface Knowledge {
  /** False until the page may read for the first time. */
  readonly canRead: boolean;
  /**
   * Advances on each `ready` and `reset`. Reads made in an earlier epoch
   * are not tied to the feed and are made again.
   */
  readonly epoch: number;
  /**
   * Advances when the server's data may have been replaced since the page
   * read it: a `reset`, or a `ready` after the page read without a stream.
   */
  readonly resets: number;
  /** Advances on each change the page did not know of. */
  readonly news: number;
  /** The newest version known per artifact, in this epoch. */
  readonly known: Readonly<Record<string, Version>>;
  /** Changes of a kind this client does not know, per artifact. */
  readonly unknown: Readonly<Record<string, number>>;
}

type Learned =
  | { readonly type: 'event'; readonly event: ChangeFeedEvent }
  | {
      readonly type: 'listed';
      readonly epoch: number;
      readonly versions: readonly ListedVersion[];
    }
  | { readonly type: 'read anyway' };

const START: Knowledge = {
  canRead: false,
  epoch: 0,
  resets: 0,
  news: 0,
  known: {},
  unknown: {},
};

/** How long a visible page waits for the stream before reading anyway. */
const FIRST_READ_WAIT_MS = 2_000;

function learnFrom(state: Knowledge, learned: Learned): Knowledge {
  switch (learned.type) {
    case 'read anyway':
      return state.canRead ? state : { ...state, canRead: true };
    case 'listed': {
      if (learned.epoch !== state.epoch) return state;
      let known = state.known;
      for (const { id, version } of learned.versions)
        if (isNewer(version, known[id]))
          known = { ...known, [id]: latest(known[id], version) };
      return known === state.known ? state : { ...state, known };
    }
    case 'event': {
      const { event } = learned;
      if (event.type !== 'change')
        return {
          canRead: true,
          epoch: state.epoch + 1,
          // Reads made before this event may show data the server no
          // longer has.
          resets:
            state.resets + (event.type === 'reset' || state.canRead ? 1 : 0),
          news: state.news,
          known: {},
          unknown: {},
        };
      const {
        artifact_id: id,
        change,
        revision,
        metadata_revision,
      } = event.change;
      // A kind added after this client may change anything about it.
      if (change === 'unknown')
        return {
          ...state,
          news: state.news + 1,
          unknown: { ...state.unknown, [id]: (state.unknown[id] ?? 0) + 1 },
        };
      const version = { revision, metadata: metadata_revision };
      if (!isNewer(version, state.known[id])) return state;
      return {
        ...state,
        news: state.news + 1,
        known: { ...state.known, [id]: latest(state.known[id], version) },
      };
    }
  }
}

/** What a connected page knows from the change feed, and when it may read. */
export interface LiveWorkspace extends Knowledge {
  readonly feed: FeedState;
  /** Records versions from a list read in `epoch`; they are not news. */
  readonly learn: (epoch: number, versions: readonly ListedVersion[]) => void;
}

/**
 * The read policy of a connected page (decision 0007). While the page is
 * visible it follows the change feed, resuming after its last cursor; a
 * hidden page gives up its stream. The first read waits for the stream's
 * `ready`, at most two seconds of visibility, and not at all once the
 * stream failed.
 */
export function useLiveWorkspace(client: ArtifreeClient): LiveWorkspace {
  const visible = usePageVisible();
  const [state, learn] = useReducer(learnFrom, START);
  const feed = useChangeFeed(client, visible, {
    onEvent: (event) => learn({ type: 'event', event }),
    onTrouble: () => learn({ type: 'read anyway' }),
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
  const learnListed = useCallback(
    (epoch: number, versions: readonly ListedVersion[]) =>
      learn({ type: 'listed', epoch, versions }),
    [],
  );
  return { ...state, feed, learn: learnListed };
}

interface FeedHandlers {
  readonly onEvent: (event: ChangeFeedEvent) => void;
  /** The stream was lost or refused; it may or may not come back. */
  readonly onTrouble: () => void;
}

/**
 * Follows the change feed while `active`. Becoming inactive aborts the
 * stream; becoming active again resumes after the last cursor. Another
 * `client` starts over, and the old subscription delivers nothing more. A
 * failure that reconnecting cannot fix, such as a rejected token, stops
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
  // A resumed stream is connecting until the server accepts it.
  const [wasActive, setWasActive] = useState(active);
  if (wasActive !== active) {
    setWasActive(active);
    if (active && connection !== 'stopped') setConnection('connecting');
  }
  const resume = useRef<{ client: ArtifreeClient; cursor: string }>(undefined);
  const onEvent = useEffectEvent(handlers.onEvent);
  const onTrouble = useEffectEvent(handlers.onTrouble);
  const stopped = connection === 'stopped';
  useEffect(() => {
    if (!active || stopped) return undefined;
    const after =
      resume.current?.client === client ? resume.current.cursor : undefined;
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
          resume.current = { client, cursor: cursorOf(event) };
          onEvent(event);
        },
        onDisconnect() {
          if (signal.aborted) return;
          setConnection('reconnecting');
          onTrouble();
        },
      })
      .catch(() => {
        if (signal.aborted) return;
        setConnection('stopped');
        onTrouble();
      });
    return () => controller.abort();
  }, [client, active, stopped]);
  if (stopped) return 'stopped';
  return active ? connection : 'paused';
}
