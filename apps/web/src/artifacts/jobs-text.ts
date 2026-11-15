/**
 * The words of the Jobs tab on an artifact's page (decision 0055, rules 13 and
 * 14, and decision 0058, R03ea). Plain code without React, so the wording is
 * bundled and tested on its own.
 */
import type {
  BackgroundStatus,
  BuildStatus,
  DiagnosticLocation,
  HeadBuildStatus,
  JobBuild,
  JobFailure,
  JobKind,
  JobProblem,
  JobResult,
  JobState,
  JobVerdict,
} from '@artifree/contracts';
import { BUILD_PROFILE } from './jobs-model';
import type { NamedRefusal, Refusal } from './jobs-model';

/** The name of each kind of job. */
const KIND_LABELS: Record<JobKind, string> = {
  verify_revision: 'Verify revision',
  run_profile: 'Build',
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
  execution_unavailable:
    'No execution worker could run the build: none claimed it in time, or its sandbox could not start.',
  build_failed: 'The build reported errors.',
  limit_exceeded:
    'The build ran past a limit of its sandbox: time, memory or log size.',
  output_rejected: 'The build produced output that does not fit the template.',
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

/**
 * What the tab says while the server cannot build the profile: the banner and
 * a refused "Build revision N" (decision 0058, R03ea).
 */
export const PROFILE_UNKNOWN_TEXT = `This server cannot build the ${BUILD_PROFILE} profile (profile_unknown).`;

/** The words of each refusal that has its own words (rule 14). */
const REFUSAL_TEXT: Record<NamedRefusal, string> = {
  too_many_jobs:
    'Too many jobs are waiting or running (too_many_jobs). Wait for one to end, or cancel one.',
  job_ended:
    'This job has already ended, so it cannot be cancelled (job_ended).',
  profile_unknown: PROFILE_UNKNOWN_TEXT,
};

/**
 * The words for a refused request. `too_many_jobs`, `job_ended` and
 * `profile_unknown` have their own words; any other refusal shows the message
 * it came with.
 */
export function refusalText(refusal: Refusal, message: string): string {
  return refusal === 'other' ? message : REFUSAL_TEXT[refusal];
}

/** The files and bytes a succeeded build published (decision 0058, R03ea). */
export function buildText(build: JobBuild): string {
  const files = build.file_count === 1 ? 'file' : 'files';
  const bytes = build.byte_count === 1 ? 'byte' : 'bytes';
  return `${build.file_count} ${files} built, ${build.byte_count} ${bytes}`;
}

/** Where a build diagnostic points, as `path:line:column`. */
export function locationText(location: DiagnosticLocation): string {
  return `${location.path}:${location.line}:${location.column}`;
}

/**
 * The notice of a build queued past `QUEUED_NOTICE_MS` while no execution worker
 * is ready (decision 0058, R03ea).
 */
export const NO_EXECUTION_TEXT =
  'No execution worker is ready, so this build waits';

/** What the notice adds: how a build that keeps waiting ends. */
export const NO_EXECUTION_DETAIL =
  'If none becomes ready, the build ends as execution_unavailable after ten minutes.';

/** How the banner names each head status. */
const HEAD_STATUS_TEXT: Record<HeadBuildStatus, string> = {
  built: 'built',
  pending: 'build pending',
  failed: 'build failed',
  none: 'not built',
};

/**
 * The banner's title: the head revision named as the newest, and its status
 * (decision 0058, R03ea).
 */
export function headStatusTitle(status: BuildStatus): string {
  return `Revision ${status.head_revision}, the newest: ${HEAD_STATUS_TEXT[status.status]}`;
}

/**
 * The banner's sentences after its title: the failure in words, the job that
 * sets the status, and the latest build's revision when it is not the head.
 * Diagnostics are shown in the failed build's row only.
 */
export function headStatusLines(status: BuildStatus): readonly string[] {
  const lines: string[] = [];
  if (status.status === 'failed' && status.failure) {
    lines.push(failureText(status.failure));
  }
  if (typeof status.job_id === 'string') {
    lines.push(`Job ${status.job_id}.`);
  }
  const latest = status.latest_build_revision;
  if (typeof latest === 'number') {
    if (latest !== status.head_revision) {
      lines.push(`The latest build is of revision ${latest}.`);
    }
  } else {
    lines.push('No revision has a build yet.');
  }
  return lines;
}

/** What the tab says when an artifact has no jobs. */
export const NO_JOBS_TEXT = 'No jobs yet.';
