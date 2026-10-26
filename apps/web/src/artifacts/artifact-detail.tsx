import { useState } from 'react';
import { RotateCwIcon, SaveIcon } from 'lucide-react';
import { ArtifreeApiError, newIdempotencyKey } from '@artifree/client';
import type { ArtifreeClient } from '@artifree/client';
import type { Artifact, Revision } from '@artifree/contracts';
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
import { describeFailure, useRequest } from './use-request';

/** Sandbox flags for previews; without allow-same-origin the content is opaque. */
const PREVIEW_SANDBOX =
  'allow-scripts allow-forms allow-modals allow-popups allow-downloads';

const TEXT_TYPES = /^(text\/|application\/json|application\/xml|image\/svg)/;

interface ArtifactDetailProps {
  client: ArtifreeClient;
  artifactId: string;
  /** Called after a revision is committed so lists can refresh. */
  onSaved: (artifact: Artifact) => void;
}

/** Preview and source editing for one artifact. */
export function ArtifactDetail({
  client,
  artifactId,
  onSaved,
}: ArtifactDetailProps) {
  const [generation, setGeneration] = useState(0);
  const details = useRequest(
    `${artifactId}:${generation}`,
    async () => {
      const [artifact, revisions] = await Promise.all([
        client.getArtifact(artifactId),
        client.listRevisions(artifactId),
      ]);
      return { artifact, revisions };
      // After a save, keep showing the card until the new revision arrives.
    },
    { keepPrevious: true },
  );
  if (details.status === 'loading')
    return (
      <output className="text-sm text-muted-foreground">
        Loading artifact…
      </output>
    );
  if (details.status === 'error')
    return (
      <Alert variant="destructive">
        <AlertTitle>Could not load the artifact</AlertTitle>
        <AlertDescription>{details.message}</AlertDescription>
      </Alert>
    );
  const { artifact, revisions } = details.value;
  function reload(): void {
    setGeneration((value) => value + 1);
  }
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
        <Tabs defaultValue="preview">
          <TabsList aria-label="Artifact views">
            <TabsTrigger value="preview">Preview</TabsTrigger>
            <TabsTrigger value="source">Source</TabsTrigger>
          </TabsList>
          <TabsContent value="preview" className="mt-4">
            <PreviewPanel
              key={artifact.current_revision}
              client={client}
              artifact={artifact}
              revisions={revisions}
            />
          </TabsContent>
          <TabsContent value="source" keepMounted className="mt-4">
            <SourcePanel
              key={artifact.current_revision}
              client={client}
              artifact={artifact}
              revisions={revisions}
              onSaved={(saved) => {
                reload();
                onSaved(saved);
              }}
              onReload={reload}
            />
          </TabsContent>
        </Tabs>
      </CardContent>
    </Card>
  );
}

interface PreviewPanelProps {
  client: ArtifreeClient;
  artifact: Artifact;
  revisions: Revision[];
}

function PreviewPanel({ client, artifact, revisions }: PreviewPanelProps) {
  const [revision, setRevision] = useState(artifact.current_revision);
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
            value={String(revision)}
            onChange={(event) => setRevision(Number(event.target.value))}
          >
            {revisions.map((item) => (
              <NativeSelectOption key={item.revision} value={item.revision}>
                {item.revision === artifact.current_revision
                  ? `${item.revision} (current)`
                  : String(item.revision)}
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
        Content runs in a sandboxed frame on a separate origin. It cannot reach
        this page, the API, or the network; data, model, and file capabilities
        are not available yet.
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
}

function SourcePanel({
  client,
  artifact,
  revisions,
  onSaved,
  onReload,
}: SourcePanelProps) {
  const current = revisions.find(
    (item) => item.revision === artifact.current_revision,
  );
  const textFiles = (current?.files ?? []).filter((file) =>
    TEXT_TYPES.test(file.media_type),
  );
  const [path, setPath] = useState('index.html');
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
            onChange={(event) => setPath(event.target.value)}
          >
            {textFiles.map((file) => (
              <NativeSelectOption key={file.path} value={file.path}>
                {file.path}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        </Field>
      </FieldGroup>
      <FileEditor
        key={path}
        client={client}
        artifact={artifact}
        path={path}
        onSaved={onSaved}
        onReload={onReload}
      />
    </div>
  );
}

interface FileEditorProps {
  client: ArtifreeClient;
  artifact: Artifact;
  path: string;
  onSaved: (artifact: Artifact) => void;
  onReload: () => void;
}

type SaveState =
  | { readonly status: 'idle' }
  | { readonly status: 'saving' }
  | { readonly status: 'conflict'; readonly current: number | undefined }
  | { readonly status: 'failed'; readonly message: string };

function FileEditor({
  client,
  artifact,
  path,
  onSaved,
  onReload,
}: FileEditorProps) {
  const base = artifact.current_revision;
  const loaded = useRequest(`${artifact.id}:${base}:${path}`, async () =>
    new TextDecoder().decode(await client.readFile(artifact.id, base, path)),
  );
  const [draft, setDraft] = useState<string>();
  const [save, setSave] = useState<SaveState>({ status: 'idle' });
  // One key per draft, so retrying after a lost response cannot commit twice.
  const [requestKey, setRequestKey] = useState(newIdempotencyKey);

  if (loaded.status === 'loading')
    return (
      <output className="text-sm text-muted-foreground">Loading {path}…</output>
    );
  if (loaded.status === 'error')
    return (
      <Alert variant="destructive">
        <AlertTitle>Could not read {path}</AlertTitle>
        <AlertDescription>{loaded.message}</AlertDescription>
      </Alert>
    );
  const value = draft ?? loaded.value;
  const changed = value !== loaded.value;

  async function commit(): Promise<void> {
    setSave({ status: 'saving' });
    try {
      const result = await client.saveRevision(
        artifact.id,
        {
          base_revision: base,
          changes: [{ op: 'put', path, encoding: 'utf8', content: value }],
        },
        { idempotencyKey: requestKey },
      );
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
          onChange={(event) => {
            setDraft(event.target.value);
            setRequestKey(newIdempotencyKey());
            if (save.status !== 'saving') setSave({ status: 'idle' });
          }}
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
      {save.status === 'failed' ? (
        <Alert variant="destructive">
          <AlertTitle>Not saved</AlertTitle>
          <AlertDescription>{save.message}</AlertDescription>
        </Alert>
      ) : null}
      <Field orientation="horizontal">
        <Button
          type="button"
          disabled={!changed || save.status === 'saving'}
          onClick={() => void commit()}
        >
          <SaveIcon data-icon="inline-start" aria-hidden="true" />
          {save.status === 'saving' ? 'Saving…' : 'Save revision'}
        </Button>
        {save.status === 'conflict' ? (
          <Button type="button" variant="outline" onClick={onReload}>
            <RotateCwIcon data-icon="inline-start" aria-hidden="true" />
            Load latest
          </Button>
        ) : null}
      </Field>
    </FieldGroup>
  );
}
