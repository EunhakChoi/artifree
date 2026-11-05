import { ArtifreeApiError } from './errors.js';

/**
 * A source of waits, so that timers can be replaced in tests. `after` runs
 * `run` once `milliseconds` have passed and returns what cancels the wait.
 */
export interface Clock {
  readonly after: (milliseconds: number, run: () => void) => () => void;
}

/** The platform's timers. */
export const systemClock: Clock = {
  after(milliseconds, run) {
    const timer = setTimeout(run, milliseconds);
    return () => {
      clearTimeout(timer);
    };
  },
};

/** Silence after a response's headers, or its previous byte, that fails it. */
export const STALL_MS = 45_000;

/** Largest JSON success body of a bounded read: 1 MiB and 16 KiB. */
export const MAX_SUCCESS_BODY_BYTES = 1_064_960;

/** Largest error body of a bounded read. */
export const MAX_ERROR_BODY_BYTES = 65_536;

/** How a failed or abandoned body read is worded and reported. */
export interface BodyContext {
  /** Cancels the read; an aborted read ends early. */
  readonly signal?: AbortSignal | undefined;
  readonly clock: Clock;
  /** Names the connection in messages, such as `The change feed connection`. */
  readonly subject: string;
  /** Whether a failure keeps the underlying error as `cause`. */
  readonly keepCause: boolean;
}

/** A body read by chunks, with a stall timer and an abort. */
export interface BodyWatch {
  /**
   * The next chunk. Resolves as done, at once, when the watch was aborted or
   * cancelled. Rejects with `network_error` when the connection failed or
   * sent no byte for {@link STALL_MS}.
   */
  readonly read: () => Promise<ReadableStreamReadResult<Uint8Array>>;
  /** Cancels the body, which closes its connection, and stops the timer. */
  readonly cancel: () => Promise<void>;
}

const DONE: ReadableStreamReadResult<Uint8Array> = {
  done: true,
  value: undefined,
};

/**
 * Reads `body` for the response whose headers have just arrived. If 45
 * seconds pass without a byte (after the headers or the previous byte), the
 * body is cancelled and the read fails as a network failure. The timer starts
 * here, not before the headers: a request that no response answers yet is
 * never cancelled by a timer (decision 0040 rule 18).
 */
export function watchBody(
  body: ReadableStream<Uint8Array>,
  { signal, clock, subject, keepCause }: BodyContext,
): BodyWatch {
  const reader = body.getReader();
  let stopTimer: (() => void) | undefined;
  let failure: ArtifreeApiError | undefined;
  let pending:
    | {
        readonly resolve: (
          result: ReadableStreamReadResult<Uint8Array>,
        ) => void;
        readonly reject: (error: ArtifreeApiError) => void;
      }
    | undefined;

  const lost = (message: string, cause: unknown): ArtifreeApiError =>
    new ArtifreeApiError(message, {
      status: 0,
      code: 'network_error',
      ...(keepCause ? { cause } : {}),
    });
  const stopWatching = (): void => {
    stopTimer?.();
    stopTimer = undefined;
    signal?.removeEventListener('abort', onAbort);
  };
  const cancel = async (): Promise<void> => {
    stopWatching();
    await reader.cancel().catch(() => undefined);
  };
  const onAbort = (): void => {
    void cancel();
    pending?.resolve(DONE);
  };
  const arm = (): void => {
    stopTimer?.();
    stopTimer = clock.after(STALL_MS, () => {
      stopTimer = undefined;
      failure = lost(
        `${subject} sent nothing for ${STALL_MS / 1000} seconds.`,
        undefined,
      );
      void cancel();
      pending?.reject(failure);
    });
  };

  signal?.addEventListener('abort', onAbort);
  arm();
  // The abort may precede the reader: nothing is read then.
  if (signal?.aborted === true) onAbort();

  const read = (): Promise<ReadableStreamReadResult<Uint8Array>> => {
    if (failure !== undefined) return Promise.reject(failure);
    if (signal?.aborted === true) {
      onAbort();
      return Promise.resolve(DONE);
    }
    return new Promise((resolve, reject) => {
      pending = { resolve, reject };
      reader.read().then(
        (result) => {
          pending = undefined;
          if (result.done) stopWatching();
          else if (failure === undefined && signal?.aborted !== true) arm();
          resolve(result);
        },
        (error: unknown) => {
          pending = undefined;
          stopWatching();
          reject(lost(`${subject} was lost.`, error));
        },
      );
    });
  };
  return { read, cancel };
}

/** The text of a body up to `limit` bytes, or `'too-large'` past it. */
async function readBounded(
  response: Response,
  limit: number,
  context: BodyContext,
): Promise<string | 'too-large'> {
  if (response.body === null) return '';
  const watch = watchBody(response.body, context);
  const chunks: Uint8Array[] = [];
  let size = 0;
  try {
    for (;;) {
      const chunk = await watch.read();
      if (chunk.done) break;
      size += chunk.value.length;
      // Cancelled at once, not after the rest has arrived.
      if (size > limit) return 'too-large';
      chunks.push(chunk.value);
    }
  } finally {
    await watch.cancel();
  }
  if (context.signal?.aborted === true)
    throw new ArtifreeApiError(`${context.subject} was lost.`, {
      status: 0,
      code: 'network_error',
    });
  const bytes = new Uint8Array(size);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.length;
  }
  return new TextDecoder().decode(bytes);
}

/** The JSON in `text`, or `undefined` when it is not JSON. */
function parseJson(text: string): unknown {
  try {
    const value: unknown = JSON.parse(text);
    return value;
  } catch {
    // A body that is not JSON fails the caller's contract check instead.
    return undefined;
  }
}

/**
 * The JSON of a success body, or `undefined` when the body is not JSON. The
 * body is read as it arrives and cancelled the moment it passes 1,064,960
 * bytes, which fails with `invalid_response`. A connection that fails or
 * stalls, and an abort, fail with `network_error`.
 */
export async function readBoundedJson(
  response: Response,
  context: BodyContext,
): Promise<unknown> {
  const text = await readBounded(response, MAX_SUCCESS_BODY_BYTES, context);
  if (text === 'too-large')
    throw new ArtifreeApiError(
      `${context.subject} sent a body of more than ${MAX_SUCCESS_BODY_BYTES} bytes.`,
      { status: response.status, code: 'invalid_response' },
    );
  return parseJson(text);
}

/**
 * The JSON of an error body, or `undefined` when the body is not JSON or
 * passes 65,536 bytes, which is then cancelled: such a response has no API
 * body, and its status decides, as for a proxy's page. A connection that
 * fails or stalls, and an abort, fail with `network_error`.
 */
export async function readBoundedErrorJson(
  response: Response,
  context: BodyContext,
): Promise<unknown> {
  const text = await readBounded(response, MAX_ERROR_BODY_BYTES, context);
  return text === 'too-large' ? undefined : parseJson(text);
}
