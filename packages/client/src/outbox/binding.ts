import { BodyReader, systemClock } from '../transport.js';
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
): Promise<Binding> {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 45_000);
  try {
    const response = await globalThis.fetch(`${base}/v1/data-outbox/binding`, {
      method: 'GET',
      headers: { Authorization: `Bearer ${accessToken}` },
      cache: 'no-store',
      redirect: 'error',
      signal: controller.signal,
    });
    if (response.status !== 200 || response.body === null) {
      if (response.body !== null) {
        void response.body.cancel().catch(() => undefined);
      }
      throw new OutboxError('binding_failed');
    }
    const reader = new BodyReader(response.body, {
      clock: systemClock,
      signal: controller.signal,
      status: 200,
      what: 'outbox binding',
      keepCause: false,
    });
    const chunks: Uint8Array[] = [];
    let size = 0;
    try {
      for (;;) {
        const chunk = await reader.read();
        if (chunk === 'end') break;
        if (chunk === 'aborted') throw new OutboxError('binding_failed');
        size += chunk.byteLength;
        if (size > 4096) throw new OutboxError('binding_failed');
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
      throw new OutboxError('binding_failed');
    return {
      actor_id: value.actor_id,
      actor_generation: value.actor_generation,
      storage_epoch: value.storage_epoch,
    };
  } catch {
    throw new OutboxError('binding_failed');
  } finally {
    clearTimeout(timeout);
  }
}
