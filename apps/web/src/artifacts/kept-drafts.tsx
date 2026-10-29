import { useState } from 'react';
import { FolderOpenIcon, Trash2Icon } from 'lucide-react';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import { discardKept } from './draft-keeper';
import type { DraftStore } from './draft-store';
import { describeFailure, useRequest } from './use-request';

interface KeptDraftsProps {
  store: DraftStore;
  /** The open artifact, whose kept draft its source view offers. */
  openId: string | undefined;
  /** Whether the open artifact has unsaved changes, which lock choosing. */
  locked: boolean;
  onOpen: (artifactId: string) => void;
}

/**
 * Names the drafts this tab may continue for artifacts other than the open
 * one, so a draft kept before a reload can be found (decision 0018).
 */
export function KeptDrafts({ store, openId, locked, onOpen }: KeptDraftsProps) {
  const [checks, setChecks] = useState(0);
  const [failure, setFailure] = useState<string>();
  const kept = useRequest(
    `${openId ?? ''}:${locked}:${checks}`,
    async () =>
      (await store.list()).filter((record) => record.artifactId !== openId),
    { keepPrevious: true },
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
                disabled={locked}
                onClick={() => onOpen(record.artifactId)}
              >
                <FolderOpenIcon data-icon="inline-start" aria-hidden="true" />
                Open {record.title}
              </Button>
              <Button
                type="button"
                variant="ghost"
                size="sm"
                onClick={() => {
                  discardKept(store, record).then(
                    () => setChecks((value) => value + 1),
                    (error: unknown) => setFailure(describeFailure(error)),
                  );
                }}
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
