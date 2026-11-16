import { ArtifreeApiError } from '@artifree/client';

/**
 * Runs one mutation under `key`. For an unknown outcome or a rate-limited
 * attempt, prints a retry hint on stderr before rethrowing. `sameRequest`
 * lists what a retry must keep identical; it completes the hint's "and the
 * same ..." clause.
 */
export async function mutate<T>(
  key: string,
  operation: (idempotencyKey: string) => Promise<T>,
  sameRequest?: string,
): Promise<T> {
  const sameClause =
    sameRequest === undefined ? '' : ` and the same ${sameRequest}`;
  try {
    return await operation(key);
  } catch (error: unknown) {
    if (
      error instanceof ArtifreeApiError &&
      ['network_error', 'unavailable', 'internal', 'invalid_response'].includes(
        error.code,
      )
    )
      process.stderr.write(
        `The outcome is unknown. Retry with --idempotency-key ${key}${sameClause} to receive the original result.\n`,
      );
    else if (error instanceof ArtifreeApiError && error.code === 'rate_limited')
      // Only this attempt was refused. An earlier attempt with the same key
      // may have committed; retrying with the key then returns its result.
      process.stderr.write(
        `This attempt was not processed. Retry after ${error.retryAfterSeconds ?? 1}s with --idempotency-key ${key}${sameClause}.\n`,
      );
    throw error;
  }
}
