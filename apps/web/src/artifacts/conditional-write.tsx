import { useState } from 'react';
import type { ReactNode } from 'react';
import { ArtifreeApiError, newIdempotencyKey } from '@artifree/client';
import type { ErrorCode } from '@artifree/contracts';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { describeFailure, mayHaveCommitted } from './use-request';
import { raise, waitFor } from './write-gate';
import type { Floor, Wait } from './write-gate';

/**
 * What every conditional write against one kind of version shares: the
 * floor its conflicts proved, and whether one is in flight. The page's own
 * writes are learned as known by the workspace (0009 decision 6). Pin and
 * rename share the metadata gate; restore and save share the source gate
 * (decision 0011, item 6).
 */
export function useWriteGate(
  /** How often the server's data may have been replaced so far. */
  resets: number,
) {
  const [floor, setFloor] = useState<Floor>();
  const [sending, setSending] = useState(false);
  return {
    /**
     * Why a write prepared against `version` waits, given the newest
     * version a notice reported and when what is shown was read.
     */
    wait: (
      version: number,
      known: number | undefined,
      shownResets: number,
    ): Wait | undefined =>
      waitFor(version, { resets, shownResets, known, floor, sending }),
    /** Records the version a conflict proved to exist. */
    refuse: (version: number) =>
      setFloor((current) => raise(current, version, resets)),
    setSending,
  };
}

export type WriteGate = ReturnType<typeof useWriteGate>;

/**
 * The version a conflict refusing `base` proves the page must read before
 * the next write: the current one it names, or else the next one. None
 * when it names one at or below the base: the server's data is then behind
 * what the page read, no newer version can be awaited, and the read the
 * conflict causes shows what there is.
 */
export function refusedFloor(
  base: number,
  current: number | undefined,
): number | undefined {
  if (current === undefined) return base + 1;
  return current > base ? current : undefined;
}

/** What the page shows about the last write it sent. */
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
  readonly gate: WriteGate;
  /** Sends the request with its idempotency key. */
  readonly send: (request: Request, idempotencyKey: string) => Promise<Result>;
  /** The version the request was prepared against. */
  readonly base: (request: Request) => number;
  /** The error code of a stale base. */
  readonly conflict: ErrorCode;
  /** The current version a conflict names, if it names one. */
  readonly current: (error: ArtifreeApiError) => number | undefined;
  readonly onDone: (result: Result) => void;
  /** Called after a conflict; the view reads the latest. */
  readonly onConflict: () => void;
}

/**
 * Sends conditional writes of one kind (decisions 0009 and 0011). The
 * same request sent again reuses its key, so a retry after a lost answer
 * applies at most once; any other request gets a new key. A conflict
 * raises the gate's floor, so no write of the gate is prepared from what
 * was shown before until the page shows the version it proved.
 */
export function useConditionalWrite<Request, Result>(
  options: ConditionalWriteOptions<Request, Result>,
) {
  const [state, setState] = useState<WriteState>({ status: 'idle' });
  const [attempt, setAttempt] = useState<{ body: string; key: string }>();
  const { gate } = options;

  async function write(request: Request): Promise<WriteResult> {
    const body = JSON.stringify(request);
    const key = attempt?.body === body ? attempt.key : newIdempotencyKey();
    setAttempt({ body, key });
    const base = options.base(request);
    setState({ status: 'sending' });
    gate.setSending(true);
    try {
      const result = await options.send(request, key);
      setState({ status: 'idle' });
      options.onDone(result);
      return 'done';
    } catch (error: unknown) {
      if (
        error instanceof ArtifreeApiError &&
        error.code === options.conflict
      ) {
        const floor = refusedFloor(base, options.current(error));
        if (floor !== undefined) gate.refuse(floor);
        setState({ status: 'conflict' });
        options.onConflict();
        return 'conflict';
      }
      setState({
        status: 'failed',
        message: describeFailure(error),
        unknown: mayHaveCommitted(error),
        base,
      });
      return 'failed';
    } finally {
      gate.setSending(false);
    }
  }

  return {
    state,
    write,
    /** Hides the outcome; what the gate learned stays. */
    clear: () => setState({ status: 'idle' }),
  };
}

/**
 * Whether a request whose answer was lost may be sent again as it was.
 * That is a replay under its key: it returns the first result if that was
 * committed, and is refused without writing if its base is stale, so a
 * newer version the page learned of meanwhile, maybe that very write, does
 * not hold it (as for saves, 0007 decision 8). A reset still does.
 */
export function retryable(
  state: WriteState,
  base: number,
  wait: Wait | undefined,
): boolean {
  return (
    state.status === 'failed' &&
    state.unknown &&
    state.base === base &&
    (wait === undefined || wait === 'newer' || wait === 'refused')
  );
}

const READING =
  'If it cannot be read, the artifact says so and offers to try again.';

/**
 * Why a write is not sent, or what became of the last one. Nothing while
 * it is idle and may be sent, or while it is sending. A failure whose
 * answer was lost is an unknown outcome, never "not changed".
 */
export function WriteNotice({
  state,
  action,
  changed,
  behind,
  waiting,
}: {
  state: WriteState;
  /** The control that sends the write, as in "rename". */
  action: string;
  /** What changes elsewhere, as in "The title or pin". */
  changed: string;
  /**
   * Whether the page has learned that the prepared request is stale; a
   * request whose outcome is unknown is retried as it was regardless.
   */
  behind: boolean;
  /** Why the next request would wait, if it would. */
  waiting: Wait | undefined;
}): ReactNode {
  const reading = waiting === 'refused' || waiting === 'newer';
  if (state.status === 'conflict')
    return (
      <Alert>
        <AlertTitle>Changed elsewhere</AlertTitle>
        <AlertDescription>
          {changed} changed since this page read it, so nothing was changed.{' '}
          {reading
            ? `The page is reading the latest; you can ${action} again once it has. ${READING}`
            : `The page shows the latest now; ${action} again if you still want to.`}
        </AlertDescription>
      </Alert>
    );
  if (state.status === 'failed')
    return state.unknown ? (
      <Alert variant="destructive">
        <AlertTitle>Outcome unknown</AlertTitle>
        <AlertDescription>
          {state.message} The change may have been made. To retry, {action}{' '}
          again: the same request is sent and applies at most once.
        </AlertDescription>
      </Alert>
    ) : (
      <Alert variant="destructive">
        <AlertTitle>Not changed</AlertTitle>
        <AlertDescription>{state.message}</AlertDescription>
      </Alert>
    );
  if (behind || waiting === 'refused')
    return (
      <Alert>
        <AlertTitle>Changed elsewhere</AlertTitle>
        <AlertDescription>
          {changed} changed since this was prepared.{' '}
          {reading
            ? `The page is reading the latest; you can ${action} once it has. ${READING}`
            : `The page shows the latest now; ${action} if you still want to.`}
        </AlertDescription>
      </Alert>
    );
  if (waiting === 'sending' && state.status !== 'sending')
    return (
      <output className="text-sm text-muted-foreground">
        Another change to this artifact is being sent; you can {action} once it
        is answered.
      </output>
    );
  if (waiting === 'newer')
    return (
      <output className="text-sm text-muted-foreground">
        The page is reading the latest version; you can {action} once it has.
      </output>
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
export function ReplacedNotice({
  state,
}: {
  /** The dialog's last write, which may have been sent before. */
  state: WriteState;
}): ReactNode {
  return (
    <Alert variant="destructive">
      <AlertTitle>The server&apos;s data may have been replaced</AlertTitle>
      <AlertDescription>
        {state.status === 'failed' && state.unknown
          ? `${state.message} The change may have been made before then. `
          : null}
        This was prepared from what the page read before, so it is not sent
        again. Close it and start again from what the page shows now.
      </AlertDescription>
    </Alert>
  );
}
