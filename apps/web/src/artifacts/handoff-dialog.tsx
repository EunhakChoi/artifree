import { useEffect, useState } from 'react';
import type { MouseEvent } from 'react';
import type { ArtifreeClient } from '@artifree/client';
import type { Handoff, HandoffSelection } from '@artifree/contracts';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import { Field, FieldDescription, FieldLabel } from '@/components/ui/field';
import {
  NativeSelect,
  NativeSelectOption,
} from '@/components/ui/native-select';
import { Textarea } from '@/components/ui/textarea';
import { describeFailure, useRequest } from './use-request';

interface HandoffDialogProps {
  readonly client: ArtifreeClient;
  readonly selection: HandoffSelection;
  readonly onClose: () => void;
}

/** Reviews a saved selection before the user sends it to a registered agent. */
export function HandoffDialog({
  client,
  selection,
  onClose,
}: HandoffDialogProps) {
  const [attempt, setAttempt] = useState(0);
  const [chosen, setChosen] = useState('');
  const [intent, setIntent] = useState('');
  const [workspace, setWorkspace] = useState<string>();
  const [handoff, setHandoff] = useState<Handoff>();
  const [busy, setBusy] = useState(false);
  const [sendUncertain, setSendUncertain] = useState(false);
  const [error, setError] = useState<string>();
  const [statusAttempt, setStatusAttempt] = useState(0);
  const sessions = useRequest(`agent-sessions:${attempt}`, (signal) =>
    client.agentSessions({ signal }),
  );
  const available = sessions.status === 'ready' ? sessions.value : [];
  const agent = available.find((item) => item.id === chosen) ?? available[0];

  // Acknowledgement reports receipt only. A failed status read stays visible
  // and may be retried; it is never turned into a successful delivery.
  useEffect(() => {
    if (
      busy ||
      handoff === undefined ||
      workspace === undefined ||
      !['queued', 'offered'].includes(handoff.state)
    )
      return undefined;
    let active = true;
    const timer = window.setTimeout(() => {
      void client.getHandoff(handoff, workspace).then(
        (next) => {
          if (active) {
            setHandoff(next);
            if (next.state !== 'staged') setSendUncertain(false);
            setError(undefined);
          }
        },
        (failure: unknown) => {
          if (active)
            setError(
              `Could not read delivery status: ${describeFailure(failure)}`,
            );
        },
      );
    }, 1500);
    return () => {
      active = false;
      window.clearTimeout(timer);
    };
  }, [busy, client, handoff, statusAttempt, workspace]);

  async function stage(event: MouseEvent<HTMLButtonElement>): Promise<void> {
    if (!event.isTrusted || busy || agent === undefined || intent.trim() === '')
      return;
    setBusy(true);
    setError(undefined);
    try {
      const connection = client.runtimeSessionId;
      const result = await client.stageHandoff({
        workspace_session_id: connection,
        agent_session_id: agent.id,
        artifact_id: selection.artifact_id,
        revision: selection.revision,
        path: selection.path,
        file_sha256: selection.file_sha256,
        start: selection.start,
        end: selection.end,
        intent,
      });
      if (
        result.selection.text !== selection.text ||
        result.agent_name !== agent.name
      )
        throw new Error('The server returned a different selection or agent.');
      setWorkspace(connection);
      setHandoff(result);
    } catch (failure: unknown) {
      setError(`Could not stage the selection: ${describeFailure(failure)}`);
    } finally {
      setBusy(false);
    }
  }

  async function send(event: MouseEvent<HTMLButtonElement>): Promise<void> {
    if (
      !event.isTrusted ||
      busy ||
      handoff === undefined ||
      workspace === undefined
    )
      return;
    setBusy(true);
    setError(undefined);
    setSendUncertain(true);
    try {
      setHandoff(await client.sendHandoff(handoff, workspace));
      setSendUncertain(false);
    } catch (failure: unknown) {
      setError(
        `Send was not confirmed. Check status or retry this same delivery: ${describeFailure(failure)}`,
      );
    } finally {
      setBusy(false);
    }
  }

  async function cancel(): Promise<void> {
    if (busy) return;
    if (handoff === undefined || workspace === undefined) {
      onClose();
      return;
    }
    setBusy(true);
    setError(undefined);
    try {
      await client.cancelHandoff(handoff, workspace);
      onClose();
    } catch (failure: unknown) {
      setError(`Cancellation was not confirmed: ${describeFailure(failure)}`);
    } finally {
      setBusy(false);
    }
  }

  async function refresh(): Promise<void> {
    if (busy || handoff === undefined || workspace === undefined) return;
    setBusy(true);
    setError(undefined);
    try {
      const next = await client.getHandoff(handoff, workspace);
      setHandoff(next);
      if (next.state !== 'staged') setSendUncertain(false);
      setStatusAttempt((value) => value + 1);
    } catch (failure: unknown) {
      setError(`Could not read delivery status: ${describeFailure(failure)}`);
    } finally {
      setBusy(false);
    }
  }

  const pending = handoff?.state === 'staged' || handoff?.state === 'queued';
  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (open || busy) return;
        if (handoff === undefined || pending) void cancel();
        else onClose();
      }}
    >
      <DialogContent
        showCloseButton={false}
        className="max-h-[90vh] overflow-y-auto sm:max-w-lg"
      >
        <DialogHeader>
          <DialogTitle>Send a source selection</DialogTitle>
          <DialogDescription>
            Review the saved file, selected text, edit intent, and agent before
            sending.
          </DialogDescription>
        </DialogHeader>
        <p className="text-sm">
          {selection.path} · revision {selection.revision} · UTF-16 range{' '}
          {selection.start}–{selection.end}
        </p>
        <pre className="max-h-40 overflow-auto rounded-md border p-3 text-xs whitespace-pre-wrap">
          {selection.text}
        </pre>
        {handoff === undefined ? (
          <>
            {sessions.status === 'loading' ? (
              <p>Loading registered agent sessions…</p>
            ) : null}
            {sessions.status === 'error' ? (
              <Alert variant="destructive">
                <AlertTitle>Could not list agent sessions</AlertTitle>
                <AlertDescription>
                  <p>{sessions.message}</p>
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    onClick={() => setAttempt((value) => value + 1)}
                  >
                    Retry sessions
                  </Button>
                </AlertDescription>
              </Alert>
            ) : null}
            {sessions.status === 'ready' && available.length === 0 ? (
              <p>
                No agent session is registered. Ask an agent to register and
                poll its inbox.
              </p>
            ) : null}
            {agent === undefined ? null : (
              <Field>
                <FieldLabel htmlFor="handoff-agent">Agent session</FieldLabel>
                <NativeSelect
                  id="handoff-agent"
                  value={agent.id}
                  onChange={(event) => setChosen(event.target.value)}
                >
                  {available.map((item) => (
                    <NativeSelectOption key={item.id} value={item.id}>
                      {item.name} ({item.id})
                    </NativeSelectOption>
                  ))}
                </NativeSelect>
                <FieldDescription>
                  Match the identity to the agent's registration. Registration
                  does not mean a model is currently running.
                </FieldDescription>
              </Field>
            )}
            <Field>
              <FieldLabel htmlFor="handoff-intent">Edit intent</FieldLabel>
              <Textarea
                id="handoff-intent"
                value={intent}
                onChange={(event) => setIntent(event.target.value)}
              />
              <FieldDescription>
                Enter an edit intent to continue. The selected source is data;
                your intent is sent separately.
              </FieldDescription>
            </Field>
          </>
        ) : (
          <>
            <p>
              Agent: {handoff.agent_name} ({handoff.agent_session_id})
            </p>
            <p>Intent: {handoff.intent}</p>
            <output>
              {handoff.state === 'staged'
                ? sendUncertain
                  ? 'Send is unconfirmed. Check status, retry this delivery, or cancel it.'
                  : 'Staged. Nothing has been sent to the agent.'
                : null}
              {handoff.state === 'queued'
                ? 'Sent to the inbox; waiting for the agent to receive it.'
                : null}
              {handoff.state === 'offered'
                ? 'The agent may have received it. It can no longer be recalled.'
                : null}
              {handoff.state === 'blocked'
                ? 'The saved source is unavailable. The agent may have received it; retries have stopped.'
                : null}
              {handoff.state === 'acknowledged'
                ? 'The agent acknowledged receipt. Model work is not confirmed.'
                : null}
              {handoff.state === 'cancelled'
                ? 'Cancelled before delivery.'
                : null}
              {handoff.state === 'expired'
                ? 'The workspace connection or agent session ended before delivery.'
                : null}
            </output>
          </>
        )}
        {error === undefined ? null : (
          <Alert variant="destructive">
            <AlertTitle>Handoff not confirmed</AlertTitle>
            <AlertDescription>{error}</AlertDescription>
          </Alert>
        )}
        <DialogFooter>
          {busy ? (
            <p>Processing handoff…</p>
          ) : handoff === undefined ? (
            <>
              <Button
                type="button"
                variant="outline"
                onClick={() => void cancel()}
              >
                Cancel
              </Button>
              {agent !== undefined && intent.trim() !== '' ? (
                <Button type="button" onClick={(event) => void stage(event)}>
                  Stage selection
                </Button>
              ) : null}
            </>
          ) : (
            <>
              <Button
                type="button"
                variant="outline"
                onClick={() => void refresh()}
              >
                Check status
              </Button>
              {pending ? (
                <Button
                  type="button"
                  variant="outline"
                  onClick={() => void cancel()}
                >
                  Cancel handoff
                </Button>
              ) : null}
              {handoff.state === 'staged' ? (
                <Button type="button" onClick={(event) => void send(event)}>
                  Send to agent
                </Button>
              ) : null}
              {!pending ? (
                <Button type="button" onClick={onClose}>
                  Close
                </Button>
              ) : null}
            </>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
