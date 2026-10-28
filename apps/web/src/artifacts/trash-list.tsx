import type { ReactNode } from 'react';
import { ArchiveRestoreIcon } from 'lucide-react';
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
  /** Called when another change won. */
  onConflict: () => void;
}

/**
 * Recovers one artifact from the trash: a metadata update with `deleted:
 * false` against `base` (decision 0014, items 5 and 6). Each artifact has
 * a gate of its own, so what a conflict proves of one holds back no other.
 * The same request sent again after a lost answer reuses its key.
 */
function useRecovery(
  props: RecoveryProps,
  id: string,
  base: number,
  /** The newest metadata revision the page knows the artifact has. */
  known: number | undefined,
  /** The page's count of possible data replacements when `base` was read. */
  shownResets: number,
) {
  const { client, resets, onRecovered, onConflict } = props;
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
    // What the page shows of the artifact is replaced once it has read the
    // trash again; nothing is said about a wait until then.
    waiting: retry || waiting === 'replaced' ? undefined : waiting,
    sendable: retry || waiting === undefined,
    recover: () =>
      void recovery.write({ base_metadata_revision: base, deleted: false }),
  };
}

interface TrashNavProps extends RecoveryProps {
  /** The artifacts in the trash, as the list read them. */
  artifacts: readonly Artifact[];
  /** The page's count of possible data replacements when they were read. */
  shownResets: number;
  /** The newest version the page knows per artifact. */
  known: Readonly<Record<string, Version>>;
  /** Shown instead of the items when nothing matches. */
  empty: ReactNode;
  /** Offered when more artifacts follow the ones shown. */
  onShowMore: (() => void) | undefined;
  children?: ReactNode;
}

/**
 * The artifacts in the trash (decision 0014, item 5). They cannot be read,
 * so none opens; each can be recovered.
 */
export function TrashNav({
  artifacts,
  shownResets,
  known,
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
  ...props
}: RecoveryProps & {
  artifact: Artifact;
  known: number | undefined;
  shownResets: number;
}) {
  const recovery = useRecovery(
    props,
    artifact.id,
    artifact.metadata_revision,
    known,
    shownResets,
  );
  return (
    <li className="flex flex-col gap-2">
      <div className="flex items-center justify-between gap-2">
        <span className="truncate text-sm">{artifact.title}</span>
        <Button
          type="button"
          variant="outline"
          size="sm"
          aria-label={`Recover ${artifact.title}`}
          disabled={!recovery.sendable}
          onClick={recovery.recover}
        >
          <ArchiveRestoreIcon data-icon="inline-start" aria-hidden="true" />
          {recovery.state.status === 'sending' ? 'Recovering…' : 'Recover'}
        </Button>
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

/**
 * Said by an open artifact the page knows went to the trash while the
 * editor holds a draft (decision 0014, item 6). The trash cannot be shown
 * without closing the draft, so the recovery is offered here. Its base is
 * the metadata revision the page learned the move from; the page forgets
 * what it learned of the trash whenever the server's data may have been
 * replaced, so that revision is never older than the last replacement.
 */
export function RecoverNotice({
  artifactId,
  base,
  ...props
}: RecoveryProps & { artifactId: string; base: number }) {
  const recovery = useRecovery(
    props,
    artifactId,
    base,
    undefined,
    props.resets,
  );
  return (
    <Alert variant="destructive">
      <AlertTitle>Moved to the trash elsewhere</AlertTitle>
      <AlertDescription>
        <p>
          This artifact is in the trash, so your changes cannot be saved unless
          it is recovered. They stay here until you save or discard them.
        </p>
        <Button
          type="button"
          variant="outline"
          size="sm"
          disabled={!recovery.sendable}
          onClick={recovery.recover}
        >
          <ArchiveRestoreIcon data-icon="inline-start" aria-hidden="true" />
          {recovery.state.status === 'sending' ? 'Recovering…' : 'Recover'}
        </Button>
        <WriteNotice
          state={recovery.state}
          action="recover it"
          changed={CHANGED}
          behind={false}
          waiting={recovery.waiting}
        />
      </AlertDescription>
    </Alert>
  );
}
