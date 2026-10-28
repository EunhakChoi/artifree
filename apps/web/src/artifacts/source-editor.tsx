import { useEffect, useEffectEvent, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import {
  ArchiveRestoreIcon,
  FilePlusIcon,
  PencilIcon,
  RotateCwIcon,
  SaveIcon,
  Trash2Icon,
  Undo2Icon,
} from 'lucide-react';
import {
  ArtifreeApiError,
  encodeFile,
  newIdempotencyKey,
} from '@artifree/client';
import type { ArtifreeClient } from '@artifree/client';
import type {
  Artifact,
  Revision,
  SaveRevisionRequest,
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
import {
  Field,
  FieldDescription,
  FieldError,
  FieldGroup,
  FieldLabel,
} from '@/components/ui/field';
import { Input } from '@/components/ui/input';
import { Textarea } from '@/components/ui/textarea';
import { toEditable } from './editable-text';
import { describePathError, pathError } from './package-path';
import { PreviewFrame } from './preview-frame';
import {
  add,
  edit,
  emptyDraft,
  entries,
  exists,
  isEmpty,
  plan,
  problems,
  remove,
  rename,
  restore,
} from './source-draft';
import type { DraftEntry, SourceDraft } from './source-draft';
import { describeFailure, mayHaveCommitted, useRequest } from './use-request';

/** Types the editor shows as text; any other file is moved or deleted only. */
const TEXT_TYPES = /^(text\/|application\/json|application\/xml|image\/svg)/;

/** An artifact's metadata and history, with when they were read. */
export interface Loaded {
  readonly artifact: Artifact;
  readonly revisions: Revision[];
  /** The detail's reload count when the read was asked for. */
  readonly generation: number;
  /** The feed epoch the read belongs to; content read for it is keyed by it. */
  readonly epoch: number;
  /** The page's reset count then; a later reset may have replaced it. */
  readonly resets: number;
}

/** The revision a draft was written against, as it was read. */
interface DraftBase {
  readonly revision: Revision;
  readonly epoch: number;
  readonly resets: number;
}

interface SourcePanelProps {
  client: ArtifreeClient;
  loaded: Loaded;
  /** The page's reset count now. */
  resets: number;
  /** The newest revision a notice or list reported, if any. */
  known: number | undefined;
  onSaved: (artifact: Artifact) => void;
  /** A save against `base` was refused as stale. */
  onRefused: (base: number, current: number | undefined) => void;
  onLoadLatest: () => void;
  unsaved: boolean;
  onUnsavedChange: (unsaved: boolean) => void;
  /** A newer state is loading and will replace the editor. */
  reloading: boolean;
  /** The artifact is in the trash, where nothing can be saved. */
  inTrash: boolean;
}

type SaveState =
  | { readonly status: 'idle' }
  | { readonly status: 'saving' }
  | { readonly status: 'conflict'; readonly current: number | undefined }
  | {
      readonly status: 'failed';
      readonly message: string;
      /** Whether the save may have been committed although it failed. */
      readonly unknown: boolean;
    };

/**
 * The files of one revision and a draft of changes to them (decision 0017).
 * Every file can be opened while the draft grows; the draft is saved as one
 * revision, against the revision it was written on, or not at all.
 */
export function SourcePanel({
  client,
  loaded,
  resets,
  known,
  onSaved,
  onRefused,
  onLoadLatest,
  unsaved,
  onUnsavedChange,
  reloading,
  inTrash,
}: SourcePanelProps) {
  const { artifact, revisions } = loaded;
  // While a draft exists it keeps the revision it was written against, even
  // when newer ones arrive or it is no longer listed (0007 decision 8).
  const [held, setHeld] = useState<DraftBase>();
  const [changes, setChanges] = useState<SourceDraft>();
  if (!unsaved && held !== undefined) setHeld(undefined);
  if (!unsaved && changes !== undefined) setChanges(undefined);
  const [chosen, setChosen] = useState<string>();
  const [save, setSave] = useState<SaveState>({ status: 'idle' });
  // The last request sent and its key. Sending the same request again
  // reuses the key, so a retry after a lost answer cannot commit twice.
  const [attempt, setAttempt] = useState<{ body: string; key: string }>();
  const [showPreview, setShowPreview] = useState(false);
  // The page's reset count as last rendered, for a save that awaits reads
  // before it sends.
  const resetsNow = useRef(resets);
  useEffect(() => {
    resetsNow.current = resets;
  }, [resets]);
  // Whatever replaces this panel discards its draft, so it must not leave
  // the workspace locked.
  const release = useEffectEvent(() => onUnsavedChange(false));
  useEffect(() => () => release(), []);

  const current = revisions.find(
    (item) => item.revision === artifact.current_revision,
  );
  const base: DraftBase | undefined =
    held ??
    (current === undefined
      ? undefined
      : { revision: current, epoch: loaded.epoch, resets: loaded.resets });
  // Without a draft, nothing is read for metadata that predates a possible
  // data replacement, and nothing read meanwhile is shown. A draft stays.
  if (held === undefined && loaded.resets !== resets)
    return (
      <output className="text-sm text-muted-foreground">
        The source is read once the artifact is read again.
      </output>
    );
  if (base === undefined)
    return (
      <p className="text-sm text-muted-foreground">
        The current revision is not listed yet.
      </p>
    );
  const baseNumber = base.revision.revision;
  const baseResets = base.resets;
  const draft =
    changes?.base === baseNumber
      ? changes
      : emptyDraft(baseNumber, base.revision.files);
  const listed = entries(draft);
  const selected =
    listed.find((entry) => entry.path === chosen) ??
    listed.find((entry) => entry.path === 'index.html') ??
    listed[0];
  const found = problems(draft);
  const saving = save.status === 'saving';
  // Read-only while a request will replace the draft: a save in flight
  // carries it as sent, and a reload brings a new revision.
  const locked = saving || reloading;
  // In the trash every read is refused, so the files a draft shows stay as
  // they are: opening, adding, moving, or restoring one would read it.
  const fixed = locked || inTrash;
  const replaced = held !== undefined && held.resets !== resets;
  const latest = Math.max(artifact.current_revision, known ?? 0);
  // While this page's own save is in flight, or after its answer was lost,
  // a newer revision may be that save: no notice then.
  const newer =
    !replaced &&
    latest > baseNumber &&
    save.status !== 'saving' &&
    !(save.status === 'failed' && save.unknown);
  const outdated = replaced || newer || save.status === 'conflict';
  const changed = !isEmpty(draft);

  function update(next: SourceDraft): void {
    setChanges(next);
    setSave({ status: 'idle' });
    const dirty = !isEmpty(next);
    setHeld(dirty ? base : undefined);
    onUnsavedChange(dirty);
  }

  async function commit(): Promise<void> {
    setSave({ status: 'saving' });
    let request: SaveRevisionRequest;
    try {
      // Moved files are read from the pinned base revision, which never
      // changes; a failed read sends nothing.
      const changes = await Promise.all(
        plan(draft).map(async (change) => {
          if (change.op === 'delete') return change;
          const bytes =
            'bytes' in change
              ? change.bytes
              : await client.readFile(artifact.id, baseNumber, change.copyOf);
          return { op: 'put' as const, ...encodeFile(change.path, bytes) };
        }),
      );
      request = { base_revision: baseNumber, changes };
    } catch (error: unknown) {
      setSave({
        status: 'failed',
        message: describeFailure(error),
        unknown: false,
      });
      return;
    }
    // The server's data may have been replaced while the moved bytes were
    // read: the base number may now name other content, so nothing is sent
    // (0011). The draft says why.
    if (resetsNow.current !== baseResets) {
      setSave({ status: 'idle' });
      return;
    }
    const body = JSON.stringify(request);
    const key = attempt?.body === body ? attempt.key : newIdempotencyKey();
    setAttempt({ body, key });
    try {
      const result = await client.saveRevision(artifact.id, request, {
        idempotencyKey: key,
      });
      setSave({ status: 'idle' });
      onSaved(result.artifact);
    } catch (error: unknown) {
      if (
        error instanceof ArtifreeApiError &&
        error.code === 'revision_conflict'
      ) {
        setSave({ status: 'conflict', current: error.currentRevision });
        onRefused(baseNumber, error.currentRevision);
      } else
        setSave({
          status: 'failed',
          message: describeFailure(error),
          unknown: mayHaveCommitted(error),
        });
    }
  }

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-col gap-4 md:flex-row">
        <FileTree
          entries={listed}
          selected={selected?.path}
          onSelect={setChosen}
          disabled={fixed}
          onAdd={(path) => {
            update(add(draft, path));
            setChosen(path);
          }}
          taken={(path) => taken(draft, path)}
        />
        <div className="flex min-w-0 flex-1 flex-col gap-4">
          {selected === undefined ? null : (
            <FileView
              key={`${baseNumber}:${base.epoch}:${selected.path}`}
              client={client}
              artifactId={artifact.id}
              draft={draft}
              entry={selected}
              epoch={base.epoch}
              locked={locked}
              fixed={fixed}
              onChange={update}
              onMoved={setChosen}
            />
          )}
        </div>
        {showPreview && !inTrash ? (
          <div className="flex min-w-0 flex-1 flex-col gap-2">
            <p className="text-sm text-muted-foreground">
              Saved revision {baseNumber}. The draft is not previewed until it
              is saved.
            </p>
            <PreviewFrame
              client={client}
              artifactId={artifact.id}
              title={`Saved revision ${baseNumber} of ${artifact.title}`}
              revision={baseNumber}
              epoch={base.epoch}
              stale={base.resets !== resets}
            />
          </div>
        ) : null}
      </div>
      <FieldGroup>
        {found.length > 0 ? (
          <Alert variant="destructive">
            <AlertTitle>The server would refuse these changes</AlertTitle>
            <AlertDescription>
              <ul className="list-disc pl-4">
                {found.map((problem) => (
                  <li key={`${problem.path ?? ''}:${problem.message}`}>
                    {problem.message}
                  </li>
                ))}
              </ul>
            </AlertDescription>
          </Alert>
        ) : null}
        {replaced ? (
          <Alert variant="destructive">
            <AlertTitle>
              The server&apos;s data may have been replaced
            </AlertTitle>
            <AlertDescription>
              This draft was written against revision {baseNumber} as it was
              before the server&apos;s data changed. A revision with that number
              may now hold other content, so saving is disabled. Copy your
              changes if you need them, then load the latest revision.
            </AlertDescription>
          </Alert>
        ) : null}
        {newer && save.status !== 'conflict' ? (
          <Alert>
            <AlertTitle>Revision {latest} was saved elsewhere</AlertTitle>
            <AlertDescription>
              This draft was written against revision {baseNumber}, so saving it
              will be refused. Copy your changes if you need them, then load the
              latest revision.
            </AlertDescription>
          </Alert>
        ) : null}
        {save.status === 'conflict' ? (
          <Alert variant="destructive">
            <AlertTitle>Not saved: the artifact changed</AlertTitle>
            <AlertDescription>
              Revision {save.current ?? 'newer'} was saved elsewhere after you
              started editing. Copy your changes if you need them, then load the
              latest revision.
            </AlertDescription>
          </Alert>
        ) : null}
        {save.status === 'failed' ? (
          <Alert variant="destructive">
            <AlertTitle>Not saved</AlertTitle>
            <AlertDescription>{save.message}</AlertDescription>
          </Alert>
        ) : null}
        <FieldDescription>
          {changed
            ? `${draft.changes.size} ${draft.changes.size === 1 ? 'path changes' : 'paths change'}. `
            : ''}
          Saving commits every change as revision {baseNumber + 1}, only if
          revision {baseNumber} is still current.
        </FieldDescription>
        <Field orientation="horizontal" className="flex-wrap">
          <Button
            type="button"
            disabled={
              !changed || found.length > 0 || locked || replaced || inTrash
            }
            onClick={() => void commit()}
          >
            <SaveIcon data-icon="inline-start" aria-hidden="true" />
            {saving ? 'Saving…' : 'Save revision'}
          </Button>
          {/* In the trash, discarding is the way out: the latest revision
              cannot be read there. */}
          {changed && !locked && (!outdated || inTrash) ? (
            <Button
              type="button"
              variant="outline"
              onClick={() => update(emptyDraft(baseNumber, draft.files))}
            >
              <Undo2Icon data-icon="inline-start" aria-hidden="true" />
              Discard changes
            </Button>
          ) : null}
          {outdated && !inTrash ? (
            <Button
              type="button"
              variant="outline"
              disabled={reloading}
              onClick={onLoadLatest}
            >
              <RotateCwIcon data-icon="inline-start" aria-hidden="true" />
              {reloading ? 'Loading latest…' : 'Load latest'}
            </Button>
          ) : null}
          {inTrash ? null : (
            <Button
              type="button"
              variant="outline"
              aria-pressed={showPreview}
              onClick={() => setShowPreview((value) => !value)}
            >
              {showPreview ? 'Hide saved preview' : 'Show saved preview'}
            </Button>
          )}
        </Field>
      </FieldGroup>
    </div>
  );
}

/** Why `path` cannot be given to a new or moved file, if it cannot. */
function taken(draft: SourceDraft, path: string): string | undefined {
  const error = pathError(path);
  if (error !== undefined) return describePathError(error);
  if (exists(draft, path)) return `${path} already exists.`;
  const folded = path.toLowerCase();
  const clash = entries(draft).find(
    (entry) =>
      entry.status !== 'deleted' && entry.path.toLowerCase() === folded,
  );
  return clash === undefined
    ? undefined
    : `${clash.path} differs from it only by case.`;
}

const STATUS_LABELS: Record<DraftEntry['status'], string | undefined> = {
  unchanged: undefined,
  edited: 'Edited',
  added: 'Added',
  deleted: 'Deleted',
};

interface FileTreeProps {
  entries: readonly DraftEntry[];
  selected: string | undefined;
  onSelect: (path: string) => void;
  disabled: boolean;
  onAdd: (path: string) => void;
  taken: (path: string) => string | undefined;
}

/** The files of the draft, by path, with what the draft does to each. */
function FileTree({
  entries,
  selected,
  onSelect,
  disabled,
  onAdd,
  taken,
}: FileTreeProps) {
  return (
    <nav aria-label="Files" className="flex flex-col gap-2 md:w-64">
      <PathDialog
        title="New file"
        description="The file starts empty. Its extension decides how it is served."
        trigger={
          <>
            <FilePlusIcon data-icon="inline-start" aria-hidden="true" />
            New file
          </>
        }
        initial=""
        disabled={disabled}
        problem={taken}
        onConfirm={onAdd}
        confirm="Add"
      />
      <ul className="flex flex-col gap-1">
        {entries.map((entry) => {
          const label = STATUS_LABELS[entry.status];
          return (
            <li key={entry.path}>
              <Button
                type="button"
                variant={entry.path === selected ? 'secondary' : 'ghost'}
                size="sm"
                className="w-full justify-start"
                aria-current={entry.path === selected ? 'true' : undefined}
                disabled={disabled}
                onClick={() => onSelect(entry.path)}
              >
                <span
                  className={
                    entry.status === 'deleted'
                      ? 'truncate line-through'
                      : 'truncate'
                  }
                >
                  {entry.path}
                </span>
                {label === undefined ? null : (
                  <Badge variant="outline">{label}</Badge>
                )}
              </Button>
            </li>
          );
        })}
      </ul>
    </nav>
  );
}

interface FileViewProps {
  client: ArtifreeClient;
  artifactId: string;
  draft: SourceDraft;
  entry: DraftEntry;
  /** The feed epoch the draft's base was read in. */
  epoch: number;
  /** The text cannot change. */
  locked: boolean;
  /** No path can be added, moved, deleted, or restored. */
  fixed: boolean;
  onChange: (draft: SourceDraft) => void;
  /** The file was moved to this path. */
  onMoved: (path: string) => void;
}

/** One path of the draft: its text, if it has any, and what can be done. */
function FileView({
  client,
  artifactId,
  draft,
  entry,
  epoch,
  locked,
  fixed,
  onChange,
  onMoved,
}: FileViewProps) {
  const { path } = entry;
  const actions = (
    <div className="flex flex-wrap gap-2">
      {entry.status === 'deleted' ? (
        <Button
          type="button"
          variant="outline"
          size="sm"
          disabled={fixed}
          onClick={() => onChange(restore(draft, path))}
        >
          <ArchiveRestoreIcon data-icon="inline-start" aria-hidden="true" />
          Restore {path}
        </Button>
      ) : (
        <>
          <PathDialog
            title={`Rename ${path}`}
            description="The file keeps its content, edited or not; only its path changes."
            trigger={
              <>
                <PencilIcon data-icon="inline-start" aria-hidden="true" />
                Rename file
              </>
            }
            initial={path}
            disabled={fixed}
            // Judged as if the file had already left its path, so a change
            // of case alone is allowed.
            problem={(to) => taken(remove(draft, path), to)}
            onConfirm={(to) => {
              onChange(rename(draft, path, to));
              onMoved(to);
            }}
            confirm="Rename"
          />
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={fixed}
            onClick={() => onChange(remove(draft, path))}
          >
            <Trash2Icon data-icon="inline-start" aria-hidden="true" />
            Delete file
          </Button>
          {entry.status === 'edited' ? (
            <Button
              type="button"
              variant="outline"
              size="sm"
              disabled={fixed}
              onClick={() => onChange(restore(draft, path))}
            >
              <Undo2Icon data-icon="inline-start" aria-hidden="true" />
              Undo changes to {path}
            </Button>
          ) : null}
        </>
      )}
    </div>
  );
  return (
    <div className="flex flex-col gap-4">
      {actions}
      {entry.status === 'deleted' ? (
        <p className="text-sm text-muted-foreground">
          {path} is deleted when the draft is saved.
        </p>
      ) : (
        <FileText
          client={client}
          artifactId={artifactId}
          draft={draft}
          entry={entry}
          epoch={epoch}
          locked={locked}
          onChange={onChange}
        />
      )}
    </div>
  );
}

interface FileTextProps {
  client: ArtifreeClient;
  artifactId: string;
  draft: SourceDraft;
  entry: DraftEntry;
  epoch: number;
  locked: boolean;
  onChange: (draft: SourceDraft) => void;
}

/**
 * The text of a file, edited as the draft holds it. A file is read from the
 * draft's base revision, from where a rename moved it, or not at all when
 * the draft added it; files that are not text, or whose bytes the editor
 * cannot keep, are moved or deleted only.
 */
function FileText({
  client,
  artifactId,
  draft,
  entry,
  epoch,
  locked,
  onChange,
}: FileTextProps) {
  const { path } = entry;
  const change = draft.changes.get(path);
  const content = change?.kind === 'put' ? change.content : undefined;
  const inBase = draft.files.some((file) => file.path === path);
  const readPath =
    content?.kind === 'copy' ? content.from : inBase ? path : undefined;
  const text =
    entry.mediaType !== undefined && TEXT_TYPES.test(entry.mediaType);
  const [readAttempt, setReadAttempt] = useState(0);
  const read = useRequest(
    text && readPath !== undefined
      ? `${artifactId}:${draft.base}:${readPath}:${epoch}:${readAttempt}`
      : undefined,
    async (signal) =>
      toEditable(
        await client.readFile(artifactId, draft.base, readPath ?? path, {
          signal,
        }),
      ),
  );
  if (!text)
    return (
      <p className="text-sm text-muted-foreground">
        {path} is not a text file. It can be renamed or deleted here, and
        replaced with <code>artifree artifact save</code>.
      </p>
    );
  let original: { readonly text: string; readonly crlf: boolean } | undefined;
  if (readPath !== undefined) {
    if (read.status === 'loading')
      return (
        <output className="text-sm text-muted-foreground">
          Loading {path}…
        </output>
      );
    if (read.status === 'error')
      return (
        <Alert variant="destructive">
          <AlertTitle>Could not read {path}</AlertTitle>
          <AlertDescription>
            <p>{read.message}</p>
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() => setReadAttempt((value) => value + 1)}
            >
              <RotateCwIcon data-icon="inline-start" aria-hidden="true" />
              Try again
            </Button>
          </AlertDescription>
        </Alert>
      );
    const file = read.value;
    if (!file.editable)
      return (
        <Alert>
          <AlertTitle>{path} cannot be edited here</AlertTitle>
          <AlertDescription>
            The file {file.reason}. Editing it as text would change bytes you
            did not touch; rename or delete it here, or replace it with{' '}
            <code>artifree artifact save</code>.
          </AlertDescription>
        </Alert>
      );
    original = file;
  }
  const crlf =
    content?.kind === 'text' ? content.crlf : (original?.crlf ?? false);
  const value =
    content?.kind === 'text' ? content.text : (original?.text ?? '');
  return (
    <Field>
      <FieldLabel htmlFor="source-editor">{path}</FieldLabel>
      <Textarea
        id="source-editor"
        variant="code"
        className="max-h-128 min-h-64"
        spellCheck={false}
        value={value}
        readOnly={locked}
        aria-busy={locked}
        onChange={(event) =>
          onChange(
            edit(
              draft,
              path,
              event.target.value,
              // Only the file's own base text is its original; text moved
              // here from another path counts as changed.
              readPath === path ? original : undefined,
              crlf,
            ),
          )
        }
      />
    </Field>
  );
}

interface PathDialogProps {
  title: string;
  description: string;
  trigger: ReactNode;
  initial: string;
  disabled: boolean;
  /** Why a path cannot be used, if it cannot. */
  problem: (path: string) => string | undefined;
  onConfirm: (path: string) => void;
  confirm: string;
}

/** Asks for a package path, refusing one the server would refuse. */
function PathDialog({
  title,
  description,
  trigger,
  initial,
  disabled,
  problem,
  onConfirm,
  confirm,
}: PathDialogProps) {
  const [open, setOpen] = useState(false);
  const [path, setPath] = useState(initial);
  const [tried, setTried] = useState(false);
  const error = problem(path);
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (next) {
          setPath(initial);
          setTried(false);
        }
        setOpen(next);
      }}
    >
      <DialogTrigger
        render={
          <Button
            variant="outline"
            size="sm"
            className="w-fit"
            disabled={disabled}
          />
        }
      >
        {trigger}
      </DialogTrigger>
      <DialogContent>
        <form
          className="flex flex-col gap-4"
          onSubmit={(event) => {
            event.preventDefault();
            setTried(true);
            if (error !== undefined) return;
            onConfirm(path);
            setOpen(false);
          }}
        >
          <DialogHeader>
            <DialogTitle>{title}</DialogTitle>
            <DialogDescription>{description}</DialogDescription>
          </DialogHeader>
          <Field data-invalid={tried && error !== undefined}>
            <FieldLabel htmlFor="package-path">Path</FieldLabel>
            <Input
              id="package-path"
              value={path}
              autoComplete="off"
              spellCheck={false}
              aria-invalid={tried && error !== undefined}
              aria-describedby={
                tried && error !== undefined ? 'package-path-error' : undefined
              }
              onChange={(event) => setPath(event.target.value)}
            />
            {tried && error !== undefined ? (
              <FieldError id="package-path-error">{error}</FieldError>
            ) : null}
          </Field>
          <DialogFooter>
            <DialogClose render={<Button variant="outline" />}>
              Cancel
            </DialogClose>
            <Button type="submit">{confirm}</Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
