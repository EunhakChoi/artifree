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
  /** The feed epoch `revision` was read in; previews are keyed by it. */
  epoch: number;
  /**
   * The server's data may have been replaced since `revision` was read, so
   * the number may name other content: nothing is prepared or shown.
   */
  stale: boolean;
  /** The frame's accessible name. */
  title: string;
}

/** A pinned revision in the sandboxed preview origin (decision 0003). */
export function PreviewFrame({
  client,
  artifactId,
  revision,
  epoch,
  stale,
  title,
}: PreviewFrameProps) {
  const preview = useRequest(
    stale ? undefined : `${artifactId}:${revision}:${epoch}`,
    (signal) => client.createPreview(artifactId, revision, { signal }),
  );
  if (preview.status === 'ready')
    return (
      <iframe
        title={title}
        src={preview.value.url}
        sandbox={PREVIEW_SANDBOX}
        referrerPolicy="no-referrer"
        className="h-128 w-full rounded-lg border"
      />
    );
  if (preview.status === 'error')
    return (
      <Alert variant="destructive">
        <AlertTitle>Preview unavailable</AlertTitle>
        <AlertDescription>{preview.message}</AlertDescription>
      </Alert>
    );
  return (
    <output className="text-sm text-muted-foreground">
      {stale
        ? 'The preview is prepared once the artifact is read again.'
        : 'Preparing preview…'}
    </output>
  );
}
