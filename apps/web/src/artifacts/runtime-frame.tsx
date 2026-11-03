import { useLayoutEffect, useRef, useState } from 'react';
import type { ArtifreeClient } from '@artifree/client';
import type { RuntimeView } from '@artifree/contracts';
import { DownloadPrompt } from './download-prompt';
import { TokenBucket } from './frame-budget';
import { FrameRpc } from './frame-rpc';
import { OfferDesk } from './offer-desk';
import { pageSaver } from './save-blob';

// `allow-downloads` is the browser's own flag for a page that starts a
// download itself. It is unrelated to the `source.download` capability, which
// asks the workspace to offer a stored file and needs the user's confirmation
// each time (decision 0032).
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

/**
 * Owns a single frame's initialization, message port and pending calls. What
 * the frame may ask is decided by `FrameRpc`, its budget and the offer desk
 * (decision 0032); this component only wires them to the port.
 */
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
  const [desk] = useState(() => new OfferDesk());

  useLayoutEffect(() => {
    // React StrictMode replays setup/cleanup synchronously. Only a cleanup
    // that is not followed by setup should revoke the server-side lease.
    window.clearTimeout(revokeTimer.current);
    let active = true;
    let port: MessagePort | undefined;
    let rpc: FrameRpc | undefined;
    const stop = (): void => {
      if (!active) return;
      active = false;
      port?.close();
      rpc?.close();
      desk.withdraw();
    };
    close.current = stop;

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
      const bucket = new TokenBucket(() => performance.now());
      const served = new FrameRpc({
        client,
        view,
        ask: (offer, signal) => desk.ask(offer, signal),
        save: (bytes, fileName, type) => {
          pageSaver.save(bytes, fileName, type);
        },
        take: () => bucket.take(),
        now: () => performance.now(),
        deadline,
        send: (reply, transfer) => {
          if (active) candidate.postMessage(reply, transfer);
        },
      });
      rpc = served;
      candidate.onmessage = (message: MessageEvent<unknown>) => {
        if (active) served.handle(message.data);
      };
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
  }, [client, view, deadline, onClose, desk]);

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
      <DownloadPrompt desk={desk} />
    </>
  );
}
