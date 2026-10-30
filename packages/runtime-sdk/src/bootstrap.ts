import type { RuntimeMetadata } from './index.js';

/** Server-issued initialization of a single frame and view generation. */
export interface RuntimeInitialization {
  readonly view_id: string;
  readonly generation: number;
  readonly initialization: string;
  readonly lease_ms: number;
}

interface Waiting {
  readonly resolve: (value: RuntimeMetadata) => void;
  readonly reject: (reason: Error) => void;
  readonly timeout: number;
}

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function metadata(value: unknown): RuntimeMetadata | undefined {
  if (!record(value)) return undefined;
  const { artifactId, title, currentRevision, metadataRevision } = value;
  if (
    typeof artifactId !== 'string' ||
    typeof title !== 'string' ||
    typeof currentRevision !== 'number' ||
    typeof metadataRevision !== 'number' ||
    !Number.isSafeInteger(currentRevision) ||
    !Number.isSafeInteger(metadataRevision) ||
    currentRevision < 1 ||
    metadataRevision < 1
  )
    return undefined;
  return { artifactId, title, currentRevision, metadataRevision };
}

/** Installs the runtime for the frame that received this initialization. */
export function start(config: RuntimeInitialization): void {
  if (window.artifree !== undefined)
    throw new Error('The Artifree runtime was already installed.');
  const channel = new MessageChannel();
  const deadline = performance.now() + config.lease_ms;
  const waiting = new Map<number, Waiting>();
  let next = 0;
  let closed = false;
  channel.port1.onmessage = (event: MessageEvent<unknown>) => {
    if (!record(event.data) || typeof event.data.id !== 'number') return;
    const pending = waiting.get(event.data.id);
    if (pending === undefined) return;
    waiting.delete(event.data.id);
    clearTimeout(pending.timeout);
    const value = metadata(event.data.metadata);
    if (event.data.ok === true && value !== undefined) pending.resolve(value);
    else pending.reject(new Error('The runtime metadata request was refused.'));
  };
  window.addEventListener(
    'pagehide',
    () => {
      closed = true;
      channel.port1.close();
      for (const pending of waiting.values()) {
        clearTimeout(pending.timeout);
        pending.reject(new Error('The runtime view closed.'));
      }
      waiting.clear();
    },
    { once: true },
  );
  window.parent.postMessage(
    {
      type: 'artifree-runtime-connect',
      viewId: config.view_id,
      generation: config.generation,
      initialization: config.initialization,
    },
    '*',
    [channel.port2],
  );
  window.artifree = Object.freeze({
    currentMetadata(): Promise<RuntimeMetadata> {
      if (closed || performance.now() >= deadline)
        return Promise.reject(new Error('The runtime view has expired.'));
      if (waiting.size >= 64)
        return Promise.reject(
          new Error('Too many runtime requests are pending.'),
        );
      if (next === Number.MAX_SAFE_INTEGER)
        return Promise.reject(
          new Error('The runtime request limit was reached.'),
        );
      const id = ++next;
      return new Promise((resolve, reject) => {
        const timeout = window.setTimeout(
          () => {
            waiting.delete(id);
            reject(new Error('The runtime metadata request timed out.'));
          },
          Math.min(10_000, Math.max(0, deadline - performance.now())),
        );
        waiting.set(id, { resolve, reject, timeout });
        try {
          channel.port1.postMessage({ type: 'metadata', id });
        } catch {
          waiting.delete(id);
          clearTimeout(timeout);
          reject(new Error('The runtime view is unavailable.'));
        }
      });
    },
  });
}
