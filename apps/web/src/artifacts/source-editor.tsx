import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import type { FormEvent } from 'react';
import {
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
  RevisionFile,
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
import type { EditableText } from './editable-text';
import { explainPathError, pathError } from './package-path';
import { PreviewFrame } from './preview-frame';
import {
  addFile,
  deleteFile,
  exists,
  isEmpty,
  listPaths,
  mediaTypeAt,
  needsBaseBytes,
  plan,
  problems,
  renameFile,
  restoreFile,
  setText,
  startDraft,
} from './source-draft';
import type { Change, DraftPath, Source, SourceDraft } from './source-draft';
import type { DraftRecord, KeptDraft } from './draft-store';
import type { Keeping } from './source-recovery';
import { describeFailure, mayHaveCommitted, useRequest } from './use-request';

/** Types the editor shows as text; other files are moved or deleted only. */
const TEXT_TYPES = /^(text\/|application\/json|application\/xml|image\/svg)/;

const REPLACED =
  "The server's data may have been replaced since this draft's revision was read.";

const UNMATCHED =
  'The server no longer lists the revision this kept draft was written on with the same files.';

/** An artifact's metadata and history, as the detail read them. */
export interface SourceRead {
  readonly artifact: Artifact;
  readonly revisions: Revision[];
  /** The feed epoch the read belongs to. */
  readonly epoch: number;
  /** The page's reset count then; a later reset may have replaced it. */
  readonly resets: number;
}

/** The revision a draft is written against, as it was read. */
interface DraftBase {
  readonly revision: Revision;
  readonly epoch: number;
  readonly resets: number;
  /**
   * A kept draft's revision that the server does not list with the same
   * files and digests (decision 0018, item 5): nothing of it is read.
   */
  readonly unmatched?: true;
}

/**
 * What may change the draft now (decision 0017, item 4): everything; only
 * the text of files the page holds; or nothing. `why` says what holds it.
 */
type Allowance =
  | { readonly level: 'all' }
  | { readonly level: 'text' | 'none'; readonly why: string };

const ALL: Allowance = { level: 'all' };

/** What a change to the draft touches. */
type Kind = 'text' | 'paths';

interface SourcePanelProps {
  client: ArtifreeClient;
  read: SourceRead;
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
  /** The page shows the artifact in the trash, where nothing is read. */
  inTrash: boolean;
  /** Where the draft is kept in the browser, and the one it continues. */
  keeping: Keeping;
}

type SaveState =
  | { readonly status: 'idle' }
  | { readonly status: 'saving' }
  | { readonly status: 'conflict'; readonly current: number | undefined }
  | { readonly status: 'failed'; readonly message: string };

type EndAction =
  | { readonly kind: 'discard' }
  | { readonly kind: 'latest' }
  | {
      readonly kind: 'saved';
      readonly artifact: Artifact;
      readonly draft: SourceDraft;
    };

/**
 * The files of one revision and a draft of changes to them, saved as one
 * revision or not at all (decision 0017).
 */
export function SourcePanel({
  client,
  read,
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
  keeping,
}: SourcePanelProps) {
  const { artifact, revisions } = read;
  const { start } = keeping;
  // While a draft exists it keeps the revision it was written against, also
  // when newer ones arrive or it is no longer listed (0007 decision 8).
  const [held, setHeld] = useState(() =>
    start === undefined ? undefined : keptBase(start, read),
  );
  const [draft, setDraft] = useState(start?.draft);
  // Whether a save of this draft may have committed although its answer
  // was lost. A later failure says nothing about the earlier request, so
  // only a success, or the draft's end, settles it.
  const [uncertain, setUncertain] = useState(start?.uncertain === true);
  if (!unsaved && held !== undefined) setHeld(undefined);
  if (!unsaved && draft !== undefined) setDraft(undefined);
  if (!unsaved && uncertain) setUncertain(false);
  const [chosen, setChosen] = useState<string>();
  const [dialog, setDialog] = useState<{ readonly rename?: string }>();
  const [save, setSave] = useState<SaveState>({ status: 'idle' });
  const [clearing, setClearing] = useState(false);
  const [endFailure, setEndFailure] = useState<{
    readonly action: EndAction;
    readonly message: string;
  }>();
  // The last request sent and its key. Sending the same request again
  // reuses the key, so a retry after a lost answer cannot commit twice. A
  // continued draft kept as sent starts with its key, so a save that may
  // have committed before the page closed replays (decision 0018, item 6).
  const [attempt, setAttempt] = useState(() =>
    start?.sent !== undefined && start.sent.version === start.version
      ? { body: JSON.stringify(requestOf(start.draft)), key: start.sent.key }
      : undefined,
  );
  const [showPreview, setShowPreview] = useState(false);
  // What the write point reads when a change arrives: the draft as last
  // written, what the page last showed, and whether a save is being sent.
  const latest = useRef<SourceDraft | undefined>(undefined);
  const shown = useRef<{
    base: number | undefined;
    allowance: Allowance;
    /** Why no file of the base revision is read now, if none is. */
    unreadable: string | undefined;
  }>({ base: undefined, allowance: ALL, unreadable: undefined });
  const sending = useRef(false);
  const clearingDraft = useRef(false);
  // What is kept with the draft: the count of its changes, the last save
  // sent, and whether a save may have committed (decision 0018).
  const version = useRef(start?.version ?? 0);
  const sent = useRef(start?.sent);
  const attempts = useRef(start?.attempts ?? []);
  const mayHaveSaved = useRef(start?.uncertain === true);
  const current = revisions.find(
    (item) => item.revision === artifact.current_revision,
  );
  const base: DraftBase | undefined =
    held ??
    (current === undefined
      ? undefined
      : { revision: current, epoch: read.epoch, resets: read.resets });
  const unmatched = base?.unmatched === true;
  const replaced = base !== undefined && (unmatched || base.resets !== resets);
  const stale = unmatched ? UNMATCHED : REPLACED;
  const saving = save.status === 'saving';
  const allowance: Allowance =
    saving || clearing || endFailure !== undefined
      ? { level: 'none', why: 'A save of this draft is in progress.' }
      : reloading
        ? { level: 'none', why: 'A newer revision is loading.' }
        : replaced
          ? { level: 'text', why: stale }
          : inTrash
            ? { level: 'text', why: 'The artifact is in the trash.' }
            : ALL;
  // A file of the base revision is read unless the server refuses every
  // read (0014) or the number may name other content (0011, 0018).
  const unreadable = replaced
    ? stale
    : inTrash
      ? 'The artifact is in the trash.'
      : undefined;
  const baseNumber = base?.revision.revision;
  useLayoutEffect(() => {
    latest.current = draft;
    shown.current = { base: baseNumber, allowance, unreadable };
  });

  // Without a draft, nothing is read for metadata that predates a possible
  // data replacement. A draft stays.
  if (held === undefined && replaced)
    return (
      <output className="text-sm text-muted-foreground">
        The source is read once the artifact is read again.
      </output>
    );
  if (base === undefined || baseNumber === undefined)
    return (
      <p className="text-sm text-muted-foreground">
        The current revision is not listed yet.
      </p>
    );
  const baseEpoch = base.epoch;
  // The revision this render's changes are made on.
  const made = baseNumber;
  const shownDraft =
    draft?.base === baseNumber
      ? draft
      : startDraft(baseNumber, base.revision.files);
  const paths = listPaths(shownDraft);
  const selected =
    paths.find((entry) => entry.path === chosen) ??
    paths.find((entry) => entry.path === 'index.html') ??
    paths[0];
  const found = problems(shownDraft);
  const changed = !isEmpty(shownDraft);
  const latestKnown = Math.max(artifact.current_revision, known ?? 0);
  // While this page's save is in flight, or may have committed, a newer
  // revision may be that save: no notice then.
  const newer = !replaced && latestKnown > baseNumber && !saving && !uncertain;
  const outdated = replaced || newer || save.status === 'conflict';

  /**
   * What may change a draft written on `made` now: what the page last
   * showed, unless a save is being sent or the draft belongs to another
   * revision. A continuation or a dialog that outlives a render is judged
   * by the state it lands in.
   */
  function allowanceNow(made: number): Allowance {
    if (shown.current.base !== made)
      return {
        level: 'none',
        why: 'The draft now belongs to another revision.',
      };
    if (sending.current || clearingDraft.current || endFailure !== undefined)
      return { level: 'none', why: 'A save of this draft is in progress.' };
    return shown.current.allowance;
  }

  /**
   * The draft's one write point. Every change names what it touches and is
   * applied to the draft as last written, or refused with the reason.
   */
  function write(
    kind: Kind,
    made: number,
    change: (draft: SourceDraft) => SourceDraft,
  ): string | undefined {
    const now = allowanceNow(made);
    if (now.level === 'none' || (kind === 'paths' && now.level !== 'all'))
      return now.why;
    const before =
      latest.current?.base === made
        ? latest.current
        : startDraft(made, base?.revision.files ?? []);
    const after = change(before);
    latest.current = after;
    setDraft(after);
    setSave({ status: 'idle' });
    const dirty = !isEmpty(after);
    setHeld(base);
    onUnsavedChange(true);
    version.current += 1;
    if (dirty) void keeping.keep(kept(after));
    else void finish({ kind: 'discard' });
    return undefined;
  }

  function markUncertain(value: boolean): void {
    mayHaveSaved.current = value;
    setUncertain(value);
  }

  /** The draft as this page keeps it now. */
  function kept(
    current: SourceDraft,
    uncertainNow = mayHaveSaved.current,
  ): DraftRecord {
    return {
      artifactId: artifact.id,
      title: artifact.title,
      keptAt: Date.now(),
      draft: current,
      version: version.current,
      sent: sent.current,
      attempts: attempts.current,
      uncertain: uncertainNow,
    };
  }

  /** Completes an end after the browser removed its kept draft, or by choice. */
  function completeEnd(action: EndAction): void {
    setEndFailure(undefined);
    // A save answer can become stale while its kept record is being removed.
    // Keep the draft again instead of applying that answer to replaced data.
    if (action.kind === 'saved' && !isCurrent(baseEpoch)) {
      markUncertain(true);
      void keeping.keep(kept(action.draft, true));
      setSave({ status: 'idle' });
      return;
    }
    sent.current = undefined;
    attempts.current = [];
    setAttempt(undefined);
    markUncertain(false);
    if (action.kind === 'saved') onSaved(action.artifact);
    else if (action.kind === 'latest') onLoadLatest();
    else {
      latest.current = undefined;
      setDraft(undefined);
      setHeld(undefined);
      setSave({ status: 'idle' });
      onUnsavedChange(false);
    }
  }

  /** Removes the kept record before completing an end, or offers a choice. */
  async function finish(action: EndAction): Promise<void> {
    if (clearingDraft.current) return;
    clearingDraft.current = true;
    setClearing(true);
    const failure = await keeping.keep(undefined);
    clearingDraft.current = false;
    setClearing(false);
    if (action.kind === 'saved' && !isCurrent(baseEpoch)) {
      completeEnd(action);
      return;
    }
    if (failure !== undefined) {
      if (action.kind === 'saved') markUncertain(true);
      setEndFailure({ action, message: failure });
      return;
    }
    completeEnd(action);
  }

  /** Reads a file of the draft's revision while its number names it. */
  async function readBase(
    path: string,
    signal: AbortSignal,
  ): Promise<Uint8Array> {
    const why =
      shown.current.base === made
        ? shown.current.unreadable
        : 'The draft now belongs to another revision.';
    if (why !== undefined) throw new Error(`Nothing was read. ${why}`);
    const bytes = await client.readFile(artifact.id, made, path, {
      signal,
    });
    // The number may name other content once the data was replaced.
    if (!isCurrent(baseEpoch)) throw new Error(`Nothing was read. ${REPLACED}`);
    return bytes;
  }

  async function commit(made: number): Promise<void> {
    const now = allowanceNow(made);
    const toSave = latest.current;
    if (now.level !== 'all' || toSave === undefined || toSave.base !== made) {
      setSave({
        status: 'failed',
        message: now.level === 'all' ? 'Nothing to save.' : now.why,
      });
      return;
    }
    const request = requestOf(toSave);
    const body = JSON.stringify(request);
    setSave({ status: 'saving' });
    sending.current = true;
    let digest: string;
    let key: string;
    let previousDigest: string | undefined;
    try {
      digest = await requestDigest(body);
      previousDigest = sent.current?.requestDigest;
      if (
        previousDigest === undefined &&
        sent.current !== undefined &&
        attempt?.key === sent.current.key
      )
        previousDigest = await requestDigest(attempt.body);
      key =
        attempt?.body === body
          ? attempt.key
          : sent.current?.requestDigest === digest
            ? sent.current.key
            : (attempts.current.find((item) => item.requestDigest === digest)
                ?.key ?? newIdempotencyKey());
    } catch (error: unknown) {
      sending.current = false;
      setSave({ status: 'failed', message: describeFailure(error) });
      return;
    }
    setAttempt({ body, key });
    // Kept as sent before it is sent: if the page closes before the answer,
    // the save may have committed and the same key replays it. The request
    // follows even if the browser refused to keep it (decision 0018).
    const previouslySent = sent.current;
    const previousAttempts = attempts.current;
    if (
      previouslySent !== undefined &&
      previousDigest !== undefined &&
      !attempts.current.some((item) => item.requestDigest === previousDigest)
    )
      attempts.current = [
        ...attempts.current,
        { key: previouslySent.key, requestDigest: previousDigest },
      ];
    if (!attempts.current.some((item) => item.requestDigest === digest))
      attempts.current = [...attempts.current, { key, requestDigest: digest }];
    sent.current = { key, version: version.current, requestDigest: digest };
    await keeping.keep(kept(toSave, true));
    // Keeping the draft can wait for an earlier IndexedDB transaction. A
    // reset or a move to the trash during that wait invalidates this base
    // before the HTTP request has been sent.
    if (
      !isCurrent(baseEpoch) ||
      shown.current.base !== made ||
      shown.current.unreadable !== undefined
    ) {
      sending.current = false;
      sent.current = previouslySent;
      attempts.current = previousAttempts;
      void keeping.keep(kept(toSave));
      setSave({ status: 'idle' });
      return;
    }
    let saved: Artifact | undefined;
    let failure: unknown;
    try {
      saved = (
        await client.saveRevision(artifact.id, request, { idempotencyKey: key })
      ).artifact;
    } catch (error: unknown) {
      failure = error;
    } finally {
      sending.current = false;
    }
    // An answer from before a possible replacement names revisions of data
    // the server may no longer hold: the draft stays (decision 0014).
    if (!isCurrent(baseEpoch)) {
      setSave({ status: 'idle' });
      return;
    }
    if (saved !== undefined) {
      setSave({ status: 'idle' });
      void finish({ kind: 'saved', artifact: saved, draft: toSave });
      return;
    }
    if (
      failure instanceof ArtifreeApiError &&
      failure.code === 'revision_conflict'
    ) {
      setSave({ status: 'conflict', current: failure.currentRevision });
      onRefused(made, failure.currentRevision);
    } else {
      setSave({ status: 'failed', message: describeFailure(failure) });
      if (mayHaveCommitted(failure)) markUncertain(true);
    }
    // A refusal committed nothing, though an earlier save still may have.
    void keeping.keep(kept(latest.current ?? toSave));
  }

  const free = allowance.level === 'all';
  return (
    <div className="flex flex-col gap-4">
      <div className="grid gap-4 md:grid-cols-[minmax(12rem,16rem)_minmax(0,1fr)]">
        <FileTree
          paths={paths}
          selected={selected?.path}
          problemPaths={new Set(found.flatMap((problem) => problem.path ?? []))}
          free={free}
          onSelect={setChosen}
          onAdd={() => setDialog({})}
        />
        <div className="flex min-w-0 flex-col gap-4">
          {selected === undefined ? (
            <p className="text-sm text-muted-foreground">
              This draft has no files.
            </p>
          ) : (
            <FileView
              key={`${made}:${baseEpoch}:${selected.path}`}
              entry={selected}
              mediaType={mediaTypeAt(shownDraft, selected.path)}
              allowance={allowance}
              readBase={readBase}
              onText={(text, crlf, source) =>
                write('text', made, (draft) =>
                  setText(draft, selected.path, text, crlf, source),
                )
              }
              onRename={() => setDialog({ rename: selected.path })}
              onDelete={() =>
                write('paths', made, (draft) =>
                  deleteFile(draft, selected.path),
                )
              }
              onRestore={() =>
                write('paths', made, (draft) =>
                  restoreFile(draft, selected.path),
                )
              }
            />
          )}
          {showPreview && !replaced && !inTrash ? (
            <PreviewFrame
              client={client}
              artifactId={artifact.id}
              revision={made}
              epoch={baseEpoch}
              stale={replaced}
              title={`Saved preview of ${artifact.title}, revision ${made}`}
              className="h-96 w-full rounded-lg border"
            />
          ) : null}
        </div>
      </div>
      {found.length > 0 ? (
        <Alert variant="destructive">
          <AlertTitle>The server would refuse this draft</AlertTitle>
          <AlertDescription>
            <ul className="list-disc pl-4">
              {found.map((problem) => (
                <li key={problem.message}>{problem.message}</li>
              ))}
            </ul>
          </AlertDescription>
        </Alert>
      ) : null}
      {unmatched ? (
        <Alert variant="destructive">
          <AlertTitle>The kept draft&apos;s revision has changed</AlertTitle>
          <AlertDescription>
            This draft was kept for revision {made}, which the server no longer
            lists with the same files. Its text can be edited and copied, but no
            file of that revision is read or moved and saving is disabled. Copy
            your changes if you need them, then load the latest revision.
          </AlertDescription>
        </Alert>
      ) : replaced ? (
        <Alert variant="destructive">
          <AlertTitle>The server&apos;s data may have been replaced</AlertTitle>
          <AlertDescription>
            This draft was written against revision {made} as it was before the
            server&apos;s data changed. A revision with that number may now hold
            other content, so no file of it is read or moved and saving is
            disabled. Copy your changes if you need them, then load the latest
            revision.
          </AlertDescription>
        </Alert>
      ) : null}
      {newer && save.status !== 'conflict' ? (
        <Alert>
          <AlertTitle>Revision {latestKnown} was saved elsewhere</AlertTitle>
          <AlertDescription>
            This draft was written against revision {made}, so saving it will be
            refused. Copy your changes if you need them, then load the latest
            revision.
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
      {keeping.failure !== undefined && changed && endFailure === undefined ? (
        <Alert variant="destructive">
          <AlertTitle>This draft is not kept in this browser</AlertTitle>
          <AlertDescription>
            {keeping.failure} Editing continues, but the draft is lost if this
            page closes; the next change is kept once the browser allows it.
          </AlertDescription>
        </Alert>
      ) : null}
      {endFailure === undefined ? null : (
        <Alert variant="destructive">
          <AlertTitle>Could not remove the kept draft</AlertTitle>
          <AlertDescription>
            <p>
              {endFailure.message}{' '}
              {endFailure.action.kind === 'saved'
                ? 'The revision was saved. '
                : null}
              The kept draft may be offered again after a reload.
            </p>
            <div className="flex flex-wrap gap-2">
              <Button
                type="button"
                size="sm"
                disabled={clearing}
                onClick={() => void finish(endFailure.action)}
              >
                Try again
              </Button>
              <Button
                type="button"
                variant="outline"
                size="sm"
                disabled={clearing}
                onClick={() => completeEnd(endFailure.action)}
              >
                Continue without removing it
              </Button>
            </div>
          </AlertDescription>
        </Alert>
      )}
      {uncertain && save.status === 'idle' ? (
        <Alert>
          <AlertTitle>This draft may already be saved</AlertTitle>
          <AlertDescription>
            An earlier save&apos;s answer was not received. Retrying the same
            changes reuses its key; different changes use a new key.
          </AlertDescription>
        </Alert>
      ) : null}
      {save.status === 'failed' ? (
        <Alert variant="destructive">
          <AlertTitle>Not saved</AlertTitle>
          <AlertDescription>
            {save.message}
            {uncertain
              ? ' The save may have been committed; saving the same draft again replays it.'
              : null}
          </AlertDescription>
        </Alert>
      ) : null}
      <FieldGroup>
        <FieldDescription>
          Saving commits every change as revision {made + 1} only if revision{' '}
          {made} is still current.
          {changed
            ? ' Save or discard your changes to switch artifacts.'
            : null}
        </FieldDescription>
        <Field orientation="horizontal" className="flex-wrap">
          <Button
            type="button"
            disabled={!changed || !free || found.length > 0}
            onClick={() => void commit(made)}
          >
            <SaveIcon data-icon="inline-start" aria-hidden="true" />
            {saving ? 'Saving…' : 'Save revision'}
          </Button>
          {/* In the trash, discarding is the way out: the latest revision
              cannot be read there. */}
          {changed && allowance.level !== 'none' && (!outdated || inTrash) ? (
            <Button
              type="button"
              variant="outline"
              onClick={() => void finish({ kind: 'discard' })}
            >
              <Undo2Icon data-icon="inline-start" aria-hidden="true" />
              Discard changes
            </Button>
          ) : null}
          {outdated && !inTrash ? (
            <Button
              type="button"
              variant="outline"
              disabled={
                reloading || saving || clearing || endFailure !== undefined
              }
              onClick={() => void finish({ kind: 'latest' })}
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
      <PathDialog
        // Each opening starts with its own path and error.
        key={dialog === undefined ? 'closed' : `open:${dialog.rename ?? ''}`}
        open={dialog !== undefined}
        from={dialog?.rename}
        draft={shownDraft}
        held={allowance.level === 'all' ? undefined : allowance.why}
        onClose={() => setDialog(undefined)}
        onConfirm={async (to, signal) => {
          const from = dialog?.rename;
          // The file opens at its new path.
          const opened = (refused: string | undefined) => {
            if (refused === undefined) setChosen(to);
            return refused;
          };
          if (from === undefined)
            return opened(write('paths', made, (draft) => addFile(draft, to)));
          let bytes: Uint8Array | undefined;
          if (
            needsBaseBytes(
              latest.current?.base === made ? latest.current : shownDraft,
              from,
            )
          ) {
            try {
              bytes = await readBase(from, signal);
            } catch (error: unknown) {
              return `${from} was not moved. ${describeFailure(error)}`;
            }
            if (signal.aborted) return `${from} was not moved.`;
          }
          const refused = opened(
            write('paths', made, (draft) => renameFile(draft, from, to, bytes)),
          );
          return refused === undefined
            ? undefined
            : `${from} was not moved. ${refused}`;
        }}
      />
    </div>
  );
}

/** The save request for `draft`. */
function requestOf(draft: SourceDraft): SaveRevisionRequest {
  return {
    base_revision: draft.base,
    changes: plan(draft).map((change) =>
      change.op === 'delete'
        ? change
        : { op: 'put', ...encodeFile(change.path, change.bytes) },
    ),
  };
}

/** Identifies the exact serialized request without storing a second copy. */
async function requestDigest(body: string): Promise<string> {
  const digest = await crypto.subtle.digest(
    'SHA-256',
    new TextEncoder().encode(body),
  );
  return Array.from(new Uint8Array(digest), (byte) =>
    byte.toString(16).padStart(2, '0'),
  ).join('');
}

/**
 * The revision a continued draft is written on. It counts as the listed
 * revision only if the server lists it with the same paths and digests;
 * otherwise it is kept as unmatched (decision 0018, item 5).
 */
function keptBase(start: KeptDraft, read: SourceRead): DraftBase {
  const listed = read.revisions.find(
    (item) => item.revision === start.draft.base,
  );
  if (listed !== undefined && sameFiles(listed.files, start.draft.files))
    return { revision: listed, epoch: read.epoch, resets: read.resets };
  return {
    revision: {
      revision: start.draft.base,
      files: [...start.draft.files],
      created_at_ms: 0,
      created_by: '',
    },
    epoch: read.epoch,
    resets: read.resets,
    unmatched: true,
  };
}

/** Whether two manifests list the same paths with the same digests. */
function sameFiles(
  left: readonly RevisionFile[],
  right: readonly RevisionFile[],
): boolean {
  return (
    left.length === right.length &&
    left.every(
      (file, index) =>
        file.path === right[index]?.path &&
        file.sha256 === right[index]?.sha256,
    )
  );
}

const STATE_LABELS = {
  unchanged: undefined,
  edited: 'edited',
  added: 'added',
  deleted: 'deleted',
} as const;

interface FileTreeProps {
  paths: DraftPath[];
  selected: string | undefined;
  problemPaths: ReadonlySet<string>;
  /** Whether any file can be opened, and files added: nothing holds the draft. */
  free: boolean;
  onSelect: (path: string) => void;
  onAdd: () => void;
}

/** The draft's paths, with what it does to each. */
function FileTree({
  paths,
  selected,
  problemPaths,
  free,
  onSelect,
  onAdd,
}: FileTreeProps) {
  return (
    <nav aria-label="Files" className="flex flex-col gap-2">
      <ul className="flex flex-col gap-1">
        {paths.map(({ path, state, change }) => {
          const label = STATE_LABELS[state];
          return (
            <li key={path}>
              <Button
                type="button"
                variant={path === selected ? 'secondary' : 'ghost'}
                size="sm"
                className="w-full justify-between"
                aria-current={path === selected ? 'true' : undefined}
                // What the draft holds opens without a read, so it stays
                // reachable, and copyable, when nothing may be read.
                disabled={!free && change === undefined}
                onClick={() => onSelect(path)}
              >
                <span
                  className={
                    state === 'deleted'
                      ? 'truncate font-mono line-through'
                      : 'truncate font-mono'
                  }
                >
                  {path}
                </span>
                {label === undefined ? null : (
                  <Badge
                    variant={problemPaths.has(path) ? 'destructive' : 'outline'}
                  >
                    {label}
                  </Badge>
                )}
              </Button>
            </li>
          );
        })}
      </ul>
      <Button
        type="button"
        variant="outline"
        size="sm"
        disabled={!free}
        onClick={onAdd}
      >
        <FilePlusIcon data-icon="inline-start" aria-hidden="true" />
        New file
      </Button>
    </nav>
  );
}

interface FileViewProps {
  entry: DraftPath;
  mediaType: string | undefined;
  allowance: Allowance;
  readBase: (path: string, signal: AbortSignal) => Promise<Uint8Array>;
  /**
   * Sets the file's text, read from `source` unless the draft holds text
   * there; returns why it was refused, if it was.
   */
  onText: (
    text: string,
    crlf: boolean,
    source: Source | undefined,
  ) => string | undefined;
  onRename: () => void;
  onDelete: () => string | undefined;
  onRestore: () => string | undefined;
}

/** One path of the draft: its text, or what the draft does to it. */
function FileView({
  entry,
  mediaType,
  allowance,
  readBase,
  onText,
  onRename,
  onDelete,
  onRestore,
}: FileViewProps) {
  const { path, change } = entry;
  const free = allowance.level === 'all';
  const [refusal, setRefusal] = useState<string>();
  const report = (why: string | undefined) => setRefusal(why);
  if (change?.kind === 'deleted')
    return (
      <Alert>
        <AlertTitle>{path} is deleted in this draft</AlertTitle>
        <AlertDescription>
          <p>
            Saving deletes it.{refusal === undefined ? null : ` ${refusal}`}
          </p>
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={!free}
            onClick={() => report(onRestore())}
          >
            <Undo2Icon data-icon="inline-start" aria-hidden="true" />
            Restore {path}
          </Button>
        </AlertDescription>
      </Alert>
    );
  const isText = mediaType !== undefined && TEXT_TYPES.test(mediaType);
  return (
    <div className="flex flex-col gap-3">
      <div className="flex flex-wrap gap-2">
        <Button
          type="button"
          variant="outline"
          size="sm"
          disabled={!free}
          onClick={onRename}
        >
          <PencilIcon data-icon="inline-start" aria-hidden="true" />
          Rename file
        </Button>
        <Button
          type="button"
          variant="outline"
          size="sm"
          disabled={!free}
          onClick={() => report(onDelete())}
        >
          <Trash2Icon data-icon="inline-start" aria-hidden="true" />
          Delete file
        </Button>
      </div>
      {refusal === undefined ? null : (
        <p className="text-sm text-destructive">{refusal}</p>
      )}
      {!isText ? (
        <NotShown
          path={path}
          why="is not a text file"
          note={
            change?.kind === 'bytes' ? `Moved from ${change.from}.` : undefined
          }
        />
      ) : change === undefined ||
        (change.kind === 'text' && change.source?.path === path) ? (
        <BaseText
          path={path}
          edited={change}
          allowance={allowance}
          readBase={readBase}
          onText={onText}
        />
      ) : (
        <HeldFile
          path={path}
          change={change}
          allowance={allowance}
          onText={onText}
        />
      )}
    </div>
  );
}

/**
 * A base file at its own path, unchanged or edited in place. Its base text
 * is read once, unless the draft already holds it, and stays while the
 * view is open: text read before the draft was held stays editable, and
 * nothing is read again when edits return to it.
 */
function BaseText({
  path,
  edited,
  allowance,
  readBase,
  onText,
}: {
  path: string;
  edited: Extract<Change, { kind: 'text' }> | undefined;
  allowance: Allowance;
  readBase: FileViewProps['readBase'];
  onText: FileViewProps['onText'];
}) {
  const [known, setKnown] = useState<EditableText | undefined>(() =>
    edited?.source === undefined
      ? undefined
      : { editable: true, text: edited.source.text, crlf: edited.crlf },
  );
  const [attempt, setAttempt] = useState(0);
  const file = useRequest(
    known === undefined ? `${path}:${attempt}` : undefined,
    async (signal) => toEditable(await readBase(path, signal)),
  );
  if (known === undefined && file.status === 'ready') setKnown(file.value);
  if (known === undefined && file.status === 'error')
    return (
      <Alert variant="destructive">
        <AlertTitle>Could not read {path}</AlertTitle>
        <AlertDescription>
          <p>{file.message}</p>
          {allowance.level === 'all' ? (
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
  const base = known ?? (file.status === 'ready' ? file.value : undefined);
  if (base === undefined)
    return (
      <output className="text-sm text-muted-foreground">Loading {path}…</output>
    );
  if (!base.editable) return <NotShown path={path} why={base.reason} />;
  return (
    <TextEditor
      path={path}
      text={edited?.text ?? base.text}
      allowance={allowance}
      onChange={(value) => onText(value, base.crlf, { path, text: base.text })}
    />
  );
}

/** Text the draft added or moved here, or bytes it moved here. */
function HeldFile({
  path,
  change,
  allowance,
  onText,
}: {
  path: string;
  change: Exclude<Change, { kind: 'deleted' }>;
  allowance: Allowance;
  onText: FileViewProps['onText'];
}) {
  // Moved bytes are shown as text when they round-trip; edited, they are
  // text read from the path they came from.
  const moved = useMemo<EditableText | undefined>(
    () => (change.kind === 'bytes' ? toEditable(change.bytes) : undefined),
    [change],
  );
  if (change.kind === 'text')
    return (
      <TextEditor
        path={path}
        text={change.text}
        allowance={allowance}
        onChange={(value) => onText(value, change.crlf, undefined)}
      />
    );
  const note = `Moved from ${change.from}.`;
  if (moved === undefined || !moved.editable)
    return (
      <NotShown
        path={path}
        why={moved?.editable === false ? moved.reason : 'is not a text file'}
        note={note}
      />
    );
  return (
    <TextEditor
      path={path}
      text={moved.text}
      note={note}
      allowance={allowance}
      onChange={(value) =>
        onText(value, moved.crlf, { path: change.from, text: moved.text })
      }
    />
  );
}

function TextEditor({
  path,
  text,
  note,
  allowance,
  onChange,
}: {
  path: string;
  text: string;
  note?: string;
  allowance: Allowance;
  /** Returns why the change was refused, if it was. */
  onChange: (text: string) => string | undefined;
}) {
  const [refusal, setRefusal] = useState<string>();
  const locked = allowance.level === 'none';
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
        onChange={(event) => setRefusal(onChange(event.target.value))}
      />
      {note === undefined ? null : <FieldDescription>{note}</FieldDescription>}
      {refusal === undefined ? null : <FieldError>{refusal}</FieldError>}
    </Field>
  );
}

function NotShown({
  path,
  why,
  note,
}: {
  path: string;
  why: string;
  note?: string | undefined;
}) {
  return (
    <Alert>
      <AlertTitle>{path} is not shown here</AlertTitle>
      <AlertDescription>
        The file {why}. It can be renamed or deleted; its bytes are saved as
        they are.{note === undefined ? null : ` ${note}`}
      </AlertDescription>
    </Alert>
  );
}

interface PathDialogProps {
  open: boolean;
  /** The path to rename, or none to add a file. */
  from: string | undefined;
  draft: SourceDraft;
  /** Why the draft's paths cannot change now, if they cannot. */
  held: string | undefined;
  onClose: () => void;
  /** Applies the path; resolves with why it was refused, if it was. */
  onConfirm: (path: string, signal: AbortSignal) => Promise<string | undefined>;
}

/** Asks for a new path, for a new file or a rename. */
function PathDialog({
  open,
  from,
  draft,
  held,
  onClose,
  onConfirm,
}: PathDialogProps) {
  const [path, setPath] = useState(from ?? '');
  const [failure, setFailure] = useState<string>();
  const [pending, setPending] = useState(false);
  const controller = useRef<AbortController | undefined>(undefined);
  // Closing the dialog stops a read it started. One still running when the
  // draft stops allowing path changes is refused where it lands.
  useEffect(() => () => controller.current?.abort(), []);
  const trimmed = path.trim();
  const invalid = trimmed === '' ? undefined : judge(draft, trimmed, from);

  async function submit(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    if (invalid !== undefined || trimmed === '') return;
    const own = new AbortController();
    controller.current = own;
    setPending(true);
    setFailure(undefined);
    const refused = await onConfirm(trimmed, own.signal);
    if (own.signal.aborted) return;
    setPending(false);
    if (refused === undefined) onClose();
    else setFailure(refused);
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) {
          controller.current?.abort();
          onClose();
        }
      }}
    >
      <DialogContent>
        <form
          className="flex flex-col gap-4"
          onSubmit={(event) => void submit(event)}
        >
          <DialogHeader>
            <DialogTitle>
              {from === undefined ? 'New file' : `Rename ${from}`}
            </DialogTitle>
            <DialogDescription>
              {from === undefined
                ? 'The file starts empty and is saved with the draft.'
                : 'The file keeps its content and moves when the draft is saved.'}
            </DialogDescription>
          </DialogHeader>
          <Field data-invalid={invalid !== undefined}>
            <FieldLabel htmlFor="draft-path">Path</FieldLabel>
            <Input
              id="draft-path"
              value={path}
              readOnly={pending}
              aria-invalid={invalid !== undefined}
              onChange={(event) => {
                setPath(event.target.value);
                setFailure(undefined);
              }}
            />
            {invalid === undefined ? null : <FieldError>{invalid}</FieldError>}
          </Field>
          {held === undefined ? null : (
            <output className="text-sm text-muted-foreground">
              Paths cannot change now. {held}
            </output>
          )}
          {failure === undefined ? null : (
            <Alert variant="destructive">
              <AlertDescription>{failure}</AlertDescription>
            </Alert>
          )}
          <DialogFooter>
            <DialogClose render={<Button variant="outline" />}>
              Cancel
            </DialogClose>
            <Button
              type="submit"
              disabled={
                pending ||
                held !== undefined ||
                trimmed === '' ||
                invalid !== undefined
              }
            >
              {pending
                ? 'Reading…'
                : from === undefined
                  ? 'Add file'
                  : 'Rename'}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/** Why `path` cannot be added, or `from` moved to it, or `undefined`. */
function judge(
  draft: SourceDraft,
  path: string,
  from: string | undefined,
): string | undefined {
  if (path === from) return 'The file is already at this path.';
  const error = pathError(path);
  if (error !== undefined) return explainPathError(error);
  if (exists(draft, path)) return `${path} already exists.`;
  const folded = path.toLowerCase();
  const twin = listPaths(draft).find(
    (entry) =>
      entry.state !== 'deleted' &&
      entry.path !== from &&
      entry.path.toLowerCase() === folded,
  );
  return twin === undefined
    ? undefined
    : `${path} differs from ${twin.path} only by case.`;
}
