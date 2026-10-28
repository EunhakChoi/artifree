import { useState } from 'react';
import { RotateCwIcon } from 'lucide-react';
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
import { Field, FieldGroup, FieldLabel } from '@/components/ui/field';
import {
  NativeSelect,
  NativeSelectOption,
} from '@/components/ui/native-select';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs';
import { refusedFloor, useWriteGate } from './conditional-write';
import { DuplicateDialog } from './duplicate-dialog';
import { ExportButton } from './export-button';
import { HistoryPanel, useRestore } from './history-panel';
import { MetadataActions } from './metadata-actions';
import { PreviewFrame } from './preview-frame';
import { SourcePanel } from './source-editor';
import type { Loaded } from './source-editor';
import { RecoverNotice } from './trash-list';
import { useChoice } from './use-choice';
import { isNewer, versionOf } from './use-live-workspace';
import type { TrashState, Version } from './use-live-workspace';
import { useRequest } from './use-request';

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
  /**
   * Called with the artifact after this page committed a revision of it or
   * created it as a copy; the workspace opens it and lists refresh.
   */
  onSaved: (artifact: Artifact) => void;
  /** Called after this page renamed or pinned the artifact. */
  onMetadataChanged: (artifact: Artifact) => void;
  /** Called after this page moved the artifact to the trash. */
  onDeleted: (artifact: Artifact) => void;
  /** Called after this page recovered the artifact from the trash. */
  onRecovered: (artifact: Artifact) => void;
  /**
   * Whether the page knows the artifact is in the trash, and as of which
   * metadata revision (decision 0014, item 6). It stays open there only
   * for a draft.
   */
  trash: TrashState | undefined;
  /** Called with the artifact when a read finds it outside the trash. */
  onFound: (artifact: Artifact) => void;
  /** Called when a refused recovery and a read show it still in the trash. */
  onStillInTrash: (metadata: number) => void;
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
  onDeleted,
  onRecovered,
  trash,
  onFound,
  onStillInTrash,
  unsaved,
  onUnsavedChange,
}: ArtifactDetailProps) {
  // In the trash every read of the artifact answers as for one that does
  // not exist, so nothing is read for it until it is recovered; what was
  // read before stays shown, with a draft.
  const inTrash = trash?.deleted === true;
  const [generation, setGeneration] = useState(0);
  // The version this view reads for, in the epoch it was known in. It
  // follows a newer known version only after its own read settled, since a
  // pending read may bring it. A new epoch forgets it, like the versions
  // known before.
  const [target, setTarget] = useState({ epoch, version: known });
  const targetVersion = target.epoch === epoch ? target.version : undefined;
  const details = useRequest<Loaded>(
    inTrash
      ? undefined
      : `${artifactId}:${generation}:${epoch}:${targetVersion?.revision}:${targetVersion?.metadata}:${unknownChanges}`,
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
  // Duplicate and download offer the revision the preview shows.
  const previewed = keptChoice(revisions, chosen) ?? artifact.current_revision;
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
            {inTrash ? null : (
              <>
                <MetadataActions
                  client={client}
                  artifact={artifact}
                  gate={metadataGate}
                  wait={metadataWait}
                  resets={resets}
                  onUpdated={onMetadataChanged}
                  onDeleted={onDeleted}
                  onConflict={reload}
                  unsaved={unsaved}
                />
                <DuplicateDialog
                  client={client}
                  artifact={artifact}
                  revisions={revisions}
                  revision={previewed}
                  unsaved={unsaved}
                  resets={resets}
                  stale={stale}
                  onDuplicated={onSaved}
                />
                <ExportButton
                  client={client}
                  artifactId={artifact.id}
                  revision={previewed}
                  stale={stale}
                />
              </>
            )}
          </div>
        </CardAction>
      </CardHeader>
      <CardContent>
        <div className="flex flex-col gap-4">
          {stale ? (
            <output className="text-sm text-muted-foreground">
              Reading the artifact again: the server&apos;s data may have
              changed wholesale. Pin, rename, restore, duplicate, download,
              previews, and comparisons wait until it is read.
            </output>
          ) : null}
          {inTrash ? (
            <RecoverNotice
              client={client}
              artifactId={artifactId}
              base={trash.metadata}
              resets={resets}
              onRecovered={onRecovered}
              onFound={onFound}
              onStillInTrash={onStillInTrash}
            />
          ) : details.status === 'error' ? (
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
            {/* The views that read the artifact are not mounted in the
                trash; they mount afresh once it is recovered. */}
            <TabsContent value="preview" className="mt-4">
              {inTrash ? (
                <InTrash what="preview it" />
              ) : (
                <PreviewPanel
                  client={client}
                  artifact={artifact}
                  revisions={revisions}
                  epoch={shown.epoch}
                  stale={stale}
                  chosen={chosen}
                  onChoose={setChosen}
                />
              )}
            </TabsContent>
            <TabsContent value="history" className="mt-4">
              {inTrash ? (
                <InTrash what="compare or restore its revisions" />
              ) : (
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
              )}
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
                inTrash={inTrash}
              />
            </TabsContent>
          </Tabs>
        </div>
      </CardContent>
    </Card>
  );
}

/** Said instead of a view that would read an artifact in the trash. */
function InTrash({ what }: { what: string }) {
  return (
    <p className="text-sm text-muted-foreground">
      This artifact is in the trash. Recover it to {what}.
    </p>
  );
}

/** The revision chosen for the preview while it is listed. */
function keptChoice(
  revisions: Revision[],
  chosen: number | undefined,
): number | undefined {
  return revisions.some((item) => item.revision === chosen)
    ? chosen
    : undefined;
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
  const kept = keptChoice(revisions, chosen);
  const revision = kept ?? latest;
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
      <PreviewFrame
        client={client}
        artifactId={artifact.id}
        title={`Preview of ${artifact.title}, revision ${revision}`}
        revision={revision}
        epoch={epoch}
        stale={stale}
      />
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
