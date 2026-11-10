import type {
  AssetList,
  DeleteAssetResponse,
  RuntimeAssetDeleteRequest,
  RuntimeAssetDownloadRequest,
  RuntimeAssetReadRequest,
  RuntimeAssetUploadRequest,
  RuntimeView,
  UploadAssetResponse,
} from '@artifree/contracts';
import {
  ASSET_ID,
  MAX_ASSET_BYTES,
  parseAssetList,
  sha256Hex,
} from './assets.js';
import { toBase64 } from './base64.js';
import { ArtifreeApiError } from './errors.js';
import { isCount, isRecord } from './responses.js';
import { compareBytes } from './runtime-bytes.js';
import { FRAME_ROUTES, frameBody, frameRoute } from './runtime-frame.js';
import type { FrameTransport } from './runtime-frame.js';
import {
  decodeBase64,
  isDownloadName,
  isRetryId,
  partNumbersFit,
} from './source-part.js';

/**
 * Calls and checks for the uploaded assets a runtime view may read, change,
 * and download (decision 0045, rule 12). Like runtime-source, this module is
 * meant to be reached only from the on-demand runtime-source module, so the
 * main client bundle does not grow (decision 0033).
 */

/** Most bytes in one part of an asset a read may ask for: 256 KiB. */
const MAX_ASSET_PART_BYTES = 256 * 1024;

/** Most bytes one asset upload may carry: 1 MiB (decision 0045, rule 12). */
const MAX_ASSET_UPLOAD_BYTES = 1_048_576;

/** The media types an asset may be stored as (decision 0045, rule 3). */
type AssetContentType = 'text/plain' | 'text/csv' | 'image/png' | 'image/jpeg';

const CONTENT_TYPES: readonly AssetContentType[] = [
  'text/plain',
  'text/csv',
  'image/png',
  'image/jpeg',
];

function contentTypeOf(value: unknown): AssetContentType | undefined {
  return CONTENT_TYPES.find((known) => known === value);
}

/** Whether `value` is a length a read may ask for: 1 to 262,144 bytes. */
function isPartLength(value: unknown): value is number {
  return isCount(value) && value >= 1 && value <= MAX_ASSET_PART_BYTES;
}

/**
 * Whether `value` is an asset space version a write can name. A write moves
 * the version to `value + 1`, so that must stay a safe integer too.
 */
function isBaseVersion(value: unknown): value is number {
  return isCount(value) && value < Number.MAX_SAFE_INTEGER;
}

/** What a read asked for; a part must answer exactly that. */
export interface RuntimeAssetPartRequest {
  /** The asset's ID: 64 lowercase hexadecimal digits. */
  readonly assetId: string;
  /** The first byte wanted, counted from 0. */
  readonly offset: number;
  /** At most this many bytes, from 1 to 262,144. */
  readonly length: number;
}

/** One part of an asset, with its bytes decoded and its numbers verified. */
export interface RuntimeAssetPartBytes {
  readonly assetId: string;
  readonly contentType: AssetContentType;
  readonly offset: number;
  /** Where the next part starts; null once the bytes reach the end. */
  readonly nextOffset: number | null;
  /** The whole asset's size in bytes, not this part's. */
  readonly size: number;
  /** The whole asset's SHA-256 digest, as 64 lowercase hexadecimal digits. */
  readonly sha256: string;
  readonly bytes: Uint8Array<ArrayBuffer>;
}

/**
 * Validates a part against the request it answers. The part must echo the
 * asset ID and the offset, state a media type from the closed set, a size from
 * 1 byte to 5 MiB and a digest of 64 lowercase hexadecimal digits, carry
 * canonical base64 content, and have numbers that fit the bytes received. The
 * digest is of the whole asset, so whoever assembles the parts checks it, not
 * this function.
 */
export function parseRuntimeAssetPart(
  value: unknown,
  request: RuntimeAssetPartRequest,
): RuntimeAssetPartBytes | undefined {
  if (!isRecord(value) || !isPartLength(request.length)) return undefined;
  const { asset_id, content, content_type, next_offset, offset, sha256, size } =
    value;
  const parsedType = contentTypeOf(content_type);
  if (
    asset_id !== request.assetId ||
    offset !== request.offset ||
    parsedType === undefined ||
    !isCount(size) ||
    size < 1 ||
    size > MAX_ASSET_BYTES ||
    typeof sha256 !== 'string' ||
    !/^[0-9a-f]{64}$/.test(sha256) ||
    typeof content !== 'string' ||
    !(next_offset === null || isCount(next_offset))
  )
    return undefined;
  const bytes = decodeBase64(content);
  if (
    bytes === undefined ||
    !partNumbersFit(
      { offset, nextOffset: next_offset, size, received: bytes.length },
      request.length,
    )
  )
    return undefined;
  return {
    assetId: asset_id,
    contentType: parsedType,
    offset,
    nextOffset: next_offset,
    size,
    sha256,
    bytes,
  };
}

/** Whether a read asks for an asset ID and a part the client can send. */
function isPartRequest(request: RuntimeAssetPartRequest): boolean {
  return (
    typeof request.assetId === 'string' &&
    ASSET_ID.test(request.assetId) &&
    isCount(request.offset) &&
    isPartLength(request.length)
  );
}

/**
 * The artifact's current asset space, listed through a runtime view. Assets
 * are not part of a source revision, so the listing does not depend on the
 * revision the view shows (decision 0045, rule 12).
 */
export function listRuntimeAssets(
  frame: FrameTransport,
  view: RuntimeView,
  signal: AbortSignal | undefined,
): Promise<AssetList> {
  return frame.json(
    frameRoute(view, FRAME_ROUTES.assetsList),
    frameBody(view, {}),
    parseAssetList,
    signal,
  );
}

/**
 * One part of an asset of the view's artifact. It resolves only with a part
 * that answers exactly `request`. A request the client cannot send is refused
 * before the frame is called. The request is copied once, so the values that
 * were checked are the values sent and the values compared.
 */
export async function readRuntimeAsset(
  frame: FrameTransport,
  view: RuntimeView,
  request: RuntimeAssetPartRequest,
  signal: AbortSignal | undefined,
): Promise<RuntimeAssetPartBytes> {
  const wanted: RuntimeAssetPartRequest = {
    assetId: request.assetId,
    offset: request.offset,
    length: request.length,
  };
  if (!isPartRequest(wanted))
    throw new ArtifreeApiError('The asset read was not valid.', {
      status: 0,
      code: 'invalid_request',
    });
  const body: Omit<RuntimeAssetReadRequest, 'initialization'> = {
    asset_id: wanted.assetId,
    offset: wanted.offset,
    length: wanted.length,
  };
  return frame.json(
    frameRoute(view, FRAME_ROUTES.assetsRead),
    frameBody(view, body),
    (value) => parseRuntimeAssetPart(value, wanted),
    signal,
  );
}

/** What a frame asked to upload: one asset under its retry identity. */
export interface RuntimeAssetUploadInput {
  /** The retry identity: 8 to 36 ASCII letters, digits, `-` or `_`. */
  readonly retryId: string;
  /** The asset space version the frame last observed. */
  readonly baseAssetVersion: number;
  /** The declared media type, checked against the four a new upload admits. */
  readonly contentType: string;
  /** The file's bytes: at least one and at most 1 MiB. */
  readonly bytes: Uint8Array<ArrayBuffer>;
}

/** What a frame asked to delete: one asset under its retry identity. */
export interface RuntimeAssetDeleteInput {
  /** The asset's ID: 64 lowercase hexadecimal digits. */
  readonly assetId: string;
  /** The retry identity, as for an upload. */
  readonly retryId: string;
  /** The asset space version the frame last observed. */
  readonly baseAssetVersion: number;
}

/** Whether an upload asks for what the client can send. */
function isUploadRequest(request: RuntimeAssetUploadInput): boolean {
  return (
    isRetryId(request.retryId) &&
    isBaseVersion(request.baseAssetVersion) &&
    contentTypeOf(request.contentType) !== undefined &&
    request.bytes instanceof Uint8Array &&
    request.bytes.length >= 1 &&
    request.bytes.length <= MAX_ASSET_UPLOAD_BYTES
  );
}

/** Whether a delete asks for what the client can send. */
function isDeleteRequest(request: RuntimeAssetDeleteInput): boolean {
  return (
    typeof request.assetId === 'string' &&
    ASSET_ID.test(request.assetId) &&
    isRetryId(request.retryId) &&
    isBaseVersion(request.baseAssetVersion)
  );
}

/**
 * Validates an upload receipt against the upload it answers. The server states
 * the asset and the version, and none of it is believed: the media type and
 * size must be the declared type and the bytes sent, the digest must be the
 * one computed from them, and the version must be the base plus one.
 */
export function parseRuntimeAssetUploadReceipt(
  value: unknown,
  input: RuntimeAssetUploadInput,
  digest: string,
): UploadAssetResponse | undefined {
  if (!isRecord(value)) return undefined;
  const { asset, asset_version, replayed } = value;
  if (
    !isRecord(asset) ||
    !isCount(asset_version) ||
    asset_version !== input.baseAssetVersion + 1 ||
    typeof replayed !== 'boolean'
  )
    return undefined;
  const { id, content_type, size_bytes, sha256, created_at_ms } = asset;
  if (
    typeof id !== 'string' ||
    !ASSET_ID.test(id) ||
    content_type !== input.contentType ||
    !isCount(size_bytes) ||
    size_bytes !== input.bytes.length ||
    sha256 !== digest ||
    !isCount(created_at_ms)
  )
    return undefined;
  return {
    asset: { id, content_type, size_bytes, sha256, created_at_ms },
    asset_version,
    replayed,
  };
}

/**
 * Validates a delete receipt against the delete it answers: the asset must be
 * the one asked for, and the version moves only when an association was
 * removed.
 */
export function parseRuntimeAssetDeleteReceipt(
  value: unknown,
  input: RuntimeAssetDeleteInput,
): DeleteAssetResponse | undefined {
  if (!isRecord(value)) return undefined;
  const { asset_id, asset_version, deleted, replayed } = value;
  if (
    asset_id !== input.assetId ||
    !isCount(asset_version) ||
    typeof deleted !== 'boolean' ||
    typeof replayed !== 'boolean'
  )
    return undefined;
  const expected = deleted
    ? input.baseAssetVersion + 1
    : input.baseAssetVersion;
  return asset_version === expected
    ? { asset_id, asset_version, deleted, replayed }
    : undefined;
}

/**
 * Uploads one asset to the view's artifact on behalf of the frame. It resolves
 * only with a receipt that matches the upload sent (decision 0045, rule 12).
 * The digest is computed before anything is sent, so a platform that cannot
 * compute one refuses the upload. Nothing here retries it: a retry must reuse
 * the retry identity for the identical upload.
 */
export async function uploadRuntimeAsset(
  frame: FrameTransport,
  view: RuntimeView,
  input: RuntimeAssetUploadInput,
  signal: AbortSignal | undefined,
): Promise<UploadAssetResponse> {
  const { bytes: given } = input;
  // One copy: the checks, the digest and the body all read the same bytes. A
  // value that is not a Uint8Array is kept, so the checks refuse it.
  const wanted: RuntimeAssetUploadInput = {
    retryId: input.retryId,
    baseAssetVersion: input.baseAssetVersion,
    contentType: input.contentType,
    bytes: given instanceof Uint8Array ? given.slice() : given,
  };
  if (!isUploadRequest(wanted))
    throw new ArtifreeApiError('The asset upload was not valid.', {
      status: 0,
      code: 'invalid_request',
    });
  let digest: string;
  try {
    digest = await sha256Hex(wanted.bytes);
  } catch (cause: unknown) {
    throw new ArtifreeApiError(
      'This page cannot compute the digest that checks an upload.',
      { status: 0, code: 'unavailable', cause },
    );
  }
  const body: Omit<RuntimeAssetUploadRequest, 'initialization'> = {
    retry_id: wanted.retryId,
    base_asset_version: wanted.baseAssetVersion,
    content_type: wanted.contentType,
    content_base64: toBase64(wanted.bytes),
  };
  return frame.json(
    frameRoute(view, FRAME_ROUTES.assetsUpload),
    frameBody(view, body),
    (value) => parseRuntimeAssetUploadReceipt(value, wanted, digest),
    signal,
  );
}

/**
 * Deletes one asset of the view's artifact on behalf of the frame. It resolves
 * only with a receipt that matches the delete sent (decision 0045, rule 12).
 * An asset the artifact no longer has is not an error: the receipt says
 * `deleted: false`, and the version does not move. Nothing here retries it.
 */
export async function deleteRuntimeAsset(
  frame: FrameTransport,
  view: RuntimeView,
  input: RuntimeAssetDeleteInput,
  signal: AbortSignal | undefined,
): Promise<DeleteAssetResponse> {
  const wanted: RuntimeAssetDeleteInput = {
    assetId: input.assetId,
    retryId: input.retryId,
    baseAssetVersion: input.baseAssetVersion,
  };
  if (!isDeleteRequest(wanted))
    throw new ArtifreeApiError('The asset delete was not valid.', {
      status: 0,
      code: 'invalid_request',
    });
  const body: Omit<RuntimeAssetDeleteRequest, 'initialization'> = {
    asset_id: wanted.assetId,
    base_asset_version: wanted.baseAssetVersion,
    retry_id: wanted.retryId,
  };
  return frame.json(
    frameRoute(view, FRAME_ROUTES.assetsDelete),
    frameBody(view, body),
    (value) => parseRuntimeAssetDeleteReceipt(value, wanted),
    signal,
  );
}

/** Most characters in the name an offer states (decision 0045, rule 12). */
const MAX_OFFER_NAME_CHARS = 100;

/** What a download of one asset would hand over, as the client states it. */
export interface RuntimeAssetOffer {
  /** The asset's ID: 64 lowercase hexadecimal digits. */
  readonly assetId: string;
  /**
   * The name to save under: 1 to 100 characters of `A-Z a-z 0-9 . _ -`, not
   * starting with a dot.
   */
  readonly fileName: string;
  /** The asset's size in bytes: at most 5 MiB. */
  readonly size: number;
  /** The asset's SHA-256 digest, as 64 lowercase hexadecimal digits. */
  readonly sha256: string;
}

/** Whether `value` is a file name an offer can state. */
function isOfferName(value: unknown): value is string {
  return isDownloadName(value) && value.length <= MAX_OFFER_NAME_CHARS;
}

/** Whether an offer's fields hold values an offer can state. */
function isAssetOffer(offer: RuntimeAssetOffer): boolean {
  return (
    ASSET_ID.test(offer.assetId) &&
    isOfferName(offer.fileName) &&
    isCount(offer.size) &&
    offer.size <= MAX_ASSET_BYTES &&
    ASSET_ID.test(offer.sha256)
  );
}

/**
 * Validates an offer against the asset it was asked for: it must name that
 * asset, and state a name, size and digest that an offer can state. Unknown
 * fields are ignored, as the neighbouring response parsers ignore them, so a
 * field the server adds later does not refuse an offer.
 */
export function parseRuntimeAssetOffer(
  value: unknown,
  assetId: string,
): RuntimeAssetOffer | undefined {
  if (!isRecord(value)) return undefined;
  const { asset_id, file_name, size, sha256 } = value;
  if (
    asset_id !== assetId ||
    typeof file_name !== 'string' ||
    typeof size !== 'number' ||
    typeof sha256 !== 'string'
  )
    return undefined;
  const offer: RuntimeAssetOffer = {
    assetId,
    fileName: file_name,
    size,
    sha256,
  };
  return isAssetOffer(offer) ? offer : undefined;
}

/** The refusal of a page that cannot compute the digest a download checks. */
function digestUnavailable(status: number, cause?: unknown): ArtifreeApiError {
  return new ArtifreeApiError(
    'This page cannot compute the digest that checks a download.',
    { status, code: 'unavailable', cause },
  );
}

/**
 * Asks whether the view's artifact may offer one of its assets for download,
 * and what the download would be, before the user confirms it. Nothing is
 * handed over, and the server decides afresh on each call (decision 0045,
 * rule 12).
 */
export async function offerRuntimeAssetDownload(
  frame: FrameTransport,
  view: RuntimeView,
  assetId: string,
  signal: AbortSignal | undefined,
): Promise<RuntimeAssetOffer> {
  if (typeof assetId !== 'string' || !ASSET_ID.test(assetId))
    throw new ArtifreeApiError('The asset download was not valid.', {
      status: 0,
      code: 'invalid_request',
    });
  const body: Omit<RuntimeAssetDownloadRequest, 'initialization'> = {
    asset_id: assetId,
  };
  return frame.json(
    frameRoute(view, FRAME_ROUTES.assetsOffer),
    frameBody(view, body),
    (value) => parseRuntimeAssetOffer(value, assetId),
    signal,
  );
}

/**
 * The bytes of a download the user confirmed. It resolves only when the
 * response is the attachment `offer` names, with the closed content type, and
 * the bytes are the size and digest the offer states. A page without Web
 * Crypto is refused before anything is requested.
 */
export async function fetchRuntimeAssetDownload(
  frame: FrameTransport,
  view: RuntimeView,
  offer: RuntimeAssetOffer,
  signal: AbortSignal | undefined,
): Promise<Uint8Array<ArrayBuffer>> {
  if (!isAssetOffer(offer))
    throw new ArtifreeApiError('The asset download was not valid.', {
      status: 0,
      code: 'invalid_request',
    });
  if (globalThis.crypto?.subtle === undefined) throw digestUnavailable(0);
  const path = frameRoute(view, FRAME_ROUTES.assetsDownload);
  const body: Omit<RuntimeAssetDownloadRequest, 'initialization'> = {
    asset_id: offer.assetId,
  };
  const response = await frame.post(path, frameBody(view, body), signal);
  const type = response.headers.get('content-type')?.trim().toLowerCase();
  const disposition = response.headers.get('content-disposition');
  const refuse = async (): Promise<never> => {
    // Only frees the connection; a body that was already read cannot be.
    await response.body?.cancel().catch(() => undefined);
    throw new ArtifreeApiError(
      `POST ${path} did not return the attachment that was offered.`,
      { status: response.status, code: 'invalid_response' },
    );
  };
  if (
    type !== 'application/octet-stream' ||
    disposition !== `attachment; filename="${offer.fileName}"`
  )
    return refuse();
  const bytes = await frame.bytes(response, offer.fileName);
  const compared = await compareBytes(bytes, offer.size, offer.sha256);
  if (compared.outcome === 'unavailable')
    throw digestUnavailable(response.status, compared.cause);
  const matches = compared.outcome === 'match';
  frame.check();
  if (!matches) return refuse();
  return bytes;
}
