import { useState } from 'react';
import { CopyIcon } from 'lucide-react';
import type { ArtifreeClient } from '@artifree/client';
import type { Artifact, Revision } from '@artifree/contracts';
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
import {
  NativeSelect,
  NativeSelectOption,
} from '@/components/ui/native-select';
import {
  FailureNotice,
  ReplacedNotice,
  useRetryableWrite,
} from './conditional-write';

interface DuplicateDialogProps {
  client: ArtifreeClient;
  artifact: Artifact;
  revisions: Revision[];
  /** The revision offered first: the one the preview shows. */
  revision: number;
  unsaved: boolean;
  /** How often the server's data may have been replaced so far. */
  resets: number;
  /** Whether what is shown was read before the last possible replacement. */
  stale: boolean;
  /** Called with the copy; the workspace opens it. */
  onDuplicated: (copy: Artifact) => void;
}

/**
 * Copies one revision's files into a new artifact with its own history
 * (decision 0012). The revision offered first is fixed when the dialog
 * opens, and sending the same revision again reuses its key, so a retry
 * after a lost answer creates at most one copy. If the server's data may
 * have been replaced since it opened, it sends nothing more (0011 decision
 * 6): the same number may name another revision.
 */
export function DuplicateDialog({
  client,
  artifact,
  revisions,
  revision,
  unsaved,
  resets,
  stale,
  onDuplicated,
}: DuplicateDialogProps) {
  const duplicate = useRetryableWrite({
    send: (request: { revision: number }, idempotencyKey: string) =>
      client.duplicateArtifact(
        artifact.id,
        request.revision,
        {},
        {
          idempotencyKey,
        },
      ),
    onDone: (result) => onDuplicated(result.artifact),
  });
  const [open, setOpen] = useState(false);
  const [chosen, setChosen] = useState(revision);
  const [openedResets, setOpenedResets] = useState(resets);
  const pending = duplicate.state.status === 'sending';
  const replaced = openedResets !== resets;

  async function confirm(): Promise<void> {
    if (replaced) return;
    if ((await duplicate.write({ revision: chosen })) === 'done')
      setOpen(false);
  }

  return (
    <FieldGroup>
      <Field>
        <Dialog
          open={open}
          onOpenChange={(next) => {
            // Stay open until the request settles, so its outcome is seen.
            if (pending) return;
            if (next) {
              setChosen(revision);
              setOpenedResets(resets);
              duplicate.clear();
            }
            setOpen(next);
          }}
        >
          <DialogTrigger
            render={
              <Button
                variant="outline"
                size="sm"
                className="w-fit self-end"
                disabled={unsaved || stale}
                aria-describedby={unsaved ? 'duplicate-locked' : undefined}
              />
            }
          >
            <CopyIcon data-icon="inline-start" aria-hidden="true" />
            Duplicate
          </DialogTrigger>
          <DialogContent>
            <DialogHeader>
              <DialogTitle>Duplicate {artifact.title}</DialogTitle>
              <DialogDescription>
                The copy is a new artifact whose history starts with the files
                of the chosen revision, byte for byte. It is named after this
                artifact followed by &ldquo;(Copy)&rdquo; and is not pinned.
                Only files are copied, and this artifact stays as it is.
              </DialogDescription>
            </DialogHeader>
            <Field orientation="horizontal">
              <FieldLabel htmlFor="duplicate-revision">Revision</FieldLabel>
              <NativeSelect
                id="duplicate-revision"
                value={String(chosen)}
                disabled={pending}
                onChange={(event) => setChosen(Number(event.target.value))}
              >
                {revisions.map((item) => (
                  <NativeSelectOption key={item.revision} value={item.revision}>
                    {String(item.revision)}
                  </NativeSelectOption>
                ))}
              </NativeSelect>
            </Field>
            {/* Replaced data sends nothing, so no retry is offered. */}
            {replaced ? (
              <ReplacedNotice state={duplicate.state} />
            ) : (
              <FailureNotice state={duplicate.state} action="duplicate" />
            )}
            <DialogFooter>
              <DialogClose
                render={<Button variant="outline" disabled={pending} />}
              >
                Cancel
              </DialogClose>
              <Button
                type="button"
                disabled={pending || replaced}
                onClick={() => void confirm()}
              >
                {pending ? 'Duplicating…' : 'Duplicate'}
              </Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>
        {unsaved ? (
          <FieldDescription id="duplicate-locked" className="text-right">
            Save or discard your changes to duplicate; the copy opens in their
            place.
          </FieldDescription>
        ) : null}
      </Field>
    </FieldGroup>
  );
}
