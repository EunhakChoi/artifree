import { useEffect, useMemo, useState } from 'react';
import { FileCode2Icon, LogOutIcon, RotateCwIcon } from 'lucide-react';
import { ArtifreeClient } from '@artifree/client';
import type { ChangeFeedEvent } from '@artifree/client';
import type { Artifact } from '@artifree/contracts';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from '@/components/ui/empty';
import { ArtifactDetail } from './artifact-detail';
import { ConnectForm } from './connect-form';
import { CreateDialog } from './create-dialog';
import { type FeedStatus, useChangeFeed } from './use-change-feed';
import { useRequest } from './use-request';

const TOKEN_KEY = 'artifree.accessToken';

function storedToken(): string | undefined {
  try {
    return sessionStorage.getItem(TOKEN_KEY) ?? undefined;
  } catch {
    // Storage can be unavailable (privacy modes); the user reconnects.
    return undefined;
  }
}

function storeToken(token: string | undefined): void {
  try {
    if (token === undefined) sessionStorage.removeItem(TOKEN_KEY);
    else sessionStorage.setItem(TOKEN_KEY, token);
  } catch {
    // Without storage the token lives only in memory for this page.
  }
}

/** Lists, creates, previews, and edits artifacts on a connected server. */
export function ArtifactWorkspace() {
  const [token, setToken] = useState(storedToken);
  function connect(next: string | undefined): void {
    storeToken(next);
    setToken(next);
  }
  if (token === undefined) return <ConnectForm onConnect={connect} />;
  return (
    <ConnectedWorkspace token={token} onDisconnect={() => connect(undefined)} />
  );
}

interface ConnectedWorkspaceProps {
  token: string;
  onDisconnect: () => void;
}

/** How long the first read waits for the change stream to start. */
const FIRST_READ_WAIT_MS = 2000;

/**
 * What the change feed has announced. `epoch` advances when everything must
 * be read again: when a stream starts at the head or is reset. `latest`
 * holds the newest revision per artifact that this page announced, saved,
 * or read in the list, and `news` advances for each newer one, so a
 * duplicate or older event changes nothing.
 */
interface Announced {
  readonly epoch: number;
  readonly news: number;
  readonly latest: Readonly<Record<string, number>>;
}

function withLatest(
  announced: Announced,
  id: string,
  revision: number,
): Announced {
  if (revision <= (announced.latest[id] ?? 0)) return announced;
  return {
    ...announced,
    news: announced.news + 1,
    latest: { ...announced.latest, [id]: revision },
  };
}

/** `announced` with the revisions a list read showed; they are not news. */
function withListed(
  announced: Announced,
  artifacts: readonly Artifact[],
): Announced {
  const newer = artifacts.filter(
    (artifact) =>
      artifact.current_revision > (announced.latest[artifact.id] ?? 0),
  );
  if (newer.length === 0) return announced;
  const latest = { ...announced.latest };
  for (const artifact of newer) latest[artifact.id] = artifact.current_revision;
  return { ...announced, latest };
}

function ConnectedWorkspace({ token, onDisconnect }: ConnectedWorkspaceProps) {
  const client = useMemo(
    () => new ArtifreeClient({ baseUrl: '', accessToken: token }),
    [token],
  );
  const [generation, setGeneration] = useState(0);
  const [selected, setSelected] = useState<string>();
  const [announced, setAnnounced] = useState<Announced>({
    epoch: 0,
    news: 0,
    latest: {},
  });
  // While the editor holds unsaved changes, actions that would replace it are
  // disabled so the draft is never dropped silently.
  const [unsaved, setUnsaved] = useState(false);
  const feed = useChangeFeed(client, (event: ChangeFeedEvent) => {
    if (event.type !== 'change') {
      // Read everything (again): changes before this position are unknown,
      // and after a reset to another database revisions may start lower.
      setAnnounced((value) => ({
        ...value,
        epoch: value.epoch + 1,
        latest: {},
      }));
      return;
    }
    const { artifact_id: id, revision } = event.change;
    setAnnounced((value) => withLatest(value, id, revision));
  });
  // Subscribe first, then read: the first read waits for the stream's
  // `ready`, so no commit falls between them. If the stream does not start
  // soon, as when the browser holds the request back, the page reads anyway
  // and reads again once it starts.
  const [patient, setPatient] = useState(true);
  useEffect(() => {
    const timer = setTimeout(() => setPatient(false), FIRST_READ_WAIT_MS);
    return () => clearTimeout(timer);
  }, []);
  const waiting =
    announced.epoch === 0 &&
    patient &&
    (feed.state === 'connecting' || feed.state === 'live');
  // Keep the list (and the open artifact) mounted while it refreshes.
  const artifacts = useRequest(
    waiting
      ? undefined
      : `${token}:${generation}:${announced.epoch}:${announced.news}`,
    () => client.listArtifacts(),
    { keepPrevious: true },
  );
  const listed = artifacts.status === 'ready' ? artifacts.value : undefined;
  if (listed !== undefined && withListed(announced, listed) !== announced)
    setAnnounced((value) => withListed(value, listed));
  function refresh(artifact: Artifact): void {
    setSelected(artifact.id);
    setGeneration((value) => value + 1);
    // This page's own commit; its feed event is then not news.
    setAnnounced((value) =>
      withLatest(value, artifact.id, artifact.current_revision),
    );
  }
  // Keep the artifact that is shown first open when the list reorders after
  // a commit elsewhere.
  if (selected === undefined && listed?.[0] !== undefined)
    setSelected(listed[0].id);
  const selectedId = selected ?? listed?.[0]?.id;
  // Once an artifact is open, a failed or retried list load must not unmount
  // it and any draft in it; the failure is shown in place of the list.
  const showGrid = selectedId !== undefined && listed?.length !== 0;
  const listFailure =
    artifacts.status === 'error' ? (
      <Alert variant="destructive">
        <AlertTitle>Could not list artifacts</AlertTitle>
        <AlertDescription>
          <p>{artifacts.message}</p>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => setGeneration((value) => value + 1)}
          >
            <RotateCwIcon data-icon="inline-start" aria-hidden="true" />
            Try again
          </Button>
        </AlertDescription>
      </Alert>
    ) : null;

  return (
    <div className="flex flex-col gap-5">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <CreateDialog client={client} disabled={unsaved} onCreated={refresh} />
        <div className="flex flex-wrap items-center gap-3">
          <output className="text-sm text-muted-foreground">
            {feedMessage(feed)}
          </output>
          <Button variant="ghost" disabled={unsaved} onClick={onDisconnect}>
            <LogOutIcon data-icon="inline-start" aria-hidden="true" />
            Disconnect
          </Button>
        </div>
      </div>
      {artifacts.status === 'loading' ? (
        <output className="text-sm text-muted-foreground">
          Loading artifacts…
        </output>
      ) : null}
      {showGrid ? (
        <div className="grid gap-5 lg:grid-cols-3">
          <nav aria-label="Artifacts" className="flex flex-col gap-2">
            {listFailure}
            {listed?.map((artifact) => (
              <Button
                key={artifact.id}
                variant={artifact.id === selectedId ? 'secondary' : 'ghost'}
                aria-current={artifact.id === selectedId ? 'true' : undefined}
                className="justify-between"
                disabled={unsaved && artifact.id !== selectedId}
                onClick={() => setSelected(artifact.id)}
              >
                <span className="truncate">{artifact.title}</span>
                <span className="text-xs text-muted-foreground">
                  r{artifact.current_revision}
                </span>
              </Button>
            ))}
          </nav>
          <div className="min-w-0 lg:col-span-2">
            <ArtifactDetail
              key={selectedId}
              client={client}
              artifactId={selectedId}
              epoch={announced.epoch}
              announcedRevision={announced.latest[selectedId] ?? 0}
              onSaved={refresh}
              unsaved={unsaved}
              onUnsavedChange={setUnsaved}
            />
          </div>
        </div>
      ) : (
        listFailure
      )}
      {listed?.length === 0 ? (
        <Empty>
          <EmptyHeader>
            <EmptyMedia variant="icon">
              <FileCode2Icon aria-hidden="true" />
            </EmptyMedia>
            <EmptyTitle>No artifacts yet</EmptyTitle>
            <EmptyDescription>
              Create one here, or from an agent with{' '}
              <code>artifree artifact create</code>.
            </EmptyDescription>
          </EmptyHeader>
        </Empty>
      ) : null}
    </div>
  );
}

function feedMessage(feed: FeedStatus): string {
  switch (feed.state) {
    case 'connecting':
      return 'Connecting to live updates…';
    case 'live':
      return 'Live updates on';
    case 'reconnecting':
      return 'Reconnecting live updates; changes made meanwhile will follow.';
    case 'paused':
      return 'Live updates paused while this tab is hidden.';
    case 'stopped':
      return 'Live updates stopped; reconnect to try again.';
  }
}
