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
import { offerText } from './offer-text';

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
  const text = offerText(offer);
  const { sourcePath, size } = text;
  return (
    <Dialog
      open={offer !== undefined}
      onOpenChange={(open) => {
        if (!open) desk.decline();
      }}
    >
      <DialogContent showCloseButton={false} initialFocus={decline}>
        <DialogHeader>
          <DialogTitle>{text.title}</DialogTitle>
          <DialogDescription>{text.description}</DialogDescription>
        </DialogHeader>
        <dl className="grid gap-1 text-sm">
          <dt className="text-muted-foreground">Saved as</dt>
          <dd className="truncate font-mono">{text.fileName}</dd>
          {sourcePath === undefined ? null : (
            <>
              <dt className="text-muted-foreground">Source path</dt>
              <dd>
                <code className="block truncate font-mono">{sourcePath}</code>
              </dd>
            </>
          )}
          {size === undefined ? null : (
            <>
              <dt className="text-muted-foreground">Size</dt>
              <dd>{size}</dd>
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
