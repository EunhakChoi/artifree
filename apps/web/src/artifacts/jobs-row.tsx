/**
 * One job of the artifact page's Jobs tab (decision 0055, rules 13 and 14): its
 * fields, its outcome in words, Cancel with the outcome of its last press, and
 * the notice of a queued job that no worker takes. The row keeps no state of
 * its own; its parent passes in what the tab knows.
 */
import { useId } from 'react';
import type { BackgroundStatus, Job, JobResult } from '@artifree/contracts';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import {
  Card,
  CardAction,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card';
import { isEnded, workerNoticeOf } from './jobs-model';
import type { PressState } from './jobs-model';
import {
  CANCEL_NOTE,
  CANCEL_REQUESTED_TEXT,
  CANCELLED_TEXT,
  NO_WORKER_TEXT,
  checkedText,
  failureText,
  kindLabel,
  moreFindings,
  problemText,
  refusalText,
  stateLabel,
  verdictText,
  workerText,
} from './jobs-text';

/** The props of one row of the Jobs tab. */
export interface JobRowProps {
  /** The job the row shows. */
  readonly job: Job;
  /** Whether the job has been queued for longer than `QUEUED_NOTICE_MS`. */
  readonly overdue: boolean;
  /** The last answer of `GET /v1/background`, once read. */
  readonly background: BackgroundStatus | undefined;
  /** The failure of the last read of `GET /v1/background`, in words. */
  readonly backgroundError: string | undefined;
  /** The latest press of "Cancel" for this job. */
  readonly cancel: PressState;
  /** Presses "Cancel" with a new request key. */
  readonly onCancel: () => void;
  /** Sends the last press of "Cancel" again, with its own request key. */
  readonly onRetryCancel: () => void;
  /** Drops the last press of "Cancel" for this job; nothing is sent again. */
  readonly onForgetCancel: () => void;
}

/**
 * One job: its kind and pinned revision, state, attempts and times, what it
 * found, and "Cancel" with its wording (rules 13 and 14).
 */
export function JobRow(props: JobRowProps) {
  const {
    job,
    overdue,
    background,
    backgroundError,
    cancel,
    onCancel,
    onRetryCancel,
    onForgetCancel,
  } = props;
  const noteId = useId();
  const ended = isEnded(job.state);
  return (
    <li>
      <Card size="sm">
        <CardHeader>
          <CardTitle>{`${kindLabel(job.kind)} ${job.revision}`}</CardTitle>
          <CardAction>
            <Badge variant="secondary">{stateLabel(job.state)}</Badge>
          </CardAction>
          <CardDescription>
            <div className="flex flex-wrap gap-x-4 gap-y-1">
              <span>
                Attempt {job.attempt} of {job.max_attempts}
              </span>
              <span>
                Asked <Time ms={job.created_at_ms} />
              </span>
              {ended && typeof job.finished_at_ms === 'number' ? (
                <span>
                  Ended <Time ms={job.finished_at_ms} />
                </span>
              ) : null}
              {job.state === 'retry_scheduled' &&
              typeof job.retry_at_ms === 'number' ? (
                <span>
                  Next attempt <Time ms={job.retry_at_ms} />
                </span>
              ) : null}
            </div>
          </CardDescription>
        </CardHeader>
        <CardContent>
          <div className="flex flex-col gap-3">
            {job.state === 'succeeded' && job.result ? (
              <ResultDetails result={job.result} />
            ) : null}
            {job.state === 'failed' && job.failure ? (
              <p>{failureText(job.failure)}</p>
            ) : null}
            {job.state === 'cancelled' ? <p>{CANCELLED_TEXT}</p> : null}
            {!ended && job.cancel_requested ? (
              <p>{CANCEL_REQUESTED_TEXT}</p>
            ) : null}
            {job.state === 'queued' && overdue ? (
              <WorkerNotice
                background={background}
                backgroundError={backgroundError}
              />
            ) : null}
            {!ended ? (
              <div className="flex flex-col items-start gap-1">
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  aria-describedby={noteId}
                  disabled={cancel.status === 'sending'}
                  onClick={onCancel}
                >
                  Cancel
                </Button>
                <p id={noteId} className="text-sm text-muted-foreground">
                  {CANCEL_NOTE}
                </p>
              </div>
            ) : null}
            {/* An ended job is offered no retry of a Cancel (rule 14). */}
            {cancel.status === 'unknown' && !ended ? (
              <Alert>
                <AlertTitle>Outcome unknown</AlertTitle>
                <AlertDescription>{cancel.message}</AlertDescription>
                <div className="flex flex-wrap gap-2">
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    onClick={onRetryCancel}
                  >
                    Retry cancel
                  </Button>
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    onClick={onForgetCancel}
                  >
                    Forget this attempt
                  </Button>
                </div>
              </Alert>
            ) : null}
            {cancel.status === 'refused' ? (
              <Alert variant="destructive">
                <AlertDescription>
                  {refusalText(cancel.refusal, cancel.message)}
                </AlertDescription>
                <div className="flex flex-wrap gap-2">
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    onClick={onForgetCancel}
                  >
                    Dismiss
                  </Button>
                </div>
              </Alert>
            ) : null}
          </div>
        </CardContent>
      </Card>
    </li>
  );
}

/** A succeeded job's verdict, what it checked and its findings (rule 13). */
function ResultDetails({ result }: { result: JobResult }) {
  const more = moreFindings(result);
  return (
    <>
      <p>{verdictText(result.verdict)}</p>
      <p>{checkedText(result)}</p>
      {result.mismatches.length > 0 ? (
        <ul aria-label="Findings" className="flex flex-col gap-1">
          {result.mismatches.map((mismatch) => (
            <li key={mismatch.path}>
              <code>{mismatch.path}</code>: {problemText(mismatch.problem)}
            </li>
          ))}
        </ul>
      ) : null}
      {more > 0 ? <p>and {more} more</p> : null}
    </>
  );
}

/** The props of the notice about a queued job that no worker takes. */
interface WorkerNoticeProps {
  /** The last answer of `GET /v1/background`, once read. */
  readonly background: BackgroundStatus | undefined;
  /** The failure of the last read of `GET /v1/background`, in words. */
  readonly backgroundError: string | undefined;
}

/**
 * The notice of a job queued past `QUEUED_NOTICE_MS` (rule 14): the worker's
 * mode and state when no worker runs, or that the worker could not be read.
 */
function WorkerNotice({ background, backgroundError }: WorkerNoticeProps) {
  const notice = workerNoticeOf(background, backgroundError);
  if (notice === undefined) return null;
  switch (notice.kind) {
    case 'unread':
      return (
        <Alert>
          <AlertTitle>Could not read whether a worker is running</AlertTitle>
          <AlertDescription>{notice.message}</AlertDescription>
        </Alert>
      );
    case 'no_worker':
      return (
        <Alert>
          <AlertTitle>{NO_WORKER_TEXT}</AlertTitle>
          <AlertDescription>{workerText(notice.status)}</AlertDescription>
        </Alert>
      );
    default: {
      const unhandled: never = notice;
      throw new Error(`Unhandled worker notice: ${JSON.stringify(unhandled)}`);
    }
  }
}

/** When a job event happened; a time outside the date range is shown as is. */
function Time({ ms }: { ms: number }) {
  const date = new Date(ms);
  if (Number.isNaN(date.getTime())) return <span>{ms} ms</span>;
  return <time dateTime={date.toISOString()}>{date.toLocaleString()}</time>;
}
