/**
 * The build choice of an artifact's preview (decision 0058, rules 14 to 16,
 * R03eb): the shown revision's build in the preview sandbox, the latest build
 * with a notice when the newest revision has none, or why no build is shown.
 * A build preview is a plain sandboxed frame: it has no runtime bridge.
 */
import {
  useEffect,
  useEffectEvent,
  useRef,
  useState,
  useSyncExternalStore,
} from 'react';
import type { ArtifreeClient } from '@artifree/client';
import {
  Alert,
  AlertDescription,
  AlertStatus,
  AlertTitle,
} from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import type { ArtifactChanges } from './artifact-changes';
import { BUILD_PROFILE, PROFILE_UNKNOWN_TEXT } from './build-profile';
import {
  EXPIRED_TEXT,
  buildTarget,
  missingBuildText,
  noBuildText,
  noticeText,
  statusIsBehind,
  ticketOutcomeOf,
} from './build-preview-model';
import type { BuildTarget, TicketOutcome } from './build-preview-model';
import { BuildStatusReader } from './build-status-reader';
import { PREVIEW_SANDBOX } from './preview-sandbox';
import { useRequest } from './use-request';
import type { RequestState } from './use-request';

/** The props of the build choice. */
export interface BuildPreviewProps {
  readonly client: ArtifreeClient;
  readonly artifactId: string;
  /** The artifact's title, which names the frame with the build it shows. */
  readonly artifactTitle: string;
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
  /** How many times the feed reset; a reset drops the status it read. */
  readonly resets: number;
  readonly className?: string;
}

/** A ticket target, with the words of its notice, as one comparable value. */
interface ShownTarget {
  readonly revision: number;
  readonly notice: string | undefined;
  /** The read of the data it was shown in. */
  readonly read: string;
}

/**
 * The build choice. The head status is read while the newest revision is
 * shown, again on each `job_changed` and `revised` change of the artifact, and
 * dropped and read again on a new epoch or reset. While a save is read, the
 * build shown before stays.
 */
export function BuildPreview({
  client,
  artifactId,
  artifactTitle,
  revision,
  newest,
  epoch,
  stale,
  changes,
  resets,
  className,
}: BuildPreviewProps) {
  const [reader] = useState(
    () =>
      new BuildStatusReader((signal) =>
        client.getBuildStatus(artifactId, BUILD_PROFILE, { signal }),
      ),
  );
  const status = useSyncExternalStore(reader.subscribe, reader.snapshot);
  const [attempt, setAttempt] = useState(0);
  const [recheck, setRecheck] = useState(0);
  const needsStatus = !stale && revision >= newest;
  const read = `${epoch}:${resets}`;

  // A new epoch or reset drops what was read, since the data may be another
  // database's; the first read is owed once the newest revision is shown.
  const readFor = useRef<string | undefined>(undefined);
  const behindFor = useRef<number | undefined>(undefined);
  // Leaving abandons the read in flight; a later mount owes its own first read.
  useEffect(
    () => () => {
      reader.stop();
      readFor.current = undefined;
      behindFor.current = undefined;
    },
    [reader],
  );
  useEffect(() => {
    if (!needsStatus) return;
    if (readFor.current === undefined) {
      readFor.current = read;
      reader.owe();
    } else if (readFor.current !== read) {
      readFor.current = read;
      reader.reset();
    }
  }, [reader, needsStatus, read]);

  // A status of a head older than the page knows is read again, once for each
  // newest revision, so a server answer can never make this repeat.
  useEffect(() => {
    if (!needsStatus || behindFor.current === newest) return;
    if (!statusIsBehind(status, newest)) return;
    behindFor.current = newest;
    reader.owe();
  }, [reader, needsStatus, status, newest]);

  const target: BuildTarget = stale
    ? { kind: 'wait' }
    : buildTarget(revision, newest, status);

  // While a save is read, the build shown before stays, rather than a wait;
  // only within the same read of the data.
  const [kept, setKept] = useState<ShownTarget>();
  const shown: ShownTarget | undefined =
    target.kind === 'ticket'
      ? {
          revision: target.revision,
          notice:
            target.notice === undefined ? undefined : noticeText(target.notice),
          read,
        }
      : undefined;
  if (
    shown !== undefined &&
    (kept?.read !== shown.read ||
      kept.revision !== shown.revision ||
      kept.notice !== shown.notice)
  ) {
    setKept(shown);
  }
  const showing =
    shown ?? (target.kind === 'wait' && kept?.read === read ? kept : undefined);

  const ticketKey =
    showing === undefined
      ? undefined
      : `${artifactId}:${showing.revision}:${read}:${attempt}:${recheck}`;
  const ticket = useRequest<TicketOutcome>(ticketKey, async (signal) => {
    if (showing === undefined) throw new Error('No build to show.');
    try {
      const preview = await client.createBuildPreview(
        artifactId,
        showing.revision,
        BUILD_PROFILE,
        { signal },
      );
      return {
        kind: 'issued',
        url: preview.url,
        expiresAtMs: preview.expires_at_ms,
      };
    } catch (error: unknown) {
      if (signal.aborted) throw error;
      return ticketOutcomeOf(error);
    }
  });

  const onChange = useEffectEvent((kind: string) => {
    if (kind !== 'job_changed' && kind !== 'revised') return;
    if (needsStatus) reader.owe();
    // A revision said to have no build is asked again when a job changes, so
    // a build that lands meanwhile is shown.
    if (
      kind === 'job_changed' &&
      ticket.status === 'ready' &&
      ticket.value.kind === 'not_found'
    ) {
      setRecheck((value) => value + 1);
    }
  });
  useEffect(
    () => changes.subscribe(artifactId, (change) => onChange(change.change)),
    [changes, artifactId],
  );

  if (showing !== undefined) {
    return (
      <Ticket
        key={ticketKey}
        ticket={ticket}
        revision={showing.revision}
        notice={showing.notice}
        title={`Build preview of ${artifactTitle}, revision ${showing.revision}`}
        className={className}
        onRefresh={() => setAttempt((value) => value + 1)}
      />
    );
  }
  switch (target.kind) {
    case 'wait':
    case 'ticket':
      return <Waiting stale={stale} />;
    case 'none':
      return <Said text={noBuildText(target.head)} />;
    case 'profile_unknown':
      return <Said text={PROFILE_UNKNOWN_TEXT} />;
    case 'failed':
      return <Failure message={target.message} onRetry={() => reader.owe()} />;
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
  readonly notice: string | undefined;
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
  const expiresAt =
    ticket.status === 'ready' && ticket.value.kind === 'issued'
      ? ticket.value.expiresAtMs
      : undefined;
  const [expired, setExpired] = useState(false);
  useEffect(() => {
    if (expiresAt === undefined) return undefined;
    const timer = window.setTimeout(
      () => setExpired(true),
      Math.max(0, expiresAt - Date.now()),
    );
    return () => window.clearTimeout(timer);
  }, [expiresAt]);

  if (ticket.status === 'loading') return <Waiting stale={false} />;
  if (ticket.status === 'error') {
    return <Failure message={ticket.message} onRetry={onRefresh} />;
  }
  const value = ticket.value;
  switch (value.kind) {
    case 'not_found':
      return (
        <div className="flex flex-col items-start gap-3">
          <Said text={missingBuildText(revision)} />
          <Button type="button" variant="outline" size="sm" onClick={onRefresh}>
            Check again
          </Button>
        </div>
      );
    case 'profile_unknown':
      return <Said text={PROFILE_UNKNOWN_TEXT} />;
    case 'failed':
      return <Failure message={value.message} onRetry={onRefresh} />;
    case 'issued':
      return (
        <div className="flex flex-col gap-3">
          {notice === undefined ? null : <Said text={notice} />}
          {expired ? <Said text={EXPIRED_TEXT} /> : null}
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
function Said({ text }: { readonly text: string }) {
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
