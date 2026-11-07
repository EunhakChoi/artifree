import type { Asset, AssetList, AssetResponse } from '@artifree/contracts';
import type { ArtifreeClient, ReadOptions } from './client.js';
import { ArtifreeApiError } from './errors.js';
import { isCount, isRecord } from './responses.js';

const MAX_ASSET_BYTES = 5 * 1024 * 1024;
const MAX_SPACE_BYTES = 25 * 1024 * 1024;
const MAX_SPACE_ASSETS = 64;
export const ASSET_ID = /^[0-9a-f]{64}$/;
const ARTIFACT_ID = /^art_[0-9a-f]{32}$/;

function mediaType(value: unknown): value is string {
  // Historical image receipts and reads survive the current text-only policy.
  return (
    value === 'text/plain' ||
    value === 'text/csv' ||
    value === 'image/png' ||
    value === 'image/jpeg'
  );
}

function asset(value: unknown): Asset | undefined {
  if (!isRecord(value)) return undefined;
  const { id, content_type, size_bytes, sha256, created_at_ms } = value;
  if (
    typeof id !== 'string' ||
    !ASSET_ID.test(id) ||
    !mediaType(content_type) ||
    !isCount(size_bytes) ||
    size_bytes < 1 ||
    size_bytes > MAX_ASSET_BYTES ||
    typeof sha256 !== 'string' ||
    !ASSET_ID.test(sha256) ||
    !Number.isSafeInteger(created_at_ms) ||
    typeof created_at_ms !== 'number'
  )
    return undefined;
  return { id, content_type, size_bytes, sha256, created_at_ms };
}

function snapshot(value: unknown): AssetList | undefined {
  if (!isRecord(value)) return undefined;
  const { asset_version, assets, bytes, max_assets, max_bytes } = value;
  if (
    !isCount(asset_version) ||
    !Array.isArray(assets) ||
    assets.length > MAX_SPACE_ASSETS ||
    !isCount(bytes) ||
    bytes > MAX_SPACE_BYTES ||
    max_assets !== MAX_SPACE_ASSETS ||
    max_bytes !== MAX_SPACE_BYTES
  )
    return undefined;
  const parsed: Asset[] = [];
  let total = 0;
  let previous = '';
  for (const item of assets) {
    const entry = asset(item);
    if (entry === undefined || entry.id <= previous) return undefined;
    total += entry.size_bytes;
    previous = entry.id;
    parsed.push(entry);
  }
  if (total !== bytes || (parsed.length > 0 && asset_version === 0))
    return undefined;
  return { asset_version, assets: parsed, bytes, max_assets, max_bytes };
}

/**
 * Lowercase hexadecimal SHA-256 of `bytes` from Web Crypto. Its absence or
 * failure rejects; there is no weaker fallback.
 */
export async function sha256Hex(
  bytes: Uint8Array<ArrayBuffer>,
): Promise<string> {
  const digest = await globalThis.crypto.subtle.digest('SHA-256', bytes);
  return Array.from(new Uint8Array(digest), (byte) =>
    byte.toString(16).padStart(2, '0'),
  ).join('');
}

function invalidResponse(): ArtifreeApiError {
  return new ArtifreeApiError(
    'The asset response does not match its metadata.',
    {
      status: 200,
      code: 'invalid_response',
    },
  );
}

/** The asset route for a validated artifact (and asset) identifier. */
export function pathOf(artifactId: string, assetId?: string): string {
  if (
    typeof artifactId !== 'string' ||
    !ARTIFACT_ID.test(artifactId) ||
    (assetId !== undefined &&
      (typeof assetId !== 'string' || !ASSET_ID.test(assetId)))
  )
    throw new ArtifreeApiError('Invalid artifact or asset identifier.', {
      status: 0,
      code: 'invalid_request',
    });
  const path = `/v1/artifacts/${artifactId}/assets`;
  return assetId === undefined ? path : `${path}/${assetId}`;
}

/**
 * Lists the authenticated user's current artifact asset space, with bounded
 * response parsing. Empty spaces may retain a nonzero asset version.
 */
export async function listAssets(
  client: ArtifreeClient,
  artifactId: string,
  options: ReadOptions = {},
): Promise<AssetList> {
  return client.json('GET', pathOf(artifactId), snapshot, {
    cache: 'no-store',
    redirect: 'error',
    successBodyLimit: 65_536,
    boundedErrorBody: true,
    errorBodyLimit: 4096,
    signal: options.signal,
  });
}

/** Reads immutable metadata and the current independent asset-space version. */
export async function getAsset(
  client: ArtifreeClient,
  artifactId: string,
  assetId: string,
  options: ReadOptions = {},
): Promise<AssetResponse> {
  return client.json(
    'GET',
    pathOf(artifactId, assetId),
    (value) => {
      if (
        !isRecord(value) ||
        !isCount(value.asset_version) ||
        value.asset_version === 0
      )
        return undefined;
      const entry = asset(value.asset);
      return entry?.id === assetId
        ? { asset_version: value.asset_version, asset: entry }
        : undefined;
    },
    {
      cache: 'no-store',
      redirect: 'error',
      successBodyLimit: 65_536,
      boundedErrorBody: true,
      errorBodyLimit: 4096,
      signal: options.signal,
    },
  );
}

/** Exact immutable bytes and metadata; not an asset URL or a frame capability. */
export interface AssetBytes extends AssetResponse {
  readonly bytes: Uint8Array<ArrayBuffer>;
}

/**
 * Reads metadata, then bounded attachment bytes, checking the received MIME,
 * byte count and SHA-256. Deletion or loss of authority between the requests
 * is a failure. `asset_version` is the metadata snapshot's version, not a
 * claim that the space stayed unchanged. No read is automatically retried.
 * Digest verification requires Web Crypto; absence or failure rejects.
 * Bytes stay opaque: no text parsing, image decoding or inline rendering.
 */
export async function readAsset(
  client: ArtifreeClient,
  artifactId: string,
  assetId: string,
  options: ReadOptions = {},
): Promise<AssetBytes> {
  const metadata = await getAsset(client, artifactId, assetId, options);
  const content = await client.boundedBytes(
    `${pathOf(artifactId, assetId)}/content`,
    metadata.asset.size_bytes,
    options,
  );
  if (
    content.contentType !== metadata.asset.content_type ||
    content.bytes.byteLength !== metadata.asset.size_bytes
  )
    throw invalidResponse();
  let hex: string;
  try {
    hex = await sha256Hex(content.bytes);
  } catch (cause: unknown) {
    throw new ArtifreeApiError('Asset digest verification failed.', {
      status: 200,
      code: 'invalid_response',
      cause,
    });
  }
  if (hex !== metadata.asset.sha256) throw invalidResponse();
  if (options.signal?.aborted === true)
    throw new ArtifreeApiError('The asset read was aborted.', {
      status: 200,
      code: 'network_error',
    });
  return { ...metadata, bytes: content.bytes };
}
