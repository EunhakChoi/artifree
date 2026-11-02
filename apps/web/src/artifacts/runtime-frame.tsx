import { useEffectEvent, useLayoutEffect, useRef } from 'react';
import { ArtifreeApiError, encodeFile } from '@artifree/client';
import type { ArtifreeClient } from '@artifree/client';
import type { RuntimeView } from '@artifree/contracts';
import type {
  RuntimeDownloadResult,
  RuntimeErrorCode,
} from '@artifree/runtime-sdk';
import {
  HostRefusal,
  base64Bytes,
  frameErrorCode,
  parseFrameMessage,
} from './runtime-requests';
import type { FrameRequest } from './runtime-requests';

const PREVIEW_SANDBOX =
  'allow-scripts allow-forms allow-modals allow-downloads';

interface RuntimeFrameProps {
  readonly client: ArtifreeClient;
  readonly view: RuntimeView;
  /** Local monotonic deadline, conservatively measured from dispatch. */
  readonly deadline: number;
  readonly onClose: () => void;
  /**
   * Asks the user to confirm a download this frame requested, and hands the
   * browser the bytes only after they do. Rejects with a {@link HostRefusal}
   * when the user declines or an offer is already open.
   */
  readonly onDownload: (request: {
    readonly path: string | undefined;
    readonly signal: AbortSignal;
  }) => Promise<RuntimeDownloadResult>;
  readonly title: string;
  readonly className?: string | undefined;
}

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/** The code a frame is told for any failure; never the failure's message. */
function failureCode(error: unknown): RuntimeErrorCode {
  if (error instanceof HostRefusal) return error.code;
  if (error instanceof ArtifreeApiError) return frameErrorCode(error.code);
  return 'refused';
}

/** Owns a single frame's initialization, message port and pending reads. */
export function RuntimeFrame({
  client,
  view,
  deadline,
  onClose,
  onDownload,
  title,
  className,
}: RuntimeFrameProps) {
  const frame = useRef<HTMLIFrameElement>(null);
  const loads = useRef(0);
  const close = useRef<() => void>(() => undefined);
  const revokeTimer = useRef<number | undefined>(undefined);
  const offerDownload = useEffectEvent(onDownload);

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

    let writing = false;
    const run = async (
      request: FrameRequest,
      signal: AbortSignal,
    ): Promise<Record<string, unknown>> => {
      switch (request.kind) {
        case 'metadata': {
          const result = await client.runtimeMetadata(view, { signal });
          return {
            metadata: {
              artifactId: result.artifact_id,
              title: result.title,
              currentRevision: result.current_revision,
              metadataRevision: result.metadata_revision,
            },
          };
        }
        case 'source.list': {
          const result = await client.runtimeSourceList(view, { signal });
          return {
            result: {
              artifactId: result.artifact_id,
              revision: result.revision,
              files: result.files.map((file) => ({
                path: file.path,
                size: file.size,
                sha256: file.sha256,
                mediaType: file.media_type,
              })),
            },
          };
        }
        case 'source.read': {
          const part = await client.runtimeSourceRead(
            view,
            request.path,
            { offset: request.offset, length: request.length },
            { signal },
          );
          return {
            result: {
              path: part.path,
              revision: part.revision,
              offset: part.offset,
              size: part.size,
              sha256: part.sha256,
              nextOffset: part.next_offset,
              bytes: base64Bytes(part.content),
            },
          };
        }
        case 'source.write': {
          const result = await client.runtimeSourceSave(
            view,
            request.changes.map((change) =>
              change.op === 'delete'
                ? { op: 'delete' as const, path: change.path }
                : {
                    op: 'put' as const,
                    ...encodeFile(change.path, change.bytes),
                  },
            ),
            { idempotencyKey: request.key, signal },
          );
          return {
            result: {
              revision: result.revision,
              createdAtMs: result.created_at_ms,
              replayed: result.replayed,
              files: result.files.map((file) => ({
                path: file.path,
                size: file.size,
                sha256: file.sha256,
                mediaType: file.media_type,
              })),
            },
          };
        }
        case 'download':
          return {
            result: await offerDownload({ path: request.path, signal }),
          };
      }
    };
    const respond = (channel: MessagePort, message: unknown): void => {
      if (!active || performance.now() >= deadline) return;
      const parsed = parseFrameMessage(message);
      if (parsed === undefined) return;
      if (!parsed.ok) {
        channel.postMessage({ id: parsed.id, ok: false, code: parsed.code });
        return;
      }
      const { request } = parsed;
      const { id } = request;
      if (pending.size >= 64 || (request.kind === 'source.write' && writing)) {
        channel.postMessage({ id, ok: false, code: 'busy' });
        return;
      }
      if (request.kind === 'source.write') writing = true;
      const controller = new AbortController();
      pending.add(controller);
      void run(request, controller.signal)
        .then(
          (body) => {
            if (!active || performance.now() >= deadline) return;
            channel.postMessage({ id, ok: true, ...body });
          },
          (error: unknown) => {
            if (!active || performance.now() >= deadline) return;
            channel.postMessage({
              id,
              ok: false,
              code: failureCode(error),
              ...(error instanceof ArtifreeApiError &&
              error.currentRevision !== undefined
                ? { currentRevision: error.currentRevision }
                : {}),
            });
          },
        )
        .finally(() => {
          pending.delete(controller);
          if (request.kind === 'source.write') writing = false;
        })
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
