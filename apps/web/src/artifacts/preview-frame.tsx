import { newRandomId } from '@artifree/client';
import type { ArtifreeClient } from '@artifree/client';
import { lazy, Suspense, useRef, useState, useSyncExternalStore } from 'react';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import type { DataChanges } from './data-changes';
import type { LiveFrame } from './runtime-frame';
import { describeFailure, useRequest } from './use-request';

// The runtime permission controls, and the frame component with the
// workspace's side of the runtime bridge behind it, load when a preview is
// first shown, not with the page: most sessions never open one, and the main
// bundle stays under the build's size warning (decision 0032).
const PREPARING = (
  <output className="text-sm text-muted-foreground">Preparing preview…</output>
);

const RuntimePermissions = lazy(() =>
  import('./runtime-permissions').then((module) => ({
    default: module.RuntimePermissions,
  })),
);
const AdoptionNotice = lazy(() =>
  import('./runtime-permissions').then((module) => ({
    default: module.AdoptionNotice,
  })),
);
const RuntimeFrame = lazy(() =>
  import('./runtime-frame').then((module) => ({
    default: module.RuntimeFrame,
  })),
);

interface PreviewFrameProps {
  client: ArtifreeClient;
  /** The feed's data changes, forwarded to a frame that holds data.read. */
  dataChanges: DataChanges;
  artifactId: string;
  revision: number;
  /** The feed epoch the revision was read in; a new one prepares again. */
  epoch: number;
  /**
   * Whether the revision number predates a possible data replacement:
   * nothing is prepared for it then, and nothing prepared meanwhile shown.
   */
  stale: boolean;
  /** Whether no revision is chosen: the preview follows the latest one. */
  following: boolean;
  /** The latest revision the workspace knows, whichever one is shown. */
  latest: number;
  /**
   * Makes the preview follow the latest revision. A caller with no such
   * choice leaves it out, and a newer revision that a chosen one cannot load
   * is then only reported, not offered.
   */
  onShowLatest?: (() => void) | undefined;
  title: string;
  className?: string;
}

/** A frame that opted in and connected, and what its view was created with. */
interface HeldFrame {
  readonly frame: LiveFrame;
  readonly revision: number;
  readonly grant: number;
  readonly epoch: number;
  readonly connection: number;
  readonly attempt: number;
}

/** A pinned revision, prepared as a preview and shown in its sandbox. */
export function PreviewFrame({
  client,
  dataChanges,
  artifactId,
  revision,
  epoch,
  stale,
  following,
  latest,
  onShowLatest,
  title,
  className,
}: PreviewFrameProps) {
  const [viewId] = useState(newRandomId);
  const [grantError, setGrantError] = useState<string>();
  const [attempt, setAttempt] = useState(0);
  const [live, setLive] = useState<HeldFrame>();
  useSyncExternalStore(
    client.subscribeRuntimeGrants,
    client.runtimeGrantSnapshot,
    client.runtimeGrantSnapshot,
  );
  const grantRevision = client.runtimeGrantRevision(artifactId, revision);
  const connectionRevision = client.runtimeConnectionRevision();
  // A frame that opted in and connected its port keeps its view across a
  // change of the revision shown and of its grant: the workspace adopts it
  // instead (decision 0054, rule 8). A new epoch, connection or attempt still
  // makes a new view (rule 11).
  const held =
    live !== undefined &&
    live.epoch === epoch &&
    live.connection === connectionRevision &&
    live.attempt === attempt
      ? live
      : undefined;
  const generation = useRef(0);
  const preview = useRequest(
    stale
      ? undefined
      : `${artifactId}:${held?.revision ?? revision}:${epoch}:${connectionRevision}:${held?.grant ?? grantRevision}:${attempt}`,
    async (signal) => {
      // A scheduler can hold reads while the page is hidden or rate-limited.
      // Time the lease from the actual attempt, not from entering that queue.
      let dispatchedAt = performance.now();
      const currentGeneration = ++generation.current;
      let closed = false;
      const close = (): void => {
        if (closed) return;
        closed = true;
        client
          .closeRuntimeView({ view_id: viewId, generation: currentGeneration })
          .catch((error: unknown) => {
            console.error('Could not revoke the runtime view.', error);
          });
      };
      // An abandoned request can still finish at the server. Close its known
      // generation even when no response reached this page.
      signal.addEventListener('abort', close, { once: true });
      const view = await client.createRuntimeView(
        artifactId,
        revision,
        viewId,
        currentGeneration,
        {
          signal,
          onDispatch: () => {
            dispatchedAt = performance.now();
          },
        },
      );
      return {
        view,
        deadline: dispatchedAt + view.lease_ms,
        close,
        revision,
        grant: grantRevision,
      };
    },
  );
  if (preview.status === 'loading')
    return (
      <output className="text-sm text-muted-foreground">
        {stale
          ? 'The preview is prepared once the artifact is read again.'
          : 'Preparing preview…'}
      </output>
    );
  if (preview.status === 'error')
    return (
      <Alert variant="destructive">
        <AlertTitle>Preview unavailable</AlertTitle>
        <AlertDescription>
          <p>{preview.message}</p>
          {grantError === undefined ? null : <p>{grantError}</p>}
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => setAttempt((value) => value + 1)}
          >
            Retry preview
          </Button>
        </AlertDescription>
      </Alert>
    );
  const value = preview.value;
  const restart = (): void => {
    setLive(undefined);
    setAttempt((current) => current + 1);
  };
  const view = held?.frame.view ?? value.view;
  const grantStatus = client.runtimeGrantStatus(view);
  async function changeGrant(capabilities: string[]): Promise<void> {
    setGrantError(undefined);
    try {
      // A live frame that opted in is paused before the change is sent and
      // adopted after it (decision 0054, rule 8).
      if (held === undefined) {
        await client.setRuntimeGrant(view, capabilities);
      } else {
        await held.frame.changePermissions(capabilities, view);
      }
    } catch (error: unknown) {
      setGrantError(
        `Permission change was not confirmed: ${describeFailure(error)}`,
      );
    }
  }
  return (
    <div className="flex flex-col gap-3">
      {grantError === undefined ? null : (
        <Alert variant="destructive">
          <AlertTitle>Permission change</AlertTitle>
          <AlertDescription>{grantError}</AlertDescription>
        </Alert>
      )}
      {!view.enabled ? null : (
        <Suspense fallback={null}>
          <RuntimePermissions
            view={view}
            grantStatus={grantStatus}
            onChange={(capabilities) => void changeGrant(capabilities)}
          />
        </Suspense>
      )}
      <Button
        type="button"
        variant="outline"
        size="sm"
        onClick={() => setAttempt((value) => value + 1)}
      >
        Refresh preview
      </Button>
      {held?.frame.adopted === true ? (
        <Suspense fallback={null}>
          <AdoptionNotice
            documentRevision={value.view.revision}
            loadedHead={value.view.head_revision}
            view={held.frame.view}
            following={following}
            // Offered once the workspace itself knows the revision the notice
            // names: loading it sooner would show an older one.
            onLoadNewer={
              latest < held.frame.view.head_revision
                ? undefined
                : following
                  ? restart
                  : onShowLatest
            }
          />
        </Suspense>
      ) : null}
      {grantStatus === 'settled' || held !== undefined ? (
        <Suspense fallback={PREPARING}>
          <RuntimeFrame
            key={`${value.view.view_id}:${value.view.generation}`}
            client={client}
            dataChanges={dataChanges}
            view={value.view}
            deadline={value.deadline}
            onClose={value.close}
            title={title}
            className={className}
            following={following}
            shownRevision={revision}
            latestRevision={latest}
            nextGeneration={() => ++generation.current}
            onLive={(frame) =>
              setLive(
                frame === undefined
                  ? undefined
                  : {
                      frame,
                      revision: value.revision,
                      grant: value.grant,
                      epoch,
                      connection: connectionRevision,
                      attempt,
                    },
              )
            }
            onRestart={restart}
          />
        </Suspense>
      ) : (
        <output className="text-sm text-muted-foreground">
          Preview paused while permission is unresolved.
        </output>
      )}
    </div>
  );
}
