import { useState } from 'react';
import type { ReactNode } from 'react';
import { ArtifreeApiError, newIdempotencyKey } from '@artifree/client';
import type { ErrorCode } from '@artifree/contracts';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { readPast } from './refusal';
import type { Refusal } from './refusal';
import { describeFailure, mayHaveCommitted } from './use-request';

/** What the page shows about the last conditional write it sent. */
export type WriteState =
  | { readonly status: 'idle' }
  | { readonly status: 'sending' }
  | { readonly status: 'conflict' }
  | {
      readonly status: 'failed';
      readonly message: string;
      /** The answer was lost: the write may have been made. */
      readonly unknown: boolean;
      /** The version the write was prepared against. */
      readonly base: number;
    };

/** How a write ended. */
export type WriteResult = 'done' | 'conflict' | 'failed';

interface ConditionalWriteOptions<Request, Result> {
  /** Sends the request with its idempotency key. */
  readonly send: (request: Request, idempotencyKey: string) => Promise<Result>;
  /** The version the request was prepared against. */
  readonly base: (request: Request) => number;
  /** The error code of a stale base. */
  readonly conflict: ErrorCode;
  /** The current version a conflict names. */
  readonly current: (error: ArtifreeApiError) => number | undefined;
  readonly onDone: (result: Result) => void;
  /** Called after a conflict; the view reads the latest. */
  readonly onConflict: () => void;
  /** How often the server's data may have been replaced so far. */
  readonly resets: number;
}

/**
 * Sends conditional writes of one kind: renames, pins, or restores
 * (decisions 0009 and 0011). Sending the same request again reuses its
 * key, so a retry after a lost answer applies at most once; any other
 * request gets a new key. After a conflict, the refused base is kept until
 * the page has read past it (see {@link readPast}), so a retry can never
 * resend it while the latest read is outstanding or failed.
 */
export function useConditionalWrite<Request, Result>(
  options: ConditionalWriteOptions<Request, Result>,
) {
  const [state, setState] = useState<WriteState>({ status: 'idle' });
  const [attempt, setAttempt] = useState<{ body: string; key: string }>();
  const [refused, setRefused] = useState<Refusal>();

  /** Whether the page, showing `version`, has read past any conflict. */
  function caughtUp(version: number): boolean {
    return readPast(refused, version, options.resets);
  }

  /** Whether a request prepared against `version` may be sent now. */
  function ready(version: number): boolean {
    return state.status !== 'sending' && caughtUp(version);
  }

  async function write(request: Request): Promise<WriteResult> {
    const body = JSON.stringify(request);
    const key = attempt?.body === body ? attempt.key : newIdempotencyKey();
    setAttempt({ body, key });
    setState({ status: 'sending' });
    try {
      const result = await options.send(request, key);
      setRefused(undefined);
      setState({ status: 'idle' });
      options.onDone(result);
      return 'done';
    } catch (error: unknown) {
      if (
        error instanceof ArtifreeApiError &&
        error.code === options.conflict
      ) {
        setRefused({
          base: options.base(request),
          current: options.current(error),
          resets: options.resets,
        });
        setState({ status: 'conflict' });
        options.onConflict();
        return 'conflict';
      }
      setState({
        status: 'failed',
        message: describeFailure(error),
        unknown: mayHaveCommitted(error),
        base: options.base(request),
      });
      return 'failed';
    }
  }

  return {
    state,
    caughtUp,
    ready,
    write,
    /** Hides the outcome; a refused base stays refused. */
    clear: () => setState({ status: 'idle' }),
  };
}

/**
 * What became of the last write: nothing while it is idle or sending. A
 * failure whose answer was lost is an unknown outcome, never "not changed".
 */
export function WriteOutcome({
  state,
  action,
  changed,
  caughtUp,
}: {
  state: WriteState;
  /** The control that sends the write, as in "rename". */
  action: string;
  /** What changed elsewhere, as in "The title or pin changed". */
  changed: string;
  /** Whether the page has read past a conflict's refused base. */
  caughtUp: boolean;
}): ReactNode {
  // A refusal that still holds is explained even after its notice was
  // cleared, so a disabled control always says why.
  if (state.status === 'conflict' || (state.status === 'idle' && !caughtUp))
    return (
      <Alert>
        <AlertTitle>Changed elsewhere</AlertTitle>
        <AlertDescription>
          {changed} since this page read it, so nothing was changed.{' '}
          {caughtUp
            ? `The page shows the latest now; ${action} again if you still want to.`
            : `The page is reading the latest; you can ${action} again once it has. If it cannot be read, close this and try again from the page.`}
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

/**
 * Said by a dialog opened before the server's data may have been replaced
 * (a `reset`, or a `ready` after the page read without a stream; decision
 * 0007, item 8). Its request names versions as the page read them then, and
 * replaced data can hold other content under the same numbers, so it sends
 * nothing.
 */
export function ReplacedNotice(): ReactNode {
  return (
    <Alert variant="destructive">
      <AlertTitle>The server&apos;s data may have been replaced</AlertTitle>
      <AlertDescription>
        This was prepared from what the page read before, so it is not sent.
        Close it and start again from what the page shows now.
      </AlertDescription>
    </Alert>
  );
}
