/**
 * The reads of one artifact's head build status for the preview's build
 * choice (decision 0058, rule 16, R03eb). Plain code without React, so its
 * timing is tested on its own.
 *
 * A read in flight is never abandoned for a later change: the change owes one
 * more read, made when the first one answers, and every answer is shown. So a
 * stream of changes, such as a running verification's, cannot keep the status
 * from being read, and the last answer is always of a read begun after the last
 * change. A `profile_unknown` answer is not read again until a reset, and a
 * reset drops what was read, since it may be another database's.
 */
import type { BuildStatus } from '@artifree/contracts';
import { STATUS_UNREAD, statusAfterFailure } from './build-status';
import type { BuildStatusState } from './build-status';

/** The one read the reader makes. */
export type ReadStatus = (signal: AbortSignal) => Promise<BuildStatus>;

/** The head build status of one artifact, read when owed. */
export class BuildStatusReader {
  private readonly read: ReadStatus;
  private state: BuildStatusState = STATUS_UNREAD;
  private readonly listeners = new Set<() => void>();
  private inFlight: AbortController | undefined;
  private again = false;

  constructor(read: ReadStatus) {
    this.read = read;
  }

  /** Calls `listener` after each change; returns the function that removes it. */
  readonly subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  /** The state: the same object until it changes. */
  readonly snapshot = (): BuildStatusState => this.state;

  /**
   * Owes a read. A read in flight owes one more after it; a `profile_unknown`
   * answer owes none until `reset`.
   */
  owe(): void {
    if (this.state.kind === 'profile_unknown') return;
    if (this.inFlight !== undefined) {
      this.again = true;
      return;
    }
    this.start();
  }

  /**
   * Drops what was read and the read in flight, since the data may be another
   * database's, then reads again.
   */
  reset(): void {
    this.stop();
    this.set(STATUS_UNREAD);
    this.start();
  }

  /** Abandons the read in flight and any read owed after it. */
  stop(): void {
    this.inFlight?.abort();
    this.inFlight = undefined;
    this.again = false;
  }

  private start(): void {
    const controller = new AbortController();
    this.inFlight = controller;
    void this.run(controller).catch((error: unknown) => {
      console.error('A build status read failed unexpectedly.', error);
    });
  }

  private async run(controller: AbortController): Promise<void> {
    let next: BuildStatusState;
    try {
      next = { kind: 'read', status: await this.read(controller.signal) };
    } catch (error: unknown) {
      next = statusAfterFailure(error);
    }
    if (controller.signal.aborted || this.inFlight !== controller) return;
    this.inFlight = undefined;
    this.set(next);
    if (this.again) {
      this.again = false;
      this.owe();
    }
  }

  private set(state: BuildStatusState): void {
    this.state = state;
    for (const listener of this.listeners) {
      try {
        listener();
      } catch (error: unknown) {
        console.error('A build status listener failed.', error);
      }
    }
  }
}
