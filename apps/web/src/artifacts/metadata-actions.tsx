import { useState } from 'react';
import type { FormEvent } from 'react';
import { PencilIcon, PinIcon } from 'lucide-react';
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
import { Field, FieldGroup, FieldLabel } from '@/components/ui/field';
import { Input } from '@/components/ui/input';
import {
  ReplacedNotice,
  WriteOutcome,
  useConditionalWrite,
} from './conditional-write';

const CHANGED = 'The title or pin changed';

/** Renames or pins one artifact (decision 0008). */
function useMetadataUpdate(
  client: ArtifreeClient,
  artifactId: string,
  onUpdated: (artifact: Artifact) => void,
  onConflict: () => void,
  resets: number,
) {
  return useConditionalWrite({
    send: (request: UpdateArtifactRequest, idempotencyKey: string) =>
      client.updateArtifact(artifactId, request, { idempotencyKey }),
    base: (request) => request.base_metadata_revision,
    conflict: 'metadata_conflict',
    current: (error) => error.currentMetadataRevision,
    onDone: (result) => onUpdated(result.artifact),
    onConflict,
    resets,
  });
}

interface MetadataActionsProps {
  client: ArtifreeClient;
  artifact: Artifact;
  /** Called with the artifact as the update left it. */
  onUpdated: (artifact: Artifact) => void;
  /** Called when another update won; the view reads the latest. */
  onConflict: () => void;
  /** How often the server's data may have been replaced so far. */
  resets: number;
  /**
   * Whether what is shown was read before the server's data may have been
   * replaced. It is read again, and nothing is prepared from it meanwhile.
   */
  stale: boolean;
}

/**
 * Pins and renames the open artifact. Neither creates a source revision,
 * so both stay available while the editor holds a draft.
 */
export function MetadataActions(props: MetadataActionsProps) {
  const { client, artifact, onUpdated, onConflict, resets, stale } = props;
  const pin = useMetadataUpdate(
    client,
    artifact.id,
    onUpdated,
    onConflict,
    resets,
  );
  const version = artifact.metadata_revision;
  return (
    <div className="flex flex-col items-end gap-2">
      <div className="flex flex-wrap justify-end gap-2">
        <Button
          type="button"
          variant="outline"
          size="sm"
          aria-pressed={artifact.pinned}
          disabled={stale || !pin.ready(version)}
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
      </div>
      {/* Once the page reads newer metadata, it shows what a lost update
          did, and pinning again would be a new request. */}
      {pin.state.status === 'failed' &&
      pin.state.unknown &&
      pin.state.base !== version ? null : (
        <WriteOutcome
          state={pin.state}
          action="pin or unpin"
          changed={CHANGED}
          caughtUp={pin.caughtUp(version)}
        />
      )}
    </div>
  );
}

/** What a rename is prepared from: the artifact as a read showed it. */
interface Prepared {
  readonly base: number;
  readonly title: string;
}

function RenameDialog({
  client,
  artifact,
  onUpdated,
  onConflict,
  resets,
  stale,
}: MetadataActionsProps) {
  const [open, setOpen] = useState(false);
  const [title, setTitle] = useState(artifact.title);
  // A rename is prepared from the artifact as the dialog opened on it, so a
  // retry after a lost answer is the same request even when the view has
  // meanwhile read that the rename took effect. After a conflict, the next
  // one is prepared from the latest read, once the page has it.
  const [prepared, setPrepared] = useState<Prepared>({
    base: artifact.metadata_revision,
    title: artifact.title,
  });
  // The page's count of possible data replacements when the dialog opened.
  const [openedAt, setOpenedAt] = useState(resets);
  const rename = useMetadataUpdate(
    client,
    artifact.id,
    onUpdated,
    onConflict,
    resets,
  );
  // Whether the user typed since the dialog opened. Until then the field
  // shows the title the request would be prepared from, so a title the
  // page has since learned is not current is never sent back.
  const [typed, setTyped] = useState(false);
  const latest = artifact.metadata_revision;
  // A refused base gives way to the latest read once the page has it.
  const from: Prepared =
    !rename.ready(prepared.base) && rename.ready(latest)
      ? { base: latest, title: artifact.title }
      : prepared;
  const replaced = openedAt !== resets;
  const pending = rename.state.status === 'sending';
  const shownTitle = typed ? title : from.title;
  const sendable =
    !replaced &&
    rename.ready(from.base) &&
    shownTitle.trim() !== '' &&
    shownTitle.trim() !== from.title;

  async function submit(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    if (!sendable) return;
    setPrepared(from);
    const request = {
      base_metadata_revision: from.base,
      title: shownTitle.trim(),
    };
    if ((await rename.write(request)) === 'done') setOpen(false);
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        // Stay open until the request settles, so its outcome is seen.
        if (pending) return;
        if (next) {
          setTitle(artifact.title);
          setTyped(false);
          setPrepared({ base: latest, title: artifact.title });
          setOpenedAt(resets);
          rename.clear();
        }
        setOpen(next);
      }}
    >
      <DialogTrigger
        render={<Button variant="outline" size="sm" disabled={stale} />}
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
                value={shownTitle}
                onChange={(event) => {
                  setTitle(event.target.value);
                  setTyped(true);
                  rename.clear();
                }}
              />
            </Field>
          </FieldGroup>
          {replaced ? <ReplacedNotice /> : null}
          <WriteOutcome
            state={rename.state}
            action="rename"
            changed={CHANGED}
            caughtUp={rename.caughtUp(latest)}
          />
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
