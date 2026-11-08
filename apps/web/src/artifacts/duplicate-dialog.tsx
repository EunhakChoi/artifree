import { useState } from 'react';
import { CopyIcon } from 'lucide-react';
import { ArtifreeApiError } from '@artifree/client';
import type { ArtifreeClient } from '@artifree/client';
import type { Artifact, Revision } from '@artifree/contracts';
import { Button } from '@/components/ui/button';
import { Checkbox } from '@/components/ui/checkbox';
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
  FieldContent,
  FieldDescription,
  FieldGroup,
  FieldLabel,
} from '@/components/ui/field';
import {
  NativeSelect,
  NativeSelectOption,
} from '@/components/ui/native-select';
import { refusalText } from './asset-attempt';
import {
  FailureNotice,
  ReplacedNotice,
  useRetryableWrite,
} from './conditional-write';
import { describeFailure } from './failure';

interface DuplicateDialogProps {
  client: ArtifreeClient;
  artifact: Artifact;
  revisions: Revision[];
  /** The revision offered first: the one the preview shows. */
  revision: number;
  unsaved: boolean;
  /**
   * Whether an asset upload or delete is being sent or has an unknown
   * outcome; opening the copy would drop it from the page.
   */
  assetsPending: boolean;
  /** How often the server's data may have been replaced so far. */
  resets: number;
  /** Whether what is shown was read before the last possible replacement. */
  stale: boolean;
  /** Called with the copy; the workspace opens it. */
  onDuplicated: (copy: Artifact) => void;
}

/** What the dialog sends: the revision, and whether assets are copied. */
interface DuplicateChoice {
  readonly revision: number;
  readonly copyAssets: boolean;
}

/**
 * A failed duplicate in words. A requested asset copy that does not fit is
 * told with the assets panel's limit wording (decision 0045, rule 10).
 */
function describeDuplicateFailure(error: unknown): string {
  return error instanceof ArtifreeApiError &&
    error.code === 'asset_limit_exceeded'
    ? `${refusalText(error)} The uploaded assets do not fit, so no copy was made.`
    : describeFailure(error);
}

/**
 * Copies one revision's files into a new artifact with its own history
 * (decision 0012), and the caller's current uploaded assets only when asked
 * (decision 0045, rule 10); application data is never copied here. The
 * revision offered first is fixed when the dialog opens and the asset copy
 * starts off each time. Sending the same choice again reuses its key, so a
 * retry after a lost answer creates at most one copy; any other choice gets
 * a new key. If the server's data may have been replaced since it opened, it
 * sends nothing more (0011 decision 6): the same number may name another
 * revision.
 */
export function DuplicateDialog({
  client,
  artifact,
  revisions,
  revision,
  unsaved,
  assetsPending,
  resets,
  stale,
  onDuplicated,
}: DuplicateDialogProps) {
  const locked = unsaved || assetsPending;
  const duplicate = useRetryableWrite({
    send: (request: DuplicateChoice, idempotencyKey: string) =>
      client.duplicateArtifact(
        artifact.id,
        request.revision,
        // Omitted unless asked: the files-only default keeps its fingerprint.
        request.copyAssets ? { copy_assets: true } : {},
        { idempotencyKey },
      ),
    onDone: (result) => onDuplicated(result.artifact),
    describe: describeDuplicateFailure,
  });
  const [open, setOpen] = useState(false);
  const [chosen, setChosen] = useState(revision);
  const [copyAssets, setCopyAssets] = useState(false);
  const [openedResets, setOpenedResets] = useState(resets);
  const pending = duplicate.state.status === 'sending';
  const replaced = openedResets !== resets;

  async function confirm(): Promise<void> {
    if (replaced) return;
    if ((await duplicate.write({ revision: chosen, copyAssets })) === 'done')
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
              setCopyAssets(false);
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
                disabled={locked || stale}
                aria-describedby={locked ? 'duplicate-locked' : undefined}
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
                This artifact stays as it is. By default only the chosen
                revision&apos;s files are copied; application data and uploaded
                assets are not.
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
            <Field orientation="horizontal">
              <Checkbox
                id="duplicate-copy-assets"
                checked={copyAssets}
                disabled={pending}
                onCheckedChange={(checked) => setCopyAssets(checked)}
              />
              <FieldContent>
                <FieldLabel htmlFor="duplicate-copy-assets">
                  Copy uploaded assets
                </FieldLabel>
                <FieldDescription>
                  Also copies all of your current uploaded assets of this
                  artifact as they are now, whatever revision is chosen. The
                  copy has its own asset version. If they do not fit the asset
                  limits, the whole duplicate is refused and nothing is created.
                </FieldDescription>
              </FieldContent>
            </Field>
            <p className="text-sm text-muted-foreground">
              Application data is copied only when asked through the CLI, MCP or
              API; this dialog does not copy it.
            </p>
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
        ) : assetsPending ? (
          <FieldDescription id="duplicate-locked" className="text-right">
            Wait for the asset upload or delete, or retry or forget it, to
            duplicate; the copy opens in place of this artifact.
          </FieldDescription>
        ) : null}
      </Field>
    </FieldGroup>
  );
}
