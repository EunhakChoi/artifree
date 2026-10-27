import { useMemo, useState } from 'react';
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
import { ReadScheduler } from './read-scheduler';
import { ReadSchedulerContext, useRequest } from './use-request';
import type { FeedState } from './use-live-workspace';
import { useLiveWorkspace } from './use-live-workspace';

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

const FEED_MESSAGES: Record<FeedState, string> = {
  connecting: 'Connecting to live updates…',
  live: 'Live updates on.',
  reconnecting:
    'Reconnecting live updates; changes made meanwhile will follow.',
  paused: 'Live updates paused while this tab is hidden.',
  stopped: 'Live updates stopped; reconnect to try again.',
};

function ConnectedWorkspace(props: ConnectedWorkspaceProps) {
  // One scheduler per connection: every read of this page goes through it.
  const [scheduler] = useState(() => new ReadScheduler(pageVisibility));
  return (
    <ReadSchedulerContext value={scheduler}>
      <LiveWorkspace {...props} />
    </ReadSchedulerContext>
  );
}

function LiveWorkspace({ token, onDisconnect }: ConnectedWorkspaceProps) {
  const client = useMemo(
    () => new ArtifreeClient({ baseUrl: '', accessToken: token }),
    [token],
  );
  const live = useLiveWorkspace(client);
  const [generation, setGeneration] = useState(0);
  const [selected, setSelected] = useState<string>();
  // While the editor holds unsaved changes, actions that would replace it are
  // disabled so the draft is never dropped silently.
  const [unsaved, setUnsaved] = useState(false);
  const artifacts = useRequest(
    live.canRead ? `${generation}:${live.epoch}:${live.news}` : undefined,
    () => client.listArtifacts(),
    { keepPrevious: true },
  );
  const listed =
    artifacts.status === 'ready'
      ? artifacts.value
      : artifacts.status === 'error'
        ? artifacts.previous
        : undefined;
  // Only a list answered for the current request says what exists now; one
  // kept while a newer read is pending may predate a reset.
  const fresh =
    artifacts.status === 'ready' && !artifacts.refreshing
      ? artifacts.value
      : undefined;
  if (fresh !== undefined) {
    live.learn(
      fresh.map((artifact) => ({
        id: artifact.id,
        revision: artifact.current_revision,
      })),
    );
    // Without a draft, the selection follows what exists: the first listed
    // artifact opens when nothing is open or the open one is gone.
    if (!unsaved && !fresh.some((artifact) => artifact.id === selected)) {
      const first = fresh[0]?.id;
      if (first !== selected) setSelected(first);
    }
  }
  function refresh(artifact: Artifact): void {
    live.learn([{ id: artifact.id, revision: artifact.current_revision }]);
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
        <CreateDialog client={client} disabled={unsaved} onCreated={refresh} />
        <Button variant="ghost" disabled={unsaved} onClick={onDisconnect}>
          <LogOutIcon data-icon="inline-start" aria-hidden="true" />
          Disconnect
        </Button>
      </div>
      <output className="text-sm text-muted-foreground">
        {FEED_MESSAGES[live.feed]}
      </output>
      {artifacts.status === 'loading' ? (
        <output className="text-sm text-muted-foreground">
          Loading artifacts…
        </output>
      ) : null}
      {selected === undefined ? listFailure : null}
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
      {selected === undefined ? null : (
        // The open artifact stays mounted whatever the list holds, so a
        // failed, empty, or replaced list cannot drop a draft in it.
        <div className="grid gap-5 lg:grid-cols-3">
          <nav aria-label="Artifacts" className="flex flex-col gap-2">
            {listFailure}
            {listed?.map((artifact) => (
              <Button
                key={artifact.id}
                variant={artifact.id === selected ? 'secondary' : 'ghost'}
                aria-current={artifact.id === selected ? 'true' : undefined}
                className="justify-between"
                disabled={unsaved && artifact.id !== selected}
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
              key={selected}
              client={client}
              artifactId={selected}
              epoch={live.epoch}
              resets={live.resets}
              knownRevision={live.known[selected] ?? 0}
              onSaved={refresh}
              unsaved={unsaved}
              onUnsavedChange={setUnsaved}
            />
          </div>
        </div>
      )}
    </div>
  );
}
