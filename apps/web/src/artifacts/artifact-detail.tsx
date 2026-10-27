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
  /** The newest revision announced for this artifact, or 0. */
  announcedRevision: number;
  /** Called after a revision is committed so lists can refresh. */
  onSaved: (artifact: Artifact) => void;
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
  announcedRevision,
  onSaved,
  unsaved,
  onUnsavedChange,
}: ArtifactDetailProps) {
  const [generation, setGeneration] = useState(0);
  // The announced revision last read for in this epoch; `generation` counts
  // only reloads this page asked for. A new epoch may restart revisions
  // lower, as after a reset to another database.
  const [caughtUp, setCaughtUp] = useState({ epoch, revision: 0 });
  const caught = caughtUp.epoch === epoch ? caughtUp.revision : 0;
  // Here rather than in the preview, which unmounts on another tab.
  const [pinned, setPinned] = useState<number>();
  const details = useRequest(
    `${artifactId}:${generation}:${epoch}:${caught}`,
    async () => {
      // Revisions are listed after the artifact is read, so the list always
      // contains its current revision.
      const artifact = await client.getArtifact(artifactId);
      const revisions = await client.listRevisions(artifactId);
      return { artifact, revisions, generation };
    },
    // Keep showing the card, and any draft in it, while a newer revision
    // loads, and when loading it fails.
    { keepPrevious: true },
  );
  function reload(): void {
    setGeneration((value) => value + 1);
  }
  // An announced revision newer than the one shown reloads in the
  // background; one already shown, or older, reads nothing.
  const loaded =
    details.status === 'ready'
      ? details.value
      : details.status === 'error'
        ? details.previous
        : undefined;
  const shown = loaded?.artifact.current_revision;
  if (
    shown !== undefined &&
    announcedRevision > shown &&
    announcedRevision > caught
  )
    setCaughtUp({ epoch, revision: announcedRevision });
  const retry = (
    <Button type="button" variant="outline" size="sm" onClick={reload}>
      <RotateCwIcon data-icon="inline-start" aria-hidden="true" />
      Try again
    </Button>
  );
  if (loaded === undefined)
    return details.status === 'error' ? (
      <Alert variant="destructive">
        <AlertTitle>Could not load the artifact</AlertTitle>
        <AlertDescription>
          <p>{details.message}</p>
          {retry}
        </AlertDescription>
      </Alert>
    ) : (
      <output className="text-sm text-muted-foreground">
        Loading artifact…
      </output>
    );
  const { artifact, revisions } = loaded;
  // The shown revision is being replaced; editing it now would be discarded.
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
        <div className="flex flex-col gap-4">
          {details.status === 'error' ? (
            <Alert variant="destructive">
              <AlertTitle>Could not refresh the artifact</AlertTitle>
              <AlertDescription>
                <p>
                  {details.message} Revision {artifact.current_revision} is
                  still shown, and any unsaved changes are kept.
                </p>
                {retry}
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
                pinned={pinned}
                onPin={setPinned}
              />
            </TabsContent>
            <TabsContent value="source" keepMounted className="mt-4">
              <SourcePanel
                client={client}
                artifact={artifact}
                revisions={revisions}
                onSaved={(saved) => {
                  onUnsavedChange(false);
                  reload();
                  onSaved(saved);
                }}
                onReload={() => {
                  // Loading the latest revision replaces the rejected draft.
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
  /** The revision the user chose, or undefined to follow the latest. */
  pinned: number | undefined;
  onPin: (revision: number | undefined) => void;
}

const LATEST = 'latest';

function PreviewPanel({
  client,
  artifact,
  revisions,
  pinned,
  onPin,
}: PreviewPanelProps) {
  const revision = pinned ?? artifact.current_revision;
  const preview = useRequest(`${artifact.id}:${revision}`, () =>
    client.createPreview(artifact.id, revision),
  );
  return (
    <div className="flex flex-col gap-4">
      <FieldGroup>
        <Field orientation="horizontal">
          <FieldLabel htmlFor="preview-revision">Revision</FieldLabel>
          <NativeSelect
            id="preview-revision"
            value={pinned === undefined ? LATEST : String(pinned)}
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
  artifact: Artifact;
  revisions: Revision[];
  onSaved: (artifact: Artifact) => void;
  onReload: () => void;
  unsaved: boolean;
  onUnsavedChange: (unsaved: boolean) => void;
  /** A newer revision is loading and will replace the editor. */
  reloading: boolean;
}

function SourcePanel({
  client,
  artifact,
  revisions,
  onSaved,
  onReload,
  unsaved,
  onUnsavedChange,
  reloading,
}: SourcePanelProps) {
  // A draft keeps the revision it was written against while newer ones
  // arrive; without a draft the editor follows the latest revision.
  const [heldBase, setHeldBase] = useState<number>();
  const base = heldBase ?? artifact.current_revision;
  function reportUnsaved(next: boolean): void {
    setHeldBase((held) => (next ? (held ?? base) : undefined));
    onUnsavedChange(next);
  }
  const current = revisions.find((item) => item.revision === base);
  const textFiles = (current?.files ?? []).filter((file) =>
    TEXT_TYPES.test(file.media_type),
  );
  const [chosen, setChosen] = useState('index.html');
  // The chosen file may be gone after loading a revision saved elsewhere.
  const path = textFiles.some((file) => file.path === chosen)
    ? chosen
    : 'index.html';
  if (textFiles.length === 0)
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
        key={`${base}:${path}`}
        client={client}
        artifact={artifact}
        base={base}
        path={path}
        onSaved={(saved) => {
          // The draft is committed: follow the latest revision again.
          setHeldBase(undefined);
          onSaved(saved);
        }}
        onReload={() => {
          setHeldBase(undefined);
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
  /** The artifact as last read; its current revision may be newer than `base`. */
  artifact: Artifact;
  /** The revision this editor reads and saves against. */
  base: number;
  path: string;
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
  // A newer revision was saved elsewhere while this draft was open. While
  // this page's own save is in flight, or after its outcome was lost, that
  // revision may be its own; the failure notice then says to retry, which
  // either replays the save or reports the conflict.
  const stale =
    artifact.current_revision > base &&
    save.status !== 'saving' &&
    save.status !== 'failed';
  // Read-only while a request will replace this editor: a save in flight
  // carries the text as sent, and a reload remounts it with a new revision.
  const locked = saving || reloading;

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
      {stale && save.status !== 'conflict' ? (
        <Alert>
          <AlertTitle>
            Revision {artifact.current_revision} was saved elsewhere
          </AlertTitle>
          <AlertDescription>
            Your changes are based on revision {base}, so saving them will be
            refused. Copy what you need; loading the latest revision discards
            your changes.
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
          disabled={!changed || locked}
          onClick={() => void commit()}
        >
          <SaveIcon data-icon="inline-start" aria-hidden="true" />
          {saving ? 'Saving…' : 'Save revision'}
        </Button>
        {changed && !locked && save.status !== 'conflict' ? (
          <Button
            type="button"
            variant="outline"
            onClick={() => edit(undefined)}
          >
            <Undo2Icon data-icon="inline-start" aria-hidden="true" />
            Discard changes
          </Button>
        ) : null}
        {save.status === 'conflict' || stale ? (
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
