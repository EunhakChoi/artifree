/**
 * How the page reads a failed API call. Plain code without React, so rules
 * built on it are bundled and tested on their own.
 */
import { ArtifreeApiError } from '@artifree/client';

/** A readable message for a failed API call. */
export function describeFailure(error: unknown): string {
  if (error instanceof ArtifreeApiError) {
    if (error.code === 'unauthenticated')
      return 'The access token was rejected. Reconnect with the token from the server data directory.';
    if (error.code === 'network_error')
      return 'The Artifree server is not reachable. Start artifree-server serve and try again.';
    return error.message;
  }
  return error instanceof Error ? error.message : 'Unexpected failure.';
}

/**
 * Whether a failed write may still have been committed: its answer was lost
 * or unreadable, or the server or a proxy failed after the request reached
 * it. A refusal (4xx, including a rate limit) committed nothing.
 */
export function mayHaveCommitted(error: unknown): boolean {
  if (!(error instanceof ArtifreeApiError)) return true;
  return (
    error.code === 'network_error' ||
    error.code === 'invalid_response' ||
    error.status >= 500
  );
}
