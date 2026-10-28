import { useEffect, useReducer, useState } from 'react';
import {
  FileCode2Icon,
  LogOutIcon,
  RotateCwIcon,
  Trash2Icon,
} from 'lucide-react';
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
import { ArtifactNav, ListControls, readArtifacts } from './artifact-list';
import { ConnectForm } from './connect-form';
import { CreateDialog } from './create-dialog';
import { isFiltered, queryKey, showing } from './list-query';
import { START, navigate, opened } from './navigation';
import { pageVisibility } from './page-visibility';
import { RequestGate } from './request-gate';
import { TrashNav } from './trash-list';
import { useLiveWorkspace } from './use-live-workspace';
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
  // What is listed and open. Writes report their outcome as events, which
  // apply to the page as it is when they arrive (decision 0014, item 5).
  const [navigation, dispatch] = useReducer(navigate, START);
  const { query, count, selected, notice } = navigation;
  // While the editor holds unsaved changes, actions that would replace it are
  // disabled so the draft is never dropped silently.
  const [unsaved, setUnsaved] = useState(false);
  const { epoch, resets } = live;
  const shownQuery = queryKey(query);
  const artifacts = useRequest(
    live.canRead
      ? `${generation}:${epoch}:${live.news}:${shownQuery}:${count}`
      : undefined,
    async (signal) => {
      const listed = await readArtifacts(client, query, count, signal);
      return {
        query: shownQuery,
        list: listed.artifacts,
        // Only a complete list without filters shows every artifact, so
        // only it can tell that one no longer exists.
        everything: listed.complete && !isFiltered(query),
        complete: listed.complete,
        epoch,
        resets,
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
    if (fresh !== undefined) learn(fresh.epoch, fresh.list);
  }, [fresh, learn]);
  const lastList =
    artifacts.status === 'ready'
      ? artifacts.value
      : artifacts.status === 'error'
        ? artifacts.previous
        : undefined;
  // Another query's results never stand in for this one's, neither while
  // it loads nor after it failed.
  const shownList = lastList?.query === shownQuery ? lastList : undefined;
  // Whether the page knows an artifact is in the trash: from a notice, a
  // list, or its own write (decision 0014, item 6).
  const inTrash = (id: string) => live.trash[id]?.deleted === true;
  // A list leaves out what the page learned left its view after the list
  // was read: a move to the trash, or a recovery from it.
  const listed = shownList?.list.filter((artifact) => {
    const learned = live.trash[artifact.id];
    return (
      learned === undefined ||
      learned.metadata <= artifact.metadata_revision ||
      learned.deleted === query.deleted
    );
  });
  const showMore = shownList !== undefined && !shownList.complete;
  const noArtifacts = listed?.length === 0 && !isFiltered(query);
  const selectedId = opened({
    selected,
    trashShown: query.deleted,
    unsaved,
    inTrash,
    fresh,
    listed,
  });
  // An artifact opened because it was listed first stays open like a chosen
  // one, also when a later list puts another first.
  if (!query.deleted && selectedId !== selected) {
    if (selected !== undefined && inTrash(selected))
      dispatch({ type: 'left', id: selected });
    dispatch({ type: 'open', id: selectedId });
  }
  // Answers to this page's own writes are learned in the epoch they were
  // sent in; one sent before the server's data may have been replaced is
  // not, and what the page reads again then says what there is.
  function saved(artifact: Artifact): void {
    // This page's own save is not news when its notice arrives.
    learn(epoch, [artifact]);
    dispatch({ type: 'choose', id: artifact.id });
    setGeneration((value) => value + 1);
  }
  function metadataChanged(artifact: Artifact): void {
    // Learning the new version also makes the open artifact read it.
    learn(epoch, [artifact]);
    setGeneration((value) => value + 1);
  }
  function deleted(artifact: Artifact): void {
    metadataChanged(artifact);
    dispatch({ type: 'trashed', artifact });
  }
  function recovered(artifact: Artifact): void {
    metadataChanged(artifact);
    dispatch({ type: 'recovered', artifact });
  }
  const changeQuery = (next: typeof query) =>
    dispatch({ type: 'query', query: next });
  const showMoreArtifacts = showMore
    ? () => dispatch({ type: 'more' })
    : undefined;
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
      {notice === undefined ? null : (
        <div className="flex flex-wrap items-center gap-3">
          <output className="text-sm">{notice.text}</output>
          {!notice.toTrash || query.deleted || unsaved ? null : (
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() => changeQuery(showing(query, 'trash'))}
            >
              <Trash2Icon data-icon="inline-start" aria-hidden="true" />
              Show trash
            </Button>
          )}
        </div>
      )}
      {shownList === undefined && artifacts.status !== 'error' ? (
        <output className="text-sm text-muted-foreground">
          Loading artifacts…
        </output>
      ) : null}
      {/* The controls stay once a list was read, also an empty one: every
          artifact may be in the trash. */}
      {selectedId === undefined && listed === undefined ? (
        listFailure
      ) : (
        <div className="grid gap-5 lg:grid-cols-3">
          <div className="flex flex-col gap-4">
            <ListControls
              query={query}
              onQueryChange={changeQuery}
              trashLocked={unsaved}
            />
            {query.deleted ? (
              <TrashNav
                client={client}
                artifacts={listed ?? []}
                resets={resets}
                shownResets={shownList?.resets ?? resets}
                known={live.known}
                onRecovered={recovered}
                onConflict={() => setGeneration((value) => value + 1)}
                empty={
                  listed === undefined ? null : (
                    <p className="text-sm text-muted-foreground">
                      Nothing in the trash matches.
                    </p>
                  )
                }
                onShowMore={showMoreArtifacts}
              >
                {listFailure}
              </TrashNav>
            ) : (
              <ArtifactNav
                artifacts={listed ?? []}
                selectedId={selectedId}
                locked={unsaved}
                onSelect={(id) => dispatch({ type: 'choose', id })}
                empty={
                  listed === undefined || noArtifacts ? null : (
                    <p className="text-sm text-muted-foreground">
                      No artifacts match.
                    </p>
                  )
                }
                onShowMore={showMoreArtifacts}
              >
                {listFailure}
              </ArtifactNav>
            )}
          </div>
          <div className="min-w-0 lg:col-span-2">
            {query.deleted ? (
              <Empty>
                <EmptyHeader>
                  <EmptyMedia variant="icon">
                    <Trash2Icon aria-hidden="true" />
                  </EmptyMedia>
                  <EmptyTitle>Trash</EmptyTitle>
                  <EmptyDescription>
                    Artifacts in the trash cannot be opened, previewed, or
                    changed. Recover one to open it; nothing here is removed.
                  </EmptyDescription>
                </EmptyHeader>
              </Empty>
            ) : selectedId === undefined ? null : (
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
                onDeleted={deleted}
                onRecovered={recovered}
                trash={live.trash[selectedId]}
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
