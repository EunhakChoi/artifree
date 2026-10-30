import { useRef, useState } from 'react';
import { FolderOpenIcon, RotateCwIcon, Trash2Icon } from 'lucide-react';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import type { KeptDraft } from './draft-store';
import type { PageDrafts } from './page-drafts';
import { describeFailure, useRequest } from './use-request';

interface KeptDraftsProps {
  drafts: PageDrafts;
  /** The open artifact, whose kept draft its source view offers. */
  openId: string | undefined;
  /** Whether the open artifact has unsaved changes, which lock choosing. */
  locked: boolean;
  onOpen: (artifactId: string) => void;
}

/**
 * Names the drafts this page may continue for artifacts other than the
 * open one, since a reload opens the first listed artifact (decision 0018).
 */
export function KeptDrafts({
  drafts,
  openId,
  locked,
  onOpen,
}: KeptDraftsProps) {
  const [discards, setDiscards] = useState(0);
  const [lookups, setLookups] = useState(0);
  const [failure, setFailure] = useState<string>();
  const [discarding, setDiscarding] = useState(false);
  const discardInFlight = useRef(false);
  const kept = useRequest(
    `${openId ?? ''}:${locked}:${discards}:${lookups}`,
    async () =>
      (await drafts.list()).filter((record) => record.artifactId !== openId),
  );
  async function discard(record: KeptDraft): Promise<void> {
    if (discardInFlight.current) return;
    discardInFlight.current = true;
    setDiscarding(true);
    setFailure(undefined);
    try {
      await drafts.discard(record);
      setDiscards((value) => value + 1);
    } catch (error: unknown) {
      setFailure(
        `The draft of ${record.title} was not discarded. ${describeFailure(error)}`,
      );
    } finally {
      discardInFlight.current = false;
      setDiscarding(false);
    }
  }
  if (kept.status === 'error')
    return (
      <Alert variant="destructive">
        <AlertTitle>Drafts kept in this browser could not be listed</AlertTitle>
        <AlertDescription>
          <p>
            {kept.message} Try again to see drafts kept for other artifacts.
          </p>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => setLookups((value) => value + 1)}
          >
            <RotateCwIcon data-icon="inline-start" aria-hidden="true" />
            Try listing again
          </Button>
        </AlertDescription>
      </Alert>
    );
  if (kept.status !== 'ready' || kept.value.length === 0) return null;
  return (
    <Alert>
      <AlertTitle>Drafts kept in this browser</AlertTitle>
      <AlertDescription>
        <ul className="flex flex-col gap-2">
          {kept.value.map((record) => (
            <li
              key={`${record.owner}:${record.artifactId}`}
              className="flex flex-wrap items-center gap-2"
            >
              <span>
                {record.title}, revision {record.draft.base}
              </span>
              <Button
                type="button"
                variant="outline"
                size="sm"
                disabled={locked || discarding}
                onClick={() => {
                  if (!discardInFlight.current) onOpen(record.artifactId);
                }}
              >
                <FolderOpenIcon data-icon="inline-start" aria-hidden="true" />
                Open {record.title}
              </Button>
              <Button
                type="button"
                variant="ghost"
                size="sm"
                disabled={discarding}
                onClick={() => void discard(record)}
              >
                <Trash2Icon data-icon="inline-start" aria-hidden="true" />
                Discard draft of {record.title}
              </Button>
            </li>
          ))}
        </ul>
        {failure === undefined ? null : <p>{failure}</p>}
        {locked ? <p>Save or discard the open draft to open another.</p> : null}
      </AlertDescription>
    </Alert>
  );
}
