/**
 * The driver of the Jobs tab (decision 0055, rules 5, 8, 14 and 15). It holds
 * the tab's state, reads the list, the jobs and `GET /v1/background` one at a
 * time, and sends the presses of "Verify revision N" and "Cancel" under their
 * request keys. It has no React and no DOM. The client, the clock and the key
 * source are injected, so a test can drive its timing and its retries.
 */
import { newIdempotencyKey } from '@artifree/client';
import type { ArtifreeClient, FeedChange } from '@artifree/client';
import type {
  BackgroundStatus,
  CancelJobResponse,
  EnqueueJobRequest,
  EnqueueJobResponse,
  Job,
  JobList,
  ListJobsQuery,
} from '@artifree/contracts';
import { describeFailure } from './failure';
import {
  IDLE,
  JOB_PAGE_SIZE,
  JOB_POLL_MS,
  afterFailure,
  appendPage,
  mergeFirstPage,
  nextBackgroundRead,
  nextOverdueAt,
  overdueJobs,
  pressCancel,
  pressVerify,
  reactionTo,
  replaceJob,
  seenQueued,
  upsertJob,
} from './jobs-model';
import type { JobRows, JobsView, Press, PressState } from './jobs-model';

/**
 * The requests for one artifact's jobs. A read takes the signal that a stop
 * uses to abandon it; a press takes the request key it is sent under.
 */
export interface JobsPorts {
  /** One page of the artifact's jobs, newest first. */
  readonly listJobs: (
    query: ListJobsQuery,
    signal: AbortSignal,
  ) => Promise<JobList>;
  /** One job of the artifact. */
  readonly getJob: (id: string, signal: AbortSignal) => Promise<Job>;
  /** `GET /v1/background`: the server's worker. */
  readonly getBackground: (signal: AbortSignal) => Promise<BackgroundStatus>;
  /** Asks for a verification of a revision, under the request key `key`. */
  readonly enqueueJob: (
    request: EnqueueJobRequest,
    key: string,
  ) => Promise<EnqueueJobResponse>;
  /** Asks for the cancellation of a job, under the request key `key`. */
  readonly cancelJob: (id: string, key: string) => Promise<CancelJobResponse>;
}

/**
 * The ports of one artifact's jobs on `client`. A client whose fetch is the
 * page's `RequestGate` holds each read in the request queue of 0007 and sends
 * each write at once (rule 15).
 */
export function portsFor(
  client: ArtifreeClient,
  artifactId: string,
): JobsPorts {
  return {
    listJobs: (query, signal) => client.listJobs(query, { artifactId, signal }),
    getJob: (id, signal) => client.getJob(id, { signal }),
    getBackground: (signal) => client.getBackground({ signal }),
    enqueueJob: (request, key) =>
      client.enqueueJob(artifactId, request, { idempotencyKey: key }),
    cancelJob: (id, key) => client.cancelJob(id, { idempotencyKey: key }),
  };
}

/** The time and the timers the driver uses. */
export interface JobsClock {
  /** The current time, in milliseconds. */
  readonly now: () => number;
  /**
   * Calls `callback` after `ms`. Returns the function that cancels the call.
   */
  readonly after: (callback: () => void, ms: number) => () => void;
}

/** The browser's clock and timers. */
export const systemJobsClock: JobsClock = {
  now: () => Date.now(),
  after: (callback, ms) => {
    const timer = setTimeout(callback, ms);
    return () => clearTimeout(timer);
  },
};

/** One read the driver makes. */
type Read =
  | { readonly kind: 'first' }
  | { readonly kind: 'job'; readonly id: string }
  | { readonly kind: 'more'; readonly token: string }
  | { readonly kind: 'background' };

/** The read in flight, and the controller that a stop aborts. */
interface InFlight {
  readonly controller: AbortController;
  readonly kind: Read['kind'];
}

/** The reads owed. A flag stays set until its read is taken. */
interface Pending {
  first: boolean;
  readonly jobs: Set<string>;
  more: boolean;
  background: boolean;
}

/**
 * The row of the job `id` in `rows`, or undefined when the job is not loaded.
 */
function rowOf(rows: JobRows | undefined, id: string): Job | undefined {
  return rows?.jobs.find((job) => job.id === id);
}

/**
 * One artifact's Jobs tab. Reads run one at a time. A read whose answer a
 * later change or a committed press has overtaken is made again, not shown
 * (rule 8).
 */
export class JobsDriver {
  private readonly ports: JobsPorts;
  private readonly artifactId: string;
  private readonly clock: JobsClock;
  private readonly newKey: () => string;
  private view: JobsView = {
    rows: undefined,
    reading: false,
    readError: undefined,
    moreError: undefined,
    background: undefined,
    backgroundError: undefined,
    overdue: new Set<string>(),
    verify: IDLE,
    cancels: new Map<string, PressState>(),
  };
  private readonly listeners = new Set<() => void>();
  private running = false;
  private connected = true;
  private readonly pending: Pending = {
    first: false,
    jobs: new Set<string>(),
    more: false,
    background: false,
  };
  private inFlight: InFlight | undefined;
  // Advanced by each change that reads jobs and by each committed press. A read
  // begun in an earlier generation is stale when it answers (rule 8).
  private generation = 0;
  private seen: ReadonlyMap<string, number> = new Map<string, number>();
  private lastBackground: number | undefined;
  private stopPoll: (() => void) | undefined;
  private stopQueued: (() => void) | undefined;
  // The feed's reset count as the page last reported it. The loaded rows were
  // read under this count (rule 15); undefined until a start or a resync says it.
  private resetsSeen: number | undefined;

  constructor(
    ports: JobsPorts,
    artifactId: string,
    clock: JobsClock,
    newKey: () => string = newIdempotencyKey,
  ) {
    this.ports = ports;
    this.artifactId = artifactId;
    this.clock = clock;
    this.newKey = newKey;
  }

  /**
   * Calls `listener` after each change of the state. Returns the function that
   * removes it.
   */
  readonly subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  /** The tab's state: the same object until the state changes. */
  readonly snapshot = (): JobsView => this.view;

  /**
   * Hears one change of the artifact's feed and reads again what it makes stale
   * (rule 15). A change that reads nothing, and any change while stopped,
   * passes.
   */
  readonly follow = (change: FeedChange): void => {
    if (!this.running) return;
    const reaction = reactionTo(change, this.artifactId, this.view.rows);
    if (reaction.type === 'none') return;
    this.generation += 1;
    if (reaction.type === 'job') {
      this.pending.jobs.add(reaction.id);
    } else {
      this.pending.first = true;
    }
    this.pump();
  };

  /**
   * Starts the tab. It reads the first page and `GET /v1/background` (rules 5
   * and 15), and reads the first page every `JOB_POLL_MS` while the feed is not
   * connected. `resets` is the feed's reset count now: when it differs from the
   * one the loaded rows were read under, those rows are dropped first.
   */
  start(resets?: number): void {
    if (this.running) return;
    this.observeResets(resets);
    this.running = true;
    this.pending.first = true;
    this.pending.background = true;
    if (!this.connected) this.armPoll();
    this.pump();
  }

  /**
   * Stops the tab. A read in flight is abandoned and its answer changes
   * nothing, and no timer stays. The presses keep their state, so an answer
   * that comes later still settles its press.
   */
  stop(): void {
    this.running = false;
    this.inFlight?.controller.abort();
    this.inFlight = undefined;
    this.pending.first = false;
    this.pending.jobs.clear();
    this.pending.more = false;
    this.pending.background = false;
    this.stopPoll?.();
    this.stopPoll = undefined;
    this.stopQueued?.();
    this.stopQueued = undefined;
    if (this.view.reading) this.update({ reading: false });
  }

  /**
   * Records whether the change feed is connected. While it is not, the first
   * page is read every `JOB_POLL_MS` (rule 15).
   */
  setConnected(connected: boolean): void {
    if (connected === this.connected) return;
    this.connected = connected;
    if (connected) {
      this.stopPoll?.();
      this.stopPoll = undefined;
    } else if (this.running) {
      this.armPoll();
    }
  }

  /**
   * Reads the first page again, as rule 15 does on `reset` and on each `ready`
   * after the first, since changes may have been missed while the feed was
   * away. `resets` is the feed's reset count now. A count other than the one
   * the loaded rows were read under is a reset: those rows are dropped first,
   * since the database may be another one. Without a count, or with the same
   * one, the first page is merged with the loaded rows. The caller decides which
   * feed events call it; a stopped tab ignores the call.
   */
  resync(resets?: number): void {
    if (!this.running) return;
    this.observeResets(resets);
    this.generation += 1;
    this.pending.first = true;
    this.pump();
  }

  /**
   * Reads the page after the loaded jobs (rule 13). Ignored when every job is
   * loaded, and while a page is already being read or is owed.
   */
  loadMore(): void {
    if (!this.running || this.view.rows?.next === undefined) return;
    if (this.pending.more || this.inFlight?.kind === 'more') return;
    this.pending.more = true;
    this.pump();
  }

  /** Reads the first page again, after a failed read. */
  retry(): void {
    if (!this.running) return;
    if (this.view.readError !== undefined) {
      this.update({ readError: undefined });
    }
    this.pending.first = true;
    this.pump();
  }

  /**
   * Asks for a verification of `revision` under a new request key (rule 14).
   * Ignored while the last press of "Verify revision N" is sending.
   */
  verify(revision: number): void {
    if (this.view.verify.status === 'sending') return;
    this.send(pressVerify(revision, this.newKey));
  }

  /**
   * Sends the verification whose answer was lost again, under its own key
   * (rule 14).
   */
  retryVerify(): void {
    const slot = this.view.verify;
    if (slot.status !== 'unknown') return;
    this.send(slot.press);
  }

  /** Drops the unknown or refused press of "Verify revision N". */
  forgetVerify(): void {
    const { status } = this.view.verify;
    if (status === 'unknown' || status === 'refused') {
      this.update({ verify: IDLE });
    }
  }

  /**
   * Asks for the cancellation of a job under a new request key (rule 14).
   * Ignored while the last press for that job is sending.
   */
  cancel(jobId: string): void {
    if (this.view.cancels.get(jobId)?.status === 'sending') return;
    this.send(pressCancel(jobId, this.newKey));
  }

  /**
   * Sends the cancellation whose answer was lost again, under its own key
   * (rule 14).
   */
  retryCancel(jobId: string): void {
    const slot = this.view.cancels.get(jobId);
    if (slot?.status !== 'unknown') return;
    this.send(slot.press);
  }

  /** Drops the unknown or refused press of "Cancel" for a job. */
  forgetCancel(jobId: string): void {
    const slot = this.view.cancels.get(jobId);
    if (slot === undefined || slot.status === 'sending') return;
    const cancels = new Map(this.view.cancels);
    cancels.delete(jobId);
    this.update({ cancels });
  }

  /** Shows `press` as sending, and sends it at once. */
  private send(press: Press): void {
    this.setPress(press, { status: 'sending', press });
    void this.deliver(press).catch((error: unknown) => {
      console.error('A Jobs tab press failed unexpectedly.', error);
    });
  }

  /** Shows `state` as the latest press in the slot that `press` belongs to. */
  private setPress(press: Press, state: PressState): void {
    if (press.kind === 'verify') {
      this.update({ verify: state });
      return;
    }
    const cancels = new Map(this.view.cancels);
    cancels.set(press.jobId, state);
    this.update({ cancels });
  }

  /**
   * Empties the slot of a committed `press`: a verification's slot becomes
   * idle, and a job's cancel is forgotten.
   */
  private clearPress(press: Press): void {
    if (press.kind === 'verify') {
      this.update({ verify: IDLE });
      return;
    }
    const cancels = new Map(this.view.cancels);
    cancels.delete(press.jobId);
    this.update({ cancels });
  }

  /** Whether `press` is still the sending press of its slot. */
  private isSending(press: Press): boolean {
    const slot =
      press.kind === 'verify'
        ? this.view.verify
        : this.view.cancels.get(press.jobId);
    return slot?.status === 'sending' && slot.press === press;
  }

  /** The request that `press` makes, under the press's key. */
  private request(press: Press): Promise<{ readonly job: Job }> {
    if (press.kind === 'verify') {
      return this.ports.enqueueJob(
        { kind: 'verify_revision', revision: press.revision },
        press.key,
      );
    }
    return this.ports.cancelJob(press.jobId, press.key);
  }

  /**
   * Settles `press` with its answer. A committed press changed the job, so a
   * read that began before the answer is made again (rule 8). The answer is
   * shown unless a read has replaced the job's row since the press was sent;
   * then the job is read again instead (rule 8).
   */
  private async deliver(press: Press): Promise<void> {
    const sentRows = this.view.rows;
    let answer: { readonly job: Job };
    try {
      answer = await this.request(press);
    } catch (error: unknown) {
      if (this.isSending(press)) {
        this.setPress(press, afterFailure(press, error));
      }
      return;
    }
    this.generation += 1;
    if (this.isSending(press)) this.clearPress(press);
    // A stopped tab leaves the rows alone; it reads the list again on start.
    if (!this.running) return;
    const rows = this.view.rows;
    const id = answer.job.id;
    if (rows === undefined) {
      // Without a loaded list, the first page brings the job.
      this.pending.first = true;
    } else if (rowOf(sentRows, id) !== rowOf(rows, id)) {
      // A read has replaced the job's row since the press was sent. The order
      // of that read and the answer is not known, so the answer is not shown:
      // the job is read again instead (rule 8).
      this.pending.jobs.add(id);
    } else {
      this.update({
        rows:
          press.kind === 'verify'
            ? upsertJob(rows, answer.job)
            : replaceJob(rows, answer.job),
      });
      this.afterRows();
    }
    this.pump();
  }

  /** Starts the next read when none is in flight and one is owed. */
  private pump(): void {
    if (!this.running || this.inFlight !== undefined) return;
    const read = this.takeRead();
    if (read === undefined) {
      if (this.view.reading) this.update({ reading: false });
      return;
    }
    const controller = new AbortController();
    this.inFlight = { controller, kind: read.kind };
    if (!this.view.reading) this.update({ reading: true });
    void this.run(read, controller).catch((error: unknown) => {
      console.error('A Jobs tab read failed unexpectedly.', error);
    });
  }

  /**
   * Takes the next owed read: first page, then jobs, then the next page, then
   * the worker.
   */
  private takeRead(): Read | undefined {
    if (this.pending.first) {
      this.pending.first = false;
      return { kind: 'first' };
    }
    const next = this.pending.jobs.values().next();
    if (next.done !== true) {
      this.pending.jobs.delete(next.value);
      return { kind: 'job', id: next.value };
    }
    if (this.pending.more) {
      this.pending.more = false;
      const token = this.view.rows?.next;
      if (token !== undefined) return { kind: 'more', token };
    }
    if (this.pending.background) {
      this.pending.background = false;
      return { kind: 'background' };
    }
    return undefined;
  }

  /** Makes one read and, when it still counts, applies its answer. */
  private async run(read: Read, controller: AbortController): Promise<void> {
    const generation = this.generation;
    try {
      await this.perform(read, controller.signal, generation);
    } finally {
      if (this.inFlight?.controller === controller) {
        this.inFlight = undefined;
        this.afterRows();
        this.pump();
      }
    }
  }

  /** Makes `read` with its own method, which applies the answer. */
  private perform(
    read: Read,
    signal: AbortSignal,
    generation: number,
  ): Promise<void> {
    switch (read.kind) {
      case 'first':
        return this.readFirst(signal, generation);
      case 'job':
        return this.readJob(read.id, signal, generation);
      case 'more':
        return this.readMore(read.token, signal, generation);
      case 'background':
        return this.readBackground(signal);
      default: {
        const unhandled: never = read;
        throw new Error(`Unhandled read: ${JSON.stringify(unhandled)}`);
      }
    }
  }

  /**
   * Reads the first page and merges it with the loaded jobs (rules 13 and 15).
   */
  private async readFirst(
    signal: AbortSignal,
    generation: number,
  ): Promise<void> {
    let list: JobList;
    try {
      list = await this.ports.listJobs({ limit: JOB_PAGE_SIZE }, signal);
    } catch (error: unknown) {
      // A read that stop() abandoned has no failure to show.
      if (!signal.aborted) this.update({ readError: describeFailure(error) });
      return;
    }
    if (signal.aborted) return;
    if (generation !== this.generation) {
      this.pending.first = true;
      return;
    }
    this.update({
      rows: mergeFirstPage(this.view.rows, list),
      readError: undefined,
    });
  }

  /** Reads one job again and replaces its row (rule 15). */
  private async readJob(
    id: string,
    signal: AbortSignal,
    generation: number,
  ): Promise<void> {
    let job: Job;
    try {
      job = await this.ports.getJob(id, signal);
    } catch (error: unknown) {
      if (!signal.aborted) this.update({ readError: describeFailure(error) });
      return;
    }
    if (signal.aborted) return;
    if (generation !== this.generation) {
      this.pending.jobs.add(id);
      return;
    }
    // Only a loaded list holds the row to replace.
    const rows = this.view.rows;
    if (rows !== undefined) this.update({ rows: replaceJob(rows, job) });
  }

  /** Reads the page after the loaded jobs and adds its jobs (rule 13). */
  private async readMore(
    token: string,
    signal: AbortSignal,
    generation: number,
  ): Promise<void> {
    let list: JobList;
    try {
      list = await this.ports.listJobs(
        { limit: JOB_PAGE_SIZE, page_token: token },
        signal,
      );
    } catch (error: unknown) {
      if (!signal.aborted) this.update({ moreError: describeFailure(error) });
      return;
    }
    if (signal.aborted) return;
    if (generation !== this.generation) {
      this.pending.more = true;
      return;
    }
    const rows = this.view.rows;
    if (rows === undefined) {
      // A page after the loaded jobs needs them; without them, read the list.
      this.pending.first = true;
      return;
    }
    this.update({ rows: appendPage(rows, list), moreError: undefined });
  }

  /**
   * Reads `GET /v1/background` (rule 5). The read's start is recorded when it
   * begins, so the next read is timed from it while this one is in flight, and
   * a failed read waits for the same schedule.
   */
  private async readBackground(signal: AbortSignal): Promise<void> {
    this.lastBackground = this.clock.now();
    let status: BackgroundStatus;
    try {
      status = await this.ports.getBackground(signal);
    } catch (error: unknown) {
      if (!signal.aborted) {
        this.update({ backgroundError: describeFailure(error) });
      }
      return;
    }
    if (signal.aborted) return;
    this.update({ background: status, backgroundError: undefined });
  }

  /**
   * Takes the feed's reset count (rule 15). A count other than the one the loaded
   * rows were read under means the server's data may be another database's, so
   * those rows are dropped. The caller reads the first page.
   */
  private observeResets(resets: number | undefined): void {
    if (resets === undefined) return;
    const replaced =
      this.resetsSeen !== undefined && this.resetsSeen !== resets;
    this.resetsSeen = resets;
    if (replaced) this.forgetRows();
  }

  /**
   * Drops the loaded rows, their next page and the reads owed for them, since the
   * database may be another one (rule 15). A read of the list or of a job in
   * flight is abandoned, so its answer cannot join rows it no longer belongs to.
   * The worker's read does not depend on the rows and is left to finish. The
   * presses keep their state (rule 14).
   */
  private forgetRows(): void {
    this.generation += 1;
    this.pending.more = false;
    this.pending.jobs.clear();
    const flight = this.inFlight;
    if (flight !== undefined && flight.kind !== 'background') {
      flight.controller.abort();
      this.inFlight = undefined;
    }
    this.update({ rows: undefined, moreError: undefined });
    this.afterRows();
  }

  /** Updates the first-seen times of the queued jobs, then their timers. */
  private afterRows(): void {
    this.seen = seenQueued(this.seen, this.view.rows, this.clock.now());
    this.rearmQueued();
  }

  /**
   * Arms one timer for the next queued notice (rule 14) or background read
   * (rule 5), whichever is due first. An owed background read is not timed:
   * the pump starts it when the read in flight ends.
   */
  private rearmQueued(): void {
    this.stopQueued?.();
    this.stopQueued = undefined;
    const now = this.clock.now();
    this.setOverdue(overdueJobs(this.seen, now));
    if (!this.running) return;
    const readAt = this.pending.background
      ? undefined
      : nextBackgroundRead(this.seen, this.lastBackground);
    const times = [nextOverdueAt(this.seen, now), readAt].filter(
      (time): time is number => time !== undefined,
    );
    if (times.length > 0) {
      this.stopQueued = this.clock.after(
        () => this.onQueuedDue(),
        Math.max(0, Math.min(...times) - now),
      );
    }
  }

  /** The timer of `rearmQueued` fired: a read or a notice is due. */
  private onQueuedDue(): void {
    this.stopQueued = undefined;
    if (!this.running) return;
    const readAt = nextBackgroundRead(this.seen, this.lastBackground);
    if (readAt !== undefined && readAt <= this.clock.now()) {
      this.pending.background = true;
    }
    this.pump();
    this.rearmQueued();
  }

  /** Replaces the overdue set only when a job joins or leaves it. */
  private setOverdue(next: ReadonlySet<string>): void {
    const current = this.view.overdue;
    const same =
      current.size === next.size && [...next].every((id) => current.has(id));
    if (!same) this.update({ overdue: next });
  }

  /**
   * Arms the poll that reads the first page every `JOB_POLL_MS` while the feed
   * is not connected (rule 15).
   */
  private armPoll(): void {
    this.stopPoll?.();
    this.stopPoll = this.clock.after(() => this.onPoll(), JOB_POLL_MS);
  }

  /** One poll tick: reads the first page, then waits for the next tick. */
  private onPoll(): void {
    this.stopPoll = undefined;
    if (!this.running || this.connected) return;
    this.pending.first = true;
    this.pump();
    this.armPoll();
  }

  /**
   * Merges `patch` into the state, then tells each listener. A listener that
   * throws is reported, and the others still hear the change.
   */
  private update(patch: Partial<JobsView>): void {
    this.view = { ...this.view, ...patch };
    for (const listener of this.listeners) {
      try {
        listener();
      } catch (error: unknown) {
        console.error('A Jobs tab listener failed.', error);
      }
    }
  }
}
