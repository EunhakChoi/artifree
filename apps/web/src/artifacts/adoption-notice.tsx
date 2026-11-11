import type { RuntimeView } from '@artifree/contracts';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';

interface AdoptionNoticeProps {
  /** The revision the frame's document was loaded from (decision 0054, rule 5). */
  readonly documentRevision: number;
  /** The generation the frame is bound to now. */
  readonly view: RuntimeView;
  /**
   * Whether the preview follows the latest revision. Refreshing a preview of
   * a chosen revision loads that revision again, not the saved one.
   */
  readonly following: boolean;
  /**
   * Loads the latest revision in a new view. A caller that cannot load it
   * leaves it out, and the notice then only reports that a newer one exists.
   */
  readonly onLoadNewer?: (() => void) | undefined;
}

/** What the workspace says beside a preview it adopted (decision 0054, rules 8 and 12). */
export function AdoptionNotice({
  documentRevision,
  view,
  following,
  onLoadNewer,
}: AdoptionNoticeProps) {
  // Only the frame's own save moves the pinned revision away from the
  // document's (decision 0054, rule 5), so this is the save it reports.
  const kept = view.revision !== documentRevision;
  const newer = view.head_revision > view.revision;
  if (!kept && !newer) return null;
  return (
    <div className="flex flex-col gap-3">
      {kept ? (
        <Alert>
          <AlertTitle>Preview kept running across a save</AlertTitle>
          <AlertDescription>
            This preview still runs the page of revision {documentRevision}; its
            requests now use revision {view.revision}, which it saved.{' '}
            {following
              ? 'Refresh the preview to load the saved revision.'
              : 'Show the latest revision to load the saved one.'}
          </AlertDescription>
        </Alert>
      ) : null}
      {newer ? (
        <Alert>
          <AlertTitle>A newer revision exists</AlertTitle>
          <AlertDescription>
            <p>
              Revision {view.head_revision} is newer than revision{' '}
              {view.revision}, which this preview runs.
            </p>
            {onLoadNewer === undefined ? null : (
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={onLoadNewer}
              >
                Load revision {view.head_revision}
              </Button>
            )}
          </AlertDescription>
        </Alert>
      ) : null}
    </div>
  );
}
