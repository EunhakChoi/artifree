import { useEffect, useState } from 'react';
import { FileCode2Icon, LogOutIcon, RotateCwIcon } from 'lucide-react';
import { ArtifreeClient } from '@artifree/client';
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
import { pageVisibility } from './page-visibility';
import { RequestGate } from './request-gate';
import { useLiveWorkspace, versionOf } from './use-live-workspace';
import type { FeedState } from './use-live-workspace';
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
    <ConnectedWorkspace
      key={token}
      token={token}
      onDisconnect={() => connect(undefined)}
    />
  );
}

interface ConnectedWorkspaceProps {
  token: string;
  onDisconnect: () => void;
}

const FEED_MESSAGES: Record<FeedState, string> = {
  connecting: 'Connecting to live updates…',
  live: 'Live updates on.',
  reconnecting:
    'Reconnecting live updates; changes made meanwhile will follow.',
  paused: 'Live updates paused while this tab is hidden.',
  stopped: 'Live updates stopped; reconnect to try again.',
};

function ConnectedWorkspace({ token, onDisconnect }: ConnectedWorkspaceProps) {
  // One gate per connection, under the client: every request this page
  // sends, whichever view asks for it, passes through it.
  const [client] = useState(
    () =>
      new ArtifreeClient({
        baseUrl: '',
        accessToken: token,
        fetch: new RequestGate(pageVisibility).fetch,
      }),
  );
  const live = useLiveWorkspace(client);
  const [generation, setGeneration] = useState(0);
  const [selected, setSelected] = useState<string>();
  // While the editor holds unsaved changes, actions that would replace it are
  // disabled so the draft is never dropped silently.
  const [unsaved, setUnsaved] = useState(false);
  const { epoch } = live;
  const artifacts = useRequest(
    live.canRead ? `${generation}:${epoch}:${live.news}` : undefined,
    async (signal) => {
      // The first page only; paging the list is roadmap A03b.
      const page = await client.listArtifacts({}, { signal });
      return { list: page.artifacts, epoch };
    },
    // Keep the list and the open artifact mounted while it refreshes.
    { keepPrevious: true },
  );
  // A list answered for the current request; only it may close or open an
  // artifact, or vouch for revisions.
  const fresh =
    artifacts.status === 'ready' && !artifacts.refreshing
      ? artifacts.value
      : undefined;
  const { learn } = live;
  useEffect(() => {
    if (fresh !== undefined)
      learn(
        fresh.epoch,
        fresh.list.map((artifact) => ({
          id: artifact.id,
          version: versionOf(artifact),
        })),
      );
  }, [fresh, learn]);
  const listed =
    artifacts.status === 'ready'
      ? artifacts.value.list
      : artifacts.status === 'error'
        ? artifacts.previous?.list
        : undefined;
  // A draft keeps its artifact open; otherwise the open artifact follows
  // the fresh list: when it is no longer listed, the first one opens.
  const keepSelected =
    selected !== undefined &&
    (unsaved ||
      fresh === undefined ||
      fresh.list.some(({ id }) => id === selected));
  const selectedId = keepSelected ? selected : (fresh?.list ?? listed)?.[0]?.id;
  // An artifact opened because it was listed first stays open like a chosen
  // one, also when a later list puts another first.
  if (selectedId !== selected) setSelected(selectedId);
  function saved(artifact: Artifact): void {
    // This page's own save is not news when its notice arrives.
    learn(epoch, [{ id: artifact.id, version: versionOf(artifact) }]);
    setSelected(artifact.id);
    setGeneration((value) => value + 1);
  }
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
        <CreateDialog client={client} disabled={unsaved} onCreated={saved} />
        <Button variant="ghost" disabled={unsaved} onClick={onDisconnect}>
          <LogOutIcon data-icon="inline-start" aria-hidden="true" />
          Disconnect
        </Button>
      </div>
      <output aria-live="polite" className="text-sm text-muted-foreground">
        {FEED_MESSAGES[live.feed]}
      </output>
      {artifacts.status === 'loading' ? (
        <output className="text-sm text-muted-foreground">
          Loading artifacts…
        </output>
      ) : null}
      {selectedId === undefined ? (
        listFailure
      ) : (
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
              epoch={epoch}
              resets={live.resets}
              known={live.known[selectedId]}
              unknownChanges={live.unknown[selectedId] ?? 0}
              onSaved={saved}
              unsaved={unsaved}
              onUnsavedChange={setUnsaved}
            />
          </div>
        </div>
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
