import { useCallback, useEffect, useState } from 'react';
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
import {
  ALL_ARTIFACTS,
  ArtifactNav,
  ListControls,
  PAGE_SIZE,
  isFiltered,
  queryKey,
  readArtifacts,
} from './artifact-list';
import type { ListQuery } from './artifact-list';
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
  const [query, setQuery] = useState(ALL_ARTIFACTS);
  // How many artifacts are shown; a notice reads them all again.
  const [count, setCount] = useState(PAGE_SIZE);
  const changeQuery = useCallback((next: ListQuery) => {
    setQuery(next);
    setCount(PAGE_SIZE);
  }, []);
  const { epoch } = live;
  const artifacts = useRequest(
    live.canRead
      ? `${generation}:${epoch}:${live.news}:${queryKey(query)}:${count}`
      : undefined,
    async (signal) => {
      const listed = await readArtifacts(client, query, count, signal);
      return {
        list: listed.artifacts,
        // Only a complete list without filters shows every artifact, so
        // only it can tell that one no longer exists.
        everything: listed.complete && !isFiltered(query),
        complete: listed.complete,
        epoch,
      };
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
  const shownList =
    artifacts.status === 'ready'
      ? artifacts.value
      : artifacts.status === 'error'
        ? artifacts.previous
        : undefined;
  const listed = shownList?.list;
  const showMore = shownList !== undefined && !shownList.complete;
  const noArtifacts = listed?.length === 0 && !isFiltered(query);
  // A draft keeps its artifact open; otherwise the open artifact stays
  // until a fresh list of every artifact no longer contains it, and then
  // the first listed one opens. A search, a filter, or a partial list does
  // not close it.
  const keepSelected =
    selected !== undefined &&
    (unsaved ||
      fresh === undefined ||
      !fresh.everything ||
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
  function metadataChanged(artifact: Artifact): void {
    // Learning the new version also makes the open artifact read it.
    learn(epoch, [{ id: artifact.id, version: versionOf(artifact) }]);
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
      {selectedId === undefined && (listed === undefined || noArtifacts) ? (
        listFailure
      ) : (
        <div className="grid gap-5 lg:grid-cols-3">
          <div className="flex flex-col gap-4">
            <ListControls query={query} onQueryChange={changeQuery} />
            <ArtifactNav
              artifacts={listed ?? []}
              selectedId={selectedId}
              locked={unsaved}
              onSelect={setSelected}
              empty={
                listed === undefined ? null : (
                  <p className="text-sm text-muted-foreground">
                    No artifacts match.
                  </p>
                )
              }
              onShowMore={
                showMore
                  ? () => setCount((value) => value + PAGE_SIZE)
                  : undefined
              }
            >
              {listFailure}
            </ArtifactNav>
          </div>
          <div className="min-w-0 lg:col-span-2">
            {selectedId === undefined ? null : (
              <ArtifactDetail
                key={selectedId}
                client={client}
                artifactId={selectedId}
                epoch={epoch}
                resets={live.resets}
                known={live.known[selectedId]}
                unknownChanges={live.unknown[selectedId] ?? 0}
                onSaved={saved}
                onMetadataChanged={metadataChanged}
                unsaved={unsaved}
                onUnsavedChange={setUnsaved}
              />
            )}
          </div>
        </div>
      )}
      {noArtifacts ? (
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
