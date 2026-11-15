/**
 * What the web knows of an artifact's head build status (decision 0058, rule
 * 16), shared by the Jobs tab (R03ea) and the preview's build choice (R03eb).
 */
import { ArtifreeApiError } from '@artifree/client';
import type { BuildStatus } from '@artifree/contracts';
import { describeFailure } from './failure';

/**
 * Not read yet, read, refused as `profile_unknown`, or a read that failed, in
 * words.
 */
export type BuildStatusState =
  | { readonly kind: 'unread' }
  | { readonly kind: 'read'; readonly status: BuildStatus }
  | { readonly kind: 'profile_unknown' }
  | { readonly kind: 'failed'; readonly message: string };

/** The head build status before its first read. */
export const STATUS_UNREAD: BuildStatusState = { kind: 'unread' };

/**
 * The status state after a read of it failed with `error`. A refusal as
 * `profile_unknown` says that the server cannot build the profile, which a
 * later read does not change until the feed resets; any other failure is shown
 * in words.
 */
export function statusAfterFailure(error: unknown): BuildStatusState {
  if (error instanceof ArtifreeApiError && error.code === 'profile_unknown') {
    return { kind: 'profile_unknown' };
  }
  return { kind: 'failed', message: describeFailure(error) };
}
