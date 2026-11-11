/**
 * The rules of the Jobs tab (decision 0055, rules 13 to 15): its rows, paging,
 * which change reads what again, the key of each press, and when a queued job
 * asks whether a worker runs. Plain code without React, so the rules are
 * bundled and tested on their own.
 */
import { ArtifreeApiError, newIdempotencyKey } from '@artifree/client';
import type { FeedChange } from '@artifree/client';
import type {
  BackgroundStatus,
  Job,
  JobList,
  JobState,
} from '@artifree/contracts';
import { describeFailure, mayHaveCommitted } from './failure';

/** Jobs per page of the list; "Load more" reads the next page (rule 13). */
export const JOB_PAGE_SIZE = 50;

/**
 * How often the first page is read while the change feed is not connected
 * (rule 15). The reads of `GET /v1/background` while a job stays queued
 * (rule 5) use this interval too, since the record sets none for them.
 */
export const JOB_POLL_MS = 15_000;

/**
 * How long a job may stay queued before the tab reads `GET /v1/background` and
 * says when no worker is running (rule 14).
 */
export const QUEUED_NOTICE_MS = 5_000;

// Terminal states never change again (the contract's JobState).
const ENDED: Record<JobState, boolean> = {
  queued: false,
  running: false,
  retry_scheduled: false,
  succeeded: true,
  failed: true,
  cancelled: true,
};

/**
 * Whether a job in `state` has ended. `ENDED` names every state, so a state
 * added to the contract does not compile until it is decided here.
 */
export function isEnded(state: JobState): boolean {
  return ENDED[state];
}

/** Jobs in listing order, newest first, and where the next page starts. */
export interface JobRows {
  /** The loaded jobs, newest first. */
  readonly jobs: readonly Job[];
  /**
   * The token of the page after the loaded jobs, undefined when every job is
   * loaded.
   */
  readonly next: string | undefined;
}

/** The first page of a list, as rows. */
export function firstPage(list: JobList): JobRows {
  return { jobs: list.jobs, next: list.next_page_token ?? undefined };
}

/**
 * Whether `job` is listed after `than`. The server lists jobs with
 * `ORDER BY created_at DESC, id` in
 * `crates/artifree-providers/src/storage/jobs.rs` at line 750, so a later row
 * is older, or has the larger id at the same creation time.
 */
export function isOlder(job: Job, than: Job): boolean {
  return (
    job.created_at_ms < than.created_at_ms ||
    (job.created_at_ms === than.created_at_ms && job.id > than.id)
  );
}

/**
 * Adds the jobs of the next page after the loaded rows, skipping any job that
 * is loaded already, and takes the page's token as the next one.
 */
export function appendPage(rows: JobRows, list: JobList): JobRows {
  const loaded = new Set(rows.jobs.map((job) => job.id));
  const added = list.jobs.filter((job) => !loaded.has(job.id));
  return {
    jobs: [...rows.jobs, ...added],
    next: list.next_page_token ?? undefined,
  };
}

/**
 * Reads the first page again (rule 15). The loaded jobs older than the new first
 * page stay after it, and the next page is the one after them, only when the
 * new first page holds a loaded job: the two runs of jobs then share a job, so
 * no job lies between them. Otherwise the jobs between the new first page and
 * the loaded ones are not known yet, so the result is the new first page alone,
 * and "Load more" reads on from it (rule 13). The result is the new first page
 * as well when nothing is loaded yet, or when the first page has no job or no
 * next page.
 */
export function mergeFirstPage(
  rows: JobRows | undefined,
  list: JobList,
): JobRows {
  const token = list.next_page_token ?? undefined;
  const last = list.jobs.at(-1);
  if (rows === undefined || token === undefined || last === undefined) {
    return firstPage(list);
  }
  const listed = new Set(list.jobs.map((job) => job.id));
  if (!rows.jobs.some((job) => listed.has(job.id))) {
    return firstPage(list);
  }
  const kept = rows.jobs.filter(
    (job) => !listed.has(job.id) && isOlder(job, last),
  );
  return {
    jobs: [...list.jobs, ...kept],
    next: kept.length > 0 ? rows.next : token,
  };
}

/**
 * Replaces the row of `job` by `job`, keeping the order. Returns `rows` itself
 * when no row has the id of `job`.
 */
export function replaceJob(rows: JobRows, job: Job): JobRows {
  if (!rows.jobs.some((listed) => listed.id === job.id)) return rows;
  return {
    jobs: rows.jobs.map((listed) => (listed.id === job.id ? job : listed)),
    next: rows.next,
  };
}

/**
 * Puts `job` in its place: replaces its row when it is listed, otherwise
 * inserts it before the first older row. A job older than every loaded row is
 * appended only when every job is loaded; otherwise it belongs to a page not
 * loaded yet, and `rows` is returned unchanged.
 */
export function upsertJob(rows: JobRows, job: Job): JobRows {
  if (rows.jobs.some((listed) => listed.id === job.id)) {
    return replaceJob(rows, job);
  }
  const index = rows.jobs.findIndex((listed) => isOlder(listed, job));
  if (index >= 0) {
    return {
      jobs: [...rows.jobs.slice(0, index), job, ...rows.jobs.slice(index)],
      next: rows.next,
    };
  }
  if (rows.next === undefined) {
    return { jobs: [...rows.jobs, job], next: rows.next };
  }
  return rows;
}

/** What the tab reads again: nothing, one job, or the first page. */
export type Reaction =
  | { readonly type: 'none' }
  | { readonly type: 'job'; readonly id: string }
  | { readonly type: 'first page' };

/**
 * What a change of the feed makes the tab read (rule 15). A `job_changed` for a
 * job the tab lists reads that job again, a `job_changed` for any other job
 * reads the first page again, and so does a `recovered` for the artifact. Other
 * changes, and changes of other artifacts, read nothing. The change carries no
 * state, so the job itself is read (rule 8).
 */
export function reactionTo(
  change: FeedChange,
  artifactId: string,
  rows: JobRows | undefined,
): Reaction {
  if (change.artifact_id !== artifactId) return { type: 'none' };
  if (change.change === 'job_changed') {
    const jobId = change.job_id;
    if (jobId !== undefined && rows?.jobs.some((job) => job.id === jobId)) {
      return { type: 'job', id: jobId };
    }
    return { type: 'first page' };
  }
  if (change.change === 'recovered') return { type: 'first page' };
  return { type: 'none' };
}

/**
 * A press of "Verify revision N" or of "Cancel". `key` is its request key: a
 * retry of the press sends the same key, and nothing makes a new one for it.
 */
export type Press =
  | { readonly kind: 'verify'; readonly revision: number; readonly key: string }
  | { readonly kind: 'cancel'; readonly jobId: string; readonly key: string };

/**
 * A press of "Verify revision N" with a new request key. Each press sends a new
 * request key; a press whose answer is lost is retried with the same key
 * (rule 14), so only a new press calls `newKey`.
 */
export function pressVerify(
  revision: number,
  newKey: () => string = newIdempotencyKey,
): Press {
  return { kind: 'verify', revision, key: newKey() };
}

/**
 * A press of "Cancel" for a job, with a new request key as `pressVerify` makes
 * one: the cancel request takes an `Idempotency-Key` (0030, rule 10).
 */
export function pressCancel(
  jobId: string,
  newKey: () => string = newIdempotencyKey,
): Press {
  return { kind: 'cancel', jobId, key: newKey() };
}

/**
 * A refusal the tab shows as such: `too_many_jobs` and `job_ended` by their
 * code, and any other refusal as `other`.
 */
export type Refusal = 'too_many_jobs' | 'job_ended' | 'other';

/**
 * Where the latest press of one kind stands. `unknown` means the answer was
 * lost, so the press may have committed and is retried with its own key;
 * `refused` means the server refused it, so it committed nothing.
 */
export type PressState =
  | { readonly status: 'idle' }
  | { readonly status: 'sending'; readonly press: Press }
  | {
      readonly status: 'unknown';
      readonly press: Press;
      readonly message: string;
    }
  | {
      readonly status: 'refused';
      readonly refusal: Refusal;
      readonly message: string;
    };

/** Nothing sent and nothing to show. */
export const IDLE: PressState = { status: 'idle' };

/**
 * The state after `press` failed with `error`. A failure that may have
 * committed stays `unknown`. A 4xx refusal committed nothing: `too_many_jobs`
 * and `job_ended` are shown as such (rule 14), and any other is `other`.
 */
export function afterFailure(press: Press, error: unknown): PressState {
  if (mayHaveCommitted(error)) {
    return { status: 'unknown', press, message: describeFailure(error) };
  }
  if (
    error instanceof ArtifreeApiError &&
    (error.code === 'too_many_jobs' || error.code === 'job_ended')
  ) {
    return {
      status: 'refused',
      refusal: error.code,
      message: describeFailure(error),
    };
  }
  return {
    status: 'refused',
    refusal: 'other',
    message: describeFailure(error),
  };
}

/**
 * The queued jobs of `rows`, each with the time this tab first saw it queued.
 * A job that was seen queued before keeps that time. A job that is no longer
 * queued, or no longer loaded, is dropped. The 5 seconds of rule 14 count from
 * that time on the browser's own clock, so a skewed server clock cannot show
 * the notice early or never.
 */
export function seenQueued(
  seen: ReadonlyMap<string, number>,
  rows: JobRows | undefined,
  now: number,
): ReadonlyMap<string, number> {
  const queued = new Map<string, number>();
  if (rows === undefined) return queued;
  for (const job of rows.jobs) {
    if (job.state === 'queued') queued.set(job.id, seen.get(job.id) ?? now);
  }
  return queued;
}

/**
 * The ids of the jobs in `seen` queued for at least `QUEUED_NOTICE_MS` by
 * `now`. These are the jobs that show the notice of rule 14.
 */
export function overdueJobs(
  seen: ReadonlyMap<string, number>,
  now: number,
): ReadonlySet<string> {
  const overdue = new Set<string>();
  for (const [id, since] of seen) {
    if (now - since >= QUEUED_NOTICE_MS) overdue.add(id);
  }
  return overdue;
}

/**
 * When the next of the jobs in `seen` passes `QUEUED_NOTICE_MS`, as a time
 * after `now`, so that a timer can show its notice; undefined when none will.
 */
export function nextOverdueAt(
  seen: ReadonlyMap<string, number>,
  now: number,
): number | undefined {
  let next: number | undefined;
  for (const since of seen.values()) {
    const at = since + QUEUED_NOTICE_MS;
    if (at > now && (next === undefined || at < next)) next = at;
  }
  return next;
}

/**
 * When the tab reads `GET /v1/background` next, given the time of its last read
 * (undefined before any). The first read comes when the earliest of the jobs in
 * `seen` passes `QUEUED_NOTICE_MS` (rule 14); later reads come every
 * `JOB_POLL_MS` again while a job stays queued (rule 5). Undefined when no job
 * is seen queued.
 */
export function nextBackgroundRead(
  seen: ReadonlyMap<string, number>,
  lastRead: number | undefined,
): number | undefined {
  let earliest: number | undefined;
  for (const since of seen.values()) {
    if (earliest === undefined || since < earliest) earliest = since;
  }
  if (earliest === undefined) return undefined;
  const first = earliest + QUEUED_NOTICE_MS;
  return lastRead === undefined || lastRead < first
    ? first
    : lastRead + JOB_POLL_MS;
}

/**
 * Whether the notice "No worker is running" applies. Only a `running` worker
 * is one; a `restarting` or a `stopped` worker shows the notice.
 */
export function showsNoWorker(status: BackgroundStatus): boolean {
  return status.worker.state !== 'running';
}

/** What a queued job's notice says about the worker (rule 14). */
export type WorkerNoticeState =
  | { readonly kind: 'unread'; readonly message: string }
  | { readonly kind: 'no_worker'; readonly status: BackgroundStatus };

/**
 * The notice of a queued job about its worker, or undefined when there is none
 * to show. A failed read is shown even after an earlier answer: that answer may
 * no longer hold, so it is not presented as the current state.
 */
export function workerNoticeOf(
  background: BackgroundStatus | undefined,
  backgroundError: string | undefined,
): WorkerNoticeState | undefined {
  if (backgroundError !== undefined) {
    return { kind: 'unread', message: backgroundError };
  }
  if (background === undefined || !showsNoWorker(background)) return undefined;
  return { kind: 'no_worker', status: background };
}

/**
 * Everything the Jobs tab shows for one artifact. `rows` stays undefined until
 * the first page answers.
 */
export interface JobsView {
  /** The loaded jobs, or undefined until the first page answers. */
  readonly rows: JobRows | undefined;
  /** Whether a read of the list is in flight. */
  readonly reading: boolean;
  /** The failure of the last read of the first page, in words. */
  readonly readError: string | undefined;
  /** The failure of the last "Load more", in words. */
  readonly moreError: string | undefined;
  /** The last answer of `GET /v1/background`, once read. */
  readonly background: BackgroundStatus | undefined;
  /** The failure of the last read of `GET /v1/background`, in words. */
  readonly backgroundError: string | undefined;
  /** The ids of the queued jobs past `QUEUED_NOTICE_MS`. */
  readonly overdue: ReadonlySet<string>;
  /** The latest press of "Verify revision N". */
  readonly verify: PressState;
  /** The latest press of "Cancel" for each job, by job id. */
  readonly cancels: ReadonlyMap<string, PressState>;
}
