import { useState } from 'react';
import type { ReactNode } from 'react';
import { ArtifreeApiError, newIdempotencyKey } from '@artifree/client';
import type { ErrorCode } from '@artifree/contracts';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { describeFailure, mayHaveCommitted } from './use-request';

/** The state of the last conditional write a view sent. */
export type WriteState =
  | { readonly status: 'idle' }
  | { readonly status: 'sending' }
  | { readonly status: 'conflict' }
  | {
      readonly status: 'failed';
      readonly message: string;
      /** The answer was lost: the write may have been made. */
      readonly unknown: boolean;
      /** The revision the write was prepared against. */
      readonly base: number;
    };

interface RetryableWriteOptions<Request, Result> {
  /** Sends the request with its idempotency key. */
  readonly send: (request: Request, idempotencyKey: string) => Promise<Result>;
  /** The error code of a stale base. */
  readonly conflict: ErrorCode;
  /** The revision the request was prepared against. */
  readonly base: (request: Request) => number;
  readonly onDone: (result: Result) => void;
  /** Called when another write won; the view reads the latest. */
  readonly onConflict: () => void;
}

/**
 * Sends conditional writes of one kind (decisions 0008, 0010). Sending the
 * same request again, base included, reuses its key, so a retry after a
 * lost answer cannot apply twice; any other request gets a new key.
 */
export function useRetryableWrite<Request, Result>(
  options: RetryableWriteOptions<Request, Result>,
) {
  const [state, setState] = useState<WriteState>({ status: 'idle' });
  const [attempt, setAttempt] = useState<{ body: string; key: string }>();
  async function write(request: Request): Promise<boolean> {
    const body = JSON.stringify(request);
    const key = attempt?.body === body ? attempt.key : newIdempotencyKey();
    setAttempt({ body, key });
    setState({ status: 'sending' });
    try {
      const result = await options.send(request, key);
      setState({ status: 'idle' });
      options.onDone(result);
      return true;
    } catch (error: unknown) {
      if (
        error instanceof ArtifreeApiError &&
        error.code === options.conflict
      ) {
        setState({ status: 'conflict' });
        options.onConflict();
      } else
        setState({
          status: 'failed',
          message: describeFailure(error),
          unknown: mayHaveCommitted(error),
          base: options.base(request),
        });
      return false;
    }
  }
  return { state, write, clear: () => setState({ status: 'idle' }) };
}

/**
 * What became of the last write: nothing while it is idle or sending. A
 * failure whose answer was lost is an unknown outcome, never "not changed".
 */
export function WriteOutcome({
  state,
  action,
  conflict,
}: {
  state: WriteState;
  /** The control that sends the write again, as in "rename again". */
  action: string;
  /** What changed elsewhere, as in "The title or pin changed". */
  conflict: string;
}): ReactNode {
  if (state.status === 'conflict')
    return (
      <Alert>
        <AlertTitle>Changed elsewhere</AlertTitle>
        <AlertDescription>
          {conflict} since this page read it, so nothing was changed. The page
          reads the latest; {action} again if you still want to.
        </AlertDescription>
      </Alert>
    );
  if (state.status === 'failed' && state.unknown)
    return (
      <Alert variant="destructive">
        <AlertTitle>Outcome unknown</AlertTitle>
        <AlertDescription>
          {state.message} The change may have been made. To retry, {action}{' '}
          again: the same request is sent and applies at most once.
        </AlertDescription>
      </Alert>
    );
  if (state.status === 'failed')
    return (
      <Alert variant="destructive">
        <AlertTitle>Not changed</AlertTitle>
        <AlertDescription>{state.message}</AlertDescription>
      </Alert>
    );
  return null;
}
