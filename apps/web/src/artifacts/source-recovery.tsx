import { useEffect, useEffectEvent, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import { RotateCwIcon, Trash2Icon } from 'lucide-react';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import type { DraftRecord, KeptDraft } from './draft-store';
import type { PageDrafts } from './page-drafts';
import { describeFailure, useRequest } from './use-request';

/** What a source view keeps of its draft in the browser (decision 0018). */
export interface Keeping {
  /** The kept draft the view continues, if the user chose one. */
  readonly start: KeptDraft | undefined;
  /** Why the last write failed, until one succeeds. */
  readonly failure: string | undefined;
  /** Keeps or forgets the draft, returning a failure if the write failed. */
  keep(record: DraftRecord | undefined): Promise<string | undefined>;
}

interface SourceRecoveryProps {
  drafts: PageDrafts;
  artifactId: string;
  onUnsavedChange: (unsaved: boolean) => void;
  /** The source view, which starts once a kept draft was looked for. */
  children: (keeping: Keeping) => ReactNode;
}

/**
 * Looks for a draft this page may continue before the source view starts,
 * and offers to recover or discard it. Nothing is edited until the lookup
 * found nothing or the user chose, since editing would replace a kept
 * draft of this page.
 */
export function SourceRecovery({
  drafts,
  artifactId,
  onUnsavedChange,
  children,
}: SourceRecoveryProps) {
  const [chosen, setChosen] = useState<{ start: KeptDraft | undefined }>();
  const [lookups, setLookups] = useState(0);
  const [failure, setFailure] = useState<string>();
  const [discardFailure, setDiscardFailure] = useState<string>();
  const [discarding, setDiscarding] = useState(false);
  const discardInFlight = useRef(false);
  // The recovery view exists before its editor and owns the workspace lock
  // until this whole view leaves. An editor mounted after a choice may have
  // its effects replayed in development without ending that choice.
  const release = useEffectEvent(() => onUnsavedChange(false));
  useEffect(() => () => release(), []);
  const kept = useRequest(
    chosen === undefined ? `${artifactId}:${lookups}` : undefined,
    async () =>
      (await drafts.list()).find((record) => record.artifactId === artifactId),
  );

  async function keep(
    record: DraftRecord | undefined,
  ): Promise<string | undefined> {
    const result = await drafts.keep(artifactId, record);
    setFailure(result);
    return result;
  }

  function recover(record: KeptDraft): void {
    if (discardInFlight.current) return;
    onUnsavedChange(true);
    setChosen({ start: record });
    // Rewritten as this page's; another tab's record is removed only with
    // that write.
    void drafts
      .keep(
        artifactId,
        { ...record, keptAt: Date.now() },
        record.mine ? undefined : record,
      )
      .then(setFailure);
  }

  async function discard(record: KeptDraft): Promise<void> {
    if (discardInFlight.current) return;
    discardInFlight.current = true;
    setDiscarding(true);
    setDiscardFailure(undefined);
    try {
      await drafts.discard(record);
    } catch (error: unknown) {
      setDiscardFailure(describeFailure(error));
      return;
    } finally {
      discardInFlight.current = false;
      setDiscarding(false);
    }
    setLookups((value) => value + 1);
  }

  if (chosen === undefined) {
    if (kept.status === 'loading')
      return (
        <output className="text-sm text-muted-foreground">
          Looking for a draft kept in this browser…
        </output>
      );
    if (kept.status === 'error')
      return (
        <LookupFailed
          message={kept.message}
          onRetry={() => setLookups((value) => value + 1)}
          onSkip={() => setChosen({ start: undefined })}
        />
      );
    const record = kept.value;
    if (record !== undefined)
      return (
        <KeptChoice
          record={record}
          failure={discardFailure}
          discarding={discarding}
          onRecover={() => recover(record)}
          onDiscard={() => void discard(record)}
        />
      );
  }
  return children({ start: chosen?.start, failure, keep });
}

/** Offers a kept draft: continue it, or discard it. */
function KeptChoice({
  record,
  failure,
  discarding,
  onRecover,
  onDiscard,
}: {
  record: KeptDraft;
  /** Why the last discard failed, if it did. */
  failure: string | undefined;
  discarding: boolean;
  onRecover: () => void;
  onDiscard: () => void;
}) {
  const changes = record.draft.changes.size;
  return (
    <Alert>
      <AlertTitle>A draft of this artifact was kept in this browser</AlertTitle>
      <AlertDescription>
        <p>
          {changes === 1 ? '1 change' : `${changes} changes`} to revision{' '}
          {record.draft.base}, kept {new Date(record.keptAt).toLocaleString()}.
          {record.uncertain
            ? ' A save of it was sent and its answer was not received.'
            : null}
        </p>
        <div className="flex flex-wrap gap-2">
          <Button
            type="button"
            size="sm"
            disabled={discarding}
            onClick={onRecover}
          >
            <RotateCwIcon data-icon="inline-start" aria-hidden="true" />
            Recover draft
          </Button>
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={discarding}
            onClick={onDiscard}
          >
            <Trash2Icon data-icon="inline-start" aria-hidden="true" />
            Discard kept draft
          </Button>
        </div>
        {failure === undefined ? null : (
          <p>The draft was not discarded. {failure}</p>
        )}
      </AlertDescription>
    </Alert>
  );
}

/** Says the kept drafts could not be read, before anything is edited. */
function LookupFailed({
  message,
  onRetry,
  onSkip,
}: {
  message: string;
  onRetry: () => void;
  onSkip: () => void;
}) {
  return (
    <Alert variant="destructive">
      <AlertTitle>Drafts kept in this browser could not be read</AlertTitle>
      <AlertDescription>
        <p>
          {message} Editing without them may replace a draft of this artifact
          kept earlier in this tab.
        </p>
        <div className="flex flex-wrap gap-2">
          <Button type="button" size="sm" onClick={onRetry}>
            <RotateCwIcon data-icon="inline-start" aria-hidden="true" />
            Try again
          </Button>
          <Button type="button" variant="outline" size="sm" onClick={onSkip}>
            Edit without them
          </Button>
        </div>
      </AlertDescription>
    </Alert>
  );
}
