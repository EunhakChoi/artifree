/**
 * The Jobs tab of an artifact's page (decision 0055, rules 13 to 15): the
 * artifact's jobs, newest first, with "Verify revision N", the outcome of its
 * last press, "Cancel" and "Load more". The page loads this module with
 * `React.lazy`, so it comes with the tab and not with the page.
 */
import { useEffect, useRef, useState, useSyncExternalStore } from 'react';
import { RotateCwIcon } from 'lucide-react';
import type { ArtifreeClient } from '@artifree/client';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import { Empty, EmptyHeader, EmptyTitle } from '@/components/ui/empty';
import type { ArtifactChanges } from './artifact-changes';
import { JobsDriver, portsFor, systemJobsClock } from './jobs-driver';
import { IDLE } from './jobs-model';
import type { PressState } from './jobs-model';
import { JobRow } from './jobs-row';
import { NO_JOBS_TEXT, refusalText } from './jobs-text';

/** The props of the Jobs tab of one artifact's page. */
export interface JobsPanelProps {
  /** The client the tab's reads and presses go through. */
  readonly client: ArtifreeClient;
  /** The artifact whose jobs the tab lists. */
  readonly artifactId: string;
  /**
   * The driver the page holds for this artifact, or undefined before the tab
   * has mounted. A later mount reuses it, so a press keeps its state and its
   * request key while the tab is away (rule 14).
   */
  readonly driver: JobsDriver | undefined;
  /** Receives the driver the first mount made, so that the page keeps it. */
  readonly onDriver: (driver: JobsDriver) => void;
  /** The revision the page shows; "Verify revision N" asks for it (rule 14). */
  readonly revision: number;
  /** The artifact's change feed, followed while mounted (rule 15). */
  readonly changes: ArtifactChanges;
  /** Whether the change feed is connected; the tab polls while it is not. */
  readonly connected: boolean;
  /**
   * How many times the page's earlier reads may have gone stale: on each reset,
   * and on each ready once the page may read (rule 15). A ready keeps the
   * server's data, so the tab takes the reset count from `changes` to tell the
   * two apart.
   */
  readonly resets: number;
  /**
   * Whether the page shows an artifact read before its data may have been
   * replaced. The tab then shows only the wait and keeps its state, so a press
   * whose answer is lost can still be retried (rule 14).
   */
  readonly stale: boolean;
}

/**
 * The Jobs tab. It reads the list when it mounts, follows the artifact's
 * changes while it is mounted, and reads the first page again whenever
 * `resets` changes. A reset of the feed drops the rows loaded before it, since
 * the database may be another one (rule 15).
 */
export function JobsPanel({
  client,
  artifactId,
  driver: kept,
  onDriver,
  revision,
  changes,
  connected,
  resets,
  stale,
}: JobsPanelProps) {
  // The first mount makes the driver and hands it to the page, so a later mount
  // reuses it (rule 14). The workspace keys the artifact's detail by its id, so
  // the driver serves one artifact for its whole life.
  const [made] = useState(
    () =>
      kept ??
      new JobsDriver(portsFor(client, artifactId), artifactId, systemJobsClock),
  );
  const driver = kept ?? made;
  useEffect(() => {
    if (kept === undefined) onDriver(made);
  }, [kept, made, onDriver]);
  const view = useSyncExternalStore(driver.subscribe, driver.snapshot);
  // The reset count the list was last read for. The mount reads the list
  // already, so only a later change of the count reads it again.
  const seenResets = useRef(resets);

  useEffect(() => {
    // Starting comes first: the connection effect below arms the poll only
    // while the tab runs.
    driver.start(changes.resetCount());
    const off = changes.subscribe(artifactId, driver.follow);
    return () => {
      off();
      driver.stop();
    };
  }, [driver, changes, artifactId]);

  useEffect(() => {
    driver.setConnected(connected);
  }, [driver, connected]);

  useEffect(() => {
    if (seenResets.current === resets) return;
    seenResets.current = resets;
    driver.resync(changes.resetCount());
  }, [driver, changes, resets]);

  if (stale) return <p>Waiting until the artifact is read again.</p>;

  const {
    verify,
    rows,
    readError,
    moreError,
    background,
    backgroundError,
    cancels,
    overdue,
  } = view;
  const jobs = rows?.jobs;
  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-col items-start gap-2">
        <Button
          type="button"
          variant="outline"
          size="sm"
          disabled={verify.status === 'sending'}
          onClick={() => driver.verify(revision)}
        >
          Verify revision {revision}
        </Button>
        <VerifyOutcome
          verify={verify}
          onRetry={() => driver.retryVerify()}
          onForget={() => driver.forgetVerify()}
        />
      </div>
      {readError !== undefined ? (
        <Alert variant="destructive">
          <AlertTitle>Could not read the jobs</AlertTitle>
          <AlertDescription>
            <p>{readError}</p>
          </AlertDescription>
          <div className="flex flex-wrap gap-2">
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() => driver.retry()}
            >
              <RotateCwIcon data-icon="inline-start" aria-hidden="true" />
              Try again
            </Button>
          </div>
        </Alert>
      ) : null}
      {jobs === undefined && readError === undefined ? (
        <output className="text-sm text-muted-foreground">Loading jobs…</output>
      ) : null}
      {jobs !== undefined && jobs.length === 0 ? (
        <Empty>
          <EmptyHeader>
            <EmptyTitle>{NO_JOBS_TEXT}</EmptyTitle>
          </EmptyHeader>
        </Empty>
      ) : null}
      {jobs !== undefined && jobs.length > 0 ? (
        <ul aria-label="Jobs" className="flex flex-col gap-2">
          {jobs.map((job) => (
            <JobRow
              key={job.id}
              job={job}
              overdue={overdue.has(job.id)}
              background={background}
              backgroundError={backgroundError}
              cancel={cancels.get(job.id) ?? IDLE}
              onCancel={() => driver.cancel(job.id)}
              onRetryCancel={() => driver.retryCancel(job.id)}
              onForgetCancel={() => driver.forgetCancel(job.id)}
            />
          ))}
        </ul>
      ) : null}
      {rows?.next !== undefined ? (
        <div className="flex flex-col items-start gap-2">
          <LoadMore failure={moreError} onLoadMore={() => driver.loadMore()} />
        </div>
      ) : null}
    </div>
  );
}

/** The props of the outcome of the last press of "Verify revision N". */
interface VerifyOutcomeProps {
  /** The latest press of "Verify revision N". */
  readonly verify: PressState;
  /** Sends the press again, with its own request key. */
  readonly onRetry: () => void;
  /** Drops the press; nothing is sent again. */
  readonly onForget: () => void;
}

/**
 * Where the last press of "Verify revision N" stands: sending, an outcome that
 * is unknown and can be retried, or a refusal (rule 14).
 */
function VerifyOutcome({ verify, onRetry, onForget }: VerifyOutcomeProps) {
  switch (verify.status) {
    case 'idle':
      return null;
    case 'sending':
      return (
        <output className="text-sm text-muted-foreground">
          Asking for a verification…
        </output>
      );
    case 'unknown':
      return (
        <Alert>
          <AlertTitle>Outcome unknown</AlertTitle>
          <AlertDescription>
            <p>{verify.message}</p>
            <p>
              Retrying sends the same request again; it never adds a second job.
            </p>
          </AlertDescription>
          <div className="flex flex-wrap gap-2">
            {verify.press.kind === 'verify' ? (
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={onRetry}
              >
                Retry verify revision {verify.press.revision}
              </Button>
            ) : null}
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={onForget}
            >
              Forget this attempt
            </Button>
          </div>
        </Alert>
      );
    case 'refused':
      return (
        <Alert variant="destructive">
          <AlertDescription>
            <p>{refusalText(verify.refusal, verify.message)}</p>
          </AlertDescription>
          <div className="flex flex-wrap gap-2">
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={onForget}
            >
              Dismiss
            </Button>
          </div>
        </Alert>
      );
    default: {
      const unhandled: never = verify;
      throw new Error(`Unhandled press state: ${JSON.stringify(unhandled)}`);
    }
  }
}

/** The props of "Load more", with the failure of the last page read. */
interface LoadMoreProps {
  /** The failure of the last "Load more", in words. */
  readonly failure: string | undefined;
  /** Reads the page after the loaded jobs. */
  readonly onLoadMore: () => void;
}

/**
 * "Load more" for the next page of jobs (rule 13). A failed page read shows
 * its failure in words, with the same control to try again.
 */
function LoadMore({ failure, onLoadMore }: LoadMoreProps) {
  const button = (
    <Button type="button" variant="outline" size="sm" onClick={onLoadMore}>
      Load more
    </Button>
  );
  if (failure === undefined) return button;
  return (
    <Alert variant="destructive">
      <AlertTitle>Could not load more jobs</AlertTitle>
      <AlertDescription>
        <p>{failure}</p>
      </AlertDescription>
      <div className="flex flex-wrap gap-2">{button}</div>
    </Alert>
  );
}
