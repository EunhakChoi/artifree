import { useState } from 'react';
import type { FormEvent } from 'react';
import { PencilIcon, PinIcon } from 'lucide-react';
import { ArtifreeApiError, newIdempotencyKey } from '@artifree/client';
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
import { Field, FieldGroup, FieldLabel } from '@/components/ui/field';
import { Input } from '@/components/ui/input';
import { describeFailure } from './use-request';

type UpdateState =
  | { readonly status: 'idle' }
  | { readonly status: 'updating' }
  | { readonly status: 'conflict' }
  | { readonly status: 'failed'; readonly message: string };

/**
 * Sends metadata updates for one artifact (decision 0008). Sending the same
 * request again, base included, reuses its key, so a retry after a lost
 * answer cannot apply twice; any other request gets a new key.
 */
function useMetadataUpdate(
  client: ArtifreeClient,
  artifactId: string,
  onUpdated: (artifact: Artifact) => void,
  onConflict: () => void,
) {
  const [state, setState] = useState<UpdateState>({ status: 'idle' });
  const [attempt, setAttempt] = useState<{ body: string; key: string }>();
  async function update(request: UpdateArtifactRequest): Promise<boolean> {
    const body = JSON.stringify(request);
    const key = attempt?.body === body ? attempt.key : newIdempotencyKey();
    setAttempt({ body, key });
    setState({ status: 'updating' });
    try {
      const result = await client.updateArtifact(artifactId, request, {
        idempotencyKey: key,
      });
      setState({ status: 'idle' });
      onUpdated(result.artifact);
      return true;
    } catch (error: unknown) {
      if (
        error instanceof ArtifreeApiError &&
        error.code === 'metadata_conflict'
      ) {
        setState({ status: 'conflict' });
        onConflict();
      } else setState({ status: 'failed', message: describeFailure(error) });
      return false;
    }
  }
  return { state, update, clear: () => setState({ status: 'idle' }) };
}

function UpdateOutcome({
  state,
  action,
}: {
  state: UpdateState;
  action: string;
}) {
  if (state.status === 'conflict')
    return (
      <Alert>
        <AlertTitle>Changed elsewhere</AlertTitle>
        <AlertDescription>
          The title or pin changed since this page read it, so nothing was
          changed. The latest is shown; {action} again if you still want to.
        </AlertDescription>
      </Alert>
    );
  if (state.status === 'failed')
    return (
      <Alert variant="destructive">
        <AlertTitle>Not changed</AlertTitle>
        <AlertDescription>{state.message}</AlertDescription>
      </Alert>
    );
  return null;
}

interface MetadataActionsProps {
  client: ArtifreeClient;
  artifact: Artifact;
  /** Called with the artifact as the update left it. */
  onUpdated: (artifact: Artifact) => void;
  /** Called when another update won; the view reads the latest. */
  onConflict: () => void;
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
          disabled={pin.state.status === 'updating'}
          onClick={() =>
            void pin.update({
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
        />
      </div>
      <UpdateOutcome state={pin.state} action="pin or unpin" />
    </div>
  );
}

function RenameDialog({
  client,
  artifact,
  onUpdated,
  onConflict,
}: MetadataActionsProps) {
  const [open, setOpen] = useState(false);
  const [title, setTitle] = useState(artifact.title);
  // The artifact as the dialog opened on it. A rename is prepared against
  // it, so a retry after a lost answer is the same request even when the
  // view has meanwhile read that the rename took effect. After a conflict,
  // the next rename is prepared against the latest read.
  const [opened, setOpened] = useState<{
    base: number | undefined;
    title: string;
  }>({
    base: artifact.metadata_revision,
    title: artifact.title,
  });
  const rename = useMetadataUpdate(client, artifact.id, onUpdated, () => {
    setOpened((current) => ({ ...current, base: undefined }));
    onConflict();
  });
  const pending = rename.state.status === 'updating';

  async function submit(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    const base = opened.base ?? artifact.metadata_revision;
    setOpened((current) => ({ ...current, base }));
    const request = { base_metadata_revision: base, title: title.trim() };
    if (await rename.update(request)) setOpen(false);
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
          });
          rename.clear();
        }
        setOpen(next);
      }}
    >
      <DialogTrigger render={<Button variant="outline" size="sm" />}>
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
          <UpdateOutcome state={rename.state} action="rename" />
          <DialogFooter>
            <DialogClose
              render={<Button variant="outline" disabled={pending} />}
            >
              Cancel
            </DialogClose>
            <Button
              type="submit"
              disabled={
                pending || title.trim() === '' || title.trim() === opened.title
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
