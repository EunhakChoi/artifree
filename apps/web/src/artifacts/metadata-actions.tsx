import { useState } from 'react';
import type { FormEvent } from 'react';
import { PencilIcon, PinIcon, Trash2Icon } from 'lucide-react';
import type { ArtifreeClient } from '@artifree/client';
import type { Artifact, UpdateArtifactRequest } from '@artifree/contracts';
import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from '@/components/ui/dialog';
import {
  Field,
  FieldDescription,
  FieldGroup,
  FieldLabel,
} from '@/components/ui/field';
import { Input } from '@/components/ui/input';
import {
  ReplacedNotice,
  WriteNotice,
  retryable,
  useConditionalWrite,
} from './conditional-write';
import type { WriteGate } from './conditional-write';
import { prepare } from './write-gate';
import type { Wait } from './write-gate';

const CHANGED = 'The title or pin';

/** Renames or pins one artifact (decision 0008) through the metadata gate. */
function useMetadataUpdate(
  client: ArtifreeClient,
  artifactId: string,
  gate: WriteGate,
  onUpdated: (artifact: Artifact) => void,
  onConflict: () => void,
) {
  return useConditionalWrite({
    gate,
    send: (request: UpdateArtifactRequest, idempotencyKey: string) =>
      client.updateArtifact(artifactId, request, { idempotencyKey }),
    base: (request) => request.base_metadata_revision,
    conflict: 'metadata_conflict',
    current: (error) => error.currentMetadataRevision,
    onDone: (result) => onUpdated(result.artifact),
    onConflict,
  });
}

/** A wait that the card already explains. */
function explained(wait: Wait | undefined): Wait | undefined {
  return wait === 'replaced' ? undefined : wait;
}

interface MetadataActionsProps {
  client: ArtifreeClient;
  artifact: Artifact;
  /** Shared by pin and rename, which name the same metadata revision. */
  gate: WriteGate;
  /** Why a metadata write prepared against a revision would wait. */
  wait: (metadataRevision: number) => Wait | undefined;
  /** How often the server's data may have been replaced so far. */
  resets: number;
  /** Called with the artifact as a rename or pin left it. */
  onUpdated: (artifact: Artifact) => void;
  /** Called with the artifact as the move to the trash left it. */
  onDeleted: (artifact: Artifact) => void;
  /** Called when another update won; the view reads the latest. */
  onConflict: () => void;
  /**
   * Whether the editor holds unsaved changes, which a move to the trash
   * would close (decision 0014, item 3).
   */
  unsaved: boolean;
}

/**
 * Pins, renames, and moves the open artifact to the trash. Pin and rename
 * create no source revision, so they stay available while the editor holds
 * a draft.
 */
export function MetadataActions(props: MetadataActionsProps) {
  const { client, artifact, gate, wait, onUpdated, onConflict } = props;
  const pin = useMetadataUpdate(
    client,
    artifact.id,
    gate,
    onUpdated,
    onConflict,
  );
  const version = artifact.metadata_revision;
  const waiting = wait(version);
  // Once the page shows newer metadata, it shows what a lost update did,
  // and pinning again is a new request.
  const outcome =
    pin.state.status === 'failed' &&
    pin.state.unknown &&
    pin.state.base !== version
      ? { status: 'idle' as const }
      : pin.state;
  return (
    <div className="flex flex-col items-end gap-2">
      <div className="flex flex-wrap justify-end gap-2">
        <Button
          type="button"
          variant="outline"
          size="sm"
          aria-pressed={artifact.pinned}
          disabled={waiting !== undefined}
          onClick={() =>
            void pin.write({
              base_metadata_revision: version,
              pinned: !artifact.pinned,
            })
          }
        >
          <PinIcon data-icon="inline-start" aria-hidden="true" />
          Pin
        </Button>
        <RenameDialog {...props} />
        <TrashDialog {...props} />
      </div>
      <WriteNotice
        state={outcome}
        action="pin or unpin"
        changed={CHANGED}
        behind={false}
        waiting={explained(waiting)}
      />
      {props.unsaved ? (
        <FieldDescription id="trash-locked" className="text-right">
          Save or discard your changes to move this artifact to the trash, which
          closes it.
        </FieldDescription>
      ) : null}
    </div>
  );
}

/** What a rename is prepared from: the artifact as a read showed it. */
interface PreparedRename {
  readonly base: number;
  readonly title: string;
  /** The page's count of possible data replacements then. */
  readonly resets: number;
}

function RenameDialog({
  client,
  artifact,
  gate,
  wait,
  resets,
  onUpdated,
  onConflict,
}: MetadataActionsProps) {
  const latest = artifact.metadata_revision;
  const [open, setOpen] = useState(false);
  const [prepared, setPrepared] = useState<PreparedRename>({
    base: latest,
    title: artifact.title,
    resets,
  });
  // The title typed since the dialog opened, if any. Until the user types,
  // the field shows the title the request is prepared from.
  const [typed, setTyped] = useState<string>();
  const rename = useMetadataUpdate(
    client,
    artifact.id,
    gate,
    onUpdated,
    onConflict,
  );
  // A request is prepared from the artifact as the dialog opened on it, so
  // a retry after a lost answer is the same request. Once the page has
  // learned of newer metadata and shows it, the next request is prepared
  // from that instead, and the notice says why.
  const retry = retryable(rename.state, prepared.base, wait(prepared.base));
  const { from, behind } = prepare(
    prepared,
    { base: latest, title: artifact.title, resets },
    wait,
    retry,
  );
  const replaced = prepared.resets !== resets;
  const pending = rename.state.status === 'sending';
  const title = typed ?? from.title;
  const sendable =
    !replaced &&
    (retry || wait(from.base) === undefined) &&
    title.trim() !== '' &&
    title.trim() !== from.title;

  async function submit(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    if (!sendable) return;
    setPrepared(from);
    const request = { base_metadata_revision: from.base, title: title.trim() };
    if ((await rename.write(request)) === 'done') setOpen(false);
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        // Stay open until the request settles, so its outcome is seen.
        if (pending) return;
        if (next) {
          setPrepared({ base: latest, title: artifact.title, resets });
          setTyped(undefined);
          rename.clear();
        }
        setOpen(next);
      }}
    >
      <DialogTrigger
        render={
          <Button
            variant="outline"
            size="sm"
            disabled={wait(latest) === 'replaced'}
          />
        }
      >
        <PencilIcon data-icon="inline-start" aria-hidden="true" />
        Rename
      </DialogTrigger>
      <DialogContent>
        <form
          className="flex flex-col gap-4"
          onSubmit={(event) => void submit(event)}
        >
          <DialogHeader>
            <DialogTitle>Rename artifact</DialogTitle>
            <DialogDescription>
              The title changes without a new revision; drafts and previews stay
              as they are.
            </DialogDescription>
          </DialogHeader>
          <FieldGroup>
            <Field>
              <FieldLabel htmlFor="rename-title">Title</FieldLabel>
              <Input
                id="rename-title"
                required
                maxLength={200}
                readOnly={pending}
                value={title}
                onChange={(event) => {
                  setTyped(event.target.value);
                  rename.clear();
                }}
              />
            </Field>
          </FieldGroup>
          {/* Nothing is sent again for data that may have been replaced. */}
          {replaced ? (
            <ReplacedNotice state={rename.state} />
          ) : (
            <WriteNotice
              state={rename.state}
              action="rename"
              changed={CHANGED}
              behind={behind}
              waiting={explained(wait(from.base))}
            />
          )}
          <DialogFooter>
            <DialogClose
              render={<Button variant="outline" disabled={pending} />}
            >
              Cancel
            </DialogClose>
            <Button type="submit" disabled={!sendable}>
              {pending ? 'Renaming…' : 'Rename'}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/** What a move to the trash is prepared from. */
interface PreparedMove {
  readonly base: number;
  /** The page's count of possible data replacements then. */
  readonly resets: number;
}

/**
 * Moves the open artifact to the trash after a confirmation (decision
 * 0014, item 1): a metadata update with `deleted: true` through the same
 * gate and preparation as a rename.
 */
function TrashDialog({
  client,
  artifact,
  gate,
  wait,
  resets,
  onDeleted,
  onConflict,
  unsaved,
}: MetadataActionsProps) {
  const latest = artifact.metadata_revision;
  const [open, setOpen] = useState(false);
  const [prepared, setPrepared] = useState<PreparedMove>({
    base: latest,
    resets,
  });
  const move = useMetadataUpdate(
    client,
    artifact.id,
    gate,
    onDeleted,
    onConflict,
  );
  const retry = retryable(move.state, prepared.base, wait(prepared.base));
  const { from, behind } = prepare(
    prepared,
    { base: latest, resets },
    wait,
    retry,
  );
  const replaced = prepared.resets !== resets;
  const pending = move.state.status === 'sending';
  const waiting = retry ? undefined : wait(from.base);
  const sendable = !replaced && waiting === undefined;

  async function confirm(): Promise<void> {
    if (!sendable) return;
    setPrepared(from);
    const request = { base_metadata_revision: from.base, deleted: true };
    if ((await move.write(request)) === 'done') setOpen(false);
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        // Stay open until the request settles, so its outcome is seen.
        if (pending) return;
        if (next) {
          setPrepared({ base: latest, resets });
          move.clear();
        }
        setOpen(next);
      }}
    >
      <DialogTrigger
        render={
          <Button
            variant="outline"
            size="sm"
            disabled={unsaved || wait(latest) === 'replaced'}
            aria-describedby={unsaved ? 'trash-locked' : undefined}
          />
        }
      >
        <Trash2Icon data-icon="inline-start" aria-hidden="true" />
        Move to trash
      </DialogTrigger>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Move {artifact.title} to the trash</DialogTitle>
          <DialogDescription>
            It leaves the list, and nothing can read, preview, or change it
            until it is recovered from the trash. Its revisions are kept.
          </DialogDescription>
        </DialogHeader>
        {/* Nothing is sent again for data that may have been replaced. */}
        {replaced ? (
          <ReplacedNotice state={move.state} />
        ) : (
          <WriteNotice
            state={move.state}
            action="move it to the trash"
            changed={CHANGED}
            behind={behind}
            waiting={explained(waiting)}
          />
        )}
        <DialogFooter>
          <DialogClose render={<Button variant="outline" disabled={pending} />}>
            Cancel
          </DialogClose>
          <Button
            type="button"
            disabled={!sendable}
            onClick={() => void confirm()}
          >
            {pending ? 'Moving…' : 'Move to trash'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
