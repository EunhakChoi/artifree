import { ArtifreeApiError } from './errors.js';

/**
 * Time for the transport, injectable so that tests can run a minute of
 * silence without waiting. The production clock is the system's.
 */
export interface Clock {
  /**
   * Runs `callback` once after `milliseconds`, or earlier, even at once,
   * before this returns. The returned function cancels it and may be called
   * any number of times.
   */
  readonly after: (callback: () => void, milliseconds: number) => () => void;
}

/** The system clock. */
export const systemClock: Clock = {
  after(callback, milliseconds) {
    const timer = setTimeout(callback, milliseconds);
    return () => clearTimeout(timer);
  },
};

/** Without a body byte for this long after the headers, a body has stalled. */
export const STALL_MS = 45_000;
/** A connection must stay open this long for the retry count to reset. */
export const RETRY_RESET_MS = 30_000;
/** The longest wait a `Retry-After` can ask for. */
export const RETRY_AFTER_CAP_SECONDS = 3600;
/** Bytes of a success body that a bounded read accepts: 1 MiB + 16 KiB. */
export const SUCCESS_BODY_LIMIT = 1_064_960;
/** Bytes of an error body that a bounded read accepts. */
export const ERROR_BODY_LIMIT = 65_536;

/**
 * Starts a timer that calls `onFire` at most once. Unlike a bare
 * {@link Clock.after}, the callback may run before this returns, and the
 * returned function then cancels nothing that matters.
 */
export function startTimer(
  clock: Clock,
  milliseconds: number,
  onFire: () => void,
): () => void {
  let live = true;
  const cancel = clock.after(() => {
    if (!live) return;
    live = false;
    onFire();
  }, milliseconds);
  return () => {
    if (!live) return;
    live = false;
    cancel();
  };
}

/**
 * Waits `milliseconds` on `clock`, or less when `signal` aborts or has
 * aborted. Settles exactly once, however the clock calls back.
 */
export function wait(
  clock: Clock,
  milliseconds: number,
  signal: AbortSignal,
): Promise<void> {
  return new Promise((resolve) => {
    if (signal.aborted) {
      resolve();
      return;
    }
    // A clock that calls back at once runs `done` before `cancel` is set.
    let cancel: (() => void) | undefined;
    const done = (): void => {
      cancel?.();
      signal.removeEventListener('abort', done);
      resolve();
    };
    signal.addEventListener('abort', done);
    cancel = startTimer(clock, milliseconds, done);
  });
}

/**
 * Starts cancelling a stream without waiting for it. A source whose
 * `cancel()` never settles or throws must not hold back any result, so no
 * caller awaits this.
 */
export function cancelQuietly(target: {
  readonly cancel: () => Promise<void>;
}): void {
  try {
    target.cancel().catch(() => undefined);
  } catch {
    // A cancel that throws at once has nothing left to cancel.
  }
}

/**
 * The delay before reconnecting. `attempt` counts retries since the last
 * reset; `base` doubles from `min` up to `max`, and the delay is `base`
 * scaled by 0.5 to just under 1 with `random()`, so reconnecting clients
 * spread out. A server's `Retry-After` is a floor, at most
 * {@link RETRY_AFTER_CAP_SECONDS}.
 */
export function reconnectDelayMs(parameters: {
  readonly min: number;
  readonly max: number;
  readonly attempt: number;
  readonly random: () => number;
  readonly retryAfterSeconds: number | undefined;
}): number {
  const { min, max, attempt, random, retryAfterSeconds } = parameters;
  // Past 2^60 the product no longer changes the minimum, and min * Infinity
  // would be NaN for a minimum of 0.
  const base = Math.min(max, min * 2 ** Math.min(attempt, 60));
  const sample = random();
  const u = Number.isFinite(sample) ? Math.min(Math.max(sample, 0), 1) : 0;
  const delay = base * (0.5 + 0.5 * u);
  if (retryAfterSeconds === undefined) return delay;
  return Math.max(
    Math.min(retryAfterSeconds, RETRY_AFTER_CAP_SECONDS) * 1000,
    delay,
  );
}

/** How a failed body read is reported. */
export interface FailureContext {
  /** The HTTP status of the response whose body failed. */
  readonly status: number;
  /** What was being read, for messages: `change feed`. */
  readonly what: string;
  /** Whether to attach the original error as `cause`. */
  readonly keepCause: boolean;
}

/** A body that failed after its headers arrived: always a network failure. */
export function bodyFailure(
  context: FailureContext,
  message: string,
  cause?: unknown,
): ArtifreeApiError {
  return new ArtifreeApiError(message, {
    status: context.status,
    code: 'network_error',
    ...(context.keepCause && cause !== undefined ? { cause } : {}),
  });
}

/** Options of a {@link BodyReader}. */
export interface BodyReaderOptions extends FailureContext {
  readonly clock: Clock;
  /** Ends the read at once; no timer or listener outlives it. */
  readonly signal: AbortSignal | undefined;
}

/** What {@link BodyReader.read} resolves with: a chunk, or why there is none. */
export type BodyRead = Uint8Array | 'end' | 'aborted';

type BodyState = 'open' | 'end' | 'aborted' | 'closed' | 'failed';

/**
 * Reads a response body whose headers have arrived, as the server's
 * silence allows.
 *
 * - The stall timer starts when this is created, which is when the headers
 *   arrived, and runs until the first byte; it runs again only while a
 *   `read()` is pending, so a consumer that holds a chunk is not a stall.
 *   After {@link STALL_MS} the body is cancelled and `read()` rejects with a
 *   network failure. An empty chunk is not a byte.
 * - Aborting `signal` settles a pending `read()` with `'aborted'`.
 * - Cancelling is started and never awaited.
 * - One `read()` at a time.
 *
 * Nothing but the timer and one abort listener is held, and
 * {@link BodyReader.close} releases both. Without a `read()` they are
 * released when the stall limit passes.
 */
export class BodyReader {
  private readonly options: BodyReaderOptions;
  private reader: ReadableStreamDefaultReader<Uint8Array> | undefined;
  private state: BodyState = 'open';
  private failure: ArtifreeApiError | undefined;
  private stopTimer: (() => void) | undefined;
  private waiter:
    | {
        readonly resolve: (read: BodyRead) => void;
        readonly reject: (error: ArtifreeApiError) => void;
      }
    | undefined;
  private readonly onAbort = (): void => this.stop('aborted');

  constructor(body: ReadableStream<Uint8Array>, options: BodyReaderOptions) {
    this.options = options;
    try {
      this.reader = body.getReader();
    } catch (error: unknown) {
      // A body that is locked or already used cannot be read.
      this.failure = bodyFailure(
        options,
        `The ${options.what} could not be read.`,
        error,
      );
      this.state = 'failed';
      cancelQuietly(body);
      return;
    }
    if (options.signal?.aborted === true) {
      this.stop('aborted');
      return;
    }
    options.signal?.addEventListener('abort', this.onAbort);
    this.arm();
  }

  /**
   * The next non-empty chunk, `'end'` when the body ended, or `'aborted'`
   * when the signal aborted or {@link BodyReader.close} was called. Rejects
   * with a network failure when the connection is lost or stalls.
   */
  read(): Promise<BodyRead> {
    switch (this.state) {
      case 'end':
        return Promise.resolve('end');
      case 'aborted':
      case 'closed':
        return Promise.resolve('aborted');
      case 'failed':
        return Promise.reject(this.lost());
      case 'open':
        break;
      default:
        return assertNever(this.state);
    }
    this.arm();
    return new Promise((resolve, reject) => {
      this.waiter = { resolve, reject };
      this.pull();
    });
  }

  /** Releases the timer and listener and starts cancelling the body. */
  close(): void {
    this.stop('closed');
  }

  private pull(): void {
    const reader = this.reader;
    if (reader === undefined) return;
    let pending: Promise<ReadableStreamReadResult<Uint8Array>>;
    try {
      pending = reader.read();
    } catch (error: unknown) {
      this.fail(error);
      return;
    }
    pending.then(
      (result) => {
        // Late answers of a body that is already over are ignored.
        if (this.state !== 'open') return;
        if (result.done) {
          this.stop('end');
        } else if (result.value.byteLength === 0) {
          // Not a byte: the timer keeps its deadline.
          this.pull();
        } else {
          this.disarm();
          const waiter = this.waiter;
          this.waiter = undefined;
          waiter?.resolve(result.value);
        }
      },
      (error: unknown) => this.fail(error),
    );
  }

  private lost(): ArtifreeApiError {
    return (
      this.failure ??
      bodyFailure(this.options, `The ${this.options.what} could not be read.`)
    );
  }

  private fail(error: unknown): void {
    if (this.state !== 'open') return;
    this.failure = bodyFailure(
      this.options,
      `The connection ended while reading the ${this.options.what}.`,
      error,
    );
    this.stop('failed');
  }

  private arm(): void {
    if (this.state !== 'open' || this.stopTimer !== undefined) return;
    this.stopTimer = startTimer(this.options.clock, STALL_MS, () => {
      this.failure = bodyFailure(
        this.options,
        `The connection sent nothing for ${STALL_MS / 1000} seconds while reading the ${this.options.what}.`,
      );
      this.stop('failed');
    });
  }

  private disarm(): void {
    this.stopTimer?.();
    this.stopTimer = undefined;
  }

  private stop(state: Exclude<BodyState, 'open'>): void {
    if (this.state !== 'open') return;
    this.state = state;
    this.disarm();
    this.options.signal?.removeEventListener('abort', this.onAbort);
    if (state !== 'end' && this.reader !== undefined)
      cancelQuietly(this.reader);
    const waiter = this.waiter;
    this.waiter = undefined;
    if (waiter === undefined) return;
    if (state === 'end') waiter.resolve('end');
    else if (state === 'failed') waiter.reject(this.lost());
    else waiter.resolve('aborted');
  }
}

function assertNever(value: never): never {
  throw new Error(`Unexpected body state ${String(value)}.`);
}

/** The text of a streamed body, until it ends, fails, or aborts. */
export async function* decodeText(reader: BodyReader): AsyncGenerator<string> {
  // Removes a leading byte order mark, as event streams require.
  const decoder = new TextDecoder();
  try {
    for (;;) {
      const chunk = await reader.read();
      if (chunk === 'aborted') return;
      if (chunk === 'end') {
        yield decoder.decode();
        return;
      }
      yield decoder.decode(chunk, { stream: true });
    }
  } finally {
    reader.close();
  }
}

/** The bytes of a bounded read, or why there are none. */
type BoundedRead = Uint8Array | 'over' | ArtifreeApiError;

/**
 * The whole body, if it is at most `limit` bytes: the limit is checked as
 * each chunk arrives, and a body past it is cancelled at once, not read
 * on. A lost or stalled connection is returned as its network failure; an
 * abort throws `signal.reason`, since the caller asked for it.
 */
async function readBounded(
  body: ReadableStream<Uint8Array>,
  limit: number,
  options: BodyReaderOptions,
): Promise<BoundedRead> {
  const reader = new BodyReader(body, options);
  const chunks: Uint8Array[] = [];
  let total = 0;
  try {
    for (;;) {
      let chunk: BodyRead;
      try {
        chunk = await reader.read();
      } catch (error: unknown) {
        if (error instanceof ArtifreeApiError) return error;
        throw error;
      }
      if (chunk === 'end') break;
      if (chunk === 'aborted') {
        options.signal?.throwIfAborted();
        // Closed by someone else: nothing more will come.
        return bodyFailure(options, `The ${options.what} could not be read.`);
      }
      total += chunk.byteLength;
      if (total > limit) return 'over';
      chunks.push(chunk);
    }
  } finally {
    reader.close();
  }
  const bytes = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return bytes;
}

function parseJson(bytes: Uint8Array): unknown {
  try {
    const value: unknown = JSON.parse(new TextDecoder().decode(bytes));
    return value;
  } catch {
    // A body that is not JSON fails the caller's contract check instead.
    return undefined;
  }
}

/**
 * The JSON of an error response's body, read within
 * {@link ERROR_BODY_LIMIT} bytes. A body past the limit, one that stalls
 * or is lost after the headers arrived, and one that is not JSON are all
 * `undefined`: the response has no API body and its status decides, as for
 * a proxy's page. Aborting `options.signal` rejects with its reason.
 */
export async function readErrorJson(
  response: Response,
  options: BodyReaderOptions,
): Promise<unknown> {
  if (response.body === null) return undefined;
  const bytes = await readBounded(response.body, ERROR_BODY_LIMIT, options);
  return bytes instanceof Uint8Array ? parseJson(bytes) : undefined;
}

/**
 * The JSON of a success response's body, read within
 * {@link SUCCESS_BODY_LIMIT} bytes. A body past the limit rejects with
 * `invalid_response`; one that is not JSON is `undefined`, for the
 * caller's contract check. A lost or stalled body rejects with a network
 * failure; aborting `options.signal` rejects with its reason.
 */
export async function readSuccessJson(
  response: Response,
  options: BodyReaderOptions,
): Promise<unknown> {
  if (response.body === null) return undefined;
  const bytes = await readBounded(response.body, SUCCESS_BODY_LIMIT, options);
  if (bytes instanceof ArtifreeApiError) throw bytes;
  if (bytes === 'over')
    throw new ArtifreeApiError(
      `The ${options.what} is larger than ${SUCCESS_BODY_LIMIT} bytes.`,
      { status: response.status, code: 'invalid_response' },
    );
  return parseJson(bytes);
}
