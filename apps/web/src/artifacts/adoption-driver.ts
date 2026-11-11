/**
 * The workspace's side of decision 0054: when a live frame adopts a new
 * generation (rule 8), how its calls are held meanwhile (rule 9), and when the
 * frame is restarted instead (rule 11).
 *
 * The module has no DOM and no React. The frame, the grants, the page's
 * visibility, the clock and the timers are injected. It imports types only, so
 * the on-demand host chunk that holds it does not grow the web entry bundle
 * (rule 18).
 */
import type { RuntimeView } from '@artifree/contracts';
import type { AdoptionReason } from './frame-rpc';
import type { Visibility } from './request-gate';

/** An adoption request has this long, within which it may repeat a 503 (rule 8). */
export const ADOPTION_BOUND_MS = 10_000;

/** A lease adoption proceeds past calls on their way in this last window (rule 8). */
export const LAST_MOMENT_MS = 10_000;

/** A lease adoption falls due when 1 / LEASE_SHARE of the lease remains (rule 8). */
export const LEASE_SHARE = 5;

/** How often a deferred adoption checks again. */
export const IDLE_POLL_MS = 250;

/** The wait before a 503 is repeated, while the bound allows it. */
export const UNAVAILABLE_RETRY_MS = 500;

/** The largest delay a timer accepts. */
const MAX_TIMER_MS = 2_147_483_647;

/** The connected frame, as the driver uses it. */
export interface DriverFrame {
  /** The view the frame is connected to. */
  readonly view: () => RuntimeView;
  /** Holds the frame's calls; resolves once the calls on their way have answered (rule 9). */
  readonly pause: (reason: AdoptionReason) => Promise<void>;
  /** Ends a pause without an adoption: the held calls go out under the current generation. */
  readonly resume: () => void;
  /** Answers the frame's calls `unavailable` at once; they are not held. */
  readonly refuse: () => void;
  /** Requests a generation pinned to `revision`. Rejects with the request's error. */
  readonly adopt: (options: {
    readonly reason: AdoptionReason;
    readonly revision: number;
    readonly generation: number;
    readonly signal?: AbortSignal | undefined;
  }) => Promise<{ readonly view: RuntimeView; readonly deadline: number }>;
  /** Answers the held calls `expired`, as a failed adoption does (rule 9). */
  readonly fail: () => void;
  /** Calls on their way: served, and neither held nor waiting for the user. */
  readonly onItsWay: () => number;
  /** Whether a `source.write` of the frame has not answered yet. */
  readonly sourceWriting: () => boolean;
}

/** The grants the frame's calls are checked against, as the driver reads them. */
export interface DriverGrants {
  /** Calls `listener` after any grant changes. Returns the function that removes it. */
  readonly subscribe: (listener: () => void) => () => void;
  /**
   * `settled` when the outcome of the grant is known, `pending` while a change
   * is being sent, and `unknown` when a change may have been made without an
   * answer (0020, rule 3).
   */
  readonly status: (view: RuntimeView) => 'settled' | 'pending' | 'unknown';
  /** The grant version for `revision` of `artifactId`. It moves with every change. */
  readonly revision: (artifactId: string, revision: number) => number;
  /** Sends a grant change for the view. Rejects when it is refused or its outcome is unknown. */
  readonly set: (view: RuntimeView, capabilities: string[]) => Promise<unknown>;
}

/** The revision a preview shows: the latest one, or one the user chose. */
export interface PreviewChoice {
  /** Whether the preview follows the latest revision. */
  readonly following: boolean;
  /** The revision shown: the chosen one, or the latest when following. */
  readonly revision: number;
  /**
   * The latest revision the workspace knows, whichever revision is shown: a
   * head that moves is adopted for a chosen revision too (rule 8).
   */
  readonly latest: number;
}

/** What the workspace shows for a frame that has adopted. */
export interface LiveState {
  /** The adopted view. */
  readonly view: RuntimeView;
  /** The end of the adopted lease, on the clock of `now`. */
  readonly deadline: number;
  /** The revision the frame's document was loaded at (rule 12). */
  readonly documentRevision: number;
}

/** The inputs of one live frame's driver. */
export interface AdoptionDriverOptions {
  readonly frame: DriverFrame;
  readonly grants: DriverGrants;
  /** The end of the current lease, on the clock of `now`. */
  readonly deadline: number;
  readonly choice: PreviewChoice;
  readonly now: () => number;
  /** Calls `callback` after `delayMs`. Returns the function that cancels the call. */
  readonly schedule: (callback: () => void, delayMs: number) => () => void;
  /** Whether the page is visible, and a way to hear when that changes (rule 8). */
  readonly visibility: Visibility;
  /** Returns the number of the next generation to request. */
  readonly nextGeneration: () => number;
  /** Closes a generation the workspace no longer uses. */
  readonly closeGeneration: (generation: number) => void;
  /** Called after every adoption. */
  readonly onChange: (state: LiveState) => void;
  /** Called once when the frame must be restarted. The driver has ended by then. */
  readonly onRestart: () => void;
}

/** Drives one live frame after it was created. */
export interface AdoptionDriver {
  /**
   * Pauses the frame, sends the grant change and adopts the new grant. Rejects
   * with the change's error when the change is refused, and when the preview
   * has ended.
   */
  readonly changePermissions: (capabilities: string[]) => Promise<void>;
  /** The frame's own save committed at `revision` (rule 8). */
  readonly saved: (revision: number) => void;
  /** The user chose a revision, or Latest. A change of mode or revision restarts the frame (rule 11). */
  readonly choose: (choice: PreviewChoice) => void;
  /** Ends the driver and closes an adopted generation it still holds. */
  readonly dispose: () => void;
}

/** What a successful adoption request yields. */
type Adopted = Awaited<ReturnType<DriverFrame['adopt']>>;

/** Lets the chain go on past a task. The task's caller already has its outcome. */
function continueChain(): void {
  // A rejected task reports to its caller, not to the chain.
}

/**
 * Recognizes the client's ArtifreeApiError with status 503 by its shape, so the
 * driver needs no value import from the client.
 */
function isUnavailable(error: unknown): boolean {
  if (typeof error !== 'object' || error === null) {
    return false;
  }
  return (
    'name' in error &&
    error.name === 'ArtifreeApiError' &&
    'status' in error &&
    error.status === 503
  );
}

/**
 * Creates the driver of one live frame. Creation arms the lease and evaluates
 * the state it starts from on the chain, so a head already ahead or a grant not
 * yet settled acts at once. Every cause runs on one chain, one at a time, since
 * the frame accepts one adoption at a time.
 */
export function createAdoptionDriver(
  options: AdoptionDriverOptions,
): AdoptionDriver {
  const {
    frame,
    grants,
    now,
    schedule,
    visibility,
    nextGeneration,
    closeGeneration,
    onChange,
    onRestart,
  } = options;
  const first = frame.view();
  const documentRevision = first.revision;
  const firstGeneration = first.generation;
  let deadline = options.deadline;
  let choice = options.choice;
  let knownHead = Math.max(first.head_revision, choice.latest);
  let savedRevision: number | undefined;
  let grantSeen = grants.revision(first.artifact_id, first.revision);
  let refusing = false;
  let leaseDue = false;
  let ended = false;
  let disposed = false;
  let controller: AbortController | undefined;
  let cancelLease: (() => void) | undefined;
  let cancelPoll: (() => void) | undefined;
  let queued = false;
  let chain: Promise<void> = Promise.resolve();

  function enqueue(): void {
    if (queued || ended) {
      return;
    }
    queued = true;
    chain = chain
      .then(() => {
        queued = false;
        return step();
      })
      .catch(failUnexpectedly);
  }

  /**
   * An evaluation that throws leaves the driver unable to vouch for the frame's
   * held calls, so the frame is failed and restarted, as a failed adoption is.
   */
  function failUnexpectedly(): void {
    if (ended) {
      return;
    }
    frame.fail();
    restart();
  }

  function armLease(): void {
    cancelLease?.();
    const view = frame.view();
    const at = deadline - view.lease_ms / LEASE_SHARE;
    cancelLease = schedule(
      () => {
        leaseDue = true;
        enqueue();
      },
      Math.min(MAX_TIMER_MS, Math.max(0, at - now())),
    );
  }

  function poll(): void {
    if (cancelPoll !== undefined) {
      return;
    }
    cancelPoll = schedule(() => {
      cancelPoll = undefined;
      enqueue();
    }, IDLE_POLL_MS);
  }

  function end(): void {
    if (ended) {
      return;
    }
    ended = true;
    cancelLease?.();
    cancelPoll?.();
    unsubscribe();
    unwatchVisibility();
  }

  /** Whether the lease fell due and its deadline has passed (rule 8). */
  function leaseLapsed(): boolean {
    return leaseDue && now() >= deadline;
  }

  function restart(): void {
    if (ended) {
      return;
    }
    // Rule 8: a lease that ended first restarts the frame when the page is next
    // shown. The visibility listener evaluates again at that moment.
    if (leaseLapsed() && !visibility.visible()) {
      return;
    }
    end();
    onRestart();
  }

  function delay(delayMs: number): Promise<void> {
    return new Promise<void>((resolve) => {
      schedule(() => {
        resolve();
      }, delayMs);
    });
  }

  /**
   * Evaluates the causes in priority order (saved, permissions, head, lease)
   * and starts the first one that applies. A lapsed lease comes before them
   * all. Each adoption covers every cause that is pending, because its
   * generation carries the current head and grant.
   */
  async function step(): Promise<void> {
    if (ended) {
      return;
    }
    if (leaseLapsed()) {
      // Rule 8: the lease ended first (a sleeping machine, a throttled timer).
      // An expired view cannot be adopted (0054, rule 4), so the other causes
      // do not start.
      frame.fail();
      restart();
      return;
    }
    const view = frame.view();
    if (grants.status(view) !== 'settled') {
      // Rule 8: the outcome is not known. The frame stays, its calls are answered
      // `unavailable` at once, and the workspace adopts once the grant settles.
      if (!refusing) {
        frame.refuse();
        refusing = true;
      }
      return;
    }
    if (savedRevision !== undefined) {
      return adopt('saved', savedRevision);
    }
    if (
      refusing ||
      grants.revision(view.artifact_id, view.revision) !== grantSeen
    ) {
      return adopt('permissions', view.revision);
    }
    if (knownHead > view.head_revision) {
      // A head change seen while a write of the frame is on its way waits for
      // that write: if it committed, it is the frame's own save.
      if (frame.sourceWriting()) {
        poll();
        return;
      }
      return adopt('head', view.revision);
    }
    if (leaseDue) {
      if (frame.onItsWay() > 0 && now() < deadline - LAST_MOMENT_MS) {
        poll();
        return;
      }
      return adopt('lease', view.revision);
    }
  }

  /**
   * Requests the generation, repeating a 503 while the bound allows. Resolves
   * with undefined when no generation is made: the bound ran out, the request
   * failed another way, or the driver ended.
   */
  async function requestGeneration(
    cause: AdoptionReason,
    target: number,
    signal: AbortSignal,
    started: number,
  ): Promise<Adopted | undefined> {
    for (;;) {
      const generation = nextGeneration();
      try {
        return await frame.adopt({
          reason: cause,
          revision: target,
          generation,
          signal,
        });
      } catch (error: unknown) {
        // An abandoned request can still finish at the server.
        if (ended || signal.aborted) {
          closeGeneration(generation);
        }
        if (
          ended ||
          signal.aborted ||
          !isUnavailable(error) ||
          now() - started + UNAVAILABLE_RETRY_MS >= ADOPTION_BOUND_MS
        ) {
          return undefined;
        }
        await delay(UNAVAILABLE_RETRY_MS);
        if (ended || signal.aborted) {
          return undefined;
        }
      }
    }
  }

  /** Adopts a generation pinned to `revision`, or fails the frame and restarts it. */
  async function adopt(
    reason: AdoptionReason,
    revision: number,
  ): Promise<void> {
    await frame.pause(reason);
    if (ended) {
      return;
    }
    // A save that committed while the calls on their way were waited for makes
    // this the frame's own save, pinned to the revision that save made (rule 8).
    const cause: AdoptionReason =
      savedRevision === undefined ? reason : 'saved';
    const target = savedRevision ?? revision;
    const abort = new AbortController();
    controller = abort;
    const started = now();
    const cancelBound = schedule(() => {
      abort.abort();
    }, ADOPTION_BOUND_MS);
    try {
      const adopted = await requestGeneration(
        cause,
        target,
        abort.signal,
        started,
      );
      if (ended) {
        if (adopted !== undefined) {
          closeGeneration(adopted.view.generation);
        }
        return;
      }
      if (adopted === undefined) {
        // Rule 11: an adoption that is refused or fails restarts the frame.
        frame.fail();
        restart();
        return;
      }
      deadline = adopted.deadline;
      if (
        savedRevision !== undefined &&
        savedRevision <= adopted.view.revision
      ) {
        savedRevision = undefined;
      }
      refusing = false;
      leaseDue = false;
      grantSeen = grants.revision(
        adopted.view.artifact_id,
        adopted.view.revision,
      );
      knownHead = Math.max(knownHead, adopted.view.head_revision);
      armLease();
      onChange({ view: adopted.view, deadline, documentRevision });
      enqueue(); // another cause may remain
    } finally {
      cancelBound();
      controller = undefined;
    }
  }

  /**
   * Pauses the frame, sends the grant change and adopts the new grant. Rejects
   * with the change's error when the change is refused, and when the preview
   * has ended.
   */
  function changePermissions(capabilities: string[]): Promise<void> {
    const task = chain.then(async () => {
      if (ended) {
        throw new Error('The preview has ended.');
      }
      await frame.pause('permissions');
      if (ended) {
        throw new Error('The preview has ended.');
      }
      const view = frame.view();
      try {
        await grants.set(view, capabilities);
      } catch (error: unknown) {
        grantSeen = grants.revision(view.artifact_id, view.revision);
        if (!ended) {
          if (grants.status(view) === 'settled') {
            // The grant is as it was, so the held calls go out under the old
            // generation, unless a save that committed during the pause has its
            // saved adoption queued behind this task. That adoption releases them
            // under its own generation: resuming here would send them under the
            // superseded view (rule 9).
            if (savedRevision === undefined) {
              frame.resume();
            }
            refusing = false;
          } else {
            frame.refuse();
            refusing = true;
          }
        }
        throw error;
      }
      grantSeen = grants.revision(view.artifact_id, view.revision);
      await adopt('permissions', view.revision);
    });
    chain = task.then(continueChain, continueChain);
    return task;
  }

  function saved(revision: number): void {
    if (ended) {
      return;
    }
    savedRevision = Math.max(savedRevision ?? 0, revision);
    enqueue();
  }

  function choose(next: PreviewChoice): void {
    if (ended) {
      return;
    }
    if (
      next.following !== choice.following ||
      (!next.following && next.revision !== choice.revision)
    ) {
      restart();
      return;
    }
    choice = next;
    if (next.latest > knownHead) {
      knownHead = next.latest;
      enqueue();
    }
  }

  function dispose(): void {
    if (disposed) {
      return;
    }
    disposed = true;
    end();
    controller?.abort();
    // The first generation is closed by the preview's own close; an adopted one
    // is closed here.
    const current = frame.view();
    if (current.generation !== firstGeneration) {
      closeGeneration(current.generation);
    }
  }

  const unsubscribe = grants.subscribe(() => {
    enqueue();
  });
  // A change of visibility evaluates again, so a lease that ended while the page
  // was hidden restarts the frame once it is shown (rule 8).
  const unwatchVisibility = visibility.subscribe(() => {
    enqueue();
  });
  armLease();
  enqueue();

  return { changePermissions, saved, choose, dispose };
}
