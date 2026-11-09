/**
 * The rules of the assets panel's writes (decision 0045, R09b2 and rule 11):
 * a frozen attempt that a deliberate retry sends again unchanged, how its
 * outcome is read, and the panel's fixed wording. Plain code without React or
 * the DOM, so the rules are bundled and tested on their own.
 */
import {
  ArtifreeApiError,
  MAX_ASSET_BYTES,
  isDownloadName,
  newIdempotencyKey,
  toBase64,
} from '@artifree/client';
import type {
  DeleteAssetRequest,
  UploadAssetRequest,
} from '@artifree/contracts';
import { describeFailure, mayHaveCommitted } from './failure';

/**
 * One upload or delete as it was first sent: the artifact, the request and
 * its idempotency key, all frozen. A retry after an unknown outcome sends
 * this same object again; nothing here makes a new key for it.
 */
export type AssetAttempt =
  | {
      readonly kind: 'upload';
      readonly artifactId: string;
      readonly key: string;
      readonly request: Readonly<UploadAssetRequest>;
    }
  | {
      readonly kind: 'delete';
      readonly artifactId: string;
      readonly assetId: string;
      readonly key: string;
      readonly request: Readonly<DeleteAssetRequest>;
    };

/**
 * An upload of `bytes` declared as `contentType` against the asset space's
 * version `base`, with a new key. The bytes are encoded once, here, so later
 * changes to the caller's buffer do not reach the attempt.
 */
export function freezeUpload(
  artifactId: string,
  base: number,
  contentType: string,
  bytes: Uint8Array,
): AssetAttempt {
  // Exactly the three fields of the envelope: the server refuses others.
  const request: UploadAssetRequest = {
    base_asset_version: base,
    content_type: contentType,
    content_base64: toBase64(bytes),
  };
  return Object.freeze({
    kind: 'upload',
    artifactId,
    key: newIdempotencyKey(),
    request: Object.freeze(request),
  });
}

/** A delete of `assetId` against the asset space's version `base`, with a new key. */
export function freezeDelete(
  artifactId: string,
  assetId: string,
  base: number,
): AssetAttempt {
  const request: DeleteAssetRequest = { base_asset_version: base };
  return Object.freeze({
    kind: 'delete',
    artifactId,
    assetId,
    key: newIdempotencyKey(),
    request: Object.freeze(request),
  });
}

/**
 * How a sent attempt failed:
 * - `unknown`: the answer was lost or unreadable, or the server failed after
 *   receiving it, so the write may have been made.
 * - `conflict`: the asset space changed since the base (`asset_conflict`).
 * - `key_reused`: the key was used for a different request.
 * - `not_found`: the artifact cannot be read, whether missing or not the
 *   user's.
 * - `refused`: any other refusal, including one made locally with nothing
 *   sent.
 */
export type AttemptFailure =
  'unknown' | 'conflict' | 'key_reused' | 'refused' | 'not_found';

/** How an attempt ended: `done` for a checked receipt, else its failure. */
export type AttemptOutcome = 'done' | AttemptFailure;

/** Reads a failed attempt's error. Never `unknown` for a refusal. */
export function classify(error: unknown): AttemptFailure {
  if (mayHaveCommitted(error) || !(error instanceof ArtifreeApiError))
    return 'unknown';
  if (error.code === 'asset_conflict') return 'conflict';
  if (error.code === 'request_key_reused') return 'key_reused';
  if (error.status === 404) return 'not_found';
  return 'refused';
}

/**
 * The attempt whose outcome is unknown after `attempt` ended with `outcome`,
 * given the one `held` before. A success of the held attempt clears it; an
 * unknown outcome holds `attempt`. Any refusal keeps what was held: a later
 * conflict, key reuse or refusal does not prove that an earlier unknown
 * attempt was not committed (R09b2).
 */
export function afterOutcome(
  held: AssetAttempt | undefined,
  attempt: AssetAttempt,
  outcome: AttemptOutcome,
): AssetAttempt | undefined {
  if (outcome === 'unknown') return attempt;
  if (outcome === 'done' && held === attempt) return undefined;
  return held;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/**
 * The `current_asset_version` an `asset_conflict` names in its error envelope
 * (`{ error: { current_asset_version } }`), when it is a safe non-negative
 * integer; the client has no typed field for it.
 */
export function conflictVersion(error: ArtifreeApiError): number | undefined {
  const { body } = error;
  if (!isRecord(body) || !isRecord(body.error)) return undefined;
  const current = body.error.current_asset_version;
  return typeof current === 'number' &&
    Number.isSafeInteger(current) &&
    current >= 0
    ? current
    : undefined;
}

/** The local refusal of a file over 5 MiB, given its size before it is read. */
export const OVERSIZE_TEXT =
  'The file is larger than 5 MiB, so it was not read or sent.';

/**
 * Why a refused attempt or read failed, in fixed words per error code. A
 * response body or URL is never repeated.
 */
export function refusalText(error: unknown): string {
  if (!(error instanceof ArtifreeApiError)) return describeFailure(error);
  switch (error.code) {
    case 'asset_type_unsupported':
      return 'This file type is not accepted. Uploads accept plain text, CSV, PNG and JPEG.';
    case 'invalid_asset':
      // The server refuses Unicode control characters (U+0000 to U+001F and
      // U+007F to U+009F) other than tab, line feed and carriage return.
      return 'The file is empty, does not match its declared type, is not valid UTF-8 text, contains control characters other than tab, line feed and carriage return, or is a PNG or JPEG that is incomplete, animated, progressive or otherwise outside the accepted baseline JPEG form, or larger than 4096 pixels per axis or 16,777,216 pixels.';
    case 'asset_limit_exceeded':
      return 'Over the asset limit: 5 MiB per file, 64 files and 25 MiB per artifact, 256 files and 100 MiB across your artifacts.';
    case 'request_key_reused':
      return 'This request key was already used for a different request, so nothing was changed.';
    case 'not_found':
      return 'This artifact cannot be read.';
    default:
      return describeFailure(error);
  }
}

/**
 * The name a downloaded asset is saved under, as the server names it.
 * Throws when `assetId` would not make a safe download name.
 */
export function downloadName(assetId: string): string {
  const name = `asset-${assetId}`;
  if (!isDownloadName(name))
    throw new Error('The asset ID does not make a safe download name.');
  return name;
}

/** A byte count in binary units, as the limits are stated. */
export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const mebibytes = bytes >= 1024 * 1024;
  const value = bytes / (mebibytes ? 1024 * 1024 : 1024);
  const shown = Number.isInteger(value) ? String(value) : value.toFixed(1);
  return `${shown} ${mebibytes ? 'MiB' : 'KiB'}`;
}

/**
 * Why a file of `size` bytes is not read or sent, or undefined when it may
 * be. An empty file is sent: the server refuses it as `invalid_asset`.
 */
export function localUploadRefusal(size: number): string | undefined {
  return size > MAX_ASSET_BYTES ? OVERSIZE_TEXT : undefined;
}
