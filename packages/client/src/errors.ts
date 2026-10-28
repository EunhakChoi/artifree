import type { ErrorCode } from '@artifree/contracts';

/** Why a request failed: a server error code or a client-side failure. */
export type FailureCode = ErrorCode | 'network_error' | 'invalid_response';

/** A failed API request. */
export class ArtifreeApiError extends Error {
  /** HTTP status, or 0 when no response arrived. */
  readonly status: number;
  readonly code: FailureCode;
  /** The current revision when `code` is `revision_conflict`. */
  readonly currentRevision: number | undefined;
  /** The current metadata revision when `code` is `metadata_conflict`. */
  readonly currentMetadataRevision: number | undefined;
  /** Seconds to wait before retrying, from `Retry-After`, when given. */
  readonly retryAfterSeconds: number | undefined;

  constructor(
    message: string,
    details: {
      status: number;
      code: FailureCode;
      currentRevision?: number;
      currentMetadataRevision?: number;
      retryAfterSeconds?: number;
      cause?: unknown;
    },
  ) {
    super(message, { cause: details.cause });
    this.name = 'ArtifreeApiError';
    this.status = details.status;
    this.code = details.code;
    this.currentRevision = details.currentRevision;
    this.currentMetadataRevision = details.currentMetadataRevision;
    this.retryAfterSeconds = details.retryAfterSeconds;
  }
}
