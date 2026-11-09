import { useState } from 'react';
import { RotateCwIcon, Trash2Icon, UploadIcon } from 'lucide-react';
import { isDeclarableMimeType } from '@artifree/client';
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
  NativeSelect,
  NativeSelectOption,
} from '@/components/ui/native-select';
import type { AssetWrites } from './use-asset-writes';

/** The types offered whatever the file reports; the server admits these now. */
const OFFERED_TYPES = ['text/plain', 'text/csv', 'image/png'] as const;

/** Whether a new upload or delete may be sent against `base` now. */
function canWrite(
  writes: AssetWrites,
  base: number | undefined,
  shownResets: number,
): base is number {
  return (
    base !== undefined &&
    writes.held === undefined &&
    !writes.sending &&
    writes.wait(base, shownResets) === undefined
  );
}

interface AssetUploadProps {
  writes: AssetWrites;
  /** The asset version of the list shown, if one is shown. */
  base: number | undefined;
  /** The page's reset count when that list was read. */
  shownResets: number;
}

/**
 * Picks one file and the type it is declared as, and uploads it. The type
 * is sent as chosen, with no sniffing or normalization; the server decides
 * whether it is admitted. The form clears after every successful upload,
 * including one made by retrying a held attempt, so the same file is not
 * sent again under a new key by accident.
 */
export function AssetUpload(props: AssetUploadProps) {
  return <UploadForm key={props.writes.uploads} {...props} />;
}

function UploadForm({ writes, base, shownResets }: AssetUploadProps) {
  const [file, setFile] = useState<File>();
  const [declared, setDeclared] = useState('');
  const detected =
    file !== undefined && isDeclarableMimeType(file.type)
      ? file.type
      : undefined;
  const ready = canWrite(writes, base, shownResets);
  const uploading =
    writes.state.status === 'sending' && writes.state.kind === 'upload';

  async function submit(): Promise<void> {
    if (
      file === undefined ||
      declared === '' ||
      !canWrite(writes, base, shownResets)
    )
      return;
    // A success remounts this form through `writes.uploads`.
    await writes.upload(file, declared, base);
  }

  return (
    <form
      onSubmit={(event) => {
        event.preventDefault();
        void submit();
      }}
    >
      <FieldGroup>
        <Field>
          <FieldLabel htmlFor="asset-file">File</FieldLabel>
          <Input
            id="asset-file"
            type="file"
            onChange={(event) => {
              const chosen = event.target.files?.[0];
              setFile(chosen);
              setDeclared(
                chosen !== undefined && isDeclarableMimeType(chosen.type)
                  ? chosen.type
                  : '',
              );
            }}
          />
          <FieldDescription>
            Plain text, CSV and PNG images, up to 5 MiB. Other types are refused
            by the server. Files are kept and downloaded, never shown here.
          </FieldDescription>
        </Field>
        <Field>
          <FieldLabel htmlFor="asset-type">Declared type</FieldLabel>
          <NativeSelect
            id="asset-type"
            value={declared}
            disabled={file === undefined}
            onChange={(event) => setDeclared(event.target.value)}
          >
            <NativeSelectOption value="" disabled>
              Choose a type
            </NativeSelectOption>
            {detected === undefined ||
            OFFERED_TYPES.some((type) => type === detected) ? null : (
              <NativeSelectOption value={detected}>
                As detected: {detected}
              </NativeSelectOption>
            )}
            {OFFERED_TYPES.map((type) => (
              <NativeSelectOption key={type} value={type}>
                {type}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        </Field>
        <Field orientation="horizontal">
          <Button
            type="submit"
            disabled={!ready || file === undefined || declared === ''}
          >
            <UploadIcon data-icon="inline-start" aria-hidden="true" />
            {uploading ? 'Uploading...' : 'Upload'}
          </Button>
        </Field>
      </FieldGroup>
    </form>
  );
}

interface AssetDeleteButtonProps {
  writes: AssetWrites;
  assetId: string;
  /** How the asset is named on the page. */
  label: string;
  /**
   * The asset version of the list shown, or undefined while it is read
   * again.
   */
  base: number | undefined;
  /** The page's reset count when that list was read. */
  shownResets: number;
}

/**
 * Deletes one asset after a confirmation. Cancelling sends nothing. The
 * dialog stays open until the request settles; its outcome is shown with
 * the panel's other outcomes.
 */
export function AssetDeleteButton({
  writes,
  assetId,
  label,
  base,
  shownResets,
}: AssetDeleteButtonProps) {
  const [open, setOpen] = useState(false);
  const deleting =
    writes.state.status === 'sending' && writes.state.kind === 'delete';
  const ready = canWrite(writes, base, shownResets);

  async function confirm(): Promise<void> {
    if (!canWrite(writes, base, shownResets)) return;
    await writes.remove(assetId, base);
    setOpen(false);
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (deleting) return;
        setOpen(next);
      }}
    >
      <DialogTrigger
        render={
          <Button
            variant="outline"
            size="sm"
            disabled={!ready}
            aria-label={`Delete asset ${label}`}
          />
        }
      >
        <Trash2Icon data-icon="inline-start" aria-hidden="true" />
        Delete
      </DialogTrigger>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Delete asset {label}</DialogTitle>
          <DialogDescription>
            Removes this file from this artifact. Copies already downloaded, and
            other artifacts that hold the same file, are not affected.
          </DialogDescription>
        </DialogHeader>
        <DialogFooter>
          <DialogClose
            render={<Button variant="outline" disabled={deleting} />}
          >
            Cancel
          </DialogClose>
          <Button
            type="button"
            variant="destructive"
            disabled={!ready}
            onClick={() => void confirm()}
          >
            {deleting ? 'Deleting...' : 'Delete'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/**
 * What became of the last upload or delete, and the attempt whose outcome
 * is unknown, which the user may send again as it was or forget.
 */
export function AssetOutcome({ writes }: { writes: AssetWrites }) {
  const { state, held } = writes;
  return (
    <>
      <StateNotice writes={writes} />
      {held === undefined ? null : (
        <Alert variant="destructive">
          <AlertTitle>Outcome unknown</AlertTitle>
          <AlertDescription>
            {state.status === 'retry_refused' ? (
              <p>
                The last retry was refused: {state.message}
                {state.current === undefined
                  ? null
                  : ` Current version ${state.current}.`}{' '}
                That does not show whether the first attempt was made.
              </p>
            ) : null}
            <p>
              {state.status === 'unknown' ? `${state.message} ` : null}
              The {held.kind} may have been made. To retry, the same request is
              sent and applies at most once. Until this one is retried or
              forgotten, no other upload or delete is sent and other artifacts
              cannot be opened. Reloading the page forgets it.
            </p>
            {writes.heldReplaced ? (
              <p>
                The server&apos;s data may have been replaced since, so it is
                not sent again.
              </p>
            ) : null}
            <div className="flex flex-wrap gap-2">
              <Button
                type="button"
                variant="outline"
                size="sm"
                disabled={writes.sending || writes.heldReplaced}
                onClick={() => void writes.retry()}
              >
                <RotateCwIcon data-icon="inline-start" aria-hidden="true" />
                {held.kind === 'upload' ? 'Retry upload' : 'Retry delete'}
              </Button>
              <Button
                type="button"
                variant="outline"
                size="sm"
                disabled={writes.sending}
                aria-describedby="asset-forget-note"
                onClick={writes.forget}
              >
                Forget this attempt
              </Button>
            </div>
            <p id="asset-forget-note">
              Forgetting sends nothing; the change may still have been made.
            </p>
          </AlertDescription>
        </Alert>
      )}
    </>
  );
}

/** The last settled write, unless it is the unknown outcome held above. */
function StateNotice({ writes }: { writes: AssetWrites }) {
  const { state } = writes;
  switch (state.status) {
    case 'idle':
    case 'sending':
    case 'unknown':
    case 'retry_refused':
      return null;
    case 'done':
      if (state.kind === 'upload')
        return (
          <Alert>
            <AlertTitle>Uploaded</AlertTitle>
            {state.replayed ? (
              <AlertDescription>
                Uploaded earlier; this is the original result.
              </AlertDescription>
            ) : null}
          </Alert>
        );
      return (
        <Alert>
          <AlertTitle>
            {state.deleted ? 'Deleted' : 'Already removed'}
          </AlertTitle>
          {state.replayed ? (
            <AlertDescription>
              Deleted earlier; this is the original result.
            </AlertDescription>
          ) : null}
        </Alert>
      );
    case 'conflict':
      return (
        <Alert>
          <AlertTitle>The assets changed elsewhere</AlertTitle>
          <AlertDescription>
            Nothing was changed.{' '}
            {state.current === undefined
              ? null
              : `Current version ${state.current}. `}
            The list is read again; upload or delete again if you still want to.
          </AlertDescription>
        </Alert>
      );
    case 'refused':
      return (
        <Alert variant="destructive">
          <AlertTitle>Not changed</AlertTitle>
          <AlertDescription>{state.message}</AlertDescription>
        </Alert>
      );
    default: {
      const unhandled: never = state;
      return unhandled;
    }
  }
}
