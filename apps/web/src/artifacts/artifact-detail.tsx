import { useEffect, useEffectEvent, useState } from 'react';
import { RotateCwIcon, SaveIcon, Undo2Icon } from 'lucide-react';
import { ArtifreeApiError, newIdempotencyKey } from '@artifree/client';
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
  Card,
  CardAction,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card';
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
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs';
import { Textarea } from '@/components/ui/textarea';
import { refusedFloor, useWriteGate } from './conditional-write';
import { fromEditable, toEditable } from './editable-text';
import { HistoryPanel, useRestore } from './history-panel';
import { MetadataActions } from './metadata-actions';
import { useChoice } from './use-choice';
import { isNewer, versionOf } from './use-live-workspace';
import type { Version } from './use-live-workspace';
import { describeFailure, mayHaveCommitted, useRequest } from './use-request';

/**
 * Sandbox flags for previews, matching the content server's CSP: without
 * allow-same-origin the content is opaque, and without allow-popups it cannot
 * open windows that escape the content policy.
 */
const PREVIEW_SANDBOX =
  'allow-scripts allow-forms allow-modals allow-downloads';

const TEXT_TYPES = /^(text\/|application\/json|application\/xml|image\/svg)/;

/** An artifact's metadata and history, with when they were read. */
interface Loaded {
  readonly artifact: Artifact;
  readonly revisions: Revision[];
  /** The detail's reload count when the read was asked for. */
  readonly generation: number;
  /** The feed epoch the read belongs to; content read for it is keyed by it. */
  readonly epoch: number;
  /** The page's reset count then; a later reset may have replaced it. */
  readonly resets: number;
}

interface ArtifactDetailProps {
  client: ArtifreeClient;
  artifactId: string;
  /** The page's feed epoch; a new one reads everything again. */
  epoch: number;
  /** How often the server's data may have been replaced so far. */
  resets: number;
  /** The newest version the page knows this artifact has, if any. */
  known: Version | undefined;
  /** Changes of a kind the client does not know; each reads again. */
  unknownChanges: number;
  /** Called after a revision is committed so lists can refresh. */
  onSaved: (artifact: Artifact) => void;
  /** Called after this page renamed or pinned the artifact. */
  onMetadataChanged: (artifact: Artifact) => void;
  /** Whether the editor holds changes that are not saved. */
  unsaved: boolean;
  /** Reports when the editor gains or loses unsaved changes. */
  onUnsavedChange: (unsaved: boolean) => void;
}

/** Preview and source editing for one artifact. */
export function ArtifactDetail({
  client,
  artifactId,
  epoch,
  resets,
  known,
  unknownChanges,
  onSaved,
  onMetadataChanged,
  unsaved,
  onUnsavedChange,
}: ArtifactDetailProps) {
  const [generation, setGeneration] = useState(0);
  // The version this view reads for, in the epoch it was known in. It
  // follows a newer known version only after its own read settled, since a
  // pending read may bring it. A new epoch forgets it, like the versions
  // known before.
  const [target, setTarget] = useState({ epoch, version: known });
  const targetVersion = target.epoch === epoch ? target.version : undefined;
  const details = useRequest<Loaded>(
    `${artifactId}:${generation}:${epoch}:${targetVersion?.revision}:${targetVersion?.metadata}:${unknownChanges}`,
    async (signal) => {
      const asked = { generation, epoch, resets };
      // Revisions are listed after the artifact is read, so the list always
      // contains its current revision.
      const artifact = await client.getArtifact(artifactId, { signal });
      const revisions = await client.listRevisions(artifactId, { signal });
      return { artifact, revisions, ...asked };
    },
    // Keep showing the card and any draft while a newer state loads.
    { keepPrevious: true },
  );
  const shown =
    details.status === 'ready'
      ? details.value
      : details.status === 'error'
        ? details.previous
        : undefined;
  const settled =
    details.status === 'error' ||
    (details.status === 'ready' && !details.refreshing);
  // Only what was shown can be behind; a first read that failed is
  // retried by the user.
  if (
    settled &&
    shown !== undefined &&
    known !== undefined &&
    isNewer(known, targetVersion) &&
    // What was read in an earlier epoch is behind whatever this one knows.
    (shown.epoch !== epoch || isNewer(known, versionOf(shown.artifact)))
  )
    setTarget({ epoch, version: known });
  function reload(): void {
    setGeneration((value) => value + 1);
  }
  // Conditional writes share a gate per kind of version (decision 0011,
  // item 6): pin and rename the metadata revision; restore and save the
  // source revision. The gates live here, so what they learned and a
  // restore's key outlive the history tab.
  const metadataGate = useWriteGate(resets);
  const sourceGate = useWriteGate(resets);
  const restore = useRestore(
    client,
    artifactId,
    sourceGate,
    (restored) => {
      reload();
      onSaved(restored);
    },
    reload,
  );
  // A revision chosen for the preview, kept across tabs; none follows the
  // latest. It belongs to the read it was chosen from.
  const [chosen, setChosen] = useChoice<number>(shown?.resets ?? resets);
  if (details.status === 'loading')
    return (
      <output className="text-sm text-muted-foreground">
        Loading artifact…
      </output>
    );
  if (shown === undefined)
    return (
      <Alert variant="destructive">
        <AlertTitle>Could not load the artifact</AlertTitle>
        <AlertDescription>
          <p>{details.status === 'error' ? details.message : null}</p>
          <Button type="button" variant="outline" size="sm" onClick={reload}>
            <RotateCwIcon data-icon="inline-start" aria-hidden="true" />
            Try again
          </Button>
        </AlertDescription>
      </Alert>
    );
  const { artifact, revisions } = shown;
  // A save or "Load latest" asked for a newer state that has not arrived.
  const reloading = shown.generation !== generation;
  // What is shown was read before the server's data may have been
  // replaced. It is read again; meanwhile nothing is read or written for
  // it (decision 0011, items 6 and 9).
  const stale = shown.resets !== resets;
  const metadataWait = (version: number) =>
    metadataGate.wait(version, known?.metadata, shown.resets);
  const sourceWait = (version: number) =>
    sourceGate.wait(version, known?.revision, shown.resets);
  return (
    <Card>
      <CardHeader>
        <CardTitle>
          <h3>{artifact.title}</h3>
        </CardTitle>
        <CardDescription>
          <code>{artifact.id}</code>
        </CardDescription>
        <CardAction>
          <div className="flex flex-col items-end gap-2">
            <Badge variant="secondary">
              Revision {artifact.current_revision}
            </Badge>
            <MetadataActions
              client={client}
              artifact={artifact}
              gate={metadataGate}
              wait={metadataWait}
              resets={resets}
              onUpdated={onMetadataChanged}
              onConflict={reload}
            />
          </div>
        </CardAction>
      </CardHeader>
      <CardContent>
        <div className="flex flex-col gap-4">
          {stale ? (
            <output className="text-sm text-muted-foreground">
              Reading the artifact again: the server&apos;s data may have
              changed wholesale. Pin, rename, restore, previews, and comparisons
              wait until it is read.
            </output>
          ) : null}
          {details.status === 'error' ? (
            <Alert variant="destructive">
              <AlertTitle>Could not refresh the artifact</AlertTitle>
              <AlertDescription>
                <p>
                  {details.message} What is shown is from the last successful
                  read.
                </p>
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={reload}
                >
                  <RotateCwIcon data-icon="inline-start" aria-hidden="true" />
                  Try again
                </Button>
              </AlertDescription>
            </Alert>
          ) : null}
          <Tabs defaultValue="preview">
            <TabsList aria-label="Artifact views">
              <TabsTrigger value="preview">Preview</TabsTrigger>
              <TabsTrigger value="source">Source</TabsTrigger>
              <TabsTrigger value="history">History</TabsTrigger>
            </TabsList>
            <TabsContent value="preview" className="mt-4">
              <PreviewPanel
                client={client}
                artifact={artifact}
                revisions={revisions}
                epoch={shown.epoch}
                stale={stale}
                chosen={chosen}
                onChoose={setChosen}
              />
            </TabsContent>
            <TabsContent value="history" className="mt-4">
              <HistoryPanel
                client={client}
                artifact={artifact}
                revisions={revisions}
                epoch={shown.epoch}
                shownResets={shown.resets}
                resets={resets}
                unsaved={unsaved}
                restore={restore}
                wait={sourceWait}
              />
            </TabsContent>
            <TabsContent value="source" keepMounted className="mt-4">
              <SourcePanel
                client={client}
                loaded={shown}
                resets={resets}
                known={known?.revision}
                onRefused={(base, current) => {
                  // A restore names the same source revision as a save.
                  const floor = refusedFloor(base, current);
                  if (floor !== undefined) sourceGate.refuse(floor);
                }}
                onSaved={(saved) => {
                  onUnsavedChange(false);
                  reload();
                  onSaved(saved);
                }}
                onLoadLatest={() => {
                  // Loading the latest revision replaces the draft.
                  onUnsavedChange(false);
                  reload();
                }}
                unsaved={unsaved}
                onUnsavedChange={onUnsavedChange}
                reloading={reloading}
              />
            </TabsContent>
          </Tabs>
        </div>
      </CardContent>
    </Card>
  );
}

interface PreviewPanelProps {
  client: ArtifreeClient;
  artifact: Artifact;
  revisions: Revision[];
  epoch: number;
  /** Whether what is shown predates a possible data replacement. */
  stale: boolean;
  /** A revision chosen for the preview; none follows the latest. */
  chosen: number | undefined;
  onChoose: (revision: number | undefined) => void;
}

function PreviewPanel({
  client,
  artifact,
  revisions,
  epoch,
  stale,
  chosen,
  onChoose,
}: PreviewPanelProps) {
  const latest = artifact.current_revision;
  // A chosen revision that is not listed falls back to the latest.
  const kept =
    chosen !== undefined && revisions.some((item) => item.revision === chosen)
      ? chosen
      : undefined;
  const revision = kept ?? latest;
  // No preview is prepared for metadata that predates a possible data
  // replacement, and none prepared meanwhile is shown.
  const preview = useRequest(
    stale ? undefined : `${artifact.id}:${revision}:${epoch}`,
    (signal) => client.createPreview(artifact.id, revision, { signal }),
  );
  return (
    <div className="flex flex-col gap-4">
      <FieldGroup>
        <Field orientation="horizontal">
          <FieldLabel htmlFor="preview-revision">Revision</FieldLabel>
          <NativeSelect
            id="preview-revision"
            value={kept === undefined ? 'latest' : String(kept)}
            onChange={(event) =>
              onChoose(
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
      {preview.status === 'loading' ? (
        <output className="text-sm text-muted-foreground">
          {stale
            ? 'The preview is prepared once the artifact is read again.'
            : 'Preparing preview…'}
        </output>
      ) : null}
      {preview.status === 'error' ? (
        <Alert variant="destructive">
          <AlertTitle>Preview unavailable</AlertTitle>
          <AlertDescription>{preview.message}</AlertDescription>
        </Alert>
      ) : null}
      {preview.status === 'ready' ? (
        <iframe
          title={`Preview of ${artifact.title}, revision ${revision}`}
          src={preview.value.url}
          sandbox={PREVIEW_SANDBOX}
          referrerPolicy="no-referrer"
          className="h-128 w-full rounded-lg border"
        />
      ) : null}
      <p className="text-xs text-muted-foreground">
        Content runs in a sandboxed frame on a separate origin. It cannot read
        this page, the API, or other artifacts, and loads and connects only to
        its own origin. Scripts can still navigate the frame to another site, so
        treat what you type into an untrusted artifact as visible to its author.
        Data, model, and file capabilities are not available yet.
      </p>
    </div>
  );
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
}

function SourcePanel({
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
}: SourcePanelProps) {
  const { artifact, revisions } = loaded;
  // While a draft exists the editor keeps the revision it was written
  // against, even when newer ones arrive or it is no longer listed.
  const [held, setHeld] = useState<DraftBase>();
  if (!unsaved && held !== undefined) setHeld(undefined);
  const current = revisions.find(
    (item) => item.revision === artifact.current_revision,
  );
  const base: DraftBase | undefined =
    held ??
    (current === undefined
      ? undefined
      : { revision: current, epoch: loaded.epoch, resets: loaded.resets });
  const textFiles = (base?.revision.files ?? []).filter((file) =>
    TEXT_TYPES.test(file.media_type),
  );
  // Without a draft, nothing is read for metadata that predates a possible
  // data replacement, and nothing read meanwhile is shown. A draft stays
  // (0007 decision 8).
  const waiting = held === undefined && loaded.resets !== resets;
  const [chosen, setChosen] = useState('index.html');
  // The chosen file may be gone after loading a revision saved elsewhere.
  const path = textFiles.some((file) => file.path === chosen)
    ? chosen
    : 'index.html';
  if (waiting)
    return (
      <output className="text-sm text-muted-foreground">
        The source is read once the artifact is read again.
      </output>
    );
  if (base === undefined || textFiles.length === 0)
    return (
      <p className="text-sm text-muted-foreground">
        This revision has no text files to edit here.
      </p>
    );
  return (
    <div className="flex flex-col gap-4">
      <FieldGroup>
        <Field orientation="horizontal">
          <FieldLabel htmlFor="source-file">File</FieldLabel>
          <NativeSelect
            id="source-file"
            value={path}
            disabled={unsaved || reloading}
            aria-describedby={unsaved ? 'source-file-locked' : undefined}
            onChange={(event) => setChosen(event.target.value)}
          >
            {textFiles.map((file) => (
              <NativeSelectOption key={file.path} value={file.path}>
                {file.path}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        </Field>
        {unsaved ? (
          <FieldDescription id="source-file-locked">
            Save or discard your changes to switch files or artifacts.
          </FieldDescription>
        ) : null}
      </FieldGroup>
      <FileEditor
        key={`${base.revision.revision}:${path}:${base.epoch}`}
        client={client}
        artifactId={artifact.id}
        base={base.revision.revision}
        epoch={base.epoch}
        // A newer revision a notice reported counts as soon as it is
        // known, also if reading it failed.
        latest={Math.max(artifact.current_revision, known ?? 0)}
        replaced={held !== undefined && held.resets !== resets}
        path={path}
        onSaved={onSaved}
        onRefused={onRefused}
        onLoadLatest={onLoadLatest}
        onDraftChange={(draft) => {
          setHeld(draft ? base : undefined);
          onUnsavedChange(draft);
        }}
        reloading={reloading}
      />
    </div>
  );
}

interface FileEditorProps {
  client: ArtifreeClient;
  artifactId: string;
  /** The revision the editor reads and saves against. */
  base: number;
  /** The feed epoch `base` was read in. */
  epoch: number;
  /** The artifact's newest revision as the page knows it. */
  latest: number;
  /** The server's data may have been replaced since `base` was read. */
  replaced: boolean;
  path: string;
  onSaved: (artifact: Artifact) => void;
  onRefused: (base: number, current: number | undefined) => void;
  onLoadLatest: () => void;
  onDraftChange: (draft: boolean) => void;
  reloading: boolean;
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

function FileEditor({
  client,
  artifactId,
  base,
  epoch,
  latest,
  replaced,
  path,
  onSaved,
  onRefused,
  onLoadLatest,
  onDraftChange,
  reloading,
}: FileEditorProps) {
  const [readAttempt, setReadAttempt] = useState(0);
  const loaded = useRequest(
    `${artifactId}:${base}:${path}:${epoch}:${readAttempt}`,
    async (signal) =>
      toEditable(await client.readFile(artifactId, base, path, { signal })),
  );
  const [draft, setDraft] = useState<string>();
  const [save, setSave] = useState<SaveState>({ status: 'idle' });
  // The last request sent and its key. Sending the same content again reuses
  // the key, so a retry after a lost response cannot commit twice.
  const [attempt, setAttempt] = useState<{ body: string; key: string }>();
  // Whatever replaces this editor discards its draft, so it must not leave
  // the workspace locked.
  const release = useEffectEvent(() => onDraftChange(false));
  useEffect(() => () => release(), []);

  if (loaded.status === 'loading')
    return (
      <output className="text-sm text-muted-foreground">Loading {path}…</output>
    );
  if (loaded.status === 'error')
    return (
      <Alert variant="destructive">
        <AlertTitle>Could not read {path}</AlertTitle>
        <AlertDescription>
          <p>{loaded.message}</p>
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
  const file = loaded.value;
  if (!file.editable)
    return (
      <Alert>
        <AlertTitle>{path} cannot be edited here</AlertTitle>
        <AlertDescription>
          The file {file.reason}. Editing it as text would change bytes you did
          not touch; replace it with <code>artifree artifact save</code>{' '}
          instead.
        </AlertDescription>
      </Alert>
    );
  const { text, crlf } = file;
  const value = draft ?? text;
  const changed = value !== text;
  const saving = save.status === 'saving';
  // Read-only while a request will replace this editor: a save in flight
  // carries the text as sent, and a reload remounts it with a new revision.
  const locked = saving || reloading;
  // While this page's own save is in flight, or after its answer was lost,
  // a newer revision may be that save: no notice then.
  const newer =
    !replaced &&
    latest > base &&
    save.status !== 'saving' &&
    !(save.status === 'failed' && save.unknown);
  const outdated = replaced || newer || save.status === 'conflict';

  function edit(next: string | undefined): void {
    setDraft(next);
    setSave({ status: 'idle' });
    onDraftChange(next !== undefined && next !== text);
  }

  async function commit(): Promise<void> {
    const request: SaveRevisionRequest = {
      base_revision: base,
      changes: [
        {
          op: 'put',
          path,
          encoding: 'utf8',
          content: fromEditable(value, crlf),
        },
      ],
    };
    const body = JSON.stringify(request);
    const key = attempt?.body === body ? attempt.key : newIdempotencyKey();
    setAttempt({ body, key });
    setSave({ status: 'saving' });
    try {
      const result = await client.saveRevision(artifactId, request, {
        idempotencyKey: key,
      });
      onSaved(result.artifact);
    } catch (error: unknown) {
      if (
        error instanceof ArtifreeApiError &&
        error.code === 'revision_conflict'
      ) {
        setSave({ status: 'conflict', current: error.currentRevision });
        onRefused(base, error.currentRevision);
      } else
        setSave({
          status: 'failed',
          message: describeFailure(error),
          unknown: mayHaveCommitted(error),
        });
    }
  }

  return (
    <FieldGroup>
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
          onChange={(event) => edit(event.target.value)}
        />
        <FieldDescription>
          Saving commits revision {base + 1} only if revision {base} is still
          current.
        </FieldDescription>
      </Field>
      {replaced ? (
        <Alert variant="destructive">
          <AlertTitle>The server&apos;s data may have been replaced</AlertTitle>
          <AlertDescription>
            This draft was written against revision {base} as it was before the
            server&apos;s data changed. A revision with that number may now hold
            other content, so saving is disabled. Copy your changes if you need
            them, then load the latest revision.
          </AlertDescription>
        </Alert>
      ) : null}
      {newer && save.status !== 'conflict' ? (
        <Alert>
          <AlertTitle>Revision {latest} was saved elsewhere</AlertTitle>
          <AlertDescription>
            This draft was written against revision {base}, so saving it will be
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
      {save.status === 'failed' ? (
        <Alert variant="destructive">
          <AlertTitle>Not saved</AlertTitle>
          <AlertDescription>{save.message}</AlertDescription>
        </Alert>
      ) : null}
      <Field orientation="horizontal">
        <Button
          type="button"
          disabled={!changed || locked || replaced}
          onClick={() => void commit()}
        >
          <SaveIcon data-icon="inline-start" aria-hidden="true" />
          {saving ? 'Saving…' : 'Save revision'}
        </Button>
        {changed && !locked && !outdated ? (
          <Button
            type="button"
            variant="outline"
            onClick={() => edit(undefined)}
          >
            <Undo2Icon data-icon="inline-start" aria-hidden="true" />
            Discard changes
          </Button>
        ) : null}
        {outdated ? (
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
      </Field>
    </FieldGroup>
  );
}
