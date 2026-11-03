import { useEffect, useState, useSyncExternalStore } from 'react';
import { DownloadIcon } from 'lucide-react';
import type { ArtifreeClient } from '@artifree/client';
import type { RuntimeView } from '@artifree/contracts';
import type { RuntimeDownloadResult } from '@artifree/runtime-sdk';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import { OfferDesk, SavedFileUrls, displayPath } from './download-offer';
import { RuntimeFrame } from './runtime-frame';
import { describeFailure } from './use-request';

interface RuntimeStageProps {
  readonly client: ArtifreeClient;
  readonly view: RuntimeView;
  /** Local monotonic deadline, conservatively measured from dispatch. */
  readonly deadline: number;
  readonly onClose: () => void;
  readonly title: string;
  readonly className?: string | undefined;
}

/**
 * A runtime frame and the downloads it asks for (decision 0032). A frame
 * cannot start a download: it can only put one question in front of the user,
 * here, and the bytes are fetched, with the server checking the frame's
 * current grant again, only after the user presses Download. One question is
 * open at a time. A question disappears with its frame. When the rules of the
 * question apply (when a press counts, when it ends) is the {@link OfferDesk}'s
 * to say; this component only shows what it says.
 */
export function RuntimeStage({
  client,
  view,
  deadline,
  onClose,
  title,
  className,
}: RuntimeStageProps) {
  const [desk] = useState(() => new OfferDesk<RuntimeDownloadResult>());
  // The saved file stays addressable for a while after the browser is handed
  // it, even if this stage is rebuilt meanwhile (see SavedFileUrls).
  const [urls] = useState(
    () => new SavedFileUrls((url) => URL.revokeObjectURL(url)),
  );
  const shown = useSyncExternalStore(desk.subscribe, desk.getView);
  useEffect(() => () => desk.dispose(), [desk]);

  function confirm(): void {
    void desk.confirm(
      ({ path, signal }) => client.runtimeDownload(view, path, { signal }),
      (file) => {
        const url = URL.createObjectURL(
          new Blob([file.bytes], { type: file.mediaType }),
        );
        urls.hold(url);
        const link = document.createElement('a');
        link.href = url;
        link.download = file.fileName;
        link.rel = 'noopener';
        link.click();
        return {
          status: 'started',
          kind: file.kind,
          name: file.fileName,
          size: file.bytes.length,
          revision: view.revision,
        };
      },
    );
  }

  const what =
    shown === undefined || shown.phase === 'failed' ? undefined : shown.path;
  return (
    <div className="relative">
      {shown === undefined ? null : shown.phase === 'failed' ? (
        <Alert
          variant="destructive"
          className="absolute inset-x-2 top-2 z-10 w-auto"
        >
          <AlertTitle>Download not saved</AlertTitle>
          <AlertDescription>
            <p>{describeFailure(shown.error)}</p>
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() => desk.dismiss()}
            >
              Dismiss
            </Button>
          </AlertDescription>
        </Alert>
      ) : (
        <Alert
          aria-label="Download request"
          className="absolute inset-x-2 top-2 z-10 w-auto"
        >
          <DownloadIcon aria-hidden="true" />
          <AlertTitle>The app wants to download a file</AlertTitle>
          <AlertDescription>
            <p>
              {what === undefined ? (
                'It asks to save this whole revision as a ZIP archive'
              ) : (
                <>
                  It asks to save the file{' '}
                  <code className="font-mono break-all text-foreground">
                    {displayPath(what)}
                  </code>{' '}
                  (a name the app chose)
                </>
              )}{' '}
              from revision {view.revision} to your computer. Nothing is saved
              until you press Download.
            </p>
            <div className="flex gap-2">
              <Button
                type="button"
                size="sm"
                disabled={shown.phase === 'saving' || !shown.active}
                onClick={confirm}
              >
                {shown.phase === 'saving' ? 'Downloading…' : 'Download'}
              </Button>
              <Button
                type="button"
                variant="outline"
                size="sm"
                disabled={shown.phase === 'saving'}
                onClick={() => desk.decline()}
              >
                Decline
              </Button>
            </div>
          </AlertDescription>
        </Alert>
      )}
      <RuntimeFrame
        client={client}
        view={view}
        deadline={deadline}
        onClose={onClose}
        onDownload={desk.ask}
        title={title}
        className={className}
      />
    </div>
  );
}
