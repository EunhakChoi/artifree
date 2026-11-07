import { OutboxError } from './types.js';

/**
 * Internal local-operation boundary shared by admission and cleanup publication.
 * The host must route every durable admission through this same instance.
 * It grants neither an owner lock nor authentication/delivery authority.
 */
export class LocalOperationGate {
  private tail: Promise<void> = Promise.resolve();
  private closed = false;
  private readonly stop = new AbortController();
  /** Opt-in native operations use this signal; queued callbacks are fenced too. */
  get signal(): AbortSignal {
    return this.stop.signal;
  }

  /** Fence queued/new callbacks and signal active work; its terminal event decides the result. */
  close(): void {
    this.closed = true;
    this.stop.abort();
  }

  run<T>(action: () => Promise<T>): Promise<T> {
    if (this.closed) return Promise.reject(new OutboxError('owner_not_ready'));
    const result = this.tail.then(() => {
      if (this.closed) throw new OutboxError('owner_not_ready');
      return action();
    });
    this.tail = result.then(
      () => undefined,
      () => undefined,
    );
    return result;
  }
}
