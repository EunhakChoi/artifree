import { useCallback, useEffect, useRef, useState } from 'react';
import { DownloadIcon } from 'lucide-react';
import type { ArtifreeClient } from '@artifree/client';
import type { RuntimeView } from '@artifree/contracts';
import type { RuntimeDownloadResult } from '@artifree/runtime-sdk';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import { OfferGate, displayPath, OFFER_TIMING } from './download-offer';
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
  /** Timers that make the question active and withdraw it unanswered. */
  readonly timers: number[];
}

type Shown =
  | {
      readonly phase: 'asking' | 'saving';
      readonly path: string | undefined;
      /** Whether a press counts yet (see {@link OFFER_TIMING}). */
      readonly active: boolean;
    }
  | { readonly phase: 'failed'; readonly message: string };

/** Stops an offer's timers and requests; the caller settles its promise. */
function end(offer: Offer | undefined): void {
  if (offer === undefined) return;
  for (const timer of offer.timers) window.clearTimeout(timer);
  offer.controller.abort();
}

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
  const gate = useRef(new OfferGate());
  // The saved file stays addressable until the next download or until the
  // frame goes away, so the browser can finish saving it.
  const saved = useRef<string | undefined>(undefined);
  useEffect(
    () => () => {
      end(open.current);
      if (saved.current !== undefined) URL.revokeObjectURL(saved.current);
    },
    [],
  );

  /** Ends the open question with a refusal and starts the pause. */
  const withdraw = useCallback((offer: Offer) => {
    if (open.current !== offer) return;
    open.current = undefined;
    gate.current.decline(performance.now());
    end(offer);
    setShown(undefined);
    offer.reject(new HostRefusal('declined'));
  }, []);

  const ask = useCallback(
    ({
      path,
      signal,
    }: {
      readonly path: string | undefined;
      readonly signal: AbortSignal;
    }) => {
      const verdict = gate.current.open(performance.now());
      // A frame that asks again at once after a "no" is told "no" again.
      if (verdict === 'cooling')
        return Promise.reject(new HostRefusal('declined'));
      if (verdict === 'busy') return Promise.reject(new HostRefusal('busy'));
      return new Promise<RuntimeDownloadResult>((resolve, reject) => {
        const offer: Offer = {
          path,
          controller: new AbortController(),
          resolve,
          reject,
          timers: [],
        };
        open.current = offer;
        setShown({ phase: 'asking', path, active: false });
        offer.timers.push(
          window.setTimeout(() => {
            setShown((current) =>
              current?.phase === 'asking' && open.current === offer
                ? { ...current, active: true }
                : current,
            );
          }, OFFER_TIMING.activationMs),
          // Unanswered for too long: the frame stops waiting before long, and
          // a press after that would save a file nobody asked for.
          window.setTimeout(() => withdraw(offer), OFFER_TIMING.lifetimeMs),
        );
        signal.addEventListener(
          'abort',
          () => {
            if (open.current !== offer) return;
            open.current = undefined;
            gate.current.close();
            end(offer);
            setShown(undefined);
            reject(new HostRefusal('view_unavailable'));
          },
          { once: true },
        );
      });
    },
    [withdraw],
  );

  async function confirm(): Promise<void> {
    const offer = open.current;
    if (offer === undefined) return;
    // A press before the question was active, or after it expired, is not an
    // answer: the frame chose when the question appeared.
    if (!gate.current.canConfirm(performance.now())) return;
    for (const timer of offer.timers) window.clearTimeout(timer);
    setShown({ phase: 'saving', path: offer.path, active: true });
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
      gate.current.close();
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
      gate.current.close();
      setShown({ phase: 'failed', message: describeFailure(error) });
      offer.reject(error);
    }
  }

  function decline(): void {
    const offer = open.current;
    if (offer !== undefined) withdraw(offer);
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
