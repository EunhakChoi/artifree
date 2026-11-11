import type { ArtifreeClient } from '@artifree/client';
import type { RuntimeView } from '@artifree/contracts';
import type { DataChanges } from './data-changes';
import {
  VIEW_ADOPTED_EVENT,
  createDataHintSender,
  type DataHint,
} from './data-hint';
import {
  DATA_WRITE_BURST,
  DATA_WRITE_PER_MINUTE,
  TokenBucket,
} from './frame-budget';
import { type AdoptionReason, FrameRpc } from './frame-rpc';
import { OfferDesk } from './offer-desk';
import { pageSaver } from './save-blob';

export { DownloadPrompt } from './download-prompt';
export type { AdoptionReason } from './frame-rpc';

/**
 * What a runtime frame's calls need on the workspace side (decision 0032):
 * the rpc, its budget, the prompt desk and the saver. Only a view that
 * declares and is offered runtime capabilities needs them, so the component
 * loads this module on demand (a dynamic import), and a plain preview never
 * downloads or runs any of it. A connected frame can also adopt a new
 * generation of its view (decision 0054).
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

/**
 * Forwards the change hint to one connected frame for as long as rule 17 of
 * decision 0053 allows, and returns the function that ends it. Nothing is
 * forwarded unless the view holds `data.read`. A change is forwarded only
 * while the artifact's source is still the one the view was issued for (a
 * view whose source moved has lost its grant and its reads are refused), and
 * nothing is sent once the lease has ended, as for a reply.
 */
export function followDataHints(options: {
  readonly view: RuntimeView;
  readonly dataChanges: DataChanges;
  /** The end of the view's lease on the `performance.now()` clock. */
  readonly deadline: number;
  readonly send: (message: Record<string, unknown>) => void;
}): () => void {
  const { view, dataChanges, deadline, send } = options;
  if (!view.granted.includes('data.read')) return () => undefined;
  const hint = createDataHint((message) => {
    if (performance.now() < deadline) send(message);
  });
  const unsubscribe = dataChanges.subscribe(
    view.artifact_id,
    (dataVersion, revision) => {
      if (revision === view.head_revision) hint.offer(dataVersion);
    },
  );
  return () => {
    unsubscribe();
    hint.close();
  };
}

/** The longest delay a browser timer accepts, in milliseconds. */
const MAX_TIMER_MS = 2_147_483_647;

/**
 * A view the server adopted, and the end of its lease on the
 * `performance.now()` clock.
 */
export interface AdoptedView {
  readonly view: RuntimeView;
  readonly deadline: number;
}

/**
 * One connected frame whose view the workspace can adopt (decision 0054,
 * rules 9, 10 and 13). Nothing in the workspace adopts a frame yet (R10c).
 */
export interface ConnectedFrame {
  /** Handles one message from the frame. It never throws. */
  readonly handle: (message: unknown) => void;
  /** The view the frame's calls are bound to now. */
  readonly view: () => RuntimeView;
  /**
   * Holds the frame's calls. Resolves once the calls on their way have ended
   * or been abandoned (rule 9).
   */
  readonly pause: (reason: AdoptionReason) => Promise<void>;
  /**
   * Ends a pause without an adoption: held calls go out under the current
   * view (rule 8).
   */
  readonly resume: () => void;
  /**
   * The grant's outcome is unknown: calls are answered `unavailable` at once
   * until the next pause, resume or adoption (rule 8).
   */
  readonly refuse: () => void;
  /**
   * Pauses for `reason` and asks the server once to adopt the view, pinned to
   * `revision`, as `generation`. On success the frame is told, the change hint
   * follows the new view, held calls go out under it, and the new view is
   * returned. If the frame ended meanwhile, nothing is sent and the caller ends
   * the returned view. A refusal or failure rejects with the client's error and
   * leaves the frame paused: the caller asks again or calls `fail`. A call
   * made while another adoption is on its way, or after the frame ended,
   * rejects without asking the server.
   */
  readonly adopt: (options: {
    readonly reason: AdoptionReason;
    readonly revision: number;
    readonly generation: number;
    readonly signal?: AbortSignal | undefined;
  }) => Promise<AdoptedView>;
  /**
   * The adoption failed: held calls are answered `expired`, the hints stop and
   * the calls end. The caller restarts the frame (rules 9 and 11).
   */
  readonly fail: () => void;
  /** Ends every call and the hints: the frame is gone. */
  readonly close: () => void;
}

/**
 * Connects one frame: its rpc, and its change hints, which follow the view the
 * frame is bound to (decision 0054). The caller ends the frame with `fail` or
 * `close`.
 */
export function connectFrame(options: {
  readonly client: ArtifreeClient;
  readonly view: RuntimeView;
  readonly desk: OfferDesk;
  readonly dataChanges: DataChanges;
  /** Local monotonic deadline, conservatively measured from dispatch. */
  readonly deadline: number;
  readonly send: (
    message: Record<string, unknown>,
    transfer: Transferable[],
  ) => void;
}): ConnectedFrame {
  const { client, desk, dataChanges, send } = options;
  const rpc = createRpc({
    client,
    view: options.view,
    desk,
    deadline: options.deadline,
    send,
  });
  const follow = (view: RuntimeView, deadline: number): (() => void) =>
    followDataHints({
      view,
      dataChanges,
      deadline,
      send: (message) => {
        send(message, []);
      },
    });
  let stopHints = follow(options.view, options.deadline);
  let ended = false;
  const end = (): void => {
    ended = true;
    stopHints();
  };
  let adopting = false;
  const adoptOnce = async ({
    reason,
    revision,
    generation,
    signal,
  }: {
    readonly reason: AdoptionReason;
    readonly revision: number;
    readonly generation: number;
    readonly signal?: AbortSignal | undefined;
  }): Promise<AdoptedView> => {
    await rpc.pause(reason);
    const old = rpc.view;
    // The server starts the lease after the last dispatch, so measuring from
    // it never runs past the server's end.
    let dispatchedAt = performance.now();
    const view = await client.createRuntimeView(
      old.artifact_id,
      revision,
      old.view_id,
      generation,
      {
        adopt: { generation: old.generation },
        onDispatch: () => {
          dispatchedAt = performance.now();
        },
        ...(signal === undefined ? {} : { signal }),
      },
    );
    const deadline = dispatchedAt + view.lease_ms;
    // The frame was ended meanwhile: nothing is sent, and the caller ends
    // the new generation.
    if (ended) return { view, deadline };
    // Rounded down. The frame starts its clock when the message arrives, so
    // its deadline can run past this one by the message's transit time.
    const leaseMs = Math.min(
      MAX_TIMER_MS,
      Math.max(1, Math.floor(deadline - performance.now())),
    );
    stopHints();
    // Sent before any held call is answered, so the frame's deadline moves
    // first.
    send({ type: 'event', event: VIEW_ADOPTED_EVENT, reason, leaseMs }, []);
    rpc.adopt(view, deadline);
    stopHints = follow(view, deadline);
    return { view, deadline };
  };
  return {
    handle: (message) => {
      rpc.handle(message);
    },
    view: () => rpc.view,
    pause: (reason) => rpc.pause(reason),
    resume: () => {
      rpc.resume();
    },
    refuse: () => {
      rpc.refuse();
    },
    adopt: async (request) => {
      // One adoption at a time, and none for a frame that is gone: a second
      // request would name a generation the first is replacing.
      if (ended) throw new Error('The frame has ended.');
      if (adopting) throw new Error('An adoption is already on its way.');
      adopting = true;
      try {
        return await adoptOnce(request);
      } finally {
        adopting = false;
      }
    },
    fail: () => {
      end();
      rpc.fail();
    },
    close: () => {
      end();
      rpc.close();
    },
  };
}
