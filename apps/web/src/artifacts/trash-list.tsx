import type { ReactNode } from 'react';
import { ArchiveRestoreIcon } from 'lucide-react';
import type { ArtifreeClient } from '@artifree/client';
import type { Artifact } from '@artifree/contracts';
import { Button } from '@/components/ui/button';
import {
  WriteNotice,
  lostBase,
  useConditionalWrite,
  useWriteGate,
} from './conditional-write';
import type { Version } from './use-live-workspace';
import { preparedBase } from './write-gate';

/** A recovery: the artifact and the metadata revision the list read. */
interface Recovery {
  readonly id: string;
  readonly base: number;
}

interface TrashNavProps {
  client: ArtifreeClient;
  /** The artifacts in the trash, as one list read showed them. */
  artifacts: readonly Artifact[];
  /** How often the server's data may have been replaced so far. */
  resets: number;
  /** The count when the list was read. */
  shownResets: number;
  /** The newest versions the page knows of. */
  known: Readonly<Record<string, Version>>;
  /** Called with the artifact as the recovery left it. */
  onRecovered: (artifact: Artifact) => void;
  /** Called when another change won; the list is read again. */
  onConflict: () => void;
  /** Shown instead of the items when nothing matches. */
  empty: ReactNode;
  /** Offered when more artifacts follow the ones shown. */
  onShowMore: (() => void) | undefined;
  children?: ReactNode;
}

/**
 * The trash (decision 0014, item 5). Its artifacts cannot be opened; each
 * can be recovered against the metadata revision the list read.
 */
export function TrashNav({
  client,
  artifacts,
  resets,
  shownResets,
  known,
  onRecovered,
  onConflict,
  empty,
  onShowMore,
  children,
}: TrashNavProps) {
  return (
    <nav aria-label="Trash" className="flex flex-col gap-2">
      {children}
      {shownResets === resets ? null : (
        <output className="text-sm text-muted-foreground">
          Reading the trash again: the server&apos;s data may have changed
          wholesale. Recovering waits until it is read.
        </output>
      )}
      {artifacts.length === 0 ? empty : null}
      {artifacts.map((artifact) => (
        <TrashItem
          key={artifact.id}
          client={client}
          artifact={artifact}
          resets={resets}
          shownResets={shownResets}
          known={known[artifact.id]?.metadata}
          onRecovered={onRecovered}
          onConflict={onConflict}
        />
      ))}
      {onShowMore === undefined ? null : (
        <Button type="button" variant="outline" onClick={onShowMore}>
          Show more
        </Button>
      )}
    </nav>
  );
}

interface TrashItemProps {
  client: ArtifreeClient;
  /** The artifact as the list read it. */
  artifact: Artifact;
  resets: number;
  shownResets: number;
  /** The newest metadata revision the page knows of for it. */
  known: number | undefined;
  onRecovered: (artifact: Artifact) => void;
  onConflict: () => void;
}

/**
 * One artifact in the trash and its recovery, through a gate of its own
 * with the rules of decision 0011, item 6: what a refused recovery proves
 * about this artifact says nothing about the others.
 */
function TrashItem({
  client,
  artifact,
  resets,
  shownResets,
  known,
  onRecovered,
  onConflict,
}: TrashItemProps) {
  const gate = useWriteGate(resets);
  const recover = useConditionalWrite({
    gate,
    send: (request: Recovery, idempotencyKey: string) =>
      client.updateArtifact(
        request.id,
        { base_metadata_revision: request.base, deleted: false },
        { idempotencyKey },
      ),
    base: (request) => request.base,
    conflict: 'metadata_conflict',
    current: (error) => error.currentMetadataRevision,
    onDone: (result) => onRecovered(result.artifact),
    onConflict,
  });
  const listed = artifact.metadata_revision;
  const wait = (version: number) => gate.wait(version, known, shownResets);
  const { base, retry } = preparedBase(
    listed,
    listed,
    wait,
    lostBase(recover.state),
  );
  const waiting = retry ? undefined : wait(base);
  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center justify-between gap-2">
        <span className="min-w-0 truncate text-sm" title={artifact.title}>
          {artifact.title}
        </span>
        <Button
          type="button"
          variant="outline"
          size="sm"
          aria-label={`Recover ${artifact.title}`}
          disabled={waiting !== undefined}
          onClick={() => void recover.write({ id: artifact.id, base })}
        >
          <ArchiveRestoreIcon data-icon="inline-start" aria-hidden="true" />
          Recover
        </Button>
      </div>
      <WriteNotice
        state={recover.state}
        action="recover it"
        changed="It"
        behind={false}
        waiting={waiting === 'replaced' ? undefined : waiting}
      />
    </div>
  );
}
