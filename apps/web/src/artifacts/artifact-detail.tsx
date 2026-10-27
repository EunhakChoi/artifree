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
import { fromEditable, toEditable } from './editable-text';
import { describeFailure, useRequest } from './use-request';

/**
 * Sandbox flags for previews, matching the content server's CSP: without
 * allow-same-origin the content is opaque, and without allow-popups it cannot
 * open windows that escape the content policy.
 */
const PREVIEW_SANDBOX =
  'allow-scripts allow-forms allow-modals allow-downloads';

const TEXT_TYPES = /^(text\/|application\/json|application\/xml|image\/svg)/;

interface ArtifactDetailProps {
  client: ArtifreeClient;
  artifactId: string;
  /** Advances when everything must be read again. */
  epoch: number;
  /** Advances when data read before may have been replaced. */
  resets: number;
  /** The newest revision announced for this artifact in this epoch. */
  knownRevision: number;
  /** Called after a revision is committed so lists can refresh. */
  onSaved: (artifact: Artifact) => void;
  /** Whether the editor holds changes that are not saved. */
  unsaved: boolean;
  /** Reports when the editor gains or loses unsaved changes. */
  onUnsavedChange: (unsaved: boolean) => void;
}

/** What one read of the artifact returned, and in which epoch. */
interface Loaded {
  readonly artifact: Artifact;
  readonly revisions: Revision[];
  readonly generation: number;
  readonly epoch: number;
  readonly resets: number;
}

/** Preview and source editing for one artifact. */
export function ArtifactDetail({
  client,
  artifactId,
  epoch,
  resets,
  knownRevision,
  onSaved,
  unsaved,
  onUnsavedChange,
}: ArtifactDetailProps) {
  const [generation, setGeneration] = useState(0);
  // The newest announced revision this view has asked for, per epoch.
  const [caughtUp, setCaughtUp] = useState({ epoch, revision: 0 });
  const target = caughtUp.epoch === epoch ? caughtUp.revision : 0;
  const details = useRequest(
    `${artifactId}:${generation}:${epoch}:${target}`,
    async (): Promise<Loaded> => {
      // Revisions are listed after the artifact is read, so the list always
      // contains its current revision.
      const artifact = await client.getArtifact(artifactId);
      const revisions = await client.listRevisions(artifactId);
      return { artifact, revisions, generation, epoch, resets };
    },
    // Keep the card, and any draft in it, while a newer read is pending.
    { keepPrevious: true },
  );
  const loaded =
    details.status === 'ready'
      ? details.value
      : details.status === 'error'
        ? details.previous
        : undefined;
  // Read again for an announced revision only once the current read has
  // settled: a read still pending may already bring it.
  const settled =
    details.status === 'error' ||
    (details.status === 'ready' && !details.refreshing);
  if (
    settled &&
    loaded !== undefined &&
    knownRevision > loaded.artifact.current_revision &&
    knownRevision > target
  )
    setCaughtUp({ epoch, revision: knownRevision });
  // A numbered revision chosen in the preview; it stays across tabs.
  const [pinned, setPinned] = useState<number>();
  function reload(): void {
    setGeneration((value) => value + 1);
  }
  if (loaded === undefined) {
    if (details.status === 'error')
      return (
        <Alert variant="destructive">
          <AlertTitle>Could not load the artifact</AlertTitle>
          <AlertDescription>
            <p>{details.message}</p>
            <Button type="button" variant="outline" size="sm" onClick={reload}>
              <RotateCwIcon data-icon="inline-start" aria-hidden="true" />
              Try again
            </Button>
          </AlertDescription>
        </Alert>
      );
    return (
      <output className="text-sm text-muted-foreground">
        Loading artifact…
      </output>
    );
  }
  const { artifact, revisions } = loaded;
  // This page asked for the shown revision to be replaced, after its own
  // save or "Load latest"; editing it now would be discarded.
  const reloading = loaded.generation !== generation;
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
          <Badge variant="secondary">
            Revision {artifact.current_revision}
          </Badge>
        </CardAction>
      </CardHeader>
      <CardContent>
        {details.status === 'error' ? (
          <Alert variant="destructive" className="mb-4">
            <AlertTitle>Could not refresh the artifact</AlertTitle>
            <AlertDescription>
              <p>{details.message}</p>
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
          </TabsList>
          <TabsContent value="preview" className="mt-4">
            <PreviewPanel
              client={client}
              artifact={artifact}
              revisions={revisions}
              epoch={loaded.epoch}
              pinned={pinned}
              onPin={setPinned}
            />
          </TabsContent>
          <TabsContent value="source" keepMounted className="mt-4">
            <SourcePanel
              client={client}
              loaded={loaded}
              resets={resets}
              onSaved={(saved) => {
                onUnsavedChange(false);
                reload();
                onSaved(saved);
              }}
              onReload={() => {
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
      </CardContent>
    </Card>
  );
}

/** The preview menu's entry that follows each new revision. */
const LATEST = 'latest';

interface PreviewPanelProps {
  client: ArtifreeClient;
  artifact: Artifact;
  revisions: Revision[];
  /** The epoch of the metadata that names these revisions. */
  epoch: number;
  /** The chosen revision, or undefined to follow the latest. */
  pinned: number | undefined;
  onPin: (revision: number | undefined) => void;
}

function PreviewPanel({
  client,
  artifact,
  revisions,
  epoch,
  pinned,
  onPin,
}: PreviewPanelProps) {
  // A pinned revision that this history no longer has shows the latest.
  const shown = revisions.some((item) => item.revision === pinned)
    ? pinned
    : undefined;
  const revision = shown ?? artifact.current_revision;
  // After a reset the same number may name other content: read it again.
  const preview = useRequest(`${artifact.id}:${revision}:${epoch}`, () =>
    client.createPreview(artifact.id, revision),
  );
  return (
    <div className="flex flex-col gap-4">
      <FieldGroup>
        <Field orientation="horizontal">
          <FieldLabel htmlFor="preview-revision">Revision</FieldLabel>
          <NativeSelect
            id="preview-revision"
            value={shown === undefined ? LATEST : String(shown)}
            onChange={(event) =>
              onPin(
                event.target.value === LATEST
                  ? undefined
                  : Number(event.target.value),
              )
            }
          >
            <NativeSelectOption value={LATEST}>
              Latest ({artifact.current_revision})
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
          Preparing preview…
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

interface SourcePanelProps {
  client: ArtifreeClient;
  loaded: Loaded;
  /** The page's current reset count. */
  resets: number;
  onSaved: (artifact: Artifact) => void;
  onReload: () => void;
  unsaved: boolean;
  onUnsavedChange: (unsaved: boolean) => void;
  /** A newer revision is loading and will replace the editor. */
  reloading: boolean;
}

/** The revision a draft was written against, and what it was read in. */
interface DraftBase {
  readonly revision: Revision;
  readonly epoch: number;
  readonly resets: number;
}

function SourcePanel({
  client,
  loaded,
  resets,
  onSaved,
  onReload,
  unsaved,
  onUnsavedChange,
  reloading,
}: SourcePanelProps) {
  const { artifact, revisions } = loaded;
  // While a draft exists the editor stays on the revision it was written
  // against, with that revision's files, even when newer metadata arrives
  // or the revision is no longer listed.
  const [held, setHeld] = useState<DraftBase>();
  // After a save or "Load latest" the draft is done, but the editor keeps
  // its revision, read-only, until the reload that replaces it arrives.
  const [releasing, setReleasing] = useState(false);
  if (releasing && !reloading) {
    setReleasing(false);
    setHeld(undefined);
  }
  function release(): void {
    if (held !== undefined) setReleasing(true);
  }
  const current =
    held?.revision ??
    revisions.find((item) => item.revision === artifact.current_revision);
  const contentEpoch = held?.epoch ?? loaded.epoch;
  const textFiles = (current?.files ?? []).filter((file) =>
    TEXT_TYPES.test(file.media_type),
  );
  const [chosen, setChosen] = useState('index.html');
  // The chosen file may be gone after loading a revision saved elsewhere.
  const path = textFiles.some((file) => file.path === chosen)
    ? chosen
    : 'index.html';
  function reportUnsaved(next: boolean): void {
    if (!next) setHeld(undefined);
    else if (held === undefined && current !== undefined)
      setHeld({
        revision: current,
        epoch: loaded.epoch,
        resets: loaded.resets,
      });
    onUnsavedChange(next);
  }
  if (current === undefined || textFiles.length === 0)
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
        key={`${current.revision}:${path}:${contentEpoch}`}
        client={client}
        artifact={artifact}
        base={current.revision}
        path={path}
        replaced={held !== undefined && held.resets !== resets}
        onSaved={(saved) => {
          release();
          onSaved(saved);
        }}
        onReload={() => {
          release();
          onReload();
        }}
        onUnsavedChange={reportUnsaved}
        reloading={reloading}
      />
    </div>
  );
}

interface FileEditorProps {
  client: ArtifreeClient;
  /** The artifact as last read; it may be newer than `base`. */
  artifact: Artifact;
  /** The revision this editor reads and saves against. */
  base: number;
  path: string;
  /**
   * The server's data may have been replaced since `base` was read, so a
   * save could overwrite content the user never saw.
   */
  replaced: boolean;
  onSaved: (artifact: Artifact) => void;
  onReload: () => void;
  onUnsavedChange: (unsaved: boolean) => void;
  reloading: boolean;
}

type SaveState =
  | { readonly status: 'idle' }
  | { readonly status: 'saving' }
  | { readonly status: 'conflict'; readonly current: number | undefined }
  | { readonly status: 'failed'; readonly message: string };

function FileEditor({
  client,
  artifact,
  base,
  path,
  replaced,
  onSaved,
  onReload,
  onUnsavedChange,
  reloading,
}: FileEditorProps) {
  const [readAttempt, setReadAttempt] = useState(0);
  const loaded = useRequest(
    `${artifact.id}:${base}:${path}:${readAttempt}`,
    async () => toEditable(await client.readFile(artifact.id, base, path)),
  );
  const [draft, setDraft] = useState<string>();
  const [save, setSave] = useState<SaveState>({ status: 'idle' });
  // The last request sent and its key. Sending the same content again reuses
  // the key, so a retry after a lost response cannot commit twice.
  const [attempt, setAttempt] = useState<{ body: string; key: string }>();
  // Whatever replaces this editor discards its draft, so it must not leave
  // the workspace locked.
  const releaseLock = useEffectEvent(() => onUnsavedChange(false));
  useEffect(() => () => releaseLock(), []);

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
  // A newer revision while this page's own save is in flight, or after its
  // response was lost, may be that save, so it is not reported then.
  const newer =
    !replaced &&
    artifact.current_revision > base &&
    (save.status === 'idle' || save.status === 'conflict');

  // The draft cannot be saved as it is; loading the latest replaces it.
  const outdated = save.status === 'conflict' || newer || replaced;

  function edit(next: string | undefined): void {
    setDraft(next);
    setSave({ status: 'idle' });
    onUnsavedChange(next !== undefined && next !== text);
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
      const result = await client.saveRevision(artifact.id, request, {
        idempotencyKey: key,
      });
      onSaved(result.artifact);
    } catch (error: unknown) {
      if (
        error instanceof ArtifreeApiError &&
        error.code === 'revision_conflict'
      )
        setSave({ status: 'conflict', current: error.currentRevision });
      else setSave({ status: 'failed', message: describeFailure(error) });
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
          <AlertTitle>The server's data may have been replaced</AlertTitle>
          <AlertDescription>
            Revision {base} was read before the server's data changed, so it may
            no longer be what you edited, and saving could overwrite content you
            have not seen. Saving is disabled: copy your changes if you need
            them, then load the latest revision.
          </AlertDescription>
        </Alert>
      ) : null}
      {newer && save.status === 'idle' ? (
        <Alert>
          <AlertTitle>
            Revision {artifact.current_revision} was saved elsewhere
          </AlertTitle>
          <AlertDescription>
            Your changes are based on revision {base}, so saving them will be
            refused. Copy them if you need them, then load the latest revision.
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
            onClick={onReload}
          >
            <RotateCwIcon data-icon="inline-start" aria-hidden="true" />
            {reloading ? 'Loading latest…' : 'Load latest'}
          </Button>
        ) : null}
      </Field>
    </FieldGroup>
  );
}
