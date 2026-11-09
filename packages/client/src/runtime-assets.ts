import type {
  AssetList,
  RuntimeAssetReadRequest,
  RuntimeView,
} from '@artifree/contracts';
import { ASSET_ID, MAX_ASSET_BYTES, parseAssetList } from './assets.js';
import { ArtifreeApiError } from './errors.js';
import { isCount, isRecord } from './responses.js';
import { decodeBase64, partNumbersFit } from './source-part.js';

/**
 * Calls and checks for the assets a runtime view may read (decision 0045,
 * rule 12). Like runtime-source, this module is meant to be reached only from
 * the on-demand runtime-source module, so the main client bundle does not grow
 * (decision 0033).
 */

/** Most bytes in one part of an asset a read may ask for: 256 KiB. */
const MAX_ASSET_PART_BYTES = 256 * 1024;

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
 * What the calls below need from the frame: its JSON call, which refuses once
 * the view's runtime connection has ended (see `FrameTransport` in
 * runtime-source). It is typed structurally here so that this module imports
 * nothing from runtime-source.
 */
export interface RuntimeAssetsFrame {
  readonly json: <T>(
    path: string,
    body: Record<string, unknown>,
    parse: (value: unknown) => T | undefined,
    signal: AbortSignal | undefined,
  ) => Promise<T>;
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

/** The path of one of a view's frame routes. */
function frameRoute(view: RuntimeView, tail: string): string {
  return `/v1/artifacts/${encodeURIComponent(view.artifact_id)}/revisions/${view.revision}/runtime-views/${view.view_id}/${view.generation}/${tail}`;
}

/** A request body: the view's secret, then what the call adds. */
function frameBody(
  view: RuntimeView,
  rest: Record<string, unknown>,
): Record<string, unknown> {
  return { initialization: view.initialization, ...rest };
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
  frame: RuntimeAssetsFrame,
  view: RuntimeView,
  signal: AbortSignal | undefined,
): Promise<AssetList> {
  return frame.json(
    frameRoute(view, 'assets/list'),
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
  frame: RuntimeAssetsFrame,
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
    frameRoute(view, 'assets/read'),
    frameBody(view, body),
    (value) => parseRuntimeAssetPart(value, wanted),
    signal,
  );
}
