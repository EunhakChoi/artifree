import type { Job, JobState } from '@artifree/contracts';
import {
  type ChangeFeedEvent,
  type FollowOptions,
  type OpenStream,
  follow,
} from './change-feed.js';
import { ArtifreeApiError } from './errors.js';
import { type Clock, startTimer } from './transport.js';

/**
 * How often a job is read while the feed is not connected (decision 0055,
 * rule 11).
 */
export const JOB_POLL_MS = 15_000;

/** Options of {@link followJob} (decision 0055, rule 11). */
export interface FollowJobOptions {
  /** Ends the wait: {@link followJob} then rejects with the signal's reason. */
  readonly signal: AbortSignal;
  /** Reconnection delays of the feed, as {@link FollowOptions.retryDelayMs}. */
  readonly retryDelayMs?: { readonly min: number; readonly max: number };
  /** A number in [0, 1) for the jitter of each reconnection delay. */
  readonly random?: () => number;
}

/** The transports {@link followJob} needs (decision 0055, rule 11). */
export interface FollowJobTransport {
  /** Times the poll, the retry and the feed's reconnection waits. */
  readonly clock: Clock;
  /** Opens one feed stream, as {@link OpenStream} describes. */
  readonly open: OpenStream;
  /**
   * Reads the job once and aborts with `signal`. A failed request rejects
   * with an {@link ArtifreeApiError}; its status decides whether it is retried.
   */
  readonly read: (signal: AbortSignal) => Promise<Job>;
}

/** One timer slot: arming it replaces the timer that is already armed. */
interface SingleTimer {
  /** Cancels the armed timer, then runs `onFire` once after `milliseconds`. */
  readonly arm: (milliseconds: number, onFire: () => void) => void;
  /** Cancels the armed timer, if any. */
  readonly cancel: () => void;
}

/**
 * A {@link SingleTimer} on `clock`. The clock may run a callback before
 * `startTimer` returns, and the callback may arm again; the handle of the
 * earlier arming is then dropped, so the newer timer is never left uncancelled.
 */
function singleTimer(clock: Clock): SingleTimer {
  let cancelArmed: (() => void) | undefined;
  // Counts arms and cancels, so that an arm can see a newer one run inside it.
  let changes = 0;
  const cancel = (): void => {
    changes += 1;
    const cancelPrevious = cancelArmed;
    cancelArmed = undefined;
    cancelPrevious?.();
  };
  const arm = (milliseconds: number, onFire: () => void): void => {
    cancel();
    const armChanges = changes;
    const cancelNew = startTimer(clock, milliseconds, onFire);
    if (changes === armChanges) cancelArmed = cancelNew;
  };
  return { arm, cancel };
}

/**
 * Whether a job in `state` has ended, so that it will not change again
 * (decision 0055, rule 11).
 */
function ended(state: JobState): boolean {
  switch (state) {
    case 'succeeded':
    case 'failed':
    case 'cancelled':
      return true;
    case 'queued':
    case 'running':
    case 'retry_scheduled':
      return false;
    default: {
      // A state added to the contract must be classified above.
      const unclassified: never = state;
      throw new Error(`Job state ${String(unclassified)} is not classified`);
    }
  }
}

/**
 * Whether a failed read is worth another one: the connection failed, the
 * server asked for a wait, or the server or a proxy before it failed
 * (decision 0055, rule 11).
 */
function retryable(error: unknown): error is ArtifreeApiError {
  return (
    error instanceof ArtifreeApiError &&
    (error.code === 'network_error' ||
      error.status === 429 ||
      error.status >= 500)
  );
}

/**
 * Waits until job `jobId` has ended and resolves with it (decision 0055,
 * rule 11).
 *
 * The feed is opened before the job is read (decision 0006): the first read
 * waits for the feed's `ready`, so no change falls between the read and the
 * stream. The job is read again on each later `ready`, on `reset`, on a
 * `job_changed` for the job, on `recovered` for its artifact, at once on
 * `deleted` or `purged` for its artifact (so that an unreadable job ends the
 * wait), and at least every 15 seconds while the feed is not connected.
 *
 * It rejects with the not-found error when a read answers `404`, with any
 * other read failure that is not retryable, and with `options.signal.reason`
 * when the signal aborts. A retryable failure is read again after
 * {@link JOB_POLL_MS}, or at once when another read is asked for meanwhile.
 * Settling leaves no timer, listener or stream behind. A caller rejected
 * because the artifact was deleted may follow again after it is recovered.
 */
export function followJob(
  transport: FollowJobTransport,
  jobId: string,
  options: FollowJobOptions,
): Promise<Job> {
  return new Promise<Job>((resolve, reject) => {
    const { signal, retryDelayMs, random } = options;
    if (signal.aborted) {
      reject(signal.reason);
      return;
    }
    // Aborting it stops the feed and the reads, and removes the abort listener.
    const stop = new AbortController();
    const poll = singleTimer(transport.clock);
    const retry = singleTimer(transport.clock);
    let settled = false;
    // Whether a read is in flight, and whether another was asked for since.
    let inFlight = false;
    let again = false;
    let artifactId: string | undefined;

    const finish = (outcome: () => void): void => {
      if (settled) return;
      settled = true;
      poll.cancel();
      retry.cancel();
      stop.abort();
      outcome();
    };
    const rejectWith = (error: unknown): void => {
      finish(() => {
        reject(error);
      });
    };
    signal.addEventListener(
      'abort',
      () => {
        rejectWith(signal.reason);
      },
      { signal: stop.signal },
    );

    const readJob = (): void => {
      if (settled) return;
      if (inFlight) {
        again = true;
        return;
      }
      retry.cancel();
      inFlight = true;
      again = false;
      // A result that arrives after the wait settled changes nothing.
      void transport
        .read(stop.signal)
        .then(
          (job): void => {
            inFlight = false;
            if (settled) return;
            artifactId = job.artifact_id;
            if (ended(job.state)) {
              finish(() => {
                resolve(job);
              });
            } else if (again) {
              readJob();
            }
          },
          (error: unknown): void => {
            inFlight = false;
            if (settled) return;
            if (!retryable(error)) {
              rejectWith(error);
            } else if (again) {
              readJob();
            } else {
              retry.arm(JOB_POLL_MS, readJob);
            }
          },
        )
        .catch(rejectWith);
    };

    // Whether the feed is connected. The poll runs exactly while it is not.
    let connected = false;
    const armPoll = (): void => {
      if (settled) return;
      poll.arm(JOB_POLL_MS, () => {
        readJob();
        armPoll();
      });
    };

    const onEvent = (event: ChangeFeedEvent): void => {
      // `ready` starts the feed and `reset` means changes may have been missed:
      // both call for a read. Only `change` names an artifact and a revision.
      if (event.type !== 'change') {
        readJob();
        return;
      }
      const { change } = event;
      const ownJob = change.change === 'job_changed' && change.job_id === jobId;
      // Trash, purge and recovery of the job's artifact decide whether the job
      // can still be read.
      const artifactState =
        change.change === 'deleted' ||
        change.change === 'purged' ||
        change.change === 'recovered';
      const ownArtifact =
        artifactId === undefined || change.artifact_id === artifactId;
      if (ownJob || (artifactState && ownArtifact)) {
        readJob();
      }
    };

    const followOptions: FollowOptions = {
      signal: stop.signal,
      onEvent,
      onConnect: () => {
        connected = true;
        poll.cancel();
      },
      onDisconnect: () => {
        // A refused reconnect also reports a disconnect. Only the loss of a
        // connection starts the poll: arming it again on every refusal would
        // push the read back each time, and it would never happen while the
        // reconnects are less than 15 seconds apart.
        if (!connected) return;
        connected = false;
        armPoll();
      },
      ...(retryDelayMs === undefined ? {} : { retryDelayMs }),
      ...(random === undefined ? {} : { random }),
    };
    armPoll();
    void follow(transport.open, followOptions, transport.clock).catch(
      rejectWith,
    );
  });
}
