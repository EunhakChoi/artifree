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
import { emptiness, inView, isFiltered, queryKey } from './list-query';
import { START, navigate, opened } from './navigation';
import type { NavigationEvent } from './navigation';
import { ConnectForm } from './connect-form';
import { CreateDialog } from './create-dialog';
import { KeptDrafts } from './kept-drafts';
import type { PageDrafts } from './page-drafts';
import { pageVisibility } from './page-visibility';
import { RequestGate } from './request-gate';
import { TrashNav } from './trash-list';
import { useLiveWorkspace } from './use-live-workspace';
import type { FeedState } from './use-live-workspace';
import { describeFailure, useRequest } from './use-request';

const TOKEN_KEY_PREFIX = 'artifree.accessToken';
const SERVER_KEY = 'artifree.serverUrl';

function tokenKeyForServer(serverUrl: string): string {
  if (!serverUrl) return TOKEN_KEY_PREFIX;
  try {
    const origin = new URL(serverUrl).origin;
    if (typeof window !== 'undefined' && origin === window.location.origin) {
      return TOKEN_KEY_PREFIX;
    }
    return `${TOKEN_KEY_PREFIX}.${origin}`;
  } catch {
    return TOKEN_KEY_PREFIX;
  }
}

function readInitialServerUrl(): { url: string; fromQuery: boolean } {
  try {
    const fromQuery = new URLSearchParams(window.location.search).get('server');
    if (fromQuery !== null && fromQuery.trim() !== '') {
      const parsed = new URL(fromQuery.trim());
      if (parsed.protocol === 'http:' || parsed.protocol === 'https:') {
        return { url: parsed.origin, fromQuery: true };
      }
    }
  } catch {
    // Malformed query param ignored
  }

  try {
    const fromStorage = sessionStorage.getItem(SERVER_KEY);
    if (fromStorage !== null && fromStorage.trim() !== '') {
      const parsed = new URL(fromStorage.trim());
      if (parsed.protocol === 'http:' || parsed.protocol === 'https:') {
        return { url: parsed.origin, fromQuery: false };
      }
    }
  } catch {
    // Session storage unavailable
  }

  return { url: '', fromQuery: false };
}

function storedToken(
  serverUrl: string,
  fromQuery: boolean,
): string | undefined {
  // Security guard against token exfiltration:
  // If an external server was specified via URL query ?server=..., do not automatically
  // reuse tokens stored for that origin or current origin. The user must explicitly connect.
  if (fromQuery) {
    return undefined;
  }
  const key = tokenKeyForServer(serverUrl);
  try {
    return sessionStorage.getItem(key) ?? undefined;
  } catch {
    return undefined;
  }
}

function storeToken(token: string | undefined, serverUrl: string): void {
  const key = tokenKeyForServer(serverUrl);
  try {
    if (token === undefined) sessionStorage.removeItem(key);
    else sessionStorage.setItem(key, token);
  } catch {
    // Without storage the token lives only in memory for this page.
  }
}

function storeServerUrl(serverUrl: string): void {
  try {
    if (!serverUrl) sessionStorage.removeItem(SERVER_KEY);
    else sessionStorage.setItem(SERVER_KEY, serverUrl);
  } catch {
    // Without storage the server lives only in memory for this page.
  }
}

/**
 * Lists, creates, previews, and edits artifacts on a connected server.
 * `drafts` belongs to the page and outlives every connection.
 */
export function ArtifactWorkspace({ drafts }: { drafts: PageDrafts }) {
  const [initialConfig] = useState(readInitialServerUrl);
  const [serverUrl, setServerUrl] = useState<string>(initialConfig.url);
  const [token, setToken] = useState<string | undefined>(() =>
    storedToken(initialConfig.url, initialConfig.fromQuery),
  );

  function connect(newToken: string | undefined, newServerUrl?: string): void {
    const effectiveServer =
      newServerUrl !== undefined ? newServerUrl : serverUrl;
    setServerUrl(effectiveServer);
    storeServerUrl(effectiveServer);
    storeToken(newToken, effectiveServer);
    setToken(newToken);
  }

  if (token === undefined) {
    return (
      <ConnectForm
        initialServerUrl={serverUrl}
        onConnect={(tok, srv) => connect(tok, srv)}
      />
    );
  }

  return (
    <ConnectedWorkspace
      key={`${serverUrl}:${token}`}
      token={token}
      serverUrl={serverUrl}
      drafts={drafts}
      onDisconnect={() => connect(undefined)}
    />
  );
}

interface ConnectedWorkspaceProps {
  token: string;
  serverUrl: string;
  drafts: PageDrafts;
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

function ConnectedWorkspace({
  token,
  serverUrl,
  drafts,
  onDisconnect,
}: ConnectedWorkspaceProps) {
  // One gate per connection, under the client: every request this page
  // sends, whichever view asks for it, passes through it.
  const [client] = useState(
    () =>
      new ArtifreeClient({
        baseUrl: serverUrl,
        accessToken: token,
        fetch: new RequestGate(pageVisibility).fetch,
      }),
  );
  const [attempt, setAttempt] = useState(0);
  const [closing, setClosing] = useState(false);
  const [closeError, setCloseError] = useState<string>();
  const session = useRequest(`runtime-session:${attempt}`, async (signal) => {
    await client.openRuntimeSession({ signal });
    return true;
  });
  async function disconnect(): Promise<void> {
    if (closing) return;
    setClosing(true);
    setCloseError(undefined);
    try {
      await client.closeRuntimeSession();
      onDisconnect();
    } catch (error: unknown) {
      setCloseError(describeFailure(error));
      setClosing(false);
    }
  }
  const error =
    closeError === undefined ? null : (
      <Alert variant="destructive">
        <AlertTitle>Disconnect not confirmed</AlertTitle>
        <AlertDescription>
          {closeError} Retry disconnect to revoke this connection. You can
          forget the token locally, but any server grant may remain until it
          expires.
          <Button variant="outline" size="sm" onClick={onDisconnect}>
            Forget token locally
          </Button>
        </AlertDescription>
      </Alert>
    );
  if (closing || closeError !== undefined) {
    return (
      <div className="flex flex-col gap-3">
        {error ?? <p>Closing connection…</p>}
        {closing ? null : (
          <Button variant="ghost" onClick={() => void disconnect()}>
            Disconnect
          </Button>
        )}
      </div>
    );
  }
  if (session.status !== 'ready') {
    return (
      <div className="flex flex-col gap-3">
        {session.status === 'loading' ? (
          <p>Opening connection…</p>
        ) : (
          <Alert variant="destructive">
            <AlertTitle>Connection unavailable</AlertTitle>
            <AlertDescription>{session.message}</AlertDescription>
          </Alert>
        )}
        <div className="flex gap-2">
          {session.status === 'error' ? (
            <Button onClick={() => setAttempt((value) => value + 1)}>
              Retry connection
            </Button>
          ) : null}
          <Button variant="ghost" onClick={() => void disconnect()}>
            Disconnect
          </Button>
        </div>
      </div>
    );
  }
  return (
    <WorkspaceBody
      client={client}
      drafts={drafts}
      onDisconnect={() => void disconnect()}
    />
  );
}

function WorkspaceBody({
  client,
  drafts,
  onDisconnect,
}: {
  client: ArtifreeClient;
  drafts: PageDrafts;
  onDisconnect: () => void;
}) {
  const live = useLiveWorkspace(client);
  const { epoch, resets, learn, learnInTrash, isCurrent } = live;
  const [generation, setGeneration] = useState(0);
  // What is listed and open, and what the page says about an artifact that
  // left the view, change together (decision 0014, item 5).
  const [navigation, dispatch] = useReducer(navigate, START);
  const { query, count, selected, notice } = navigation;
  // While the editor holds unsaved changes, actions that would replace it are
  // disabled so the draft is never dropped silently.
  const [unsaved, setUnsaved] = useState(false);
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
  // artifact, or vouch for versions.
  const fresh =
    artifacts.status === 'ready' && !artifacts.refreshing
      ? artifacts.value
      : undefined;
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
  const inTrash = (id: string) => live.trash[id]?.deleted === true;
  // A list leaves out what the page learned left its view after the list
  // was read: a move to the trash, or a recovery from it (item 4).
  const listed = shownList?.list.filter((artifact) => {
    const learned = live.trash[artifact.id];
    return (
      learned === undefined ||
      learned.metadata <= artifact.metadata_revision ||
      learned.deleted === query.deleted
    );
  });
  const empty = emptiness(query, listed?.length, shownList?.complete ?? true);
  const showMore = shownList !== undefined && !shownList.complete;
  const selectedId = opened({
    selected,
    trashShown: query.deleted,
    unsaved,
    inTrash,
    fresh,
    listed,
  });
  // An artifact opened because it was listed first stays open like a chosen
  // one, also when a later list puts another first. One that closed because
  // it went to the trash says so.
  if (!query.deleted && selectedId !== selected) {
    if (selected !== undefined && inTrash(selected))
      dispatch({ type: 'left', id: selected });
    dispatch({ type: 'open', id: selectedId });
  }
  // The answers to this page's own writes are learned in the epoch they
  // were sent in: each handler below runs as it was rendered when its
  // write was sent, so `epoch` is that epoch. One sent before the server's
  // data may have been replaced is not learned, and what the page reads
  // again says what there is (item 4).
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
  // A move or recovery from an earlier epoch does not move the page either.
  // `isCurrent` follows the feed as events arrive, so an answer that
  // settles before the page renders a new epoch is not taken as current.
  function answered(event: NavigationEvent): void {
    if (isCurrent(epoch)) dispatch(event);
  }
  function deleted(artifact: Artifact): void {
    metadataChanged(artifact);
    answered({ type: 'trashed', artifact });
  }
  function recovered(artifact: Artifact): void {
    metadataChanged(artifact);
    answered({ type: 'recovered', artifact });
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
  // Why the list shows nothing, unless the whole workspace is empty, which
  // is said below it (item 7).
  const emptyList =
    empty === 'left' ? (
      <p className="text-sm text-muted-foreground">
        The artifacts listed so far have left this view. Show more to list the
        next ones.
      </p>
    ) : empty === 'unmatched' ? (
      <p className="text-sm text-muted-foreground">
        {query.deleted
          ? 'Nothing in the trash matches.'
          : 'No artifacts match.'}
      </p>
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
              onClick={() => changeQuery(inView(query, 'trash'))}
            >
              <Trash2Icon data-icon="inline-start" aria-hidden="true" />
              Show trash
            </Button>
          )}
        </div>
      )}
      <KeptDrafts
        drafts={drafts}
        openId={selectedId}
        locked={unsaved}
        onOpen={(id) => dispatch({ type: 'choose', id })}
      />
      {shownList === undefined && artifacts.status !== 'error' ? (
        <output className="text-sm text-muted-foreground">
          Loading artifacts…
        </output>
      ) : null}
      {/* Once any list was read, the fields stay: also for an empty list,
          since every artifact may be in the trash, and while another
          search's or view's list loads or fails, so typing and the way back
          are kept (item 3). */}
      {selectedId === undefined && lastList === undefined ? (
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
                empty={emptyList}
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
                empty={emptyList}
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
                resets={resets}
                known={live.known[selectedId]}
                unknownChanges={live.unknown[selectedId] ?? 0}
                onSaved={saved}
                onMetadataChanged={metadataChanged}
                onDeleted={deleted}
                onRecovered={recovered}
                trash={live.trash[selectedId]}
                onFound={(artifact) => learn(epoch, [artifact])}
                onStillInTrash={(metadata) =>
                  learnInTrash(epoch, selectedId, metadata)
                }
                isCurrent={isCurrent}
                unsaved={unsaved}
                onUnsavedChange={setUnsaved}
                drafts={drafts}
              />
            )}
          </div>
        </div>
      )}
      {empty === 'none' ? (
        <Empty>
          <EmptyHeader>
            <EmptyMedia variant="icon">
              <FileCode2Icon aria-hidden="true" />
            </EmptyMedia>
            <EmptyTitle>No artifacts</EmptyTitle>
            <EmptyDescription>
              Create one here, or from an agent with{' '}
              <code>artifree artifact create</code>. Artifacts moved to the
              trash are listed when Show is set to Trash.
            </EmptyDescription>
          </EmptyHeader>
        </Empty>
      ) : null}
    </div>
  );
}
