import type {
  DeleteAssetRequest,
  DeleteAssetResponse,
  UploadAssetRequest,
  UploadAssetResponse,
} from '@artifree/contracts';
import type { ArtifreeClient, MutationOptions, ReadOptions } from './client.js';
import { ArtifreeApiError } from './errors.js';
import { ASSET_ID, pathOf, sha256Hex } from './assets.js';
import { isCount, isRecord } from './responses.js';

/** One asset's decoded bytes are at most 5 MiB (decision 0045 rule 4). */
const MAX_ASSET_BYTES = 5 * 1024 * 1024;
/** The stable upload envelope spelling of a MIME declaration, not a policy. */
const MIME_TYPE = /^[a-z0-9!#$%&'*+.^_`|~-]+\/[a-z0-9!#$%&'*+.^_`|~-]+$/;
const MAX_MIME_CHARS = 127;
const REQUEST_KEY = /^[A-Za-z0-9._:-]{8,128}$/;

/** Options of an asset mutation: the caller's key, and cancellation. */
export interface AssetMutationOptions extends MutationOptions, ReadOptions {}

function refused(message: string): ArtifreeApiError {
  return new ArtifreeApiError(message, { status: 0, code: 'invalid_request' });
}

const SEXTETS: Int8Array = (() => {
  const table = new Int8Array(128).fill(-1);
  const alphabet =
    'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
  for (let index = 0; index < alphabet.length; index += 1)
    table[alphabet.charCodeAt(index)] = index;
  return table;
})();

/**
 * Strict standard base64 with canonical padding and trailing bits, decoded
 * without building large intermediate strings. The size is computed from the
 * text's length and padding and bounded before the output is allocated.
 */
function decodeUpload(text: string): Uint8Array<ArrayBuffer> {
  if (text.length % 4 !== 0)
    throw refused('content_base64 must be strict standard base64.');
  const padding = text.endsWith('==') ? 2 : text.endsWith('=') ? 1 : 0;
  const size = (text.length / 4) * 3 - padding;
  if (size > MAX_ASSET_BYTES) throw refused('The asset is larger than 5 MiB.');
  const bytes = new Uint8Array(size);
  const sextet = (index: number): number => {
    const code = text.charCodeAt(index);
    const value = code < 128 ? (SEXTETS[code] ?? -1) : -1;
    if (value < 0)
      throw refused('content_base64 must be strict standard base64.');
    return value;
  };
  const full = (text.length - (padding > 0 ? 4 : 0)) / 4;
  let out = 0;
  for (let group = 0; group < full; group += 1) {
    const at = group * 4;
    const bits =
      (sextet(at) << 18) |
      (sextet(at + 1) << 12) |
      (sextet(at + 2) << 6) |
      sextet(at + 3);
    bytes[out] = bits >> 16;
    bytes[out + 1] = (bits >> 8) & 0xff;
    bytes[out + 2] = bits & 0xff;
    out += 3;
  }
  if (padding > 0) {
    const at = full * 4;
    const first = sextet(at);
    const second = sextet(at + 1);
    if (padding === 2) {
      // Unused low bits must be zero, as an encoder writes them.
      if ((second & 0x0f) !== 0)
        throw refused('content_base64 must be strict standard base64.');
      bytes[out] = (first << 2) | (second >> 4);
    } else {
      const third = sextet(at + 2);
      if ((third & 0x03) !== 0)
        throw refused('content_base64 must be strict standard base64.');
      bytes[out] = (first << 2) | (second >> 4);
      bytes[out + 1] = ((second & 0x0f) << 4) | (third >> 2);
    }
  }
  return bytes;
}

function requestKey(value: unknown): string {
  if (typeof value !== 'string' || !REQUEST_KEY.test(value))
    throw refused('The idempotency key must be 8-128 of A-Za-z0-9._:-.');
  return value;
}

/**
 * A base whose successor is still a safe integer, so that the receipt's
 * version can be checked exactly.
 */
function requestBase(value: unknown): number {
  if (!isCount(value) || value >= Number.MAX_SAFE_INTEGER)
    throw refused(
      'base_asset_version must be a whole number from 0 below 2^53 - 1.',
    );
  return value;
}

function requestRecord(value: unknown): Record<string, unknown> {
  if (!isRecord(value)) throw refused('The asset request must be an object.');
  return value;
}

function requestSignal(value: unknown): AbortSignal | undefined {
  if (value === undefined) return undefined;
  // Silently dropping a cancellation handle would hide an abort the caller wants.
  if (!(value instanceof AbortSignal))
    throw refused('signal must be an AbortSignal.');
  return value;
}

function refuseIfAborted(signal: AbortSignal | undefined): void {
  if (signal?.aborted === true) throw aborted();
}

function aborted(): ArtifreeApiError {
  return new ArtifreeApiError(
    'The asset request was aborted; nothing was sent.',
    {
      status: 0,
      code: 'network_error',
    },
  );
}

/**
 * SHA-256 of the bounded original bytes, before any request. Web Crypto's
 * absence or failure, and an abort while it runs, reject with nothing sent;
 * the abort also ends a digest that never settles.
 */
function digestBeforeDispatch(
  bytes: Uint8Array<ArrayBuffer>,
  signal: AbortSignal | undefined,
): Promise<string> {
  return new Promise((resolve, reject) => {
    if (signal?.aborted === true) {
      reject(aborted());
      return;
    }
    const onAbort = (): void => reject(aborted());
    signal?.addEventListener('abort', onAbort, { once: true });
    sha256Hex(bytes).then(
      (digest) => {
        signal?.removeEventListener('abort', onAbort);
        resolve(digest);
      },
      (cause: unknown) => {
        signal?.removeEventListener('abort', onAbort);
        reject(
          new ArtifreeApiError(
            'The asset digest could not be computed; nothing was sent.',
            { status: 0, code: 'invalid_request', cause },
          ),
        );
      },
    );
  });
}

function boolean(value: unknown): value is boolean {
  return typeof value === 'boolean';
}

/**
 * Uploads original bytes as one new asset, with one request and no retry.
 *
 * The envelope (artifact, base, MIME declaration, base64 text) and the key
 * are captured before the first await, so later changes to the caller's
 * objects change neither the attempt nor the receipt checks. Only the stable
 * envelope is checked here: MIME spelling, strict base64 and 5 MiB; the
 * server alone decides which types and contents are admitted now, including
 * the refusal of empty content. SHA-256 comes from Web Crypto before dispatch;
 * its absence or failure rejects with no request sent.
 *
 * Only HTTP 201 is accepted, initial or replayed. The receipt must echo an
 * asset ID, the declared MIME, the byte count and digest of the decoded bytes,
 * and version `base_asset_version + 1`; anything else, and any oversize,
 * malformed or stalled body, is a failure and never success evidence. A
 * replayed receipt's version is historical, not the current space version.
 *
 * A failure after dispatch (network, receipt, 409, 429) does not show that the
 * upload did not commit. Retry deliberately with the identical request and key;
 * this function never rotates a key, refreshes the base or retries itself.
 */
export async function uploadAsset(
  client: ArtifreeClient,
  artifactId: string,
  request: UploadAssetRequest,
  options: AssetMutationOptions,
): Promise<UploadAssetResponse> {
  const fields = requestRecord(request);
  const settings = requestRecord(options);
  const base = requestBase(fields.base_asset_version);
  const declared = fields.content_type;
  const encoded = fields.content_base64;
  const key = requestKey(settings.idempotencyKey);
  const signal = requestSignal(settings.signal);
  if (
    typeof declared !== 'string' ||
    declared.length > MAX_MIME_CHARS ||
    !MIME_TYPE.test(declared)
  )
    throw refused(
      'content_type must be a lowercase MIME type without parameters.',
    );
  if (typeof encoded !== 'string')
    throw refused('content_base64 must be a string.');
  const path = pathOf(artifactId);
  const bytes = decodeUpload(encoded);
  const body = {
    base_asset_version: base,
    content_type: declared,
    content_base64: encoded,
  };
  const digest = await digestBeforeDispatch(bytes, signal);
  return client.json(
    'POST',
    path,
    (value): UploadAssetResponse | undefined => {
      if (!isRecord(value) || !isRecord(value.asset)) return undefined;
      const { id, content_type, size_bytes, sha256, created_at_ms } =
        value.asset;
      if (
        typeof id !== 'string' ||
        !ASSET_ID.test(id) ||
        content_type !== declared ||
        size_bytes !== bytes.byteLength ||
        sha256 !== digest ||
        typeof created_at_ms !== 'number' ||
        !Number.isSafeInteger(created_at_ms) ||
        value.asset_version !== base + 1 ||
        !boolean(value.replayed)
      )
        return undefined;
      return {
        asset: {
          id,
          content_type: declared,
          size_bytes: bytes.byteLength,
          sha256: digest,
          created_at_ms,
        },
        asset_version: base + 1,
        replayed: value.replayed,
      };
    },
    {
      mutation: { body, idempotencyKey: key },
      cache: 'no-store',
      redirect: 'error',
      successBodyLimit: 65_536,
      boundedErrorBody: true,
      errorBodyLimit: 4096,
      expectStatus: 201,
      signal,
    },
  );
}

/**
 * Removes one asset association with one request and no retry, under the same
 * capture, redirect, size, stall and no-retry rules as {@link uploadAsset}.
 *
 * Only HTTP 200 is accepted, initial or replayed. The receipt must echo the
 * requested asset ID and boolean flags, with version `base_asset_version + 1`
 * when `deleted` is true and the base otherwise. A delete of an absent asset
 * succeeds with `deleted: false`, and a replay keeps its original `deleted`.
 * A failure after dispatch does not show that nothing was deleted: retry the
 * identical request and key.
 */
export async function deleteAsset(
  client: ArtifreeClient,
  artifactId: string,
  assetId: string,
  request: DeleteAssetRequest,
  options: AssetMutationOptions,
): Promise<DeleteAssetResponse> {
  const fields = requestRecord(request);
  const settings = requestRecord(options);
  const base = requestBase(fields.base_asset_version);
  const key = requestKey(settings.idempotencyKey);
  const signal = requestSignal(settings.signal);
  const path = pathOf(artifactId, assetId);
  refuseIfAborted(signal);
  return client.json(
    'DELETE',
    path,
    (value): DeleteAssetResponse | undefined => {
      if (!isRecord(value)) return undefined;
      const { asset_id, asset_version, deleted, replayed } = value;
      if (!boolean(deleted) || !boolean(replayed)) return undefined;
      const version = deleted ? base + 1 : base;
      if (asset_id !== assetId || asset_version !== version) return undefined;
      return { asset_id: assetId, asset_version: version, deleted, replayed };
    },
    {
      mutation: { body: { base_asset_version: base }, idempotencyKey: key },
      cache: 'no-store',
      redirect: 'error',
      successBodyLimit: 65_536,
      boundedErrorBody: true,
      errorBodyLimit: 4096,
      expectStatus: 200,
      signal,
    },
  );
}
