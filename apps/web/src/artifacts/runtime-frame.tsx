import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import type { ArtifreeClient } from '@artifree/client';
import type { RuntimeView } from '@artifree/contracts';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import type { DataChanges } from './data-changes';
import type { FrameRpc } from './frame-rpc';
import type * as RuntimeHost from './runtime-host';

// `allow-downloads` is the browser's own flag for a page that starts a
// download itself. It is unrelated to the `source.download` capability, which
// asks the workspace to offer a stored file and needs the user's confirmation
// each time (decision 0032).
const PREVIEW_SANDBOX =
  'allow-scripts allow-forms allow-modals allow-downloads';

interface RuntimeFrameProps {
  readonly client: ArtifreeClient;
  readonly view: RuntimeView;
  /**
   * The feed's data changes; a view holding `data.read` forwards them (0053,
   * rule 16).
   */
  readonly dataChanges: DataChanges;
  /** Local monotonic deadline, conservatively measured from dispatch. */
  readonly deadline: number;
  readonly onClose: () => void;
  readonly title: string;
  readonly className?: string | undefined;
}

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/**
 * Owns a single frame's initialization, message port and pending calls. What
 * the frame may ask is decided by `FrameRpc`, its budget and the offer desk
 * (decision 0032); this component only wires them to the port. Those parts
 * load on demand, and only for a view that is enabled: a plain preview never
 * fetches them, and an enabled frame is not shown until they are here, so
 * the connection its script announces on load is never missed.
 */
export function RuntimeFrame({
  client,
  view,
  dataChanges,
  deadline,
  onClose,
  title,
  className,
}: RuntimeFrameProps) {
  const frame = useRef<HTMLIFrameElement>(null);
  const loads = useRef(0);
  const close = useRef<() => void>(() => undefined);
  const revokeTimer = useRef<number | undefined>(undefined);
  const [host, setHost] = useState<typeof RuntimeHost>();
  const [hostFailed, setHostFailed] = useState(false);
  const desk = useMemo(() => host?.createDesk(), [host]);

  useEffect(() => {
    if (!view.enabled) return undefined;
    let live = true;
    void import('./runtime-host').then(
      (loaded) => {
        if (live) setHost(loaded);
      },
      () => {
        if (live) setHostFailed(true);
      },
    );
    return () => {
      live = false;
    };
  }, [view.enabled]);

  useLayoutEffect(() => {
    // React StrictMode replays setup/cleanup synchronously. Only a cleanup
    // that is not followed by setup should revoke the server-side lease.
    window.clearTimeout(revokeTimer.current);
    let active = true;
    let port: MessagePort | undefined;
    let rpc: FrameRpc | undefined;
    let stopHints: (() => void) | undefined;
    const stop = (): void => {
      if (!active) return;
      active = false;
      port?.close();
      rpc?.close();
      stopHints?.();
      desk?.withdraw();
    };
    close.current = stop;

    const connect = (event: MessageEvent<unknown>): void => {
      if (!record(event.data) || event.data.type !== 'artifree-runtime-connect')
        return;
      const candidate = event.ports[0];
      if (
        host === undefined ||
        desk === undefined ||
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
      const served = host.createRpc({
        client,
        view,
        desk,
        deadline,
        send: (reply, transfer) => {
          if (active) candidate.postMessage(reply, transfer);
        },
      });
      rpc = served;
      candidate.onmessage = (message: MessageEvent<unknown>) => {
        if (active) served.handle(message.data);
      };
      // The change hint (decision 0053, rules 16 and 17), only while this
      // connection lasts.
      stopHints = host.followDataHints({
        view,
        dataChanges,
        deadline,
        send: (message) => {
          if (active) candidate.postMessage(message);
        },
      });
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
  }, [client, view, deadline, onClose, host, desk, dataChanges]);

  if (view.enabled && hostFailed)
    return (
      <Alert variant="destructive">
        <AlertTitle>Preview unavailable</AlertTitle>
        <AlertDescription>
          The preview could not be prepared. Refresh the preview to try again.
        </AlertDescription>
      </Alert>
    );
  if (view.enabled && host === undefined)
    return (
      <output className="text-sm text-muted-foreground">
        Preparing preview…
      </output>
    );
  return (
    <>
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
      {host === undefined || desk === undefined ? null : (
        <host.DownloadPrompt desk={desk} />
      )}
    </>
  );
}
