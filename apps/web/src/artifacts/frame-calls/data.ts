import type { DataWriteResponse } from '@artifree/contracts';
import {
  type FrameReply,
  type Message,
  MAX_DATA_READ,
  MAX_DATA_WRITE,
  RETRY_ID,
  defineCall,
  isCount,
  ok,
} from './call';

/**
 * The most UTF-16 code units a valid data path can have: 16 segments of at
 * most 64 UTF-8 bytes and 15 separators (decision 0046). A UTF-16 length never
 * exceeds the UTF-8 length, so no valid path is refused here; the server
 * judges the grammar (decision 0053, rule 6).
 */
const MAX_DATA_PATH_CHARS = 1_039;

/**
 * Whether `text` is a string of at most `limit` UTF-8 bytes. The host never
 * parses a value (decision 0053, rule 5).
 */
function isTextWithin(text: unknown, limit: number): text is string {
  // A UTF-16 unit takes one to three UTF-8 bytes, so most texts are settled by
  // their length and only one in the range between is encoded to be counted.
  if (typeof text !== 'string' || text.length > limit) return false;
  if (text.length * 3 <= limit) return true;
  return new TextEncoder().encode(text).byteLength <= limit;
}

/** Whether `text` is a string of at most MAX_DATA_READ UTF-8 bytes. */
function isReadText(text: unknown): text is string {
  return isTextWithin(text, MAX_DATA_READ);
}

/**
 * Whether `text` is a string of at most MAX_DATA_WRITE UTF-8 bytes: the bound
 * of a value or patch a frame writes (decision 0053, rule 15).
 */
function isWriteText(text: unknown): text is string {
  return isTextWithin(text, MAX_DATA_WRITE);
}

/** The fields every data write carries (decision 0053, rule 13). */
interface WriteFields {
  readonly path: string;
  readonly baseDataVersion: number;
  readonly retryId: string;
}

/**
 * The path, base version and retry identity of a data write, or undefined when
 * one is wrong. The base is below the largest safe integer so that the version
 * after a change, base plus one, is a safe integer too.
 */
function parseWriteFields(message: Message): WriteFields | undefined {
  const { path, baseDataVersion, retryId } = message;
  if (
    typeof path !== 'string' ||
    path.length > MAX_DATA_PATH_CHARS ||
    !isCount(baseDataVersion) ||
    baseDataVersion >= Number.MAX_SAFE_INTEGER ||
    typeof retryId !== 'string' ||
    !RETRY_ID.test(retryId)
  )
    return undefined;
  return { path, baseDataVersion, retryId };
}

/**
 * The reply to a data write the server answered, checked against the call. The
 * receipt must name the path that was written and carry a version that is the
 * base plus one when the write changed the data and the base when it did not
 * (decision 0053, rule 13). A receipt that fails the check fails the call.
 */
function writeReply(
  call: { readonly id: number } & WriteFields,
  receipt: DataWriteResponse,
): FrameReply {
  if (
    receipt.path !== call.path ||
    !isCount(receipt.data_version) ||
    typeof receipt.changed !== 'boolean' ||
    receipt.data_version !==
      (receipt.changed ? call.baseDataVersion + 1 : call.baseDataVersion)
  )
    throw new Error(
      'The data write answered another path or an invalid version.',
    );
  return ok(call.id, {
    written: {
      path: receipt.path,
      dataVersion: receipt.data_version,
      changed: receipt.changed,
    },
  });
}

/** The application data calls of a runtime view (decision 0053). */
export const DATA_CALLS = [
  defineCall({
    type: 'data.get',
    keys: ['type', 'id', 'path'],
    parse(message: Message) {
      const { path } = message;
      if (typeof path !== 'string' || path.length > MAX_DATA_PATH_CHARS)
        return undefined;
      return { path };
    },
    async serve(call, deps, signal) {
      const { client, view } = deps;
      const read = await client.runtimeCall(view, (source, frame) =>
        source.readRuntimeData(frame, view, call.path, signal),
      );
      // The frame always gets `value`: null when nothing is at the path.
      const value = read.exists ? read.value : null;
      if (
        read.path !== call.path ||
        !isCount(read.data_version) ||
        (read.exists && !isReadText(value))
      )
        throw new Error(
          'The data read answered another path or an invalid version or value.',
        );
      return ok(call.id, {
        data: {
          path: read.path,
          dataVersion: read.data_version,
          exists: read.exists,
          value,
        },
      });
    },
  }),
  defineCall({
    type: 'data.set',
    keys: ['type', 'id', 'path', 'value', 'baseDataVersion', 'retryId'],
    write: true,
    parse(message: Message) {
      const { value } = message;
      const fields = parseWriteFields(message);
      if (fields === undefined || !isWriteText(value)) return undefined;
      return { ...fields, value };
    },
    async serve(call, deps, signal) {
      // The frame's own write budget is taken before the server is asked
      // (decision 0053, rule 23).
      if (!deps.takeDataWrite())
        return {
          message: { id: call.id, ok: false, code: 'rate_limited' },
          transfer: [],
        };
      const { client, view } = deps;
      const input = {
        path: call.path,
        value: call.value,
        baseDataVersion: call.baseDataVersion,
        retryId: call.retryId,
      };
      const receipt = await client.runtimeCall(view, (source, frame) =>
        source.setRuntimeData(frame, view, input, signal),
      );
      return writeReply(call, receipt);
    },
  }),
  defineCall({
    type: 'data.update',
    keys: ['type', 'id', 'path', 'patch', 'baseDataVersion', 'retryId'],
    write: true,
    parse(message: Message) {
      const { patch } = message;
      const fields = parseWriteFields(message);
      if (fields === undefined || !isWriteText(patch)) return undefined;
      return { ...fields, patch };
    },
    async serve(call, deps, signal) {
      if (!deps.takeDataWrite())
        return {
          message: { id: call.id, ok: false, code: 'rate_limited' },
          transfer: [],
        };
      const { client, view } = deps;
      const input = {
        path: call.path,
        patch: call.patch,
        baseDataVersion: call.baseDataVersion,
        retryId: call.retryId,
      };
      const receipt = await client.runtimeCall(view, (source, frame) =>
        source.updateRuntimeData(frame, view, input, signal),
      );
      return writeReply(call, receipt);
    },
  }),
  defineCall({
    type: 'data.remove',
    keys: ['type', 'id', 'path', 'baseDataVersion', 'retryId'],
    write: true,
    parse: parseWriteFields,
    async serve(call, deps, signal) {
      if (!deps.takeDataWrite())
        return {
          message: { id: call.id, ok: false, code: 'rate_limited' },
          transfer: [],
        };
      const { client, view } = deps;
      const input = {
        path: call.path,
        baseDataVersion: call.baseDataVersion,
        retryId: call.retryId,
      };
      const receipt = await client.runtimeCall(view, (source, frame) =>
        source.removeRuntimeData(frame, view, input, signal),
      );
      return writeReply(call, receipt);
    },
  }),
] as const;
