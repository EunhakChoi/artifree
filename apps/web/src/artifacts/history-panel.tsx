import { useState } from 'react';
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
import { Field, FieldDescription, FieldLabel } from '@/components/ui/field';
import { LineDiff, LineDiffGap, LineDiffRow } from '@/components/ui/line-diff';
import {
  NativeSelect,
  NativeSelectOption,
} from '@/components/ui/native-select';
import { changedFiles, compareLines, excerpt, toLines } from './compare';
import type { FileChange } from './compare';
import {
  ReplacedNotice,
  WriteNotice,
  lostBase,
  useConditionalWrite,
} from './conditional-write';
import type { WriteGate } from './conditional-write';
import { useChoice } from './use-choice';
import { useRequest } from './use-request';
import { preparedBase } from './write-gate';
import type { Wait } from './write-gate';

/** Text up to this size is compared line by line. */
const MAX_TEXT_BYTES = 1024 * 1024;

const TEXT_TYPES =
  /^(?:text\/|application\/(?:json|xml|javascript|ld\+json)|image\/svg\+xml)/;

/** A restore of `revision`, prepared against `base_revision`. */
interface RestoreRequest extends RestoreRevisionRequest {
  readonly revision: number;
}

/**
 * Restores revisions of one artifact (decision 0010) through the source
 * gate. The detail view holds it, so a retry keeps its key when the
 * history tab closes and opens again.
 */
export function useRestore(
  client: ArtifreeClient,
  artifactId: string,
  gate: WriteGate,
  onRestored: (artifact: Artifact) => void,
  onConflict: () => void,
) {
  return useConditionalWrite({
    gate,
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
  });
}

type Restore = ReturnType<typeof useRestore>;

interface HistoryPanelProps {
  client: ArtifreeClient;
  artifact: Artifact;
  /** Revisions, newest first. */
  revisions: Revision[];
  /** The feed epoch what is shown was read in; file reads are keyed by it. */
  epoch: number;
  /** The count of possible data replacements when it was read. */
  shownResets: number;
  /** How often the server's data may have been replaced so far. */
  resets: number;
  /** Whether the editor holds a draft, which a restore would leave stale. */
  unsaved: boolean;
  restore: Restore;
  /** Why a source write prepared against a revision would wait. */
  wait: (revision: number) => Wait | undefined;
}

/**
 * Compares a revision with another, file by file and for one text file
 * line by line, and restores the earlier one (decision 0011).
 */
export function HistoryPanel(props: HistoryPanelProps) {
  const { artifact, revisions, shownResets } = props;
  const latest = artifact.current_revision;
  // Chosen revisions and file; none means the default.
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
  const { changes, unchanged } = changedFiles(earlier.files, later.files);
  const shown = changes.find((change) => change.path === path) ?? changes[0];
  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-end gap-4">
        <Field orientation="horizontal" className="w-fit">
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
        <Field orientation="horizontal" className="w-fit">
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
        <RestoreDialog {...props} revision={earlier.revision} />
      </div>
      <p className="text-sm">
        {changes.length === 0
          ? `Revisions ${earlier.revision} and ${later.revision} have the same files.`
          : `${count(changes.length, 'file')} differ from revision ${earlier.revision} to revision ${later.revision}; ${count(unchanged, 'file')} unchanged.`}
      </p>
      {changes.length === 0 ? null : (
        <nav aria-label="Changed files" className="flex flex-wrap gap-2">
          {changes.map((change) => (
            <Button
              key={change.path}
              type="button"
              size="sm"
              variant={change === shown ? 'secondary' : 'ghost'}
              aria-current={change === shown ? 'true' : undefined}
              // A path can be 240 bytes: it is cut short here, and shown
              // whole above its comparison and on hover.
              className="max-w-full"
              title={change.path}
              onClick={() => setPath(change.path)}
            >
              <Badge variant={BADGE[change.kind]}>{KIND[change.kind]}</Badge>
              <span className="min-w-0 truncate">{change.path}</span>
            </Button>
          ))}
        </nav>
      )}
      {shown === undefined ? null : (
        <FileComparison
          // Another pair of files starts from its own read.
          key={`${earlier.revision}:${later.revision}:${shown.path}`}
          {...props}
          change={shown}
          earlier={earlier.revision}
          later={later.revision}
        />
      )}
    </div>
  );
}

const KIND = { added: 'Added', removed: 'Removed', changed: 'Changed' };
const BADGE = {
  added: 'secondary',
  removed: 'destructive',
  changed: 'outline',
} as const;

function count(n: number, noun: string): string {
  return `${n} ${noun}${n === 1 ? '' : 's'}`;
}

/** Whether a file is compared line by line. */
function comparable(file: RevisionFile | undefined): boolean {
  return (
    file === undefined ||
    (TEXT_TYPES.test(file.media_type) && file.size <= MAX_TEXT_BYTES)
  );
}

/** The lines of a text file, or undefined for bytes that are not UTF-8. */
function decode(bytes: Uint8Array): string[] | undefined {
  try {
    // A byte order mark is kept, so that a change to it shows.
    return toLines(
      new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(bytes),
    );
  } catch {
    return undefined;
  }
}

/** A line with its invisible differences made visible. */
function visible(text: string): string {
  const body = text.endsWith('\n')
    ? text.slice(0, -1)
    : `${text}⟨no line feed⟩`;
  return body.replaceAll('\uFEFF', '⟨BOM⟩').replaceAll('\r', '␍');
}

function FileComparison({
  client,
  artifact,
  epoch,
  shownResets,
  resets,
  change,
  earlier,
  later,
}: HistoryPanelProps & {
  change: FileChange;
  earlier: number;
  later: number;
}) {
  const [attempt, setAttempt] = useState(0);
  const textual = comparable(change.before) && comparable(change.after);
  // Nothing is read for metadata read before the server's data may have
  // been replaced, and nothing read meanwhile is shown: the same revision
  // and path may name other bytes now (decision 0011, item 9).
  const readable = textual && shownResets === resets;
  const lines = useRequest(
    readable
      ? `${artifact.id}:${earlier}:${later}:${change.path}:${epoch}:${attempt}`
      : undefined,
    async (signal) => {
      const read = (revision: number, file: RevisionFile | undefined) =>
        file === undefined
          ? Promise.resolve([])
          : client
              .readFile(artifact.id, revision, file.path, { signal })
              .then(decode);
      const [before, after] = await Promise.all([
        read(earlier, change.before),
        read(later, change.after),
      ]);
      return before === undefined || after === undefined
        ? undefined
        : compareLines(before, after);
    },
  );
  const sizes = (
    <p className="text-sm text-muted-foreground">
      {change.path} is not compared line by line; only its size is:{' '}
      {change.before?.size ?? 0} → {change.after?.size ?? 0} bytes.
    </p>
  );
  const heading = <h4 className="text-sm font-medium">{change.path}</h4>;
  if (!textual)
    return (
      <section className="flex flex-col gap-2 wrap-anywhere">
        {heading}
        {sizes}
      </section>
    );
  if (shownResets !== resets)
    return (
      <output className="text-sm wrap-anywhere text-muted-foreground">
        {change.path} is compared once the artifact is read again.
      </output>
    );
  if (lines.status === 'loading')
    return (
      <output className="text-sm wrap-anywhere text-muted-foreground">
        Comparing {change.path}…
      </output>
    );
  if (lines.status === 'error')
    return (
      <Alert variant="destructive" className="wrap-anywhere">
        <AlertTitle>Could not compare {change.path}</AlertTitle>
        <AlertDescription>
          <p>{lines.message}</p>
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
  const compared = lines.value;
  return (
    <section className="flex flex-col gap-2 wrap-anywhere">
      {heading}
      {compared === undefined ? (
        sizes
      ) : (
        <LineDiff label={`Changes to ${change.path}`}>
          {excerpt(compared).map((item, index) =>
            item.kind === 'gap' ? (
              <LineDiffGap key={`gap:${index}`}>
                {count(item.count, 'unchanged line')}
              </LineDiffGap>
            ) : (
              <LineDiffRow
                key={`${item.line.old}:${item.line.new}`}
                kind={item.line.kind}
                earlier={item.line.old}
                later={item.line.new}
              >
                {visible(item.line.text)}
              </LineDiffRow>
            ),
          )}
        </LineDiff>
      )}
    </section>
  );
}

/** A restore as the dialog prepared it. */
interface Prepared {
  readonly revision: number;
  readonly base: number;
  /** The page's count of possible data replacements then. */
  readonly resets: number;
}

function RestoreDialog({
  artifact,
  resets,
  unsaved,
  restore,
  wait,
  revision,
}: HistoryPanelProps & { revision: number }) {
  const latest = artifact.current_revision;
  const [open, setOpen] = useState(false);
  const [prepared, setPrepared] = useState<Prepared>({
    revision,
    base: latest,
    resets,
  });
  // As for a rename: the base the dialog opened on, until the page has
  // learned of a newer revision and shows it.
  const { base, behind, retry } = preparedBase(
    prepared.base,
    latest,
    wait,
    lostBase(restore.state),
  );
  const replaced = prepared.resets !== resets;
  const pending = restore.state.status === 'sending';
  const waiting = retry ? undefined : wait(base);

  async function confirm(): Promise<void> {
    if (replaced || waiting !== undefined) return;
    setPrepared({ ...prepared, base });
    const result = await restore.write({
      revision: prepared.revision,
      base_revision: base,
    });
    if (result === 'done') setOpen(false);
  }

  return (
    <Field className="w-fit">
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
              size="sm"
              disabled={
                revision === latest ||
                unsaved ||
                wait(latest) === 'replaced' ||
                wait(latest) === 'sending'
              }
              aria-describedby={
                unsaved
                  ? 'restore-locked'
                  : revision === latest
                    ? 'restore-latest'
                    : undefined
              }
            />
          }
        >
          <HistoryIcon data-icon="inline-start" aria-hidden="true" />
          Restore revision {revision}
        </DialogTrigger>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Restore revision {prepared.revision}</DialogTitle>
            <DialogDescription>
              The next revision will have the files of revision{' '}
              {prepared.revision}, byte for byte. Every revision stays in the
              history, and the title and pin stay as they are.
            </DialogDescription>
          </DialogHeader>
          {/* Nothing is sent again for data that may have been replaced. */}
          {replaced ? (
            <ReplacedNotice state={restore.state} />
          ) : (
            <WriteNotice
              state={restore.state}
              action="restore"
              changed="The artifact"
              behind={behind}
              waiting={waiting}
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
              disabled={replaced || waiting !== undefined}
              onClick={() => void confirm()}
            >
              {pending ? 'Restoring…' : 'Restore'}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
      {unsaved ? (
        <FieldDescription id="restore-locked">
          Restoring waits while the editor holds unsaved changes: save them,
          discard them, or load the latest revision in the source tab.
        </FieldDescription>
      ) : revision === latest ? (
        <FieldDescription id="restore-latest">
          Revision {revision} is the latest; choose an earlier one to restore.
        </FieldDescription>
      ) : null}
    </Field>
  );
}
