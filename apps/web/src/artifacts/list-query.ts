/**
 * Which artifacts the list shows (decisions 0009 and 0014). Plain code
 * without React, so the page's navigation rules are tested on their own.
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
  /** True for the trash, false for every artifact outside it. */
  readonly deleted: boolean;
}

export const ALL_ARTIFACTS: ListQuery = {
  search: '',
  sort: 'updated',
  pinned: undefined,
  deleted: false,
};

/** Whether `query` shows only some of the artifacts outside the trash. */
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

/** The choices of the list's "Show" field. */
const SHOWN = {
  all: { pinned: undefined, deleted: false },
  pinned: { pinned: true, deleted: false },
  unpinned: { pinned: false, deleted: false },
  trash: { pinned: undefined, deleted: true },
} as const;

export type Shown = keyof typeof SHOWN;

export function isShown(value: string): value is Shown {
  return Object.hasOwn(SHOWN, value);
}

/** What `query` shows. */
export function shownBy(query: ListQuery): Shown {
  if (query.deleted) return 'trash';
  if (query.pinned === undefined) return 'all';
  return query.pinned ? 'pinned' : 'unpinned';
}

/**
 * `query` showing `shown` instead; the search and order stay as they are
 * (decision 0014, item 5).
 */
export function showing(query: ListQuery, shown: Shown): ListQuery {
  return { ...query, ...SHOWN[shown] };
}

/**
 * Why a list shows nothing, if it does: every artifact it read left its
 * view and more follow (`left`), nothing matches `query`, or, only for a
 * complete list of every artifact outside the trash, there are none
 * (decision 0014, item 7).
 */
export type Emptiness = 'left' | 'unmatched' | 'none';

export function emptiness(
  query: ListQuery,
  shown: number | undefined,
  complete: boolean,
): Emptiness | undefined {
  if (shown !== 0) return undefined;
  if (!complete) return 'left';
  return isFiltered(query) ? 'unmatched' : 'none';
}
