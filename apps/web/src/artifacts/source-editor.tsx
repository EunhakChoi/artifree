import { useEffect, useEffectEvent, useState } from 'react';
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
import { explainPathError, pathError } from './package-path';
import { PreviewFrame } from './preview-frame';
import {
  add,
  edit,
  emptyDraft,
  entries,
  exists,
  isEmpty,
  movingReads,
  plan,
  problems,
  remove,
  rename,
  restore,
} from './source-draft';
import type { Content, DraftEntry, SourceDraft } from './source-draft';
import { describeFailure, mayHaveCommitted, useRequest } from './use-request';

/** Types the editor shows as text; other files are moved or deleted only. */
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

/** Reads a file of the draft's base revision (decision 0017, item 4). */
type ReadBase = (path: string, signal?: AbortSignal) => Promise<Uint8Array>;

interface SourcePanelProps {
  client: ArtifreeClient;
  loaded: Loaded;
  /** The page's reset count now. */
  resets: number;
  /** Whether an epoch is still the feed's, as of the last event received. */
  isCurrent: (epoch: number) => boolean;
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
  /** The artifact is in the trash, where nothing is read or saved. */
  inTrash: boolean;
}

type SaveState =
  | { readonly status: 'idle' }
  | { readonly status: 'saving' }
  | { readonly status: 'conflict'; readonly current: number | undefined }
  | { readonly status: 'failed'; readonly message: string };

/**
 * The files of one revision and a draft of changes to them (decision 0017).
 * Any file can be opened while the draft grows; the draft is saved as one
 * revision against the revision it was written on, or not at all.
 */
export function SourcePanel({
  client,
  loaded,
  resets,
  isCurrent,
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
  // Whether a save of this draft may have committed although its answer
  // was lost. Only a success settles it, or the draft's end: a later
  // failure says nothing about the earlier request.
  const [uncertain, setUncertain] = useState(false);
  if (!unsaved && held !== undefined) setHeld(undefined);
  if (!unsaved && changes !== undefined) setChanges(undefined);
  if (!unsaved && uncertain) setUncertain(false);
  const [chosen, setChosen] = useState<string>();
  const [save, setSave] = useState<SaveState>({ status: 'idle' });
  // The last request sent and its key. Sending the same request again
  // reuses the key, so a retry after a lost answer cannot commit twice.
  const [attempt, setAttempt] = useState<{ body: string; key: string }>();
  const [showPreview, setShowPreview] = useState(false);
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
  const baseEpoch = base.epoch;
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
  const changed = !isEmpty(draft);
  const saving = save.status === 'saving';
  // Read-only while a request will replace the draft: a save in flight
  // carries it as sent, and a reload brings a new revision.
  const locked = saving || reloading;
  // In the trash every read is refused, so the paths stay as they are:
  // opening, adding, moving, or restoring a file would read it.
  const fixed = locked || inTrash;
  // The base number may now name other content: nothing is read for it,
  // nothing read before is moved, and nothing is saved (0011).
  const replaced = base.resets !== resets;
  const latest = Math.max(artifact.current_revision, known ?? 0);
  // While this page's own save is in flight, or may have committed, a
  // newer revision may be that save: no notice then.
  const newer = !replaced && latest > baseNumber && !saving && !uncertain;
  const outdated = replaced || newer || save.status === 'conflict';

  // The one reader of the draft's revision. The feed's epoch changes as
  // soon as a `reset` arrives, before this page renders it, so a read that
  // settles in between is refused too. In the trash no read starts: no
  // file can be opened or moved there.
  const readBase: ReadBase = async (path, signal) => {
    const refused = new Error(
      "Nothing was read: the server's data may have been replaced since this draft's revision was read.",
    );
    if (!isCurrent(baseEpoch)) throw refused;
    const bytes = await client.readFile(
      artifact.id,
      baseNumber,
      path,
      signal === undefined ? {} : { signal },
    );
    if (!isCurrent(baseEpoch)) throw refused;
    return bytes;
  };

  function update(next: SourceDraft): void {
    setChanges(next);
    setSave({ status: 'idle' });
    const dirty = !isEmpty(next);
    setHeld(dirty ? base : undefined);
    onUnsavedChange(dirty);
  }

  async function commit(): Promise<void> {
    // The draft holds every byte it saves, so nothing is awaited between
    // this check of the base and the request.
    const request: SaveRevisionRequest = {
      base_revision: baseNumber,
      changes: plan(draft).map((change) =>
        change.op === 'delete'
          ? change
          : { op: 'put', ...encodeFile(change.path, change.bytes) },
      ),
    };
    const body = JSON.stringify(request);
    const key = attempt?.body === body ? attempt.key : newIdempotencyKey();
    setAttempt({ body, key });
    setSave({ status: 'saving' });
    try {
      const result = await client.saveRevision(artifact.id, request, {
        idempotencyKey: key,
      });
      setSave({ status: 'idle' });
      setUncertain(false);
      onSaved(result.artifact);
    } catch (error: unknown) {
      if (
        error instanceof ArtifreeApiError &&
        error.code === 'revision_conflict'
      ) {
        setSave({ status: 'conflict', current: error.currentRevision });
        onRefused(baseNumber, error.currentRevision);
        return;
      }
      setSave({ status: 'failed', message: describeFailure(error) });
      if (mayHaveCommitted(error)) setUncertain(true);
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
          taken={(path) => whyTaken(draft, path)}
          onAdd={(path) => {
            update(add(draft, path));
            setChosen(path);
          }}
        />
        <div className="flex min-w-0 flex-1 flex-col gap-4">
          {selected === undefined ? null : (
            <FileView
              key={`${baseNumber}:${base.epoch}:${selected.path}`}
              draft={draft}
              entry={selected}
              readBase={readBase}
              canRead={!replaced && !inTrash}
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
              Saved revision {baseNumber}. The draft is previewed once it is
              saved.
            </p>
            <PreviewFrame
              client={client}
              artifactId={artifact.id}
              revision={baseNumber}
              epoch={base.epoch}
              stale={replaced}
              title={`Saved revision ${baseNumber} of ${artifact.title}`}
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
                  <li key={problem.message}>{problem.message}</li>
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
              may now hold other content, so saving is disabled and no file of
              it is read. Copy your changes if you need them, then load the
              latest revision.
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
            <AlertDescription>
              {save.message}
              {uncertain
                ? ' An earlier attempt may have been saved; saving the same changes again will not save them twice.'
                : ''}
            </AlertDescription>
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

/** Why no file can be added or moved to `path` in `draft`, if none can. */
function whyTaken(draft: SourceDraft, path: string): string | undefined {
  const error = pathError(path);
  if (error !== undefined) return explainPathError(error);
  if (exists(draft, path)) return `${path} already exists.`;
  const folded = path.toLowerCase();
  const twin = entries(draft).find(
    (entry) =>
      entry.status !== 'deleted' && entry.path.toLowerCase() === folded,
  );
  return twin === undefined
    ? undefined
    : `${twin.path} differs from it only by case.`;
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
  /** No file can be opened or added. */
  disabled: boolean;
  taken: (path: string) => string | undefined;
  onAdd: (path: string) => void;
}

/** The draft's paths, with what the draft does to each. */
function FileTree({
  entries,
  selected,
  onSelect,
  disabled,
  taken,
  onAdd,
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
        confirm="Add"
        onConfirm={(path) => {
          onAdd(path);
          return Promise.resolve(undefined);
        }}
      />
      <ul className="flex flex-col gap-1">
        {entries.map((entry) => {
          const label = STATUS_LABELS[entry.status];
          const current = entry.path === selected;
          return (
            <li key={entry.path}>
              <Button
                type="button"
                variant={current ? 'secondary' : 'ghost'}
                size="sm"
                className="w-full justify-start"
                aria-current={current ? 'true' : undefined}
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
  draft: SourceDraft;
  entry: DraftEntry;
  readBase: ReadBase;
  /**
   * Files of the base revision can be read now; a read refused for it is
   * not offered again. What was read before stays shown.
   */
  canRead: boolean;
  /** The text cannot change. */
  locked: boolean;
  /** No path can be added, moved, deleted, or restored. */
  fixed: boolean;
  onChange: (draft: SourceDraft) => void;
  /** The file now is at this path. */
  onMoved: (path: string) => void;
}

/** One path of the draft: what can be done to it, and its text. */
function FileView({
  draft,
  entry,
  readBase,
  canRead,
  locked,
  fixed,
  onChange,
  onMoved,
}: FileViewProps) {
  const { path } = entry;
  if (entry.status === 'deleted')
    return (
      <div className="flex flex-col gap-4">
        <Button
          type="button"
          variant="outline"
          size="sm"
          className="w-fit"
          disabled={fixed}
          onClick={() => onChange(restore(draft, path))}
        >
          <ArchiveRestoreIcon data-icon="inline-start" aria-hidden="true" />
          Restore {path}
        </Button>
        <p className="text-sm text-muted-foreground">
          {path} is deleted when the draft is saved.
        </p>
      </div>
    );
  async function move(to: string): Promise<string | undefined> {
    let bytes: Uint8Array | undefined;
    if (movingReads(draft, path))
      try {
        bytes = await readBase(path);
      } catch (error: unknown) {
        return `${path} was not moved. ${describeFailure(error)}`;
      }
    onChange(rename(draft, path, to, bytes));
    onMoved(to);
    return undefined;
  }
  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap gap-2">
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
          // Judged as if the file had left its path, so changing only the
          // case of its name is allowed.
          problem={(to) => whyTaken(remove(draft, path), to)}
          confirm="Rename"
          onConfirm={move}
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
      </div>
      <FileText
        draft={draft}
        entry={entry}
        readBase={readBase}
        canRead={canRead}
        locked={locked}
        onChange={onChange}
      />
    </div>
  );
}

interface FileTextProps {
  draft: SourceDraft;
  entry: DraftEntry;
  readBase: ReadBase;
  canRead: boolean;
  locked: boolean;
  onChange: (draft: SourceDraft) => void;
}

/** Text a file shows in the editor, or why it shows none. */
type Shown =
  | {
      readonly kind: 'text';
      readonly text: string;
      readonly crlf: boolean;
      /** The path's own base text, against which an edit is compared. */
      readonly original: string | undefined;
    }
  | { readonly kind: 'none'; readonly message: ReactNode };

/** What the draft itself holds for a path, without reading anything. */
function shownFromDraft(content: Content): Shown {
  if (content.kind === 'text')
    return {
      kind: 'text',
      text: content.text,
      crlf: content.crlf,
      original: content.original,
    };
  const decoded = toEditable(content.bytes);
  return decoded.editable
    ? // Moved bytes are not this path's own base text.
      {
        kind: 'text',
        text: decoded.text,
        crlf: decoded.crlf,
        original: undefined,
      }
    : { kind: 'none', message: `The file ${decoded.reason}.` };
}

/**
 * The text of a path, edited as the draft holds it. What the draft holds is
 * shown without reading; a path the draft has not changed is read from the
 * base revision, and stays read while its text is edited, so returning to
 * the original does not wait again. Text read before the artifact moved to
 * the trash, or before a possible replacement, stays shown and editable;
 * the reader refuses any read after either.
 */
function FileText({
  draft,
  entry,
  readBase,
  canRead,
  locked,
  onChange,
}: FileTextProps) {
  const { path } = entry;
  const change = draft.changes.get(path);
  const content = change?.kind === 'put' ? change.content : undefined;
  const isText =
    entry.mediaType !== undefined && TEXT_TYPES.test(entry.mediaType);
  const ownBase =
    draft.files.some((file) => file.path === path) &&
    (content === undefined ||
      (content.kind === 'text' && content.original !== undefined));
  const [attempt, setAttempt] = useState(0);
  const read = useRequest(
    isText && ownBase ? `${draft.base}:${path}:${attempt}` : undefined,
    async (signal) => toEditable(await readBase(path, signal)),
  );
  if (!isText)
    return (
      <p className="text-sm text-muted-foreground">
        {path} is not a text file. It can be renamed or deleted here, and
        replaced with <code>artifree artifact save</code>.
      </p>
    );
  let shown: Shown;
  if (content !== undefined) shown = shownFromDraft(content);
  else if (read.status === 'loading')
    return (
      <output className="text-sm text-muted-foreground">Loading {path}…</output>
    );
  else if (read.status === 'error')
    return (
      <Alert variant="destructive">
        <AlertTitle>Could not read {path}</AlertTitle>
        <AlertDescription>
          <p>{read.message}</p>
          {canRead ? (
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() => setAttempt((value) => value + 1)}
            >
              <RotateCwIcon data-icon="inline-start" aria-hidden="true" />
              Try again
            </Button>
          ) : null}
        </AlertDescription>
      </Alert>
    );
  else
    shown = read.value.editable
      ? {
          kind: 'text',
          text: read.value.text,
          crlf: read.value.crlf,
          original: read.value.text,
        }
      : { kind: 'none', message: `The file ${read.value.reason}.` };
  if (shown.kind === 'none')
    return (
      <Alert>
        <AlertTitle>{path} cannot be edited here</AlertTitle>
        <AlertDescription>
          {shown.message} Editing it as text would change bytes you did not
          touch; rename or delete it here, or replace it with{' '}
          <code>artifree artifact save</code>.
        </AlertDescription>
      </Alert>
    );
  const { text, crlf, original } = shown;
  return (
    <Field>
      <FieldLabel htmlFor="source-editor">{path}</FieldLabel>
      <Textarea
        id="source-editor"
        variant="code"
        className="max-h-128 min-h-64"
        spellCheck={false}
        value={text}
        readOnly={locked}
        aria-busy={locked}
        onChange={(event) =>
          onChange(edit(draft, path, event.target.value, crlf, original))
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
  confirm: string;
  /** Uses the path; resolves with why it did not, if it did not. */
  onConfirm: (path: string) => Promise<string | undefined>;
}

/** Asks for a package path, refusing one the server would refuse. */
function PathDialog({
  title,
  description,
  trigger,
  initial,
  disabled,
  problem,
  confirm,
  onConfirm,
}: PathDialogProps) {
  const [open, setOpen] = useState(false);
  const [path, setPath] = useState(initial);
  const [tried, setTried] = useState(false);
  const [pending, setPending] = useState(false);
  const [failure, setFailure] = useState<string>();
  const invalid = problem(path);
  const shown = tried ? (invalid ?? failure) : undefined;
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        // Stay open until the path is used, so its outcome is seen.
        if (pending) return;
        if (next) {
          setPath(initial);
          setTried(false);
          setFailure(undefined);
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
            setFailure(undefined);
            if (invalid !== undefined) return;
            setPending(true);
            void onConfirm(path).then((why) => {
              setPending(false);
              if (why === undefined) setOpen(false);
              else setFailure(why);
            });
          }}
        >
          <DialogHeader>
            <DialogTitle>{title}</DialogTitle>
            <DialogDescription>{description}</DialogDescription>
          </DialogHeader>
          <Field data-invalid={shown !== undefined}>
            <FieldLabel htmlFor="package-path">Path</FieldLabel>
            <Input
              id="package-path"
              value={path}
              autoComplete="off"
              spellCheck={false}
              readOnly={pending}
              aria-invalid={shown !== undefined}
              aria-describedby={
                shown === undefined ? undefined : 'package-path-error'
              }
              onChange={(event) => {
                setPath(event.target.value);
                setFailure(undefined);
              }}
            />
            {shown === undefined ? null : (
              <FieldError id="package-path-error">{shown}</FieldError>
            )}
          </Field>
          <DialogFooter>
            <DialogClose
              render={<Button variant="outline" disabled={pending} />}
            >
              Cancel
            </DialogClose>
            <Button type="submit" disabled={pending}>
              {pending ? 'Reading…' : confirm}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
