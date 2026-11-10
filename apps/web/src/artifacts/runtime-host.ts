import type { ArtifreeClient } from '@artifree/client';
import type { RuntimeView } from '@artifree/contracts';
import { createDataHintSender, type DataHint } from './data-hint';
import {
  DATA_WRITE_BURST,
  DATA_WRITE_PER_MINUTE,
  TokenBucket,
} from './frame-budget';
import { FrameRpc } from './frame-rpc';
import { OfferDesk } from './offer-desk';
import { pageSaver } from './save-blob';

export { DownloadPrompt } from './download-prompt';
export type { DataHint } from './data-hint';

/**
 * What a runtime frame's calls need on the workspace side (decision 0032):
 * the rpc, its budget, the prompt desk and the saver. Only a view that
 * declares and is offered runtime capabilities needs them, so the component
 * loads this module on demand (a dynamic import), and a plain preview never
 * downloads or runs any of it.
 */

/** The desk that serializes the download prompts of one frame. */
export function createDesk(): OfferDesk {
  return new OfferDesk();
}

/**
 * The rpc for one frame, with the budget of that frame alone and a separate
 * budget for its data writes (decision 0053, rule 23).
 */
export function createRpc(options: {
  readonly client: ArtifreeClient;
  readonly view: RuntimeView;
  readonly desk: OfferDesk;
  /** Local monotonic deadline, conservatively measured from dispatch. */
  readonly deadline: number;
  readonly send: (
    message: Record<string, unknown>,
    transfer: Transferable[],
  ) => void;
}): FrameRpc {
  const bucket = new TokenBucket(() => performance.now());
  const writes = new TokenBucket(
    () => performance.now(),
    DATA_WRITE_BURST,
    DATA_WRITE_PER_MINUTE,
  );
  return new FrameRpc({
    client: options.client,
    view: options.view,
    ask: (offer, signal) => options.desk.ask(offer, signal),
    save: (bytes, fileName, type) => {
      pageSaver.save(bytes, fileName, type);
    },
    take: () => bucket.take(),
    takeDataWrite: () => writes.take(),
    now: () => performance.now(),
    deadline: options.deadline,
    send: options.send,
  });
}

/**
 * The change hints of one frame whose view holds `data.read` (decision 0053,
 * rules 16 and 17). The timer is the global `setTimeout`, so a test can
 * replace it.
 */
export function createDataHint(
  send: (message: Record<string, unknown>) => void,
): DataHint {
  return createDataHintSender({
    send,
    schedule: (callback, delayMs) => {
      const timer = setTimeout(callback, delayMs);
      return () => clearTimeout(timer);
    },
  });
}
