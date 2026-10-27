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

function ConnectedWorkspace({ token, onDisconnect }: ConnectedWorkspaceProps) {
  const client = useMemo(
    () => new ArtifreeClient({ baseUrl: '', accessToken: token }),
    [token],
  );
  const [generation, setGeneration] = useState(0);
  const [selected, setSelected] = useState<string>();
  // While the editor holds unsaved changes, actions that would replace it are
  // disabled so the draft is never dropped silently.
  const [unsaved, setUnsaved] = useState(false);
  // Keep the list (and the open artifact) mounted while it refreshes.
  const artifacts = useRequest(
    `${token}:${generation}`,
    () => client.listArtifacts(),
    { keepPrevious: true },
  );
  function refresh(artifact: Artifact): void {
    setSelected(artifact.id);
    setGeneration((value) => value + 1);
  }
  const listed = artifacts.status === 'ready' ? artifacts.value : undefined;
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
        <Button variant="ghost" disabled={unsaved} onClick={onDisconnect}>
          <LogOutIcon data-icon="inline-start" aria-hidden="true" />
          Disconnect
        </Button>
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
