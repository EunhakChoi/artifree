import type { AppData } from '@artifree/contracts';
import type { ChangeFeedEvent } from './change-feed.js';
import { ArtifreeApiError } from './errors.js';
import {
  type Clock,
  RETRY_RESET_MS,
  reconnectDelayMs,
  startTimer,
  wait,
} from './transport.js';

/** A data snapshot, a newer value, or an artifact that is no longer available. */
export type DataEvent =
  | { readonly type: 'snapshot' | 'update'; readonly data: AppData }
  | { readonly type: 'unavailable' };

/** Options for following one path of an artifact's application data. */
export interface FollowDataOptions {
  readonly path?: string;
  readonly signal: AbortSignal;
  readonly onData: (event: DataEvent) => void;
  readonly retryDelayMs?: { readonly min: number; readonly max: number };
  readonly random?: () => number;
}

/** The authenticated transports supplied by the client. */
export interface FollowDataTransport {
  readonly clock: Clock;
  readonly open: (
    signal: AbortSignal,
  ) => Promise<AsyncGenerator<ChangeFeedEvent>>;
  readonly read: (signal: AbortSignal) => Promise<AppData>;
}

type StreamResult =
  | {
      readonly type: 'stream';
      readonly result: IteratorResult<ChangeFeedEvent>;
    }
  | { readonly type: 'stream-error'; readonly error: unknown };
type ReadResult =
  | { readonly type: 'read'; readonly id: number; readonly value: AppData }
  | {
      readonly type: 'read-error';
      readonly id: number;
      readonly error: unknown;
    };
type End =
  | { readonly type: 'ended' }
  | {
      readonly type: 'retry';
      readonly error?: ArtifreeApiError;
      readonly lasted: boolean;
    };

function canRetry(error: unknown): error is ArtifreeApiError {
  return (
    error instanceof ArtifreeApiError &&
    (error.code === 'network_error' ||
      error.status === 429 ||
      error.status >= 500)
  );
}

function invalid(message: string): ArtifreeApiError {
  return new ArtifreeApiError(message, {
    status: 200,
    code: 'invalid_response',
  });
}

/** Follows the feed and re-reads a path without missing a committed change. */
export async function followData(
  transport: FollowDataTransport,
  artifactId: string,
  options: FollowDataOptions,
): Promise<void> {
  const { signal, onData } = options;
  const { min, max } = options.retryDelayMs ?? { min: 500, max: 30_000 };
  const random = options.random ?? Math.random;
  let seen: number | undefined;
  let attempt = 0;

  async function connection(): Promise<End> {
    const connectionAbort = new AbortController();
    let readAbort: AbortController | undefined;
    let iterator: AsyncGenerator<ChangeFeedEvent> | undefined;
    let stopTimer: (() => void) | undefined;
    let lasted = false;
    let timerStarted = false;
    let readId = 0;
    let named = 0;
    let asked = 0;
    let ready = false;
    let snapshot = false;
    // Each in-flight operation gets one handler. Repeated races against a
    // long-pending read or abort would retain a handler for every event.
    let reading = false;
    let readResult: ReadResult | undefined;
    let streamResult: StreamResult | undefined;
    let waiter: (() => void) | undefined;
    let active = true;
    function wake(): void {
      const notify = waiter;
      waiter = undefined;
      notify?.();
    }
    let markAborted = (): void => {};
    const aborted = new Promise<{ readonly type: 'aborted' }>((resolve) => {
      markAborted = () => resolve({ type: 'aborted' });
    });
    const abort = (): void => {
      connectionAbort.abort(signal.reason);
      markAborted();
      wake();
    };
    signal.addEventListener('abort', abort, { once: true });

    function abandon(): void {
      readId += 1;
      readAbort?.abort();
      readAbort = undefined;
      reading = false;
      readResult = undefined;
    }
    function read(isSnapshot: boolean): void {
      abandon();
      snapshot = isSnapshot;
      asked = named;
      const id = readId;
      readAbort = new AbortController();
      reading = true;
      void transport.read(readAbort.signal).then(
        (value): void => {
          if (!active || id !== readId) return;
          readResult = { type: 'read', id, value };
          wake();
        },
        (error: unknown): void => {
          if (!active || id !== readId) return;
          readResult = { type: 'read-error', id, error };
          wake();
        },
      );
    }
    function unavailable(): End {
      abandon();
      if (!signal.aborted) onData({ type: 'unavailable' });
      return { type: 'ended' };
    }

    try {
      if (signal.aborted) return { type: 'ended' };
      const opening = transport.open(connectionAbort.signal).then(
        (
          value,
        ): {
          readonly type: 'opened';
          readonly value: AsyncGenerator<ChangeFeedEvent>;
        } => ({ type: 'opened', value }),
        (
          error: unknown,
        ): { readonly type: 'open-error'; readonly error: unknown } => ({
          type: 'open-error',
          error,
        }),
      );
      const initial = await Promise.race([opening, aborted]);
      if (initial.type === 'aborted') {
        void opening.then((late) => {
          if (late.type === 'opened')
            void late.value.return(undefined).catch(() => undefined);
        });
        return { type: 'ended' };
      }
      if (initial.type === 'open-error') {
        if (canRetry(initial.error))
          return { type: 'retry', error: initial.error, lasted };
        throw initial.error;
      }
      iterator = initial.value;
      const opened = iterator;
      function next(): void {
        void opened.next().then(
          (result): void => {
            if (!active) return;
            streamResult = { type: 'stream', result };
            wake();
          },
          (error: unknown): void => {
            if (!active) return;
            streamResult = { type: 'stream-error', error };
            wake();
          },
        );
      }
      next();
      while (!signal.aborted) {
        while (
          !signal.aborted &&
          streamResult === undefined &&
          readResult === undefined
        )
          await new Promise<void>((resolve) => {
            waiter = resolve;
          });
        if (signal.aborted) return { type: 'ended' };
        const item = streamResult ?? readResult;
        if (item === undefined) continue;
        if (streamResult !== undefined) streamResult = undefined;
        else readResult = undefined;
        if (item.type === 'stream-error') {
          if (canRetry(item.error))
            return { type: 'retry', error: item.error, lasted };
          throw item.error;
        }
        if (item.type === 'stream') {
          if (item.result.done) return { type: 'retry', lasted };
          const event = item.result.value;
          next();
          if (event.type !== 'change') {
            ready = true;
            named = 0;
            read(true);
          } else {
            if (!ready) throw invalid('A change arrived before ready.');
            if (event.change.artifact_id !== artifactId) continue;
            if (
              event.change.change === 'deleted' ||
              event.change.change === 'purged'
            )
              return unavailable();
            if (
              event.change.change === 'data_changed' &&
              event.change.data_version !== undefined
            ) {
              named = Math.max(named, event.change.data_version);
              if (!reading && seen !== undefined && named > seen) read(false);
            }
          }
          continue;
        }
        if (item.id !== readId) continue;
        reading = false;
        readAbort = undefined;
        if (item.type === 'read-error') {
          if (
            item.error instanceof ArtifreeApiError &&
            item.error.status === 404
          )
            return unavailable();
          if (canRetry(item.error))
            return { type: 'retry', error: item.error, lasted };
          throw item.error;
        }
        const version = item.value.data_version;
        if (version < asked)
          throw invalid('A data read missed a previously observed change.');
        if (!timerStarted) {
          timerStarted = true;
          stopTimer = startTimer(transport.clock, RETRY_RESET_MS, () => {
            lasted = true;
          });
        }
        const kind =
          snapshot || seen === undefined || version < seen
            ? 'snapshot'
            : version > seen
              ? 'update'
              : undefined;
        seen = version;
        if (kind !== undefined) onData({ type: kind, data: item.value });
        if (signal.aborted) return { type: 'ended' };
        if (named > version) read(false);
      }
      return { type: 'ended' };
    } catch (error: unknown) {
      if (signal.aborted) return { type: 'ended' };
      throw error;
    } finally {
      active = false;
      waiter = undefined;
      signal.removeEventListener('abort', abort);
      abandon();
      stopTimer?.();
      connectionAbort.abort();
      if (iterator !== undefined)
        void iterator.return(undefined).catch(() => undefined);
    }
  }

  while (!signal.aborted) {
    const outcome = await connection();
    if (outcome.type === 'ended' || signal.aborted) return;
    if (outcome.lasted) attempt = 0;
    const delay = reconnectDelayMs({
      min,
      max,
      attempt,
      random,
      retryAfterSeconds: outcome.error?.retryAfterSeconds,
    });
    attempt += 1;
    await wait(transport.clock, delay, signal);
  }
}
