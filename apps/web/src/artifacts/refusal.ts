/**
 * A base a conflict refused (decision 0011, item 6). Plain code, so it is
 * tested without a browser.
 */
export interface Refusal {
  readonly base: number;
  /** The current version the conflict named, when it named one. */
  readonly current: number | undefined;
  /** The page's count of possible data replacements at the conflict. */
  readonly resets: number;
}

/**
 * Whether the page has read past `refusal` at `version`: a version after
 * the refused base and at least the one the conflict named. A refusal from
 * before the server's data may have been replaced no longer holds; the
 * versions it names may not exist any more.
 */
export function readPast(
  refusal: Refusal | undefined,
  version: number,
  resets: number,
): boolean {
  if (refusal === undefined || refusal.resets !== resets) return true;
  return (
    version > refusal.base &&
    (refusal.current === undefined || version >= refusal.current)
  );
}
