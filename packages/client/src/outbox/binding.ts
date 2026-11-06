import { parseRetryAfter } from '../client.js';
import { ArtifreeApiError, type FailureCode } from '../errors.js';
import { parseError } from '../responses.js';
import {
  BodyReader,
  RETRY_AFTER_CAP_SECONDS,
  systemClock,
} from '../transport.js';
import { OutboxError, type Binding } from './types.js';

const GENERATION_MAX = '9223372036854775807';

export function validGeneration(value: unknown): value is string {
  return (
    typeof value === 'string' &&
    /^[1-9][0-9]{0,18}$/.test(value) &&
    (value.length < 19 || value <= GENERATION_MAX)
  );
}

export function validActor(value: unknown): value is string {
  return typeof value === 'string' && /^(local|u-[0-9a-f]{32})$/.test(value);
}

export function validId(value: unknown): value is string {
  return typeof value === 'string' && /^[0-9a-f]{32}$/.test(value);
}

/** Must run before storage/owner/network access; never echo parser errors. */
export function normalizeBase(raw: string, origin: string): string {
  try {
    if (
      typeof raw !== 'string' ||
      /[?#]/.test(raw) ||
      (raw !== '' && !raw.startsWith('/') && !/^https?:\/\//i.test(raw)) ||
      raw.startsWith('//')
    ) {
      throw new OutboxError('invalid_outbox_configuration');
    }
    const url = new URL(raw, origin);
    if (
      !['http:', 'https:'].includes(url.protocol) ||
      url.username !== '' ||
      url.password !== ''
    ) {
      throw new OutboxError('invalid_outbox_configuration');
    }
    if (raw.startsWith('/') && url.origin !== new URL(origin).origin)
      throw new OutboxError('invalid_outbox_configuration');
    return `${url.origin}${url.pathname.replace(/\/+$/, '')}`;
  } catch {
    throw new OutboxError('invalid_outbox_configuration');
  }
}

/** Exactly one bounded authenticated GET. No automatic read retries. */
export async function readBinding(
  base: string,
  accessToken: string,
  signal?: AbortSignal,
): Promise<Binding> {
  const controller = new AbortController();
  const abort = (): void => controller.abort();
  signal?.addEventListener('abort', abort, { once: true });
  if (signal?.aborted) controller.abort();
  const headersTimeout = setTimeout(() => controller.abort(), 15_000);
  const wholeTimeout = setTimeout(() => controller.abort(), 60_000);
  let status = 0;
  let retryAfterSeconds: number | undefined;
  const failure = (code: FailureCode): ArtifreeApiError =>
    new ArtifreeApiError(status === 429 ? 'rate_limited' : code, {
      status,
      code: status === 429 ? 'rate_limited' : code,
      retryAfterSeconds,
    });
  try {
    const response = await globalThis.fetch(`${base}/v1/data-outbox/binding`, {
      method: 'GET',
      headers: { Authorization: `Bearer ${accessToken}` },
      cache: 'no-store',
      redirect: 'error',
      signal: controller.signal,
    });
    clearTimeout(headersTimeout);
    status = response.status;
    const retryAfter = parseRetryAfter(response.headers.get('retry-after'));
    retryAfterSeconds =
      retryAfter === undefined
        ? undefined
        : Math.min(retryAfter, RETRY_AFTER_CAP_SECONDS);
    if (response.body === null) throw failure('invalid_response');
    const reader = new BodyReader(response.body, {
      clock: systemClock,
      signal: controller.signal,
      status,
      what: 'outbox binding',
      keepCause: false,
    });
    const chunks: Uint8Array[] = [];
    let size = 0;
    try {
      for (;;) {
        const chunk = await reader.read();
        if (chunk === 'end') break;
        if (chunk === 'aborted') throw failure('network_error');
        size += chunk.byteLength;
        if (size > 4096) throw failure('invalid_response');
        chunks.push(chunk);
      }
    } finally {
      reader.close();
    }
    const bytes = new Uint8Array(size);
    let offset = 0;
    for (const chunk of chunks) {
      bytes.set(chunk, offset);
      offset += chunk.byteLength;
    }
    const value: unknown = JSON.parse(
      new TextDecoder('utf-8', { fatal: true }).decode(bytes),
    );
    if (status !== 200) {
      const error = parseError(value);
      throw failure(
        status === 429 ? 'rate_limited' : (error?.code ?? 'invalid_response'),
      );
    }
    if (
      typeof value !== 'object' ||
      value === null ||
      Object.keys(value).sort().join(',') !==
        'actor_generation,actor_id,storage_epoch' ||
      !('actor_id' in value) ||
      !validActor(value.actor_id) ||
      !('actor_generation' in value) ||
      !validGeneration(value.actor_generation) ||
      !('storage_epoch' in value) ||
      !validId(value.storage_epoch) ||
      (value.actor_id === 'local' && value.actor_generation !== '1')
    )
      throw failure('invalid_response');
    return {
      actor_id: value.actor_id,
      actor_generation: value.actor_generation,
      storage_epoch: value.storage_epoch,
    };
  } catch (error: unknown) {
    if (error instanceof ArtifreeApiError) throw failure(error.code);
    throw failure(status === 0 ? 'network_error' : 'invalid_response');
  } finally {
    clearTimeout(headersTimeout);
    clearTimeout(wholeTimeout);
    signal?.removeEventListener('abort', abort);
  }
}
