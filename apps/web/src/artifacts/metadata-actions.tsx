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
  WriteNotice,
  retryable,
  useConditionalWrite,
} from './conditional-write';
import type { WriteGate } from './conditional-write';
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
  /** Called when another update won; the view reads the latest. */
  onConflict: () => void;
}

/**
 * Pins and renames the open artifact. Neither creates a source revision,
 * so both stay available while the editor holds a draft.
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
      </div>
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
  const learned = wait(prepared.base);
  const retry = retryable(rename.state, prepared.base, learned);
  const behind = !retry && (learned === 'refused' || learned === 'newer');
  const from: Prepared =
    behind && wait(latest) === undefined
      ? { base: latest, title: artifact.title, resets }
      : prepared;
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
