import { sha256Hex } from './assets.js';

/**
 * The comparison of received bytes with what an offer stated (decision 0052,
 * rule 5). Only `runtime-source.ts` and `runtime-assets.ts` import it, and the
 * package index does not export it, so a page that never shows a runtime view
 * carries none of it. A test holds that.
 */

/**
 * How received bytes compare with a stated size and SHA-256. `unavailable`
 * means the platform could not compute the digest (no `crypto.subtle`, as on
 * a page that is not a secure context, or a digest that fails); it is never a
 * match, and each caller decides how it refuses.
 */
export type BytesComparison =
  | { readonly outcome: 'match' | 'mismatch' }
  | { readonly outcome: 'unavailable'; readonly cause: unknown };

/**
 * Compares `bytes` with the size and the lowercase hexadecimal SHA-256 an
 * offer stated. A different length is a mismatch before any digest is
 * computed.
 */
export async function compareBytes(
  bytes: Uint8Array<ArrayBuffer>,
  size: number,
  sha256: string,
): Promise<BytesComparison> {
  if (bytes.length !== size) return { outcome: 'mismatch' };
  let hex: string;
  try {
    hex = await sha256Hex(bytes);
  } catch (cause: unknown) {
    return { outcome: 'unavailable', cause };
  }
  return { outcome: hex === sha256 ? 'match' : 'mismatch' };
}
