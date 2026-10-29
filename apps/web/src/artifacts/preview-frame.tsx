import type { ArtifreeClient } from '@artifree/client';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { useRequest } from './use-request';

/**
 * Sandbox flags for previews, matching the content server's CSP: without
 * allow-same-origin the content is opaque, and without allow-popups it cannot
 * open windows that escape the content policy.
 */
const PREVIEW_SANDBOX =
  'allow-scripts allow-forms allow-modals allow-downloads';

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
  const preview = useRequest(
    stale ? undefined : `${artifactId}:${revision}:${epoch}`,
    (signal) => client.createPreview(artifactId, revision, { signal }),
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
    <iframe
      title={title}
      src={preview.value.url}
      sandbox={PREVIEW_SANDBOX}
      referrerPolicy="no-referrer"
      className={className}
    />
  );
}
