import { useCallback, useEffect, useRef, useState } from 'react';
import { DownloadIcon } from 'lucide-react';
import type { ArtifreeClient } from '@artifree/client';
import type { RuntimeView } from '@artifree/contracts';
import type { RuntimeDownloadResult } from '@artifree/runtime-sdk';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import { HostRefusal } from './runtime-requests';
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

/** A download a frame asked for, waiting for the user. */
interface Offer {
  readonly path: string | undefined;
  readonly controller: AbortController;
  readonly resolve: (receipt: RuntimeDownloadResult) => void;
  readonly reject: (reason: unknown) => void;
}

type Shown =
  | { readonly phase: 'asking' | 'saving'; readonly path: string | undefined }
  | { readonly phase: 'failed'; readonly message: string };

/**
 * A runtime frame and the downloads it asks for (decision 0032). A frame
 * cannot start a download: it can only put one question in front of the user,
 * here, and the bytes are fetched, with the server checking the frame's
 * current grant again, only after the user presses Download. One question is
 * open at a time. A question disappears with its frame.
 */
export function RuntimeStage({
  client,
  view,
  deadline,
  onClose,
  title,
  className,
}: RuntimeStageProps) {
  const [shown, setShown] = useState<Shown>();
  const open = useRef<Offer | undefined>(undefined);
  // The saved file stays addressable until the next download or until the
  // frame goes away, so the browser can finish saving it.
  const saved = useRef<string | undefined>(undefined);
  useEffect(
    () => () => {
      open.current?.controller.abort();
      if (saved.current !== undefined) URL.revokeObjectURL(saved.current);
    },
    [],
  );

  const ask = useCallback(
    ({
      path,
      signal,
    }: {
      readonly path: string | undefined;
      readonly signal: AbortSignal;
    }) => {
      if (open.current !== undefined)
        return Promise.reject(new HostRefusal('busy'));
      return new Promise<RuntimeDownloadResult>((resolve, reject) => {
        const offer: Offer = {
          path,
          controller: new AbortController(),
          resolve,
          reject,
        };
        open.current = offer;
        setShown({ phase: 'asking', path });
        signal.addEventListener(
          'abort',
          () => {
            if (open.current !== offer) return;
            open.current = undefined;
            offer.controller.abort();
            setShown(undefined);
            reject(new HostRefusal('view_unavailable'));
          },
          { once: true },
        );
      });
    },
    [],
  );

  async function confirm(): Promise<void> {
    const offer = open.current;
    if (offer === undefined) return;
    setShown({ phase: 'saving', path: offer.path });
    try {
      const file = await client.runtimeDownload(view, offer.path, {
        signal: offer.controller.signal,
      });
      // The frame went away while the bytes were on their way.
      if (open.current !== offer) return;
      if (saved.current !== undefined) URL.revokeObjectURL(saved.current);
      saved.current = URL.createObjectURL(
        new Blob([file.bytes], { type: file.mediaType }),
      );
      const link = document.createElement('a');
      link.href = saved.current;
      link.download = file.fileName;
      link.rel = 'noopener';
      link.click();
      open.current = undefined;
      setShown(undefined);
      offer.resolve({
        status: 'started',
        kind: file.kind,
        name: file.fileName,
        size: file.bytes.length,
        revision: view.revision,
      });
    } catch (error: unknown) {
      if (open.current !== offer) return;
      open.current = undefined;
      setShown({ phase: 'failed', message: describeFailure(error) });
      offer.reject(error);
    }
  }

  function decline(): void {
    const offer = open.current;
    if (offer === undefined) return;
    open.current = undefined;
    offer.controller.abort();
    setShown(undefined);
    offer.reject(new HostRefusal('declined'));
  }

  const what =
    shown === undefined || shown.phase === 'failed' || shown.path === undefined
      ? 'this whole revision as a ZIP archive'
      : shown.path;
  return (
    <div className="flex flex-col gap-3">
      {shown === undefined ? null : shown.phase === 'failed' ? (
        <Alert variant="destructive">
          <AlertTitle>Download not saved</AlertTitle>
          <AlertDescription>
            <p>{shown.message}</p>
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() => setShown(undefined)}
            >
              Dismiss
            </Button>
          </AlertDescription>
        </Alert>
      ) : (
        <Alert aria-label="Download request">
          <DownloadIcon aria-hidden="true" />
          <AlertTitle>The app wants to download a file</AlertTitle>
          <AlertDescription>
            <p>
              It asks to save {what} from revision {view.revision} to your
              computer. Nothing is saved until you press Download.
            </p>
            <div className="flex gap-2">
              <Button
                type="button"
                size="sm"
                disabled={shown.phase === 'saving'}
                onClick={() => void confirm()}
              >
                {shown.phase === 'saving' ? 'Downloading…' : 'Download'}
              </Button>
              <Button
                type="button"
                variant="outline"
                size="sm"
                disabled={shown.phase === 'saving'}
                onClick={decline}
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
        onDownload={ask}
        title={title}
        className={className}
      />
    </div>
  );
}
