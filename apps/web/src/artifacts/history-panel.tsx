import { useMemo, useState } from 'react';
import { HistoryIcon } from 'lucide-react';
import type { ArtifreeClient } from '@artifree/client';
import type { Artifact, Revision, RevisionFile } from '@artifree/contracts';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Badge } from '@/components/ui/badge';
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
import { Diff, DiffGap, DiffLine } from '@/components/ui/diff';
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
import { compareManifests, diffLines, hunks, splitLines } from './compare';
import type { FileDifference } from './compare';
import { WriteOutcome, useRetryableWrite } from './retryable-write';
import { useRequest } from './use-request';

/** Text files up to this size are compared line by line. */
const MAX_TEXT_BYTES = 1024 * 1024;

const TEXT_TYPES = /^(text\/|application\/json|application\/xml|image\/svg)/;

const KIND_LABELS = {
  added: 'Added',
  removed: 'Removed',
  changed: 'Changed',
} as const;

interface HistoryPanelProps {
  client: ArtifreeClient;
  artifact: Artifact;
  revisions: Revision[];
  /** The feed epoch the revisions were read in; file reads are keyed by it. */
  epoch: number;
  /** Whether the editor holds a draft, which a restore would leave stale. */
  unsaved: boolean;
  /** The view's restore requests, from {@link useRestore}. */
  restore: Restore;
}

/**
 * Compares two revisions and restores an earlier one (decision 0011). Files
 * are compared by digest; only a changed text file is read, to show its
 * lines.
 */
export function HistoryPanel({
  client,
  artifact,
  revisions,
  epoch,
  unsaved,
  restore,
}: HistoryPanelProps) {
  const latest = artifact.current_revision;
  // The revision to inspect, and the one it is compared with; none follows
  // the latest. A choice no longer listed, as after a reset, falls back.
  const [chosen, setChosen] = useState<number>();
  const [against, setAgainst] = useState<number>();
  const [path, setPath] = useState<string>();
  const listed = (revision: number | undefined) =>
    revisions.find((item) => item.revision === revision);
  const before =
    listed(chosen) ?? revisions.find((item) => item.revision < latest);
  const after = listed(against) ?? listed(latest);
  if (before === undefined || after === undefined)
    return (
      <p className="text-sm text-muted-foreground">
        This artifact has one revision; there is nothing to compare yet.
      </p>
    );
  const { differences, unchanged } = compareManifests(before, after);
  const shown =
    differences.find((difference) => difference.path === path) ??
    differences[0];
  return (
    <div className="flex flex-col gap-4">
      <FieldGroup className="sm:flex-row">
        <Field orientation="horizontal">
          <FieldLabel htmlFor="history-revision">Revision</FieldLabel>
          <NativeSelect
            id="history-revision"
            value={String(before.revision)}
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
          <FieldLabel htmlFor="history-against">Compared with</FieldLabel>
          <NativeSelect
            id="history-against"
            value={listed(against) === undefined ? 'latest' : String(against)}
            onChange={(event) =>
              setAgainst(
                event.target.value === 'latest'
                  ? undefined
                  : Number(event.target.value),
              )
            }
          >
            <NativeSelectOption value="latest">
              Latest ({latest})
            </NativeSelectOption>
            {revisions.map((item) => (
              <NativeSelectOption key={item.revision} value={item.revision}>
                {String(item.revision)}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        </Field>
      </FieldGroup>
      <p className="text-sm text-muted-foreground">
        {differences.length === 0
          ? `Revisions ${before.revision} and ${after.revision} have the same files.`
          : `${plural(differences.length, 'file')} differ from revision ${before.revision} to revision ${after.revision}; ${plural(unchanged, 'file')} unchanged.`}
      </p>
      {differences.length === 0 ? null : (
        <nav aria-label="Changed files" className="flex flex-col gap-1">
          {differences.map((difference) => (
            <Button
              key={difference.path}
              type="button"
              variant="ghost"
              size="sm"
              className="justify-start"
              aria-current={difference === shown ? 'true' : undefined}
              onClick={() => setPath(difference.path)}
            >
              <Badge
                variant={
                  difference.kind === 'removed' ? 'destructive' : 'secondary'
                }
              >
                {KIND_LABELS[difference.kind]}
              </Badge>{' '}
              <span className="truncate">{difference.path}</span>
            </Button>
          ))}
        </nav>
      )}
      {shown === undefined ? null : (
        <FileComparison
          key={`${before.revision}:${after.revision}:${shown.path}`}
          client={client}
          artifactId={artifact.id}
          epoch={epoch}
          from={before.revision}
          to={after.revision}
          difference={shown}
        />
      )}
      <RestoreDialog
        restore={restore}
        artifact={artifact}
        revision={before.revision}
        unsaved={unsaved}
      />
    </div>
  );
}

function plural(count: number, noun: string): string {
  return `${count} ${noun}${count === 1 ? '' : 's'}`;
}

function isText(file: RevisionFile | undefined): boolean {
  return (
    file === undefined ||
    (TEXT_TYPES.test(file.media_type) && file.size <= MAX_TEXT_BYTES)
  );
}

interface FileComparisonProps {
  client: ArtifreeClient;
  artifactId: string;
  epoch: number;
  from: number;
  to: number;
  difference: FileDifference;
}

/** The lines of one file that differ, read only for text files. */
function FileComparison({
  client,
  artifactId,
  epoch,
  from,
  to,
  difference,
}: FileComparisonProps) {
  const text = isText(difference.before) && isText(difference.after);
  const read = useRequest(
    text
      ? `${artifactId}:${from}:${to}:${difference.path}:${epoch}`
      : undefined,
    async (signal) => {
      const content = async (revision: number, file?: RevisionFile) =>
        file === undefined
          ? ''
          : decode(
              await client.readFile(artifactId, revision, file.path, {
                signal,
              }),
            );
      return {
        before: await content(from, difference.before),
        after: await content(to, difference.after),
      };
    },
  );
  const texts = read.status === 'ready' ? read.value : undefined;
  // Comparing is the costly part; feed refreshes re-render the panel.
  const groups = useMemo(() => {
    if (texts?.before === undefined || texts.after === undefined)
      return undefined;
    const lines = diffLines(splitLines(texts.before), splitLines(texts.after));
    return lines === undefined ? null : hunks(lines);
  }, [texts]);
  const sizes = `${difference.before?.size ?? 0} → ${difference.after?.size ?? 0} bytes`;
  if (!text)
    return (
      <p className="text-sm text-muted-foreground">
        {difference.path} is not a text file up to 1 MiB, so only its size is
        compared: {sizes}.
      </p>
    );
  if (read.status === 'loading')
    return (
      <output className="text-sm text-muted-foreground">
        Reading {difference.path}…
      </output>
    );
  if (read.status === 'error')
    return (
      <Alert variant="destructive">
        <AlertTitle>Could not read {difference.path}</AlertTitle>
        <AlertDescription>{read.message}</AlertDescription>
      </Alert>
    );
  if (groups === undefined)
    return (
      <p className="text-sm text-muted-foreground">
        {difference.path} is not valid UTF-8 text, so only its size is compared:{' '}
        {sizes}.
      </p>
    );
  if (groups === null)
    return (
      <p className="text-sm text-muted-foreground">
        {difference.path} has too many changed lines to show here: {sizes}.
      </p>
    );
  if (groups.length === 0)
    return (
      <p className="text-sm text-muted-foreground">
        {difference.kind === 'changed'
          ? `${difference.path} has the same lines; only the line ending at its end differs.`
          : `${difference.path} was ${difference.kind} with no lines.`}
      </p>
    );
  return (
    <section aria-label={`Changes to ${difference.path}`}>
      <Diff>
        {groups.map((group, index) => (
          <div key={index}>
            {index === 0 ? null : <DiffGap>…</DiffGap>}
            {group.lines.map((line) => (
              <DiffLine
                key={`${line.kind}:${'before' in line ? line.before : ''}:${'after' in line ? line.after : ''}`}
                kind={line.kind}
                before={'before' in line ? line.before : undefined}
                after={'after' in line ? line.after : undefined}
              >
                {visible(line.text)}
              </DiffLine>
            ))}
          </div>
        ))}
      </Diff>
    </section>
  );
}

/**
 * A line with its invisible marks shown: a carriage return as ␍ and a byte
 * order mark as ⟨BOM⟩, so a change of line endings or encoding mark shows.
 */
function visible(text: string): string {
  return text.replaceAll('\r', '␍').replaceAll('\uFEFF', '⟨BOM⟩');
}

/** UTF-8 text, or undefined for other bytes. */
function decode(bytes: Uint8Array): string | undefined {
  try {
    return new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(
      bytes,
    );
  } catch {
    return undefined;
  }
}

/** One restore request, as its retry identity. */
interface RestoreRequest {
  readonly revision: number;
  readonly base_revision: number;
}

/**
 * Restores revisions of one artifact (decision 0010). The detail view holds
 * it, so a retry keeps its key when the history tab closes and opens again.
 */
export function useRestore(
  client: ArtifreeClient,
  artifactId: string,
  onRestored: (artifact: Artifact) => void,
  onConflict: () => void,
) {
  return useRetryableWrite({
    send: (request: RestoreRequest, idempotencyKey: string) =>
      client.restoreRevision(
        artifactId,
        request.revision,
        { base_revision: request.base_revision },
        { idempotencyKey },
      ),
    conflict: 'revision_conflict',
    base: (request) => request.base_revision,
    onDone: (result) => onRestored(result.artifact),
    onConflict,
  });
}

type Restore = ReturnType<typeof useRestore>;

interface RestoreDialogProps {
  restore: Restore;
  artifact: Artifact;
  revision: number;
  unsaved: boolean;
}

/**
 * Restores `revision` as the next revision. The revision and the base are
 * fixed when the dialog opens, as for a rename, so a retry after a lost
 * answer is the same request even while newer revisions arrive; after a
 * conflict the next attempt is prepared against the latest read.
 */
function RestoreDialog({
  restore,
  artifact,
  revision,
  unsaved,
}: RestoreDialogProps) {
  const [open, setOpen] = useState(false);
  const [opened, setOpened] = useState({
    revision,
    base: artifact.current_revision,
  });
  const pending = restore.state.status === 'sending';
  const latest = revision === artifact.current_revision;
  const target = open ? opened.revision : revision;

  async function confirm(): Promise<void> {
    const base =
      restore.state.status === 'conflict'
        ? artifact.current_revision
        : opened.base;
    setOpened({ revision: opened.revision, base });
    if (await restore.write({ revision: opened.revision, base_revision: base }))
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
              setOpened({ revision, base: artifact.current_revision });
              restore.clear();
            }
            setOpen(next);
          }}
        >
          <DialogTrigger
            render={
              <Button
                variant="outline"
                className="w-fit"
                disabled={latest || unsaved}
                aria-describedby={unsaved ? 'restore-locked' : undefined}
              />
            }
          >
            <HistoryIcon data-icon="inline-start" aria-hidden="true" />
            Restore revision {revision}
          </DialogTrigger>
          <DialogContent>
            <DialogHeader>
              <DialogTitle>Restore revision {target}</DialogTitle>
              <DialogDescription>
                The next revision will have the files of revision {target}, byte
                for byte. Every revision is kept, and the title and pin stay as
                they are.
              </DialogDescription>
            </DialogHeader>
            <WriteOutcome
              state={restore.state}
              action="restore"
              conflict="A revision was saved"
            />
            <DialogFooter>
              <DialogClose
                render={<Button variant="outline" disabled={pending} />}
              >
                Cancel
              </DialogClose>
              <Button
                type="button"
                disabled={pending}
                onClick={() => void confirm()}
              >
                {pending ? 'Restoring…' : 'Restore'}
              </Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>
        {unsaved ? (
          <FieldDescription id="restore-locked">
            Save or discard your changes to restore a revision.
          </FieldDescription>
        ) : latest ? (
          <FieldDescription>
            Revision {revision} is the latest; choose an earlier one to restore.
          </FieldDescription>
        ) : null}
      </Field>
    </FieldGroup>
  );
}
