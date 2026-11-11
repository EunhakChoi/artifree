/**
 * The least time between two change hints to one view, in milliseconds
 * (decision 0053, rule 17).
 */
export const DATA_HINT_INTERVAL_MS = 250;

/** The event of a data change hint (decision 0053, rule 16). */
export const DATA_CHANGED_EVENT = 'data.changed';

/** The event that tells a frame its view was adopted (decision 0054, rule 10). */
export const VIEW_ADOPTED_EVENT = 'view.adopted';

/**
 * Every event the host can send a frame: the messages that are not replies.
 * The cross-layer test pairs this list with the events the SDK handles.
 */
export const FRAME_EVENTS: readonly string[] = Object.freeze([
  DATA_CHANGED_EVENT,
  VIEW_ADOPTED_EVENT,
]);

/** The change hints of one view's connection. */
export interface DataHint {
  /** Offers the version a data write left the space at. */
  readonly offer: (dataVersion: number) => void;
  /** Ends the hints: a waiting version is dropped and nothing is sent later. */
  readonly close: () => void;
}

/**
 * Sends one view the change hint (decision 0053, rule 17): only versions
 * higher than the last one sent, and at most one hint in 250 ms. The first
 * version that arrives while none is waiting starts the window; when it ends,
 * the highest version received meanwhile is sent. A replayed stretch of the
 * feed therefore becomes one hint, and two hints are at least 250 ms apart.
 */
export function createDataHintSender(options: {
  readonly send: (message: Record<string, unknown>) => void;
  /** Runs `callback` after `delayMs`; the returned function cancels it. */
  readonly schedule: (callback: () => void, delayMs: number) => () => void;
}): DataHint {
  // data_changed names a version of at least 1, so 0 is below every one.
  let lastSent = 0;
  let waiting: number | undefined;
  let scheduled = false;
  let cancel: (() => void) | undefined;
  let closed = false;
  const flush = (): void => {
    scheduled = false;
    cancel = undefined;
    if (closed || waiting === undefined) {
      return;
    }
    const dataVersion = waiting;
    waiting = undefined;
    lastSent = dataVersion;
    options.send({ type: 'event', event: DATA_CHANGED_EVENT, dataVersion });
  };
  return {
    offer: (dataVersion) => {
      if (
        closed ||
        !Number.isSafeInteger(dataVersion) ||
        dataVersion <= lastSent
      ) {
        return;
      }
      if (waiting === undefined || dataVersion > waiting) {
        waiting = dataVersion;
      }
      if (!scheduled) {
        // Set before scheduling, so a callback that runs at once clears it.
        scheduled = true;
        cancel = options.schedule(flush, DATA_HINT_INTERVAL_MS);
      }
    },
    close: () => {
      closed = true;
      cancel?.();
      cancel = undefined;
      waiting = undefined;
    },
  };
}
