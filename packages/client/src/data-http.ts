import type { ClientConfiguration } from './client.js';
import { parseRetryAfter } from './client.js';
import { ArtifreeApiError } from './errors.js';
import { parseError } from './responses.js';

const ARTIFACT_ID = /^art_[0-9a-f]{32}$/;

function refused(message: string): ArtifreeApiError {
  return new ArtifreeApiError(message, { status: 0, code: 'invalid_request' });
}

/** Refuses an artifact identifier the server could not have issued. */
export function checkArtifactId(artifactId: string): void {
  if (!ARTIFACT_ID.test(artifactId))
    throw refused('The artifact identifier is not `art_` and 32 hex digits.');
}

/** Retry-After bounds in milliseconds: a value outside them is clamped. */
export const RETRY_AFTER_FLOOR_MS = 500;
export const RETRY_AFTER_CEILING_MS = 3_600_000;

/** The wait a failure asks for, clamped, or undefined when it asks for none. */
export function retryAfterMs(error: ArtifreeApiError): number | undefined {
  if (error.retryAfterSeconds === undefined) return undefined;
  return Math.min(
    RETRY_AFTER_CEILING_MS,
    Math.max(RETRY_AFTER_FLOOR_MS, error.retryAfterSeconds * 1000),
  );
}

/**
 * Sends one GET with the bearer token and resolves with the response when
 * its status is a success. Any other status rejects with the server's typed
 * error, with `Retry-After` when given; a connection that fails rejects with
 * `network_error`. The abort `signal` is raced against the request, so a
 * `fetch` that ignores it cannot hold the caller.
 */
export async function getResponse(
  configuration: ClientConfiguration,
  path: string,
  accept: string,
  signal: AbortSignal | undefined,
): Promise<Response> {
  const fetchImpl =
    configuration.fetch ?? ((input, init) => fetch(input, init));
  const base = configuration.baseUrl.replace(/\/+$/, '');
  const init: RequestInit = {
    method: 'GET',
    headers: {
      authorization: `Bearer ${configuration.accessToken}`,
      accept,
    },
  };
  if (signal !== undefined) init.signal = signal;
  let response: Response;
  try {
    response = await abortable(fetchImpl(`${base}${path}`, init), signal);
  } catch (error: unknown) {
    if (error instanceof ArtifreeApiError) throw error;
    throw new ArtifreeApiError('Could not reach the Artifree API.', {
      status: 0,
      code: 'network_error',
      cause: error,
    });
  }
  if (response.ok) return response;
  const retryAfter = parseRetryAfter(response.headers.get('retry-after'));
  let answer: unknown;
  try {
    answer = JSON.parse(await response.text());
  } catch {
    answer = undefined;
  }
  const failure = parseError(answer);
  const limited = response.status === 429;
  const timing =
    retryAfter === undefined ? {} : { retryAfterSeconds: retryAfter };
  if (failure === undefined)
    throw new ArtifreeApiError(
      `GET ${path} failed with HTTP ${response.status}.`,
      {
        status: response.status,
        code: limited ? 'rate_limited' : 'invalid_response',
        ...timing,
      },
    );
  throw new ArtifreeApiError(failure.message, {
    status: response.status,
    code: limited ? 'rate_limited' : failure.code,
    body: answer,
    ...timing,
  });
}

/**
 * Resolves with `promise`, or rejects with an abort error as soon as
 * `signal` aborts. What `promise` later does is handled here, and a response
 * that arrives too late has its body cancelled.
 */
export function abortable<T>(
  promise: Promise<T>,
  signal: AbortSignal | undefined,
): Promise<T> {
  const settle = (): void => undefined;
  if (signal === undefined) return promise;
  return new Promise<T>((resolve, reject) => {
    let finished = false;
    const onAbort = (): void => {
      if (finished) return;
      finished = true;
      reject(new DOMException('Aborted', 'AbortError'));
    };
    if (signal.aborted) {
      onAbort();
    } else {
      signal.addEventListener('abort', onAbort, { once: true });
    }
    promise.then(
      (value) => {
        signal.removeEventListener('abort', onAbort);
        if (finished) {
          if (value instanceof Response) value.body?.cancel().catch(settle);
          return;
        }
        finished = true;
        resolve(value);
      },
      (error: unknown) => {
        signal.removeEventListener('abort', onAbort);
        if (finished) return;
        finished = true;
        reject(error instanceof Error ? error : new Error(String(error)));
      },
    );
  });
}
