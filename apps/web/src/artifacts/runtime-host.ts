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
export { createAdoptionDriver } from './adoption-driver';
export type {
  AdoptionDriver,
  LiveState,
  PreviewChoice,
} from './adoption-driver';

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
  /**
   * Told the revision of each save of the frame that committed (decision
   * 0054, rule 8).
   */
  readonly saved?: ((revision: number) => void) | undefined;
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
    saved: options.saved,
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

/**
 * The change hint of a frame whose view can be adopted: one sender for the
 * frame's life, so "only versions higher than the last one sent" (decision
 * 0053, rule 17) holds across an adoption and a version waiting in its window
 * is not lost. Whether a hint may go out is judged against the view the frame
 * is bound to at that moment, when the change arrives and again when it is
 * sent (decision 0054, rule 13).
 */
function followLiveHints(options: {
  readonly current: () => { view: RuntimeView; deadline: number };
  readonly artifactId: string;
  readonly dataChanges: DataChanges;
  readonly send: (message: Record<string, unknown>) => void;
}): () => void {
  const { current, artifactId, dataChanges, send } = options;
  const allowed = (): RuntimeView | undefined => {
    const { view, deadline } = current();
    return view.granted.includes('data.read') && performance.now() < deadline
      ? view
      : undefined;
  };
  // The head the waiting version was offered at: a view that pins another
  // head by the time it is sent gets no hint (decision 0053, rule 17).
  let offeredAt: number | undefined;
  const hint = createDataHint((message) => {
    const view = allowed();
    if (view !== undefined && view.head_revision === offeredAt) send(message);
  });
  const unsubscribe = dataChanges.subscribe(
    artifactId,
    (dataVersion, revision) => {
      if (allowed()?.head_revision !== revision) return;
      offeredAt = revision;
      hint.offer(dataVersion);
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
 * rules 9, 10 and 13). The workspace's adoption driver (adoption-driver.ts)
 * decides when.
 */
export interface ConnectedFrame {
  /** Handles one message from the frame. It never throws. */
  readonly handle: (message: unknown) => void;
  /** The view the frame's calls are bound to now. */
  readonly view: () => RuntimeView;
  /**
   * Calls on their way to the server, not held and not waiting for the user
   * (rule 9).
   */
  readonly onItsWay: () => number;
  /** Whether a `source.write` of the frame has not ended (rule 8). */
  readonly sourceWriting: () => boolean;
  /**
   * Holds the frame's calls. Resolves once the calls on their way have ended
   * or been abandoned (rule 9).
   */
  readonly pause: (reason: AdoptionReason) => Promise<void>;
  /**
   * Ends a pause without an adoption: held calls go out under the current
   * view (rule 8). Throws while an adoption is on its way.
   */
  readonly resume: () => void;
  /**
   * The grant's outcome is unknown: calls are answered `unavailable` at once
   * until the next pause, resume or adoption (rule 8). The pause of a save
   * leaves that as it is.
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
  /**
   * Told the revision of each save of the frame that committed. The pause
   * for the `saved` adoption begins first, before the receipt is posted, so
   * the frame's next call is held, unless the grant is unresolved and the
   * frame is refused (decision 0054, rule 8). Without it a save pauses
   * nothing. It must not throw: the save has committed, and a throw
   * would turn its receipt into a failed reply.
   */
  readonly onSaved?: ((revision: number) => void) | undefined;
}): ConnectedFrame {
  const { client, desk, dataChanges, send } = options;
  const { onSaved } = options;
  const rpc = createRpc({
    client,
    view: options.view,
    desk,
    deadline: options.deadline,
    send,
    saved:
      onSaved === undefined
        ? undefined
        : (revision) => {
            // The pause begins before the receipt is posted (rule 8). Nothing
            // here awaits it.
            void rpc.pauseForSave().catch((error: unknown) => {
              console.error('Could not hold the calls after a save.', error);
            });
            onSaved(revision);
          },
  });
  const stopHints = followLiveHints({
    current: () => ({ view: rpc.view, deadline: rpc.deadline }),
    artifactId: options.view.artifact_id,
    dataChanges,
    send: (message) => {
      send(message, []);
    },
  });
  let ended = false;
  const end = (): void => {
    ended = true;
    stopHints();
  };
  let adopting = false;
  /** Settles when the adoption on its way ends, however it ends. */
  let adoption: Promise<void> = Promise.resolve();
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
    // The frame may have ended while its calls were waited for.
    if (ended || rpc.closed) throw new Error('The frame has ended.');
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
    if (ended || rpc.closed) return { view, deadline };
    // Rounded down. The frame starts its clock when the message arrives, so
    // its deadline can run past this one by the message's transit time.
    const leaseMs = Math.min(
      MAX_TIMER_MS,
      Math.max(1, Math.floor(deadline - performance.now())),
    );
    // Sent before any held call is answered, so the frame's deadline moves
    // first.
    send({ type: 'event', event: VIEW_ADOPTED_EVENT, reason, leaseMs }, []);
    rpc.adopt(view, deadline);
    return { view, deadline };
  };
  return {
    handle: (message) => {
      rpc.handle(message);
    },
    view: () => rpc.view,
    onItsWay: () => rpc.onItsWay,
    sourceWriting: () => rpc.sourceWriting,
    pause: async (reason) => {
      // A pause asked for while an adoption is on its way begins after it,
      // under the generation it made: the adoption would otherwise end it.
      if (adopting) await adoption;
      await rpc.pause(reason);
    },
    resume: () => {
      if (adopting) throw new Error('An adoption is on its way.');
      rpc.resume();
    },
    refuse: () => {
      if (adopting) throw new Error('An adoption is on its way.');
      rpc.refuse();
    },
    adopt: async (request) => {
      // One adoption at a time, and none for a frame that is gone: a second
      // request would name a generation the first is replacing.
      if (ended) throw new Error('The frame has ended.');
      if (adopting) throw new Error('An adoption is already on its way.');
      adopting = true;
      const attempt = adoptOnce(request);
      adoption = attempt.then(
        () => undefined,
        () => undefined,
      );
      try {
        return await attempt;
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
