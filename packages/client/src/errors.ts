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
  /** The current data version when `code` is `data_conflict`. */
  readonly currentDataVersion: number | undefined;
  /** Seconds to wait before retrying, from `Retry-After`, when given. */
  readonly retryAfterSeconds: number | undefined;
  /**
   * The JSON the server answered with, as parsed and not otherwise checked,
   * for the modules that understand an error code the client class does
   * not (see `editFiles`). Absent when the answer was not an API error.
   */
  readonly body: unknown;

  constructor(
    message: string,
    details: {
      status: number;
      code: FailureCode;
      currentRevision?: number | undefined;
      currentMetadataRevision?: number | undefined;
      currentDataVersion?: number | undefined;
      retryAfterSeconds?: number;
      body?: unknown;
      cause?: unknown;
    },
  ) {
    super(message, { cause: details.cause });
    this.name = 'ArtifreeApiError';
    this.status = details.status;
    this.code = details.code;
    this.currentRevision = details.currentRevision;
    this.currentMetadataRevision = details.currentMetadataRevision;
    this.currentDataVersion = details.currentDataVersion;
    this.retryAfterSeconds = details.retryAfterSeconds;
    this.body = details.body;
  }
}

/**
 * A runtime call that was not sent because the runtime connection it belongs
 * to was closed or replaced. It carries its own marker so that a caller can
 * tell it from any other failure that happens to share its status and code.
 */
export class RuntimeConnectionReplacedError extends ArtifreeApiError {
  /** Always true; the marker callers test, whichever realm made the error. */
  readonly connectionReplaced = true;

  constructor() {
    super('The runtime connection was closed or replaced.', {
      status: 0,
      code: 'invalid_request',
    });
  }
}
