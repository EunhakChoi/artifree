import { newRandomId } from '@artifree/client';
import type { ArtifreeClient } from '@artifree/client';
import { useRef, useState, useSyncExternalStore } from 'react';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import { RuntimeFrame } from './runtime-frame';
import { describeFailure, useRequest } from './use-request';

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
  async function changeGrant(allowed: boolean): Promise<void> {
    setGrantError(undefined);
    try {
      await client.setRuntimeGrant(view, allowed ? ['metadata.read'] : []);
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
        <section
          aria-label="Runtime permissions"
          className="flex flex-col gap-2 rounded-lg border p-3 text-sm"
        >
          <p>SDK: {view.sdk}</p>
          <p>Declared: {view.declared.join(', ') || 'none'}</p>
          <p>Supported: {view.supported.join(', ') || 'none'}</p>
          {grantStatus === 'settled' ? (
            <p>Granted: {view.granted.join(', ') || 'none'}</p>
          ) : (
            <p>
              {grantStatus === 'pending'
                ? 'Permission change pending.'
                : 'Permission status is unknown. Clear access to resolve it.'}
            </p>
          )}
          {grantStatus === 'unknown' ? (
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() => void changeGrant(false)}
            >
              Resolve by revoking access
            </Button>
          ) : grantStatus === 'settled' &&
            view.supported.includes('metadata.read') &&
            view.declared.includes('metadata.read') ? (
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() =>
                void changeGrant(!view.granted.includes('metadata.read'))
              }
            >
              {view.granted.includes('metadata.read')
                ? 'Revoke metadata access'
                : 'Allow metadata access'}
            </Button>
          ) : null}
        </section>
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
        <RuntimeFrame
          key={`${preview.value.view.view_id}:${preview.value.view.generation}`}
          client={client}
          view={preview.value.view}
          deadline={preview.value.deadline}
          onClose={preview.value.close}
          title={title}
          className={className}
        />
      ) : (
        <output className="text-sm text-muted-foreground">
          Preview paused while permission is unresolved.
        </output>
      )}
    </div>
  );
}
