/**
 * The build choice of an artifact's preview (decision 0058, rules 14 to 16,
 * R03eb): the shown revision's build in the preview sandbox, the latest build
 * with a notice when the newest revision has none, or why no build is shown.
 * A build preview is a plain sandboxed frame: it has no runtime bridge.
 */
import { useEffect, useState } from 'react';
import type { ArtifreeClient } from '@artifree/client';
import {
  Alert,
  AlertDescription,
  AlertStatus,
  AlertTitle,
} from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import type { ArtifactChanges } from './artifact-changes';
import { BUILD_PROFILE } from './build-profile';
import {
  PROFILE_UNKNOWN_TEXT,
  buildTarget,
  missingBuildText,
  noBuildText,
  noticeText,
  statusOutcomeOf,
  ticketOutcomeOf,
} from './build-preview-model';
import type {
  BuildNotice,
  StatusOutcome,
  TicketOutcome,
} from './build-preview-model';
import { PREVIEW_SANDBOX } from './preview-sandbox';
import { useRequest } from './use-request';
import type { RequestState } from './use-request';

/** The props of the build choice. */
export interface BuildPreviewProps {
  readonly client: ArtifreeClient;
  readonly artifactId: string;
  /** The revision the page shows. */
  readonly revision: number;
  /** The newest revision the page knows. */
  readonly newest: number;
  /** The feed epoch the revision was read in; a new one reads again. */
  readonly epoch: number;
  /**
   * Whether the revision number predates a possible data replacement: nothing
   * is read for it then.
   */
  readonly stale: boolean;
  /** The artifact's change feed: a build's changes read the status again. */
  readonly changes: ArtifactChanges;
  /** How many times the feed reset; a reset reads the status again. */
  readonly resets: number;
  readonly title: string;
  readonly className?: string;
}

/**
 * The build choice. The head status is read only while the newest revision is
 * shown, again on each `job_changed` and `revised` change of the artifact,
 * and after a reset; a `profile_unknown` answer is not read again until a
 * reset.
 */
export function BuildPreview({
  client,
  artifactId,
  revision,
  newest,
  epoch,
  stale,
  changes,
  resets,
  title,
  className,
}: BuildPreviewProps) {
  const [tick, setTick] = useState(0);
  const [attempt, setAttempt] = useState(0);
  const needsStatus = !stale && revision >= newest;
  const status = useRequest<StatusOutcome>(
    needsStatus
      ? `${artifactId}:${epoch}:${resets}:${newest}:${tick}`
      : undefined,
    async (signal) => {
      try {
        return {
          kind: 'read',
          status: await client.getBuildStatus(artifactId, BUILD_PROFILE, {
            signal,
          }),
        };
      } catch (error: unknown) {
        if (signal.aborted) throw error;
        return statusOutcomeOf(error);
      }
    },
    { keepPrevious: true },
  );
  const outcome = status.status === 'ready' ? status.value : undefined;
  const refused = outcome?.kind === 'profile_unknown';

  useEffect(() => {
    if (!needsStatus || refused) return undefined;
    return changes.subscribe(artifactId, (change) => {
      if (change.change === 'job_changed' || change.change === 'revised') {
        setTick((value) => value + 1);
      }
    });
  }, [changes, artifactId, needsStatus, refused]);

  const target = stale
    ? ({ kind: 'wait' } as const)
    : buildTarget(revision, newest, outcome);
  const ticketRevision = target.kind === 'ticket' ? target.revision : undefined;
  const ticket = useRequest<TicketOutcome>(
    ticketRevision === undefined
      ? undefined
      : `${artifactId}:${ticketRevision}:${epoch}:${attempt}`,
    async (signal) => {
      try {
        const preview = await client.createBuildPreview(
          artifactId,
          ticketRevision ?? revision,
          BUILD_PROFILE,
          { signal },
        );
        return {
          kind: 'issued',
          url: preview.url,
          revision: preview.revision,
        };
      } catch (error: unknown) {
        if (signal.aborted) throw error;
        return ticketOutcomeOf(error);
      }
    },
  );

  switch (target.kind) {
    case 'wait':
      return <Waiting stale={stale} />;
    case 'none':
      return <Said text={noBuildText(target.head)} />;
    case 'profile_unknown':
      return <Said text={PROFILE_UNKNOWN_TEXT} />;
    case 'failed':
      return (
        <Failure
          message={target.message}
          onRetry={() => setTick((v) => v + 1)}
        />
      );
    case 'ticket':
      return (
        <Ticket
          ticket={ticket}
          revision={target.revision}
          notice={target.notice}
          title={title}
          className={className}
          onRefresh={() => setAttempt((value) => value + 1)}
        />
      );
    default: {
      const unhandled: never = target;
      throw new Error(`Unhandled build target: ${JSON.stringify(unhandled)}`);
    }
  }
}

/** The props of an issued, refused or pending ticket. */
interface TicketProps {
  readonly ticket: RequestState<TicketOutcome>;
  readonly revision: number;
  readonly notice: BuildNotice | undefined;
  readonly title: string;
  readonly className: string | undefined;
  readonly onRefresh: () => void;
}

/** The build's frame with its notice, or why the ticket gave none. */
function Ticket({
  ticket,
  revision,
  notice,
  title,
  className,
  onRefresh,
}: TicketProps) {
  if (ticket.status === 'loading') return <Waiting stale={false} />;
  if (ticket.status === 'error') {
    return <Failure message={ticket.message} onRetry={onRefresh} />;
  }
  const value = ticket.value;
  switch (value.kind) {
    case 'not_found':
      return <Said text={missingBuildText(revision)} />;
    case 'profile_unknown':
      return <Said text={PROFILE_UNKNOWN_TEXT} />;
    case 'failed':
      return <Failure message={value.message} onRetry={onRefresh} />;
    case 'issued':
      return (
        <div className="flex flex-col gap-3">
          {notice === undefined ? null : <Said text={noticeText(notice)} />}
          <div>
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={onRefresh}
            >
              Refresh preview
            </Button>
          </div>
          <iframe
            title={title}
            src={value.url}
            sandbox={PREVIEW_SANDBOX}
            referrerPolicy="no-referrer"
            className={className}
          />
        </div>
      );
    default: {
      const unhandled: never = value;
      throw new Error(`Unhandled ticket: ${JSON.stringify(unhandled)}`);
    }
  }
}

/** A statement in place of the build, as a polite status region. */
export function Said({ text }: { readonly text: string }) {
  return (
    <AlertStatus>
      <AlertDescription>
        <p>{text}</p>
      </AlertDescription>
    </AlertStatus>
  );
}

/** A read that is waiting, or waits for the artifact to be read again. */
function Waiting({ stale }: { readonly stale: boolean }) {
  return (
    <output className="text-sm text-muted-foreground">
      {stale
        ? 'The build is shown once the artifact is read again.'
        : 'Preparing the build preview…'}
    </output>
  );
}

/** A failed read, in words, with a way to try again. */
function Failure({
  message,
  onRetry,
}: {
  readonly message: string;
  readonly onRetry: () => void;
}) {
  return (
    <Alert variant="destructive">
      <AlertTitle>Build preview unavailable</AlertTitle>
      <AlertDescription>
        <p>{message}</p>
        <Button type="button" variant="outline" size="sm" onClick={onRetry}>
          Retry
        </Button>
      </AlertDescription>
    </Alert>
  );
}
