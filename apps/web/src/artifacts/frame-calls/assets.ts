import {
  type Message,
  ASSET_ID,
  MAX_ASSET_PART,
  MAX_MEDIA_TYPE_LENGTH,
  MAX_WRITE,
  MEDIA_TYPE,
  RETRY_ID,
  defineCall,
  isCount,
  ok,
} from './call';
import { download } from './download';

/** The asset calls of a runtime view (decision 0045). */
export const ASSET_CALLS = [
  defineCall({
    type: 'assets.list',
    keys: ['type', 'id'],
    parse: () => ({}),
    async serve(call, deps, signal) {
      const { client, view } = deps;
      const result = await client.runtimeCall(view, (source, frame) =>
        source.listRuntimeAssets(frame, view, signal),
      );
      return ok(call.id, {
        listing: {
          assetVersion: result.asset_version,
          assets: result.assets.map((asset) => ({
            id: asset.id,
            contentType: asset.content_type,
            size: asset.size_bytes,
            sha256: asset.sha256,
            createdAtMs: asset.created_at_ms,
          })),
        },
      });
    },
  }),
  defineCall({
    type: 'assets.read',
    keys: ['type', 'id', 'assetId', 'offset', 'length'],
    parse(message: Message) {
      const { assetId, offset, length } = message;
      if (
        typeof assetId !== 'string' ||
        !ASSET_ID.test(assetId) ||
        !isCount(offset) ||
        !isCount(length) ||
        length < 1 ||
        length > MAX_ASSET_PART
      )
        return undefined;
      return { assetId, offset, length };
    },
    async serve(call, deps, signal) {
      const { client, view } = deps;
      const request = {
        assetId: call.assetId,
        offset: call.offset,
        length: call.length,
      };
      const part = await client.runtimeCall(view, (source, frame) =>
        source.readRuntimeAsset(frame, view, request, signal),
      );
      const bytes = part.bytes.buffer;
      return {
        message: {
          id: call.id,
          ok: true,
          part: {
            assetId: part.assetId,
            contentType: part.contentType,
            offset: part.offset,
            nextOffset: part.nextOffset,
            size: part.size,
            sha256: part.sha256,
            bytes,
          },
        },
        transfer: [bytes],
      };
    },
  }),
  defineCall({
    type: 'assets.upload',
    keys: ['type', 'id', 'retryId', 'baseAssetVersion', 'contentType', 'bytes'],
    parse(message: Message) {
      const { retryId, baseAssetVersion, contentType, bytes } = message;
      if (
        typeof retryId !== 'string' ||
        !RETRY_ID.test(retryId) ||
        !isCount(baseAssetVersion) ||
        typeof contentType !== 'string' ||
        contentType.length > MAX_MEDIA_TYPE_LENGTH ||
        !MEDIA_TYPE.test(contentType) ||
        !(bytes instanceof ArrayBuffer) ||
        bytes.byteLength > MAX_WRITE
      )
        return undefined;
      return { retryId, baseAssetVersion, contentType, bytes };
    },
    async serve(call, deps, signal) {
      const { client, view } = deps;
      // A frame's bytes are copied once, here; the client reads the copy.
      const input = {
        retryId: call.retryId,
        baseAssetVersion: call.baseAssetVersion,
        contentType: call.contentType,
        bytes: new Uint8Array(call.bytes.slice(0)),
      };
      const receipt = await client.runtimeCall(view, (source, frame) =>
        source.uploadRuntimeAsset(frame, view, input, signal),
      );
      return ok(call.id, {
        uploaded: {
          asset: {
            id: receipt.asset.id,
            contentType: receipt.asset.content_type,
            size: receipt.asset.size_bytes,
            sha256: receipt.asset.sha256,
            createdAtMs: receipt.asset.created_at_ms,
          },
          assetVersion: receipt.asset_version,
        },
      });
    },
  }),
  defineCall({
    type: 'assets.delete',
    keys: ['type', 'id', 'retryId', 'baseAssetVersion', 'assetId'],
    parse(message: Message) {
      const { retryId, baseAssetVersion, assetId } = message;
      if (
        typeof retryId !== 'string' ||
        !RETRY_ID.test(retryId) ||
        !isCount(baseAssetVersion) ||
        typeof assetId !== 'string' ||
        !ASSET_ID.test(assetId)
      )
        return undefined;
      return { retryId, baseAssetVersion, assetId };
    },
    async serve(call, deps, signal) {
      const { client, view } = deps;
      const input = {
        assetId: call.assetId,
        retryId: call.retryId,
        baseAssetVersion: call.baseAssetVersion,
      };
      const receipt = await client.runtimeCall(view, (source, frame) =>
        source.deleteRuntimeAsset(frame, view, input, signal),
      );
      return ok(call.id, {
        removed: {
          assetId: receipt.asset_id,
          assetVersion: receipt.asset_version,
          deleted: receipt.deleted,
        },
      });
    },
  }),
  defineCall({
    type: 'assets.download',
    keys: ['type', 'id', 'assetId'],
    parse(message: Message) {
      const { assetId } = message;
      if (typeof assetId !== 'string' || !ASSET_ID.test(assetId))
        return undefined;
      return { assetId };
    },
    async serve(call, deps, signal) {
      const { client, view } = deps;
      return download(call.id, deps, signal, {
        offer: () =>
          client.runtimeCall(view, (source, frame) =>
            source.offerRuntimeAssetDownload(frame, view, call.assetId, signal),
          ),
        prompt: (offer) => ({
          kind: 'asset',
          fileName: offer.fileName,
          bytes: offer.size,
        }),
        fetch: (offer) =>
          client.runtimeCall(view, (source, frame) =>
            source.fetchRuntimeAssetDownload(frame, view, offer, signal),
          ),
        saveAs: (offer) => ({
          fileName: offer.fileName,
          type: 'application/octet-stream',
        }),
        receipt: (offer, bytes) => ({
          started: true,
          assetId: offer.assetId,
          bytes: bytes.length,
        }),
      });
    },
  }),
] as const;
