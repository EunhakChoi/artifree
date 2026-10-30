import { newRandomId } from '@artifree/client';
import type { ArtifreeClient } from '@artifree/client';
import { useRef, useState } from 'react';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { RuntimeFrame } from './runtime-frame';
import { useRequest } from './use-request';

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
  const generation = useRef(0);
  const preview = useRequest(
    stale ? undefined : `${artifactId}:${revision}:${epoch}`,
    (signal) =>
      client.createRuntimeView(
        artifactId,
        revision,
        viewId,
        ++generation.current,
        { signal },
      ),
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
        <AlertDescription>{preview.message}</AlertDescription>
      </Alert>
    );
  return (
    <RuntimeFrame
      key={`${preview.value.view_id}:${preview.value.generation}`}
      client={client}
      view={preview.value}
      title={title}
      className={className}
    />
  );
}
