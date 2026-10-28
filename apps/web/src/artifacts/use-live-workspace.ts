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

/**
 * Whether an artifact is in the trash, as of a metadata revision
 * (decision 0014, item 4).
 */
export interface TrashState {
  readonly deleted: boolean;
  readonly metadata: number;
}

/**
 * The epoch after `event`: each `ready` and `reset` starts one. The feed
 * handler and the reducer both apply it, so the handler knows the epoch
 * as soon as an event arrives, before the page renders it.
 */
function epochAfter(epoch: number, event: ChangeFeedEvent): number {
  return event.type === 'change' ? epoch : epoch + 1;
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
  /**
   * Per artifact the page saw in the trash in this epoch, whether it still
   * is, as of the newest metadata revision the page saw.
   */
  readonly trash: Readonly<Record<string, TrashState>>;
}

type Learned =
  | { readonly type: 'event'; readonly event: ChangeFeedEvent }
  | {
      readonly type: 'read';
      readonly epoch: number;
      readonly artifacts: readonly Artifact[];
    }
  | {
      readonly type: 'in trash';
      readonly epoch: number;
      readonly id: string;
      readonly metadata: number;
    }
  | { readonly type: 'read anyway' };

const START: Knowledge = {
  canRead: false,
  epoch: 0,
  resets: 0,
  news: 0,
  known: {},
  unknown: {},
  trash: {},
};

/** How long a visible page waits for the stream before reading anyway. */
const FIRST_READ_WAIT_MS = 2_000;

function learnFrom(state: Knowledge, learned: Learned): Knowledge {
  switch (learned.type) {
    case 'read anyway':
      return state.canRead ? state : { ...state, canRead: true };
    case 'read': {
      if (learned.epoch !== state.epoch) return state;
      let { known, trash } = state;
      for (const artifact of learned.artifacts) {
        const { id } = artifact;
        const version = versionOf(artifact);
        if (isNewer(version, known[id]))
          known = { ...known, [id]: latest(known[id], version) };
        trash = seen(trash, id, {
          deleted: artifact.deleted_at_ms !== null,
          metadata: version.metadata,
        });
      }
      return known === state.known && trash === state.trash
        ? state
        : { ...state, known, trash };
    }
    case 'in trash': {
      if (learned.epoch !== state.epoch) return state;
      const { id, metadata } = learned;
      const trash = seen(state.trash, id, { deleted: true, metadata });
      return trash === state.trash ? state : { ...state, trash };
    }
    case 'event': {
      const { event } = learned;
      if (event.type !== 'change')
        return {
          canRead: true,
          epoch: epochAfter(state.epoch, event),
          // Reads made before this event may show data the server no
          // longer has.
          resets:
            state.resets + (event.type === 'reset' || state.canRead ? 1 : 0),
          news: state.news,
          known: {},
          unknown: {},
          trash: {},
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
      // The notice of the page's own move may arrive after its answer, as
      // nothing new; what it says of the trash is learned either way.
      const trash =
        change === 'deleted' || change === 'recovered'
          ? seen(state.trash, id, {
              deleted: change === 'deleted',
              metadata: metadata_revision,
            })
          : state.trash;
      const version = { revision, metadata: metadata_revision };
      if (!isNewer(version, state.known[id]))
        return trash === state.trash ? state : { ...state, trash };
      return {
        ...state,
        news: state.news + 1,
        known: { ...state.known, [id]: latest(state.known[id], version) },
        trash,
      };
    }
  }
}

/**
 * `trash` with what `state` shows of artifact `id`, if that is newer than
 * the record. An artifact enters the record when it is seen in the trash;
 * one never seen there is not recorded.
 */
function seen(
  trash: Readonly<Record<string, TrashState>>,
  id: string,
  state: TrashState,
): Readonly<Record<string, TrashState>> {
  const before = trash[id];
  if (before === undefined ? !state.deleted : state.metadata <= before.metadata)
    return trash;
  return { ...trash, [id]: state };
}

/** What a connected page knows from the change feed, and when it may read. */
export interface LiveWorkspace extends Knowledge {
  readonly feed: FeedState;
  /**
   * Records what a read in `epoch`, or the answer to a write sent in it,
   * showed of artifacts: their versions, which are not news, and whether
   * they are in the trash. Nothing from an earlier epoch is learned, since
   * the server's data may have been replaced since.
   */
  readonly learn: (epoch: number, artifacts: readonly Artifact[]) => void;
  /**
   * Records that artifact `id` was in the trash at metadata revision
   * `metadata`, as a refusal and a read in `epoch` showed together.
   */
  readonly learnInTrash: (epoch: number, id: string, metadata: number) => void;
  /**
   * Whether `epoch` is still the feed's epoch. It changes as soon as a
   * `ready` or `reset` arrives, also before the page has rendered it, so
   * an answer that settles in between is not taken as current.
   */
  readonly isCurrent: (epoch: number) => boolean;
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
  // The epoch as of the last event received, which the reducer reaches
  // once it has applied every event dispatched so far.
  const epochNow = useRef(START.epoch);
  const feed = useChangeFeed(client, visible, {
    onEvent: (event) => {
      epochNow.current = epochAfter(epochNow.current, event);
      learn({ type: 'event', event });
    },
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
  const learnRead = useCallback(
    (epoch: number, artifacts: readonly Artifact[]) =>
      learn({ type: 'read', epoch, artifacts }),
    [],
  );
  const learnInTrash = useCallback(
    (epoch: number, id: string, metadata: number) =>
      learn({ type: 'in trash', epoch, id, metadata }),
    [],
  );
  const isCurrent = useCallback(
    (epoch: number) => epoch === epochNow.current,
    [],
  );
  return { ...state, feed, learn: learnRead, learnInTrash, isCurrent };
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
