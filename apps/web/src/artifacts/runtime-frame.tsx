import { useLayoutEffect, useRef } from 'react';
import type { ArtifreeClient } from '@artifree/client';
import type { RuntimeView } from '@artifree/contracts';

const PREVIEW_SANDBOX =
  'allow-scripts allow-forms allow-modals allow-downloads';

interface RuntimeFrameProps {
  readonly client: ArtifreeClient;
  readonly view: RuntimeView;
  /** Local monotonic deadline, conservatively measured from dispatch. */
  readonly deadline: number;
  readonly onClose: () => void;
  readonly title: string;
  readonly className?: string | undefined;
}

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/** Owns a single frame's initialization, message port and pending reads. */
export function RuntimeFrame({
  client,
  view,
  deadline,
  onClose,
  title,
  className,
}: RuntimeFrameProps) {
  const frame = useRef<HTMLIFrameElement>(null);
  const loads = useRef(0);
  const close = useRef<() => void>(() => undefined);
  const revokeTimer = useRef<number | undefined>(undefined);

  useLayoutEffect(() => {
    // React StrictMode replays setup/cleanup synchronously. Only a cleanup
    // that is not followed by setup should revoke the server-side lease.
    window.clearTimeout(revokeTimer.current);
    let active = true;
    let port: MessagePort | undefined;
    const pending = new Set<AbortController>();
    const stop = (): void => {
      if (!active) return;
      active = false;
      port?.close();
      for (const controller of pending) controller.abort();
      pending.clear();
    };
    close.current = stop;

    const respond = (channel: MessagePort, message: unknown): void => {
      if (!active || performance.now() >= deadline || !record(message)) return;
      const { id, type } = message;
      if (
        type !== 'metadata' ||
        typeof id !== 'number' ||
        !Number.isSafeInteger(id) ||
        id < 1
      )
        return;
      if (pending.size >= 64) {
        channel.postMessage({ id, ok: false });
        return;
      }
      const controller = new AbortController();
      pending.add(controller);
      void client
        .runtimeMetadata(view, { signal: controller.signal })
        .then(
          (result) => {
            if (!active || performance.now() >= deadline) return;
            channel.postMessage({
              id,
              ok: true,
              metadata: {
                artifactId: result.artifact_id,
                title: result.title,
                currentRevision: result.current_revision,
                metadataRevision: result.metadata_revision,
              },
            });
          },
          () => {
            if (active && performance.now() < deadline)
              channel.postMessage({ id, ok: false });
          },
        )
        .finally(() => pending.delete(controller))
        .catch((error: unknown) => {
          console.error('The runtime message could not be delivered.', error);
          stop();
        });
    };
    const connect = (event: MessageEvent<unknown>): void => {
      if (!record(event.data) || event.data.type !== 'artifree-runtime-connect')
        return;
      const candidate = event.ports[0];
      if (
        !view.enabled ||
        !active ||
        loads.current > 1 ||
        port !== undefined ||
        performance.now() >= deadline ||
        event.origin !== 'null' ||
        event.source !== frame.current?.contentWindow ||
        event.ports.length !== 1 ||
        candidate === undefined ||
        event.data.viewId !== view.view_id ||
        event.data.generation !== view.generation ||
        event.data.initialization !== view.initialization
      ) {
        for (const rejected of event.ports) rejected.close();
        return;
      }
      port = candidate;
      port.onmessage = (message: MessageEvent<unknown>) =>
        respond(candidate, message.data);
    };
    window.addEventListener('message', connect);
    const expiry = window.setTimeout(
      stop,
      Math.max(0, deadline - performance.now()),
    );
    return () => {
      window.removeEventListener('message', connect);
      window.clearTimeout(expiry);
      stop();
      revokeTimer.current = window.setTimeout(onClose, 0);
    };
  }, [client, view, deadline, onClose]);

  return (
    <iframe
      ref={frame}
      title={title}
      src={view.url}
      sandbox={PREVIEW_SANDBOX}
      referrerPolicy="no-referrer"
      className={className}
      onLoad={() => {
        loads.current += 1;
        if (view.enabled && loads.current > 1) {
          close.current();
          onClose();
        }
      }}
    />
  );
}
