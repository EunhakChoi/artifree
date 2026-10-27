import { parseRetryAfter } from '@artifree/client';

/** Whether the page is visible, and a way to hear when that changes. */
export interface Visibility {
  readonly visible: () => boolean;
  /** Calls `onChange` after each change; returns a function that stops. */
  readonly subscribe: (onChange: () => void) => () => void;
}

/** How long a running read holds back the next one. */
export const STALL_MS = 10_000;

/** How the gate treats a request. */
export type RequestKind = 'read' | 'stream' | 'write';

/**
 * Reads change nothing: every `GET` except the change feed, and issuing a
 * preview link. The feed is one long request. Everything else writes.
 */
export function requestKind(method: string, url: string): RequestKind {
  const { pathname } = new URL(url, 'http://gate.invalid');
  const verb = method.toUpperCase();
  if (verb === 'GET')
    return pathname.endsWith('/v1/changes') ? 'stream' : 'read';
  if (verb === 'POST' && pathname.endsWith('/previews')) return 'read';
  return 'write';
}

interface Waiter {
  readonly start: (release: () => void) => void;
}

/**
 * Decides when each HTTP request of a page is sent (decision 0007). It sits
 * under the API client as its `fetch`, so every request passes through it
 * whichever view asked, and it checks its conditions just before sending:
 *
 * - Reads are sent one at a time in the order asked for, only while the
 *   page is visible and no `Retry-After` delay is pending. A read that has
 *   run for {@link STALL_MS} stops holding back the next one; this bound
 *   never ends a delay or sends a read from a hidden page.
 * - The stream is sent once no delay is pending; the caller decides when
 *   to open it.
 * - Writes are sent at once: the user started each one and sees its result.
 *
 * Any `429` answer, whatever its body and whichever kind of request it
 * answered, delays every read for its `Retry-After`, at least one second,
 * because request limits apply to the actor. A refused read was not
 * processed, so the gate sends it again after the delay; the caller sees
 * only its final answer. Aborting a read's signal withdraws it if it has
 * not been sent; a sent read is not cancelled, since the server is working
 * on it, and keeps its slot until it ends while its caller stops waiting.
 */
export class RequestGate {
  readonly #visibility: Visibility;
  readonly #send: typeof fetch;
  // Waiting reads, oldest first; a refused read returns to the front.
  readonly #waiting: Waiter[] = [];
  #running: object | undefined;
  #notBefore = 0;
  #timer: ReturnType<typeof setTimeout> | undefined;
  #stopWatching: (() => void) | undefined;

  constructor(
    visibility: Visibility,
    send: typeof fetch = (input, init) => fetch(input, init),
  ) {
    this.#visibility = visibility;
    this.#send = send;
  }

  /** A `fetch` for the API client: every request goes through the gate. */
  readonly fetch: typeof fetch = async (input, init) => {
    const request = describe(input, init);
    const kind = requestKind(request.method, request.url);
    if (kind === 'write') return this.#observe(await this.#send(input, init));
    if (kind === 'stream') {
      await this.#afterDelay(request.signal);
      return this.#observe(await this.#send(input, init));
    }
    let retry = false;
    for (;;) {
      const release = await this.#turn(request.signal, retry);
      // The caller may have stopped waiting while its turn came; then the
      // read has not been sent and is withdrawn.
      if (request.signal?.aborted === true) {
        release();
        request.signal.throwIfAborted();
      }
      // A sent read is not cancelled: the server is working on it. It keeps
      // its slot until its answer has been read or cancelled, also when its
      // caller stops waiting.
      const sent = this.#send(input, unsignalled(input, init)).then(
        (response) => holding(this.#observe(response), release),
        (error: unknown) => {
          release();
          throw error;
        },
      );
      const response = await abandonable(sent, request.signal);
      if (response.status !== 429) return response;
      // The refusal is consumed here; its body is not needed. Waiting for
      // the cancellation could let later reads ahead of this one.
      response.body?.cancel().catch(() => undefined);
      retry = true;
    }
  };

  /** Delays every read that has not been sent until `seconds` from now. */
  holdFor(seconds: number): void {
    this.#notBefore = Math.max(this.#notBefore, Date.now() + seconds * 1000);
    this.#pump();
  }

  #observe(response: Response): Response {
    // At least one second, also for `Retry-After: 0`: a refused read keeps
    // its place, and a proxy that refuses at once is not asked again at once.
    if (response.status === 429)
      this.holdFor(
        Math.max(1, parseRetryAfter(response.headers.get('retry-after')) ?? 1),
      );
    return response;
  }

  /** Waits for a read's turn; resolves with the function that ends it. */
  #turn(signal: AbortSignal | undefined, first: boolean): Promise<() => void> {
    signal?.throwIfAborted();
    return new Promise((resolve, reject) => {
      const withdraw = (): void => {
        const index = this.#waiting.indexOf(waiter);
        if (index !== -1) this.#waiting.splice(index, 1);
        this.#watch();
        reject(signal?.reason);
      };
      const waiter: Waiter = {
        start(release) {
          signal?.removeEventListener('abort', withdraw);
          resolve(release);
        },
      };
      signal?.addEventListener('abort', withdraw, { once: true });
      if (first) this.#waiting.unshift(waiter);
      else this.#waiting.push(waiter);
      this.#watch();
      this.#pump();
    });
  }

  /** Resolves once no `Retry-After` delay is pending. */
  async #afterDelay(signal: AbortSignal | undefined): Promise<void> {
    for (;;) {
      signal?.throwIfAborted();
      const wait = this.#notBefore - Date.now();
      if (wait <= 0) return;
      await new Promise<void>((resolve, reject) => {
        const timer = setTimeout(done, wait);
        signal?.addEventListener('abort', abort, { once: true });
        function done(): void {
          signal?.removeEventListener('abort', abort);
          resolve();
        }
        function abort(): void {
          clearTimeout(timer);
          reject(signal?.reason);
        }
      });
    }
  }

  // Visibility matters only while reads wait, so the gate listens only then
  // and holds no listener once its page is gone.
  #watch(): void {
    if (this.#waiting.length > 0 && this.#stopWatching === undefined)
      this.#stopWatching = this.#visibility.subscribe(() => this.#pump());
    else if (this.#waiting.length === 0 && this.#stopWatching !== undefined) {
      this.#stopWatching();
      this.#stopWatching = undefined;
    }
  }

  #pump(): void {
    clearTimeout(this.#timer);
    this.#timer = undefined;
    if (this.#running !== undefined || !this.#visibility.visible()) return;
    const next = this.#waiting[0];
    if (next === undefined) return;
    const wait = this.#notBefore - Date.now();
    if (wait > 0) {
      this.#timer = setTimeout(() => this.#pump(), wait);
      return;
    }
    this.#waiting.shift();
    this.#watch();
    const running = {};
    this.#running = running;
    const release = (): void => {
      clearTimeout(stalled);
      if (this.#running !== running) return;
      this.#running = undefined;
      this.#pump();
    };
    const stalled = setTimeout(release, STALL_MS);
    next.start(release);
  }
}

/** `response`, calling `release` once its body was read, failed, or cancelled. */
function holding(response: Response, release: () => void): Response {
  const { body } = response;
  if (body === null) {
    release();
    return response;
  }
  const reader = body.getReader();
  const passed = new ReadableStream<Uint8Array>({
    async pull(controller) {
      try {
        const chunk = await reader.read();
        if (chunk.done) {
          controller.close();
          release();
        } else controller.enqueue(chunk.value);
      } catch (error: unknown) {
        controller.error(error);
        release();
      }
    },
    async cancel(reason: unknown) {
      release();
      await reader.cancel(reason);
    },
  });
  return new Response(passed, {
    status: response.status,
    statusText: response.statusText,
    headers: response.headers,
  });
}

/** The request without its abort signal. */
function unsignalled(
  input: RequestInfo | URL,
  init: RequestInit | undefined,
): RequestInit | undefined {
  if (init?.signal === undefined && !(input instanceof Request)) return init;
  return { ...init, signal: null };
}

/**
 * `sent`, or a rejection with the signal's reason as soon as `signal`
 * aborts. An abandoned answer's body is cancelled when it arrives.
 */
function abandonable(
  sent: Promise<Response>,
  signal: AbortSignal | undefined,
): Promise<Response> {
  if (signal === undefined) return sent;
  return new Promise((resolve, reject) => {
    const abandon = (): void => {
      reject(signal.reason);
      sent.then(
        (response) => response.body?.cancel().catch(() => undefined),
        () => undefined,
      );
    };
    if (signal.aborted) {
      abandon();
      return;
    }
    signal.addEventListener('abort', abandon, { once: true });
    sent.then(
      (response) => {
        signal.removeEventListener('abort', abandon);
        resolve(response);
      },
      (error: unknown) => {
        signal.removeEventListener('abort', abandon);
        reject(error);
      },
    );
  });
}

function describe(
  input: RequestInfo | URL,
  init: RequestInit | undefined,
): { method: string; url: string; signal: AbortSignal | undefined } {
  if (input instanceof Request)
    return {
      method: init?.method ?? input.method,
      url: input.url,
      signal: init?.signal ?? input.signal,
    };
  return {
    method: init?.method ?? 'GET',
    url: input instanceof URL ? input.href : input,
    signal: init?.signal ?? undefined,
  };
}
