/**
 * The rules of the build choice of an artifact's preview (decision 0058, rules
 * 14 to 16, and the R03e split note, R03eb): which revision's build is shown,
 * when a notice names an older build, and what is said when there is none.
 * Plain code without React, so the rules are bundled and tested on their own.
 */
import { ArtifreeApiError } from '@artifree/client';
import type { HeadBuildStatus } from '@artifree/contracts';
import type { BuildStatusState } from './build-status';
import { describeFailure } from './failure';

/** What the preview shows: the revision's files, or its build. */
export type PreviewSource = 'files' | 'build';

/**
 * The build the choice shows, or why it shows none:
 *
 * - `ticket`: ask for a ticket for `revision`'s build. `notice` is set when
 *   that build is older than the head, and names the head's status.
 * - `wait`: the head status is needed and not read yet, or it describes
 *   another head than the newest one the page knows.
 * - `none`: the head has no build and no revision has one; no ticket is
 *   asked for.
 * - `profile_unknown` and `failed`: the status read was refused or failed.
 */
export type BuildTarget =
  | {
      readonly kind: 'ticket';
      readonly revision: number;
      readonly notice: BuildNotice | undefined;
    }
  | { readonly kind: 'wait' }
  | { readonly kind: 'none'; readonly head: HeadBuildStatus }
  | { readonly kind: 'profile_unknown' }
  | { readonly kind: 'failed'; readonly message: string };

/** The notice of a latest build older than the head (rule 15). */
export interface BuildNotice {
  /** The revision of the build shown. */
  readonly latest: number;
  /** The head revision, which has no build. */
  readonly head: number;
  /** Where the head's build stands: pending, failed or none. */
  readonly headStatus: HeadBuildStatus;
}

/**
 * Which build the choice shows for `shown` while `newest` is the newest
 * revision the page knows (R03eb).
 *
 * A revision older than the newest needs no status: its own build is asked
 * for, and a `not_found` says it has none, never a newer build. The newest
 * revision follows the head status: a built head is shown; otherwise the
 * latest build, older than the head, is shown with a notice, so a failed or
 * newer save never hides the last good build (rule 15). A status of another
 * head waits: a newer one means the page has not read the save yet, and an
 * older one is read again.
 */
export function buildTarget(
  shown: number,
  newest: number,
  status: BuildStatusState,
): BuildTarget {
  if (shown < newest) {
    return { kind: 'ticket', revision: shown, notice: undefined };
  }
  if (status.kind === 'unread') return { kind: 'wait' };
  if (status.kind !== 'read') return status;
  const head = status.status;
  if (head.head_revision !== shown) return { kind: 'wait' };
  if (head.status === 'built') {
    return { kind: 'ticket', revision: shown, notice: undefined };
  }
  const latest = head.latest_build_revision;
  if (typeof latest !== 'number') return { kind: 'none', head: head.status };
  return {
    kind: 'ticket',
    revision: latest,
    notice: { latest, head: head.head_revision, headStatus: head.status },
  };
}

/**
 * Whether the status describes a head older than the newest revision the page
 * knows, so it must be read again before the choice can decide.
 */
export function statusIsBehind(
  status: BuildStatusState,
  newest: number,
): boolean {
  return status.kind === 'read' && status.status.head_revision < newest;
}

/** What a ticket request gave: a URL, a refusal by code, or a failure. */
export type TicketOutcome =
  | {
      readonly kind: 'issued';
      readonly url: string;
      readonly expiresAtMs: number;
    }
  | { readonly kind: 'not_found' }
  | { readonly kind: 'profile_unknown' }
  | { readonly kind: 'failed'; readonly message: string };

/** The outcome of a ticket request that failed with `error`. */
export function ticketOutcomeOf(error: unknown): TicketOutcome {
  if (error instanceof ArtifreeApiError) {
    if (error.code === 'not_found') return { kind: 'not_found' };
    if (error.code === 'profile_unknown') return { kind: 'profile_unknown' };
  }
  return { kind: 'failed', message: describeFailure(error) };
}

/** How the notice names each head status. */
const HEAD_TEXT: Record<HeadBuildStatus, string> = {
  built: 'is built',
  pending: 'has a build pending',
  failed: 'failed to build',
  none: 'has no build',
};

/** The notice above an older build (rule 15). */
export function noticeText(notice: BuildNotice): string {
  return `Showing the build of revision ${notice.latest}, the latest build. Revision ${notice.head}, the newest, ${HEAD_TEXT[notice.headStatus]}.`;
}

/** What the choice says when no revision has a build. */
export function noBuildText(head: HeadBuildStatus): string {
  return `No revision has a build yet. The newest revision ${HEAD_TEXT[head]}.`;
}

/** What the choice says when a revision it asked for has no build. */
export function missingBuildText(revision: number): string {
  return `Revision ${revision} has no build.`;
}

/** What the choice says once a ticket has expired. */
export const EXPIRED_TEXT =
  'This build preview has expired. Refresh the preview to load it again.';
