import { useState } from 'react';
import type { FormEvent } from 'react';
import {
  ArchiveRestoreIcon,
  PencilIcon,
  PinIcon,
  Trash2Icon,
} from 'lucide-react';
import type { ArtifreeClient } from '@artifree/client';
import type { Artifact, UpdateArtifactRequest } from '@artifree/contracts';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
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
  lostBase,
  useConditionalWrite,
} from './conditional-write';
import type { WriteGate } from './conditional-write';
import { preparedBase } from './write-gate';
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
  /** Called with the artifact as the update left it. */
  onUpdated: (artifact: Artifact) => void;
  /** Called with the artifact as the page's move to the trash left it. */
  onDeleted: (artifact: Artifact) => void;
  /** Called when another update won; the view reads the latest. */
  onConflict: () => void;
  /** Whether the editor holds unsaved changes. */
  unsaved: boolean;
}

/**
 * Pins, renames, and moves the open artifact to the trash. Pin and rename
 * create no source revision, so they stay available while the editor holds
 * a draft; the trash does not (decision 0014, item 3).
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
      {props.unsaved ? (
        <FieldDescription id="trash-locked-artifact" className="text-right">
          Save or discard your changes to move this artifact to the trash.
        </FieldDescription>
      ) : null}
      <WriteNotice
        state={outcome}
        action="pin or unpin"
        changed={CHANGED}
        behind={false}
        waiting={explained(waiting)}
      />
    </div>
  );
}

/** What a rename is prepared from: the artifact as a read showed it. */
interface Prepared {
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
  const [prepared, setPrepared] = useState<Prepared>({
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
  const { base, behind, retry } = preparedBase(
    prepared.base,
    latest,
    wait,
    lostBase(rename.state),
  );
  // A request prepared from the latest offers the title read with it.
  const from: Prepared =
    base === prepared.base ? prepared : { base, title: artifact.title, resets };
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

/**
 * Moves the open artifact to the trash after a confirmation (decision 0014,
 * item 1): a metadata update through the gate pin and rename share, with
 * the base the dialog opened on.
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
  const [prepared, setPrepared] = useState({ base: latest, resets });
  const trash = useMetadataUpdate(
    client,
    artifact.id,
    gate,
    onDeleted,
    onConflict,
  );
  const { base, behind, retry } = preparedBase(
    prepared.base,
    latest,
    wait,
    lostBase(trash.state),
  );
  const replaced = prepared.resets !== resets;
  const pending = trash.state.status === 'sending';
  const waiting = retry ? undefined : wait(base);

  async function confirm(): Promise<void> {
    if (replaced || waiting !== undefined) return;
    setPrepared({ base, resets: prepared.resets });
    const request = { base_metadata_revision: base, deleted: true };
    if ((await trash.write(request)) === 'done') setOpen(false);
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        // Stay open until the request settles, so its outcome is seen.
        if (pending) return;
        if (next) {
          setPrepared({ base: latest, resets });
          trash.clear();
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
            aria-describedby={unsaved ? 'trash-locked-artifact' : undefined}
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
            It leaves the list, and nothing can read, preview, or change it,
            agents included, until it is recovered from the trash. Nothing is
            removed: its revisions, title, and pin stay as they are.
          </DialogDescription>
        </DialogHeader>
        {/* Nothing is sent again for data that may have been replaced. */}
        {replaced ? (
          <ReplacedNotice state={trash.state} />
        ) : (
          <WriteNotice
            state={trash.state}
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
            variant="destructive"
            disabled={replaced || waiting !== undefined}
            onClick={() => void confirm()}
          >
            {pending ? 'Moving…' : 'Move to trash'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

interface RecoverNoticeProps {
  client: ArtifreeClient;
  artifactId: string;
  /** The metadata gate pin and rename share. */
  gate: WriteGate;
  /** Why a metadata write prepared against a revision would wait. */
  wait: (metadataRevision: number) => Wait | undefined;
  /** The metadata revision the page knows the move to the trash from. */
  base: number;
  /** Whether the editor holds unsaved changes. */
  unsaved: boolean;
  /** Called with the artifact as the recovery left it. */
  onRecovered: (artifact: Artifact) => void;
  /** Called when another update won. */
  onConflict: () => void;
}

/**
 * Said by an open artifact that was moved to the trash elsewhere (decision
 * 0014, item 6). It cannot be read, so its recovery is prepared from the
 * metadata revision the page knows the move from, the only one it can know.
 */
export function RecoverNotice({
  client,
  artifactId,
  gate,
  wait,
  base,
  unsaved,
  onRecovered,
  onConflict,
}: RecoverNoticeProps) {
  const recover = useMetadataUpdate(
    client,
    artifactId,
    gate,
    onRecovered,
    onConflict,
  );
  const { retry } = preparedBase(base, base, wait, lostBase(recover.state));
  const waiting = retry ? undefined : wait(base);
  return (
    <Alert>
      <AlertTitle>In the trash</AlertTitle>
      <AlertDescription>
        <p>
          This artifact was moved to the trash elsewhere, so it cannot be read
          or changed.{' '}
          {unsaved
            ? 'Your draft is kept here: recover the artifact to save it, or copy what you need.'
            : 'Recover it to work on it again.'}
        </p>
        <Button
          type="button"
          variant="outline"
          size="sm"
          disabled={waiting !== undefined}
          onClick={() =>
            void recover.write({ base_metadata_revision: base, deleted: false })
          }
        >
          <ArchiveRestoreIcon data-icon="inline-start" aria-hidden="true" />
          Recover
        </Button>
        <WriteNotice
          state={recover.state}
          action="recover it"
          changed="It"
          behind={false}
          waiting={explained(waiting)}
        />
      </AlertDescription>
    </Alert>
  );
}
