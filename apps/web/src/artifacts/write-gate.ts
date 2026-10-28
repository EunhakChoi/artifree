/**
 * When a conditional write may be prepared (decision 0011, item 6). Plain
 * code without React, so the rule is tested on its own.
 */

/**
 * Why a write against one version waits:
 * - `replaced`: what is shown was read before the server's data may have
 *   been replaced; it is read again.
 * - `sending`: a write of the same gate is in flight.
 * - `refused`: a conflict proved a newer version than is shown.
 * - `newer`: the page knows of a newer version than is shown, from a
 *   notice, a list, or its own write, which the workspace learns as known
 *   (0009 decision 6).
 */
export type Wait = 'replaced' | 'sending' | 'refused' | 'newer';

/** The lowest version a write may be prepared from, as conflicts proved. */
export interface Floor {
  readonly version: number;
  /** The page's count of possible data replacements when it was learned. */
  readonly resets: number;
}

/**
 * `floor` raised to `version`. A floor learned before the server's data may
 * have been replaced no longer holds, since the versions it names may not
 * exist any more.
 */
export function raise(
  floor: Floor | undefined,
  version: number,
  resets: number,
): Floor {
  return floor === undefined ||
    floor.resets !== resets ||
    version > floor.version
    ? { version, resets }
    : floor;
}

/** What the page knows when it decides whether a write may be prepared. */
export interface Knowledge {
  /** The page's count of possible data replacements now. */
  readonly resets: number;
  /** The count when what is shown was read. */
  readonly shownResets: number;
  /** The newest version a notice or list reported, if any. */
  readonly known: number | undefined;
  readonly floor: Floor | undefined;
  readonly sending: boolean;
}

/**
 * Why a write prepared against `version` waits, or undefined when it may
 * be sent: the page shows a read made since the last possible replacement,
 * no write of its gate is in flight, and `version` is at least every
 * version the page has learned of.
 */
export function waitFor(version: number, page: Knowledge): Wait | undefined {
  if (page.shownResets !== page.resets) return 'replaced';
  if (page.sending) return 'sending';
  const { floor } = page;
  if (
    floor !== undefined &&
    floor.resets === page.resets &&
    version < floor.version
  )
    return 'refused';
  if (page.known !== undefined && version < page.known) return 'newer';
  return undefined;
}

/** What a dialog's request is prepared from: a read the page showed. */
export interface Prepared {
  /** The version the request names as its base. */
  readonly base: number;
}

/**
 * What a dialog prepares its next request from (decision 0014, item 2,
 * for rename and the move to the trash). It keeps what it opened on, so a
 * request whose answer was lost is sent again as it was (`retry`), also
 * after the page learned of a newer version, which may be that very write.
 * Otherwise, once the page has learned of a newer version than it opened
 * on and shows it, the next request is prepared from what is shown, and
 * `behind` says the dialog's view changed.
 */
export function prepare<T extends Prepared>(
  opened: T,
  shown: T,
  wait: (version: number) => Wait | undefined,
  retry: boolean,
): { readonly from: T; readonly behind: boolean } {
  const learned = wait(opened.base);
  const behind = !retry && (learned === 'refused' || learned === 'newer');
  return {
    from: behind && wait(shown.base) === undefined ? shown : opened,
    behind,
  };
}
