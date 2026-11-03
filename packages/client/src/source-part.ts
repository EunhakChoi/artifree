/**
 * Checks for the source a runtime view reads (decision 0032). A server, a
 * proxy or a frame can state numbers that do not match the bytes it sent,
 * so every number is checked against the bytes actually received, here and
 * again by the frame's own script. Nothing here trusts an advertised value.
 */

/** Most bytes in one part of a file, as the server's contract states. */
export const MAX_SOURCE_PART_BYTES = 1_048_576;

/** Most bytes in one file a frame saves, as the server's contract states. */
export const MAX_SOURCE_WRITE_BYTES = 1_048_576;

/** Whether `value` can be a frame's retry identity: 8 to 36 of `A-Za-z0-9_-`. */
export function isRetryId(value: unknown): value is string {
  return typeof value === 'string' && /^[A-Za-z0-9_-]{8,36}$/.test(value);
}

/**
 * Longest name a download may be offered under. The server offers at most
 * 100 characters, the longest package path segment; this bound is looser
 * on purpose, and a test holds it to that relationship.
 */
export const MAX_DOWNLOAD_NAME_CHARS = 128;

/** A file name safe to hand to the browser as a download name. */
export function isDownloadName(value: unknown): value is string {
  return (
    typeof value === 'string' &&
    value.length <= MAX_DOWNLOAD_NAME_CHARS &&
    /^[A-Za-z0-9_-][A-Za-z0-9._-]*$/.test(value)
  );
}

/** What a part states about where it lies in its file. */
export interface PartNumbers {
  readonly offset: number;
  /** Where the next part starts, or null when this one reaches the end. */
  readonly nextOffset: number | null;
  readonly size: number;
  /** The number of bytes actually received. */
  readonly received: number;
}

function count(value: unknown): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0;
}

/**
 * Whether the stated numbers fit the bytes received: all safe integers from
 * 0, the offset not past the end, a next offset strictly after the offset
 * and strictly before the end (a part that makes no progress is refused),
 * and exactly `nextOffset - offset` bytes, or `size - offset` for the final
 * part. `requested` bounds the part: it holds no more bytes than asked for,
 * and a part that does not reach the end holds exactly as many.
 */
export function partNumbersFit(
  numbers: PartNumbers,
  requested: number,
): boolean {
  const { offset, nextOffset, size, received } = numbers;
  if (
    !count(offset) ||
    !count(size) ||
    !count(received) ||
    offset > size ||
    received > MAX_SOURCE_PART_BYTES ||
    received > requested
  )
    return false;
  if (nextOffset === null) return received === size - offset;
  return (
    count(nextOffset) &&
    nextOffset > offset &&
    nextOffset < size &&
    received === nextOffset - offset &&
    received === requested
  );
}

/**
 * The bytes of canonical base64, or undefined for anything else: other
 * characters, whitespace, missing or misplaced padding. `atob` alone would
 * accept some of those.
 */
export function decodeBase64(
  text: string,
): Uint8Array<ArrayBuffer> | undefined {
  if (
    text.length % 4 !== 0 ||
    !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(
      text,
    )
  )
    return undefined;
  let binary: string;
  try {
    binary = atob(text);
  } catch {
    return undefined;
  }
  // Padding bits other than zero decode but are not what an encoder writes.
  if (btoa(binary) !== text) return undefined;
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1)
    bytes[index] = binary.charCodeAt(index);
  return bytes;
}
