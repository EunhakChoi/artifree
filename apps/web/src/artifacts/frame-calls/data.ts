import { type Message, MAX_DATA_READ, defineCall, isCount, ok } from './call';

/**
 * The most UTF-16 code units a valid data path can have: 16 segments of at
 * most 64 UTF-8 bytes and 15 separators (decision 0046). A UTF-16 length never
 * exceeds the UTF-8 length, so no valid path is refused here; the server
 * judges the grammar (decision 0053, rule 6).
 */
const MAX_DATA_PATH_CHARS = 1_039;

/**
 * Whether `text` is a string of at most MAX_DATA_READ UTF-8 bytes. The host
 * never parses a value (decision 0053, rule 5).
 */
function isReadText(text: unknown): text is string {
  // A UTF-16 unit takes one to three UTF-8 bytes, so most texts are settled by
  // their length and only one in the range between is encoded to be counted.
  if (typeof text !== 'string' || text.length > MAX_DATA_READ) return false;
  if (text.length * 3 <= MAX_DATA_READ) return true;
  return new TextEncoder().encode(text).byteLength <= MAX_DATA_READ;
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
] as const;
