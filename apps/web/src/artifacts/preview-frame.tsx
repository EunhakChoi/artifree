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
  /** Names the frame for assistive technology. */
  title: string;
  /** The pinned revision shown. */
  revision: number;
  /** The feed epoch the revision number was read in. */
  epoch: number;
  /** Whether that number predates a possible data replacement. */
  stale: boolean;
}

/**
 * One pinned revision in the sandboxed preview origin (decision 0003). No
 * preview is prepared for a number that predates a possible data
 * replacement, and none prepared meanwhile is shown.
 */
export function PreviewFrame({
  client,
  artifactId,
  title,
  revision,
  epoch,
  stale,
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
      className="h-128 w-full rounded-lg border"
    />
  );
}
