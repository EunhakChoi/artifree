import { useState } from 'react';
import type { ReactNode } from 'react';
import { ArchiveRestoreIcon, RotateCwIcon } from 'lucide-react';
import { ArtifreeApiError } from '@artifree/client';
import type { ArtifreeClient } from '@artifree/client';
import type { Artifact } from '@artifree/contracts';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import {
  WriteNotice,
  retryable,
  useConditionalWrite,
  useWriteGate,
} from './conditional-write';
import type { Version } from './use-live-workspace';

const CHANGED = 'The artifact';

interface RecoveryProps {
  client: ArtifreeClient;
  /** How often the server's data may have been replaced so far. */
  resets: number;
  /** Called with the artifact as the recovery left it. */
  onRecovered: (artifact: Artifact) => void;
}

/**
 * Recovers one artifact from the trash: a metadata update with `deleted:
 * false` against `base` (decision 0014, item 3). Each artifact has a gate
 * of its own, so what a conflict proves of one holds back no other. The
 * same request sent again after a lost answer reuses its key.
 */
function useRecovery(
  { client, resets, onRecovered }: RecoveryProps,
  id: string,
  base: number,
  /** The newest metadata revision the page knows the artifact has. */
  known: number | undefined,
  /** The page's count of possible data replacements when `base` was read. */
  shownResets: number,
  onConflict: (current: number | undefined) => void,
) {
  const gate = useWriteGate(resets);
  const recovery = useConditionalWrite({
    gate,
    send: (
      request: { base_metadata_revision: number; deleted: false },
      idempotencyKey: string,
    ) => client.updateArtifact(id, request, { idempotencyKey }),
    base: (request) => request.base_metadata_revision,
    conflict: 'metadata_conflict',
    current: (error) => error.currentMetadataRevision,
    onDone: (result) => onRecovered(result.artifact),
    onConflict,
  });
  const waiting = gate.wait(base, known, shownResets);
  const retry = retryable(recovery.state, base, waiting);
  return {
    state: recovery.state,
    // Until the page has read the trash again after a possible
    // replacement, nothing is said about a wait.
    waiting: retry || waiting === 'replaced' ? undefined : waiting,
    sendable: retry || waiting === undefined,
    recover: () =>
      void recovery.write({ base_metadata_revision: base, deleted: false }),
  };
}

function RecoverButton({
  label,
  sending,
  sendable,
  onClick,
}: {
  label: string | undefined;
  sending: boolean;
  sendable: boolean;
  onClick: () => void;
}) {
  return (
    <Button
      type="button"
      variant="outline"
      size="sm"
      aria-label={label}
      disabled={!sendable}
      onClick={onClick}
    >
      <ArchiveRestoreIcon data-icon="inline-start" aria-hidden="true" />
      {sending ? 'Recovering…' : 'Recover'}
    </Button>
  );
}

interface TrashNavProps extends RecoveryProps {
  /** The artifacts in the trash, as the list read them. */
  artifacts: readonly Artifact[];
  /** The page's count of possible data replacements when they were read. */
  shownResets: number;
  /** The newest version the page knows per artifact. */
  known: Readonly<Record<string, Version>>;
  /** Called when another change won; the list is read again. */
  onConflict: () => void;
  /** Shown instead of the items when nothing is listed. */
  empty: ReactNode;
  /** Offered when more artifacts follow the ones shown. */
  onShowMore: (() => void) | undefined;
  children?: ReactNode;
}

/**
 * The artifacts in the trash (decision 0014, item 3). None can be read, so
 * none opens; each can be recovered.
 */
export function TrashNav({
  artifacts,
  shownResets,
  known,
  onConflict,
  empty,
  onShowMore,
  children,
  ...recovery
}: TrashNavProps) {
  return (
    <nav aria-label="Trash" className="flex flex-col gap-2">
      {children}
      {artifacts.length === 0 ? empty : null}
      <ul className="flex flex-col gap-2">
        {artifacts.map((artifact) => (
          <TrashItem
            key={artifact.id}
            {...recovery}
            artifact={artifact}
            known={known[artifact.id]?.metadata}
            shownResets={shownResets}
            onConflict={onConflict}
          />
        ))}
      </ul>
      {onShowMore === undefined ? null : (
        <Button type="button" variant="outline" onClick={onShowMore}>
          Show more
        </Button>
      )}
    </nav>
  );
}

function TrashItem({
  artifact,
  known,
  shownResets,
  onConflict,
  ...props
}: RecoveryProps & {
  artifact: Artifact;
  known: number | undefined;
  shownResets: number;
  onConflict: () => void;
}) {
  const recovery = useRecovery(
    props,
    artifact.id,
    artifact.metadata_revision,
    known,
    shownResets,
    onConflict,
  );
  return (
    <li className="flex flex-col gap-2">
      <div className="flex items-center justify-between gap-2">
        <span className="truncate text-sm">{artifact.title}</span>
        <RecoverButton
          label={`Recover ${artifact.title}`}
          sending={recovery.state.status === 'sending'}
          sendable={recovery.sendable}
          onClick={recovery.recover}
        />
      </div>
      <WriteNotice
        state={recovery.state}
        action="recover it"
        changed={CHANGED}
        behind={false}
        waiting={recovery.waiting}
      />
    </li>
  );
}

interface RecoverNoticeProps extends RecoveryProps {
  artifactId: string;
  /** The metadata revision the page last saw the artifact in the trash at. */
  base: number;
  /** Called with the artifact when a read finds it outside the trash. */
  onFound: (artifact: Artifact) => void;
  /** Called when a refusal and a read show it in the trash at `metadata`. */
  onStillInTrash: (metadata: number) => void;
}

/**
 * Said by an open artifact the page knows is in the trash while the editor
 * holds a draft (decision 0014, item 6). The trash cannot be shown without
 * closing the draft, so the recovery is offered here, against the metadata
 * revision the page learned the move from. The page forgets what it learned
 * of the trash whenever the server's data may have been replaced, so that
 * revision is never older than the last replacement.
 *
 * A refusal names the current metadata revision, but not whether the
 * artifact is still in the trash, and the change feed that would say so may
 * be down. So the page reads the artifact once: found, it left the trash;
 * not found, it is still there at the revision the refusal named, which the
 * next recovery is sent against.
 */
export function RecoverNotice({
  artifactId,
  base,
  onFound,
  onStillInTrash,
  ...props
}: RecoverNoticeProps) {
  const [probe, setProbe] = useState<
    | { readonly status: 'reading' }
    | { readonly status: 'failed'; readonly current: number | undefined }
  >();
  async function read(current: number | undefined): Promise<void> {
    setProbe({ status: 'reading' });
    try {
      const artifact = await props.client.getArtifact(artifactId);
      setProbe(undefined);
      onFound(artifact);
    } catch (error: unknown) {
      if (error instanceof ArtifreeApiError && error.code === 'not_found') {
        setProbe(undefined);
        if (current !== undefined) onStillInTrash(current);
      } else setProbe({ status: 'failed', current });
    }
  }
  const recovery = useRecovery(
    props,
    artifactId,
    base,
    undefined,
    props.resets,
    (current) => void read(current),
  );
  return (
    <Alert variant="destructive">
      <AlertTitle>Moved to the trash elsewhere</AlertTitle>
      <AlertDescription>
        <p>
          This artifact is in the trash, so your changes cannot be saved unless
          it is recovered. They stay here until you save or discard them.
        </p>
        <RecoverButton
          label={undefined}
          sending={recovery.state.status === 'sending'}
          sendable={recovery.sendable && probe === undefined}
          onClick={recovery.recover}
        />
        <WriteNotice
          state={recovery.state}
          action="recover it"
          changed={CHANGED}
          behind={false}
          waiting={recovery.waiting}
        />
        {probe?.status === 'failed' ? (
          <div className="flex flex-col items-start gap-2">
            <p>Could not read whether it is still in the trash.</p>
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() => void read(probe.current)}
            >
              <RotateCwIcon data-icon="inline-start" aria-hidden="true" />
              Try again
            </Button>
          </div>
        ) : null}
      </AlertDescription>
    </Alert>
  );
}
