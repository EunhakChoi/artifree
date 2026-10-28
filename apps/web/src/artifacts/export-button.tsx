import { useEffect, useRef, useState } from 'react';
import { DownloadIcon } from 'lucide-react';
import type { ArtifreeClient } from '@artifree/client';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import { describeFailure } from './use-request';

interface ExportButtonProps {
  client: ArtifreeClient;
  artifactId: string;
  /** The revision to export: the one the preview shows. */
  revision: number;
  /** Whether what is shown was read before the last possible replacement. */
  stale: boolean;
}

type Download =
  | { readonly status: 'idle' | 'loading' }
  | { readonly status: 'failed'; readonly message: string };

/**
 * Saves the source export of a revision (decision 0015): a ZIP archive of
 * its files with their manifest. It is a read, so a failure changed
 * nothing and can simply be tried again. While what is shown may predate a
 * replacement of the server's data it waits, since the same number may
 * then name other files (0011 decision 6).
 */
export function ExportButton({
  client,
  artifactId,
  revision,
  stale,
}: ExportButtonProps) {
  const [download, setDownload] = useState<Download>({ status: 'idle' });
  // The archive stays addressable until the next download or until the
  // page closes the artifact, so the browser can finish saving it.
  const saved = useRef<string | undefined>(undefined);
  const running = useRef<AbortController | undefined>(undefined);
  useEffect(
    () => () => {
      running.current?.abort();
      if (saved.current !== undefined) URL.revokeObjectURL(saved.current);
    },
    [],
  );

  async function start(): Promise<void> {
    const controller = new AbortController();
    running.current = controller;
    setDownload({ status: 'loading' });
    let archive: Uint8Array<ArrayBuffer>;
    try {
      archive = await client.exportRevision(artifactId, revision, {
        signal: controller.signal,
      });
    } catch (error: unknown) {
      if (!controller.signal.aborted)
        setDownload({ status: 'failed', message: describeFailure(error) });
      return;
    }
    if (controller.signal.aborted) return;
    if (saved.current !== undefined) URL.revokeObjectURL(saved.current);
    saved.current = URL.createObjectURL(
      new Blob([archive], { type: 'application/zip' }),
    );
    const link = document.createElement('a');
    link.href = saved.current;
    link.download = `${artifactId}-r${revision}.zip`;
    link.click();
    setDownload({ status: 'idle' });
  }

  return (
    <div className="flex flex-col items-end gap-2">
      <Button
        type="button"
        variant="outline"
        size="sm"
        disabled={stale || download.status === 'loading'}
        onClick={() => void start()}
      >
        <DownloadIcon data-icon="inline-start" aria-hidden="true" />
        Download revision {revision}
      </Button>
      {download.status === 'failed' ? (
        <Alert variant="destructive">
          <AlertTitle>Not downloaded</AlertTitle>
          <AlertDescription>
            {download.message} Nothing changed; try again.
          </AlertDescription>
        </Alert>
      ) : null}
    </div>
  );
}
