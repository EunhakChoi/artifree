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
  useRetryableWrite,
} from './retryable-write';

/** Renames or pins one artifact (decision 0008). */
function useMetadataUpdate(
  client: ArtifreeClient,
  artifactId: string,
  onUpdated: (artifact: Artifact) => void,
  onConflict: () => void,
) {
  return useRetryableWrite({
    send: (request: UpdateArtifactRequest, idempotencyKey: string) =>
      client.updateArtifact(artifactId, request, { idempotencyKey }),
    conflict: 'metadata_conflict',
    base: (request) => request.base_metadata_revision,
    onDone: (result) => onUpdated(result.artifact),
    onConflict,
  });
}

const METADATA_CONFLICT = 'The title or pin changed';

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
   * Whether what is shown was read before the last possible replacement;
   * it is read again, and nothing is sent meanwhile.
   */
  stale: boolean;
}

/**
 * Pins and renames the open artifact. Neither creates a source revision,
 * so both stay available while the editor holds a draft.
 */
export function MetadataActions({
  client,
  artifact,
  onUpdated,
  onConflict,
  resets,
  stale,
}: MetadataActionsProps) {
  const pin = useMetadataUpdate(client, artifact.id, onUpdated, onConflict);
  return (
    <div className="flex flex-col items-end gap-2">
      <div className="flex flex-wrap justify-end gap-2">
        <Button
          type="button"
          variant="outline"
          size="sm"
          aria-pressed={artifact.pinned}
          disabled={pin.state.status === 'sending' || stale}
          onClick={() =>
            void pin.write({
              base_metadata_revision: artifact.metadata_revision,
              pinned: !artifact.pinned,
            })
          }
        >
          <PinIcon data-icon="inline-start" aria-hidden="true" />
          Pin
        </Button>
        <RenameDialog
          client={client}
          artifact={artifact}
          onUpdated={onUpdated}
          onConflict={onConflict}
          resets={resets}
          stale={stale}
        />
      </div>
      {/* Once the page reads newer metadata, it shows what a lost update
          did, and pinning again would be a new request. */}
      {pin.state.status === 'failed' &&
      pin.state.unknown &&
      pin.state.base !== artifact.metadata_revision ? null : (
        <WriteOutcome
          state={pin.state}
          action="pin or unpin"
          conflict={METADATA_CONFLICT}
        />
      )}
    </div>
  );
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
  // The artifact as the dialog opened on it. A rename is prepared against
  // it, so a retry after a lost answer is the same request even when the
  // view has meanwhile read that the rename took effect. After a conflict,
  // the next rename is prepared against the latest read and title.
  const [opened, setOpened] = useState<{
    base: number | undefined;
    title: string | undefined;
    resets: number;
  }>({
    base: artifact.metadata_revision,
    title: artifact.title,
    resets,
  });
  const rename = useMetadataUpdate(client, artifact.id, onUpdated, () => {
    setOpened((current) => ({ ...current, base: undefined, title: undefined }));
    onConflict();
  });
  const pending = rename.state.status === 'sending';
  const replaced = opened.resets !== resets;

  async function submit(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    if (replaced) return;
    const base = opened.base ?? artifact.metadata_revision;
    setOpened((current) => ({ ...current, base }));
    const request = { base_metadata_revision: base, title: title.trim() };
    if (await rename.write(request)) setOpen(false);
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        // Stay open until the request settles, so its outcome is seen.
        if (pending) return;
        if (next) {
          setTitle(artifact.title);
          setOpened({
            base: artifact.metadata_revision,
            title: artifact.title,
            resets,
          });
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
                value={title}
                onChange={(event) => {
                  setTitle(event.target.value);
                  rename.clear();
                }}
              />
            </Field>
          </FieldGroup>
          {replaced ? <ReplacedNotice /> : null}
          <WriteOutcome
            state={rename.state}
            action="rename"
            conflict={METADATA_CONFLICT}
          />
          <DialogFooter>
            <DialogClose
              render={<Button variant="outline" disabled={pending} />}
            >
              Cancel
            </DialogClose>
            <Button
              type="submit"
              disabled={
                pending ||
                replaced ||
                title.trim() === '' ||
                title.trim() === (opened.title ?? artifact.title)
              }
            >
              {pending ? 'Renaming…' : 'Rename'}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
