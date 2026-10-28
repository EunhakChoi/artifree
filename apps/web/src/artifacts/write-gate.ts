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

/** The base of a dialog's next request, and why. */
export interface PreparedBase {
  readonly base: number;
  /**
   * The page has learned that the request the dialog opened on is stale;
   * the next one is prepared from the latest once it is shown.
   */
  readonly behind: boolean;
  /** The request whose answer was lost is resent as it was. */
  readonly retry: boolean;
}

/**
 * The base of a dialog's next request (decision 0011, item 6): the version
 * it opened on, so that a retry after a lost answer is the same request,
 * until the page has learned of a newer version and shows it as `latest`.
 * A request whose answer was lost (`lost` is its base) is resent as it was,
 * also after a newer version is known, which may be that very write: a
 * replay under its key, refused without writing if its base is stale. A
 * possible replacement of the server's data still holds it.
 */
export function preparedBase(
  opened: number,
  latest: number,
  wait: (version: number) => Wait | undefined,
  lost: number | undefined,
): PreparedBase {
  const learned = wait(opened);
  const stale = learned === 'refused' || learned === 'newer';
  const retry = lost === opened && (learned === undefined || stale);
  const behind = !retry && stale;
  return {
    base: behind && wait(latest) === undefined ? latest : opened,
    behind,
    retry,
  };
}
