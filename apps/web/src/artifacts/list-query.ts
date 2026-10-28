/**
 * What the artifact list shows (decisions 0009 and 0014). Plain code
 * without React, so the rules are tested on their own.
 */
import type { ListSort } from '@artifree/contracts';

/** Artifacts shown at first, and added by each "Show more". */
export const PAGE_SIZE = 50;

/** Which artifacts the list shows, and in what order. */
export interface ListQuery {
  /** Search text, trimmed; empty searches nothing. */
  readonly search: string;
  readonly sort: ListSort;
  readonly pinned: boolean | undefined;
  /** The trash instead of the artifacts outside it. */
  readonly deleted: boolean;
}

export const ALL_ARTIFACTS: ListQuery = {
  search: '',
  sort: 'updated',
  pinned: undefined,
  deleted: false,
};

/** Whether `query` shows less than every artifact outside the trash. */
export function isFiltered(query: ListQuery): boolean {
  return query.search !== '' || query.pinned !== undefined || query.deleted;
}

/** A query's identity, for request keys. */
export function queryKey(query: ListQuery): string {
  return JSON.stringify([
    query.search,
    query.sort,
    query.pinned ?? null,
    query.deleted,
  ]);
}

/** The choices of the list's "Show" field. The trash has no pin filter. */
const VIEWS = {
  all: { pinned: undefined, deleted: false },
  pinned: { pinned: true, deleted: false },
  unpinned: { pinned: false, deleted: false },
  trash: { pinned: undefined, deleted: true },
} as const;

export type View = keyof typeof VIEWS;

export function isView(value: string): value is View {
  return Object.hasOwn(VIEWS, value);
}

/** The view `query` shows. */
export function viewOf(query: ListQuery): View {
  if (query.deleted) return 'trash';
  if (query.pinned === undefined) return 'all';
  return query.pinned ? 'pinned' : 'unpinned';
}

/** `query` showing `view`, with its search and order kept. */
export function inView(query: ListQuery, view: View): ListQuery {
  return { ...query, ...VIEWS[view] };
}

/**
 * Why a list shows nothing:
 * - `left`: more follow, and every artifact read so far left the view.
 * - `none`: it read every artifact outside the trash, and there is none.
 * - `unmatched`: nothing matches the search or view.
 */
export type Emptiness = 'left' | 'none' | 'unmatched';

/**
 * Why a list of `shown` artifacts for `query` is empty, or undefined when
 * it is not, or not read yet. `complete` says that no artifact follows.
 */
export function emptiness(
  query: ListQuery,
  shown: number | undefined,
  complete: boolean,
): Emptiness | undefined {
  if (shown !== 0) return undefined;
  if (!complete) return 'left';
  return isFiltered(query) ? 'unmatched' : 'none';
}
