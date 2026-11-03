import { newRandomId } from '@artifree/client';
import type { ArtifreeClient } from '@artifree/client';
import { lazy, Suspense, useRef, useState, useSyncExternalStore } from 'react';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
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
const RuntimeFrame = lazy(() =>
  import('./runtime-frame').then((module) => ({
    default: module.RuntimeFrame,
  })),
);

interface PreviewFrameProps {
  client: ArtifreeClient;
  artifactId: string;
  revision: number;
  /** The feed epoch the revision was read in; a new one prepares again. */
  epoch: number;
  /**
   * Whether the revision number predates a possible data replacement:
   * nothing is prepared for it then, and nothing prepared meanwhile shown.
   */
  stale: boolean;
  title: string;
  className?: string;
}

/** A pinned revision, prepared as a preview and shown in its sandbox. */
export function PreviewFrame({
  client,
  artifactId,
  revision,
  epoch,
  stale,
  title,
  className,
}: PreviewFrameProps) {
  const [viewId] = useState(newRandomId);
  const [grantError, setGrantError] = useState<string>();
  const [attempt, setAttempt] = useState(0);
  useSyncExternalStore(
    client.subscribeRuntimeGrants,
    client.runtimeGrantSnapshot,
    client.runtimeGrantSnapshot,
  );
  const grantRevision = client.runtimeGrantRevision(artifactId, revision);
  const connectionRevision = client.runtimeConnectionRevision();
  const generation = useRef(0);
  const preview = useRequest(
    stale
      ? undefined
      : `${artifactId}:${revision}:${epoch}:${connectionRevision}:${grantRevision}:${attempt}`,
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
      return { view, deadline: dispatchedAt + view.lease_ms, close };
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
  const view = preview.value.view;
  const grantStatus = client.runtimeGrantStatus(view);
  async function changeGrant(capabilities: string[]): Promise<void> {
    setGrantError(undefined);
    try {
      await client.setRuntimeGrant(view, capabilities);
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
      {grantStatus === 'settled' ? (
        <Suspense fallback={PREPARING}>
          <RuntimeFrame
            key={`${preview.value.view.view_id}:${preview.value.view.generation}`}
            client={client}
            view={preview.value.view}
            deadline={preview.value.deadline}
            onClose={preview.value.close}
            title={title}
            className={className}
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
