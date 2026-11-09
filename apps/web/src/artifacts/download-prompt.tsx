import { useRef, useSyncExternalStore } from 'react';
import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import type { OfferDesk } from './offer-desk';

/** The longest stretch of frame-supplied path text shown. */
const SHOWN_PATH_CHARS = 80;

interface DownloadPromptProps {
  readonly desk: OfferDesk;
}

/**
 * The prompt for a download a preview asked for (decisions 0032 and 0045). The
 * rules are the desk's: this shows its snapshot and forwards presses. It is a
 * modal over the page, so nothing around the frame moves when it appears. The
 * safe answer holds the focus, and the accept button stays disabled until
 * the desk would count it.
 */
export function DownloadPrompt({ desk }: DownloadPromptProps) {
  const { offer, armed } = useSyncExternalStore(
    desk.subscribe,
    desk.snapshot,
    desk.snapshot,
  );
  const decline = useRef<HTMLButtonElement>(null);
  const sourcePath = offer?.kind === 'asset' ? undefined : offer?.path;
  const shown =
    sourcePath === undefined || sourcePath === null
      ? undefined
      : sourcePath.length > SHOWN_PATH_CHARS
        ? `${sourcePath.slice(0, SHOWN_PATH_CHARS)}…`
        : sourcePath;
  const size = offer?.kind === 'asset' ? offer.bytes : offer?.size;
  return (
    <Dialog
      open={offer !== undefined}
      onOpenChange={(open) => {
        if (!open) desk.decline();
      }}
    >
      <DialogContent showCloseButton={false} initialFocus={decline}>
        <DialogHeader>
          <DialogTitle>
            {offer?.kind === 'asset'
              ? 'Save uploaded file?'
              : 'Save a file from this preview?'}
          </DialogTitle>
          <DialogDescription>
            {offer?.kind === 'asset' ? (
              <>
                The preview asked to save a file uploaded to this artifact.
                Saving it does not make it safe to open. Nothing is saved unless
                you accept.
              </>
            ) : (
              <>
                The preview asked to save{' '}
                {offer?.kind === 'archive'
                  ? 'the source files of this revision as a ZIP archive'
                  : 'a source file of this revision'}
                . Nothing is saved unless you accept.
              </>
            )}
          </DialogDescription>
        </DialogHeader>
        <dl className="grid gap-1 text-sm">
          <dt className="text-muted-foreground">Saved as</dt>
          <dd className="truncate font-mono">{offer?.fileName}</dd>
          {shown === undefined ? null : (
            <>
              <dt className="text-muted-foreground">Source path</dt>
              <dd>
                <code className="block truncate font-mono">{shown}</code>
              </dd>
            </>
          )}
          {size === undefined || size === null ? null : (
            <>
              <dt className="text-muted-foreground">Size</dt>
              <dd>{size} bytes</dd>
            </>
          )}
        </dl>
        <DialogFooter>
          <Button
            ref={decline}
            type="button"
            variant="outline"
            onClick={() => desk.decline()}
          >
            Decline
          </Button>
          <Button
            type="button"
            disabled={!armed}
            onClick={(event) => desk.accept(event.nativeEvent.isTrusted)}
          >
            Save file
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
