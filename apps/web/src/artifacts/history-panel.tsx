import { Fragment, useMemo, useState } from 'react';
import { HistoryIcon, RotateCwIcon } from 'lucide-react';
import type { ArtifreeClient } from '@artifree/client';
import type {
  Artifact,
  RestoreRevisionRequest,
  Revision,
  RevisionFile,
} from '@artifree/contracts';
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
import { Diff, DiffGap, DiffRow } from '@/components/ui/diff';
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
import {
  ReplacedNotice,
  WriteOutcome,
  useConditionalWrite,
  useWriteGate,
} from './conditional-write';
import { useChoice } from './use-choice';
import { useRequest } from './use-request';

/** Text up to this size is compared line by line. */
const MAX_TEXT_BYTES = 1024 * 1024;

const TEXT_MEDIA =
  /^(?:text\/|application\/(?:json|xml|javascript|ld\+json)|image\/svg\+xml)/;

/** A restore of `revision`, prepared against `base_revision`. */
interface RestoreRequest extends RestoreRevisionRequest {
  readonly revision: number;
}

/**
 * Restores revisions of one artifact (decision 0010). The detail view
 * holds it, so a retry keeps its key, and a refused base stays refused,
 * when the history tab closes and opens again.
 */
export function useRestore(
  client: ArtifreeClient,
  artifactId: string,
  onRestored: (artifact: Artifact) => void,
  onConflict: () => void,
  resets: number,
) {
  const gate = useWriteGate(resets);
  return useConditionalWrite({
    send: (
      { revision, base_revision }: RestoreRequest,
      idempotencyKey: string,
    ) =>
      client.restoreRevision(
        artifactId,
        revision,
        { base_revision },
        { idempotencyKey },
      ),
    base: (request) => request.base_revision,
    conflict: 'revision_conflict',
    current: (error) => error.currentRevision,
    onDone: (result) => onRestored(result.artifact),
    onConflict,
    gate,
  });
}

type Restore = ReturnType<typeof useRestore>;

interface HistoryPanelProps {
  client: ArtifreeClient;
  artifact: Artifact;
  /** Revisions, newest first. */
  revisions: Revision[];
  /** The feed epoch the revisions were read in; file reads are keyed by it. */
  epoch: number;
  /** Whether the editor holds a draft, which a restore would leave stale. */
  unsaved: boolean;
  restore: Restore;
  /** How often the server's data may have been replaced so far. */
  resets: number;
  /** Whether what is shown was read before the last possible replacement. */
  stale: boolean;
  /** The count of possible replacements when what is shown was read. */
  shownResets: number;
}

/**
 * Compares a revision with another, file by file and for one text file
 * line by line, and restores the earlier one (decision 0011).
 */
export function HistoryPanel({
  client,
  artifact,
  revisions,
  epoch,
  unsaved,
  restore,
  resets,
  stale,
  shownResets,
}: HistoryPanelProps) {
  const latest = artifact.current_revision;
  // Chosen revisions and file; none means the default. A choice belongs to
  // the data it was made in: it is forgotten once the page shows data read
  // after a possible replacement.
  const [chosen, setChosen] = useChoice<number>(shownResets);
  const [against, setAgainst] = useChoice<number>(shownResets);
  const [path, setPath] = useChoice<string>(shownResets);
  const find = (revision: number | undefined) =>
    revisions.find((item) => item.revision === revision);
  const earlier =
    find(chosen) ?? revisions.find((item) => item.revision < latest);
  const later = find(against) ?? find(latest);
  if (earlier === undefined || later === undefined)
    return (
      <p className="text-sm text-muted-foreground">
        This artifact has one revision, so there is nothing to compare yet.
      </p>
    );
  const { differences, unchanged } = compareManifests(
    earlier.files,
    later.files,
  );
  const shown =
    differences.find((difference) => difference.path === path) ??
    differences[0];
  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap gap-4">
        <Field orientation="horizontal">
          <FieldLabel htmlFor="history-revision">Revision</FieldLabel>
          <NativeSelect
            id="history-revision"
            value={String(earlier.revision)}
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
            value={find(against) === undefined ? 'latest' : String(against)}
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
      </div>
      <p className="text-sm">
        {differences.length === 0
          ? `Revisions ${earlier.revision} and ${later.revision} have the same files.`
          : `${count(differences.length, 'file')} differ from revision ${earlier.revision} to revision ${later.revision}; ${count(unchanged, 'file')} unchanged.`}
      </p>
      {differences.length === 0 ? null : (
        <nav aria-label="Changed files" className="flex flex-wrap gap-2">
          {differences.map((difference) => (
            <Button
              key={difference.path}
              type="button"
              size="sm"
              variant={difference === shown ? 'secondary' : 'ghost'}
              aria-current={difference === shown ? 'true' : undefined}
              // A path can be 240 bytes: it is cut short to fit, and shown
              // whole with its comparison and on hover.
              className="max-w-full"
              title={difference.path}
              onClick={() => setPath(difference.path)}
            >
              <Badge variant={BADGES[difference.kind]}>
                {KINDS[difference.kind]}
              </Badge>
              <span className="min-w-0 truncate">{difference.path}</span>
            </Button>
          ))}
        </nav>
      )}
      {shown === undefined ? null : (
        <FileComparison
          // A new pair of files starts from its own read.
          key={`${earlier.revision}:${later.revision}:${shown.path}:${epoch}`}
          client={client}
          artifactId={artifact.id}
          difference={shown}
          revisions={[earlier.revision, later.revision]}
          epoch={epoch}
        />
      )}
      <RestoreDialog
        restore={restore}
        artifact={artifact}
        revision={earlier.revision}
        unsaved={unsaved}
        resets={resets}
        stale={stale}
      />
    </div>
  );
}

const KINDS = { added: 'Added', removed: 'Removed', changed: 'Changed' };
const BADGES = {
  added: 'secondary',
  removed: 'destructive',
  changed: 'outline',
} as const;

function count(value: number, noun: string): string {
  return `${value} ${noun}${value === 1 ? '' : 's'}`;
}

/** The two sides of a text file, or why only sizes are compared. */
type Texts =
  | { readonly kind: 'text'; readonly before: string; readonly after: string }
  | { readonly kind: 'sizes' };

interface FileComparisonProps {
  client: ArtifreeClient;
  artifactId: string;
  difference: FileDifference;
  /** The earlier and later revision. */
  revisions: readonly [number, number];
  epoch: number;
}

/** One differing file: its lines when both sides are text, else sizes. */
function FileComparison({
  client,
  artifactId,
  difference,
  revisions: [earlier, later],
  epoch,
}: FileComparisonProps) {
  const { path, before, after } = difference;
  const comparable = [before, after].every(
    (file) =>
      file === undefined ||
      (TEXT_MEDIA.test(file.media_type) && file.size <= MAX_TEXT_BYTES),
  );
  const [attempt, setAttempt] = useState(0);
  const texts = useRequest<Texts>(
    comparable
      ? `${artifactId}:${earlier}:${later}:${path}:${epoch}:${attempt}`
      : undefined,
    async (signal) => {
      const read = async (revision: number, file: RevisionFile | undefined) =>
        file === undefined
          ? ''
          : decode(
              await client.readFile(artifactId, revision, path, { signal }),
            );
      const [old, next] = await Promise.all([
        read(earlier, before),
        read(later, after),
      ]);
      return old === undefined || next === undefined
        ? { kind: 'sizes' }
        : { kind: 'text', before: old, after: next };
    },
  );
  const lines = useMemo(
    () =>
      texts.status === 'ready' && texts.value.kind === 'text'
        ? diffLines(
            splitLines(texts.value.before),
            splitLines(texts.value.after),
          )
        : undefined,
    [texts],
  );
  const sizes = (
    <p className="text-sm text-muted-foreground">
      {difference.kind === 'changed'
        ? `${path} is not compared line by line; only its size is compared: ${before?.size} → ${after?.size} bytes.`
        : `${path} was ${difference.kind} (${(after ?? before)?.size} bytes).`}
    </p>
  );
  if (!comparable) return sizes;
  if (texts.status === 'loading')
    return (
      <output className="text-sm text-muted-foreground">Reading {path}…</output>
    );
  if (texts.status === 'error')
    return (
      <Alert variant="destructive">
        <AlertTitle>Could not read {path}</AlertTitle>
        <AlertDescription>
          <p>{texts.message}</p>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => setAttempt((value) => value + 1)}
          >
            <RotateCwIcon data-icon="inline-start" aria-hidden="true" />
            Try again
          </Button>
        </AlertDescription>
      </Alert>
    );
  if (texts.value.kind === 'sizes' || lines === undefined) return sizes;
  if (lines.length === 0)
    return (
      <p className="text-sm text-muted-foreground">
        {path} was {difference.kind} empty.
      </p>
    );
  const groups = hunks(lines);
  return (
    <section aria-label={`Changes to ${path}`} className="flex flex-col gap-2">
      <h3 className="text-sm font-medium break-all">{path}</h3>
      <Diff caption={`Changes to ${path}`}>
        {groups.map((group, index) => (
          <Fragment key={group[0]?.before ?? group[0]?.after ?? index}>
            {group[0] === lines[0] ? null : <DiffGap>⋯</DiffGap>}
            {group.map((line) => (
              <DiffRow
                key={`${line.kind}:${line.before}:${line.after}`}
                kind={line.kind}
                before={line.before}
                after={line.after}
              >
                {visible(line.text)}
              </DiffRow>
            ))}
          </Fragment>
        ))}
        {groups.at(-1)?.at(-1) === lines.at(-1) ? null : <DiffGap>⋯</DiffGap>}
      </Diff>
    </section>
  );
}

/** UTF-8 text, or undefined for bytes that are not. */
function decode(bytes: Uint8Array): string | undefined {
  try {
    // The byte order mark is kept, so that a change to it shows.
    return new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(
      bytes,
    );
  } catch {
    return undefined;
  }
}

/**
 * A line as shown: without its line feed, with visible marks for a
 * carriage return, a byte order mark, and a missing final line feed.
 */
function visible(line: string): string {
  const ended = line.endsWith('\n');
  const text = (ended ? line.slice(0, -1) : line)
    .replaceAll('\uFEFF', '⟨BOM⟩')
    .replaceAll('\r', '␍');
  return ended ? text : `${text}⟨no line feed⟩`;
}

interface RestoreDialogProps {
  restore: Restore;
  artifact: Artifact;
  /** The revision the comparison shows on the left. */
  revision: number;
  unsaved: boolean;
  resets: number;
  stale: boolean;
}

/** What a restore is prepared from when its dialog opens. */
interface Prepared {
  readonly revision: number;
  readonly base: number;
  /** The page's count of possible data replacements then. */
  readonly resets: number;
}

/**
 * Restores `revision` as the next revision after a confirmation. What it
 * restores and the base are fixed when the dialog opens, so a retry after
 * a lost answer is the same request while newer revisions arrive. After a
 * conflict, the next attempt waits until the page has read past the
 * refused base, and is prepared against what it read. Opened before the
 * server's data may have been replaced, it sends nothing.
 */
function RestoreDialog({
  restore,
  artifact,
  revision,
  unsaved,
  resets,
  stale,
}: RestoreDialogProps) {
  const latest = artifact.current_revision;
  const [open, setOpen] = useState(false);
  const [prepared, setPrepared] = useState<Prepared>({
    revision,
    base: latest,
    resets,
  });
  // A refused base gives way to the latest read once the page has it.
  const base =
    !restore.ready(prepared.base) && restore.ready(latest)
      ? latest
      : prepared.base;
  const replaced = prepared.resets !== resets;
  const pending = restore.state.status === 'sending';
  const target = open ? prepared.revision : revision;

  async function confirm(): Promise<void> {
    if (replaced || !restore.ready(base)) return;
    setPrepared({ ...prepared, base });
    const result = await restore.write({
      revision: prepared.revision,
      base_revision: base,
    });
    if (result === 'done') setOpen(false);
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
              setPrepared({ revision, base: latest, resets });
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
                disabled={revision === latest || unsaved || stale || pending}
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
            {/* Replaced data sends nothing, so no retry is offered. */}
            {replaced ? (
              <ReplacedNotice />
            ) : (
              <WriteOutcome
                state={restore.state}
                action="restore"
                changed="A revision was saved"
                caughtUp={restore.caughtUp(latest)}
              />
            )}
            <DialogFooter>
              <DialogClose
                render={<Button variant="outline" disabled={pending} />}
              >
                Cancel
              </DialogClose>
              <Button
                type="button"
                disabled={replaced || !restore.ready(base)}
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
        ) : revision === latest ? (
          <FieldDescription>
            Revision {revision} is the latest; choose an earlier one to restore.
          </FieldDescription>
        ) : null}
      </Field>
    </FieldGroup>
  );
}
