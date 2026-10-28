/**
 * Where the workspace is: what the list shows, which artifact is open, and
 * what the page says about an artifact that left the view (decision 0014,
 * item 5). Plain code without React, so the rules are tested on their own.
 */
import type { Artifact } from '@artifree/contracts';
import { ALL_ARTIFACTS, PAGE_SIZE, inView } from './list-query';
import type { ListQuery } from './list-query';

/** What the page says about one artifact. */
export interface Notice {
  readonly id: string;
  readonly text: string;
  /** It went to the trash, so the page can offer to show the trash. */
  readonly toTrash: boolean;
}

export interface Navigation {
  readonly query: ListQuery;
  /** How many artifacts the list shows. */
  readonly count: number;
  /** The artifact the user chose, or that opened because it was first. */
  readonly selected: string | undefined;
  readonly notice: Notice | undefined;
}

export const START: Navigation = {
  query: ALL_ARTIFACTS,
  count: PAGE_SIZE,
  selected: undefined,
  notice: undefined,
};

/**
 * What happens to the page. The answers to the page's own writes are
 * events too, applied to the page as it is when they arrive.
 */
export type NavigationEvent =
  /** The user changed the search, order, or view. */
  | { readonly type: 'query'; readonly query: ListQuery }
  /** The user asked for more of the list. */
  | { readonly type: 'more' }
  /** The user chose an artifact, or the page saved or created one. */
  | { readonly type: 'choose'; readonly id: string }
  /** The page opened an artifact because it is listed first. */
  | { readonly type: 'open'; readonly id: string | undefined }
  /** The page's own move to the trash was answered. */
  | { readonly type: 'trashed'; readonly artifact: Artifact }
  /** The page's own recovery was answered. */
  | { readonly type: 'recovered'; readonly artifact: Artifact }
  /** The open artifact closed because the page learned it is in the trash. */
  | { readonly type: 'left'; readonly id: string };

/** The list of artifacts instead of the trash, with the same search. */
function outOfTrash(state: Navigation): Navigation {
  return state.query.deleted
    ? { ...state, query: inView(state.query, 'all'), count: PAGE_SIZE }
    : state;
}

export function navigate(
  state: Navigation,
  event: NavigationEvent,
): Navigation {
  switch (event.type) {
    case 'query':
      return {
        ...state,
        query: event.query,
        count: PAGE_SIZE,
        notice: undefined,
      };
    case 'more':
      return { ...state, count: state.count + PAGE_SIZE };
    case 'choose':
      // Nothing in the trash opens, so a choice is made outside it.
      return { ...outOfTrash(state), selected: event.id, notice: undefined };
    case 'open':
      return event.id === state.selected
        ? state
        : { ...state, selected: event.id };
    case 'trashed': {
      const { id, title } = event.artifact;
      return {
        ...state,
        notice: { id, text: `Moved “${title}” to the trash.`, toTrash: true },
      };
    }
    case 'recovered': {
      const { id, title } = event.artifact;
      const notice = { id, text: `Recovered “${title}”.`, toTrash: false };
      // Answered while the trash is shown, it opens in the list of
      // artifacts with the search and order shown now. A page that left
      // the trash meanwhile keeps what it shows, which may hold a draft.
      return state.query.deleted
        ? { ...outOfTrash(state), selected: id, notice }
        : { ...state, notice };
    }
    case 'left': {
      // The page's own move said why already; any other notice about the
      // artifact, such as its recovery, is out of date now.
      const { notice } = state;
      if (notice?.id === event.id && notice.toTrash) return state;
      return {
        ...state,
        notice: {
          id: event.id,
          text: 'The open artifact was moved to the trash.',
          toTrash: true,
        },
      };
    }
  }
}

/** What decides which artifact is open. */
export interface Opening {
  readonly selected: string | undefined;
  /** The trash is shown; nothing in it can be read, so nothing opens. */
  readonly trashShown: boolean;
  /** The editor holds unsaved changes, which keep their artifact open. */
  readonly unsaved: boolean;
  /** Whether the page knows the artifact is in the trash. */
  readonly inTrash: (id: string) => boolean;
  /** A list answered for the current request, if one is. */
  readonly fresh:
    | {
        readonly list: readonly { readonly id: string }[];
        /** Whether it lists every artifact, so it can tell one is gone. */
        readonly everything: boolean;
      }
    | undefined;
  /** What the list shows meanwhile. */
  readonly listed: readonly { readonly id: string }[] | undefined;
}

/**
 * The artifact that is open (decisions 0007 and 0014). A draft keeps its
 * artifact open. Otherwise it closes once the page knows it is in the
 * trash, or once a fresh list of every artifact no longer contains it; a
 * search, a filter, or a partial list does not close it. The first listed
 * artifact outside the trash then opens, also when a list read before a
 * move still names one the page knows went there.
 */
export function opened(page: Opening): string | undefined {
  if (page.trashShown) return undefined;
  const { selected, fresh, inTrash } = page;
  const kept =
    selected !== undefined &&
    (page.unsaved ||
      (!inTrash(selected) &&
        (fresh === undefined ||
          !fresh.everything ||
          fresh.list.some(({ id }) => id === selected))));
  if (kept) return selected;
  return (fresh?.list ?? page.listed)?.find(({ id }) => !inTrash(id))?.id;
}
