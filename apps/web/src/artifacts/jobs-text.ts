/**
 * The words of the Jobs tab on an artifact's page (decision 0055, rules 13 and
 * 14). Plain code without React, so the wording is bundled and tested on its
 * own.
 */
import type {
  BackgroundStatus,
  JobFailure,
  JobKind,
  JobProblem,
  JobResult,
  JobState,
  JobVerdict,
} from '@artifree/contracts';

/** The name of each kind of job. */
const KIND_LABELS: Record<JobKind, string> = {
  verify_revision: 'Verify revision',
};

/** The name of a kind of job, as the tab shows it. */
export function kindLabel(kind: JobKind): string {
  return KIND_LABELS[kind];
}

/** The name of each state of a job. */
const STATE_LABELS: Record<JobState, string> = {
  queued: 'Queued',
  running: 'Running',
  retry_scheduled: 'Retry scheduled',
  succeeded: 'Succeeded',
  failed: 'Failed',
  cancelled: 'Cancelled',
};

/** The name of a job's state, as the tab shows it. */
export function stateLabel(state: JobState): string {
  return STATE_LABELS[state];
}

/** The sentence that states the outcome of a finished verification. */
const VERDICT_TEXT: Record<JobVerdict, string> = {
  ok: 'Every file matches its listing.',
  mismatch: 'Some files do not match their listing.',
};

/** The verdict of a verification that ran to the end, in words. */
export function verdictText(verdict: JobVerdict): string {
  return VERDICT_TEXT[verdict];
}

/** The words each failure is shown with (rule 13). */
const FAILURE_TEXT: Record<JobFailure, string> = {
  revision_unavailable:
    'The revision could not be read: its artifact was purged or its files no longer form a listing.',
  storage_unavailable: 'Storage kept failing for every attempt.',
  lease_expired: 'A worker held the final attempt and stopped answering.',
  internal: 'The worker failed in a way it did not expect.',
};

/** The failure of a job that ended as `failed`, in words (rule 13). */
export function failureText(failure: JobFailure): string {
  return FAILURE_TEXT[failure];
}

/** What is wrong with each mismatched file, as a clause after its path. */
const PROBLEM_TEXT: Record<JobProblem, string> = {
  digest_mismatch: 'its content has another digest than its listing',
  size_mismatch: 'its content has another size than its listing',
  content_missing: 'its content is not stored',
};

/** What is wrong with one mismatched file, as a clause after its path. */
export function problemText(problem: JobProblem): string {
  return PROBLEM_TEXT[problem];
}

/**
 * How many mismatched files a result does not list, which the tab shows as
 * "and N more" (rule 13). It is zero unless the list was truncated.
 */
export function moreFindings(result: JobResult): number {
  return result.truncated
    ? Math.max(0, result.mismatch_count - result.mismatches.length)
    : 0;
}

/** The files and bytes a verification read, as the tab states them. */
export function checkedText(result: JobResult): string {
  const files = result.files_checked === 1 ? 'file' : 'files';
  const bytes = result.bytes_checked === 1 ? 'byte' : 'bytes';
  return `${result.files_checked} ${files} checked, ${result.bytes_checked} ${bytes} checked`;
}

/** What Cancel says: cancelling stops the work and undoes nothing (rule 14). */
export const CANCEL_NOTE = 'Cancelling stops the work; it undoes nothing.';

/** What the tab says after a Cancel of a job that is still running. */
export const CANCEL_REQUESTED_TEXT =
  'Cancel requested. The job stops at its next checkpoint.';

/** What the tab says for a job that was cancelled. */
export const CANCELLED_TEXT = 'Cancelled. Nothing was undone.';

/**
 * The opening of the notice for a queued job while no worker runs (rule 14).
 * The tab adds the worker's mode and state after it.
 */
export const NO_WORKER_TEXT = 'No worker is running, so this job waits';

/** The worker's mode and state, as the notice shows them (rule 14). */
export function workerText(status: BackgroundStatus): string {
  return `Worker mode: ${status.worker.mode}. Worker state: ${status.worker.state}.`;
}

/** The words of each refusal that has its own words (rule 14). */
const REFUSAL_TEXT: Record<'too_many_jobs' | 'job_ended', string> = {
  too_many_jobs:
    'Too many jobs are waiting or running (too_many_jobs). Wait for one to end, or cancel one.',
  job_ended:
    'This job has already ended, so it cannot be cancelled (job_ended).',
};

/**
 * The words for a refused request. `too_many_jobs` and `job_ended` have their
 * own words; any other refusal shows the message it came with.
 */
export function refusalText(
  refusal: 'too_many_jobs' | 'job_ended' | 'other',
  message: string,
): string {
  return refusal === 'other' ? message : REFUSAL_TEXT[refusal];
}

/** What the tab says when an artifact has no jobs. */
export const NO_JOBS_TEXT = 'No jobs yet.';
