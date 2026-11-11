import { pathError } from '../package-path';
import {
  type Message,
  MAX_PART,
  MAX_WRITE,
  RETRY_ID,
  defineCall,
  isCount,
  ok,
} from './call';
import { download } from './download';

/** The source calls of a runtime view (decisions 0032 and 0033). */
export const SOURCE_CALLS = [
  defineCall({
    type: 'source.list',
    keys: ['type', 'id'],
    parse: () => ({}),
    async serve(call, deps, signal) {
      const { client, view } = deps;
      const result = await client.runtimeSourceList(view, { signal });
      return ok(call.id, {
        listing: {
          revision: result.revision,
          files: result.files.map((file) => ({
            path: file.path,
            size: file.size,
            sha256: file.sha256,
            mediaType: file.media_type,
          })),
        },
      });
    },
  }),
  defineCall({
    type: 'source.read',
    keys: ['type', 'id', 'path', 'offset', 'length'],
    parse(message: Message) {
      const { path, offset, length } = message;
      if (
        typeof path !== 'string' ||
        pathError(path) !== undefined ||
        !isCount(offset) ||
        !isCount(length) ||
        length < 1 ||
        length > MAX_PART
      )
        return undefined;
      return { path, offset, length };
    },
    async serve(call, deps, signal) {
      const { client, view } = deps;
      const part = await client.runtimeSourceRead(
        view,
        { path: call.path, offset: call.offset, length: call.length },
        { signal },
      );
      const bytes = part.bytes.buffer;
      return {
        message: {
          id: call.id,
          ok: true,
          part: {
            path: part.path,
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
    type: 'source.download',
    keys: ['type', 'id', 'kind', 'path'],
    parse(message: Message) {
      const { kind, path } = message;
      if (kind === 'archive' && path === null) return { path: null };
      if (
        kind === 'file' &&
        typeof path === 'string' &&
        pathError(path) === undefined
      )
        return { path };
      return undefined;
    },
    async serve(call, deps, signal) {
      const { client, view } = deps;
      const target =
        call.path === null
          ? ({ kind: 'archive' } as const)
          : ({ kind: 'file', path: call.path } as const);
      return download(call.id, deps, signal, {
        offer: () => client.runtimeSourceOffer(view, target, { signal }),
        prompt: (offer) => ({
          kind: offer.kind,
          path: offer.path,
          fileName: offer.file_name,
          size: offer.size,
        }),
        // Read at fetch time: a prompt kept through a lease adoption downloads
        // under the new generation (decision 0054, rule 13).
        fetch: (offer) =>
          client.runtimeSourceDownload(deps.view, offer, { signal }),
        saveAs: (offer) => ({
          fileName: offer.file_name,
          type:
            offer.kind === 'file'
              ? 'application/octet-stream'
              : 'application/zip',
        }),
        receipt: (offer, bytes) => ({
          started: true,
          kind: offer.kind,
          bytes: bytes.length,
        }),
      });
    },
  }),
  defineCall({
    type: 'source.write',
    keys: ['type', 'id', 'retryId', 'path', 'bytes'],
    write: true,
    parse(message: Message) {
      const { retryId, path, bytes } = message;
      if (
        typeof retryId !== 'string' ||
        !RETRY_ID.test(retryId) ||
        typeof path !== 'string' ||
        pathError(path) !== undefined ||
        !(bytes instanceof ArrayBuffer) ||
        bytes.byteLength > MAX_WRITE
      )
        return undefined;
      return { retryId, path, bytes };
    },
    async serve(call, deps, signal) {
      const { client, view } = deps;
      // A frame's bytes are copied once, here; the client reads the copy.
      const input = {
        path: call.path,
        retryId: call.retryId,
        bytes: new Uint8Array(call.bytes.slice(0)),
      };
      const receipt = await client.runtimeCall(view, (source, frame) =>
        source.writeSource(frame, view, input, signal),
      );
      return ok(call.id, {
        written: {
          path: receipt.path,
          revision: receipt.revision,
          size: receipt.size,
          sha256: receipt.sha256,
        },
      });
    },
  }),
] as const;
