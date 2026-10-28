/**
 * Where the page is: which artifacts it lists, which one is open, and what
 * it says about one that left the view (decisions 0009 and 0014). Plain
 * code without React, so the rules are tested on their own.
 *
 * Writes end after the page may have moved on: the user can search, change
 * the order, or leave the trash while a recovery is answered. Their
 * outcomes are therefore events applied to the page as it is when they
 * arrive, never to the state captured when the write was sent.
 */
import { ALL_ARTIFACTS, PAGE_SIZE, showing } from './list-query';
import type { ListQuery } from './list-query';

/** What the page says about an artifact that left the view or came back. */
export interface Notice {
  readonly id: string;
  readonly text: string;
  /** Whether it went to the trash, which the page then offers to show. */
  readonly toTrash: boolean;
}

export interface Navigation {
  readonly query: ListQuery;
  /** How many artifacts are listed; a notice reads them all again. */
  readonly count: number;
  /** The open artifact, unless the list shows the trash. */
  readonly selected: string | undefined;
  readonly notice: Notice | undefined;
}

/** An artifact as an event names it. */
interface Named {
  readonly id: string;
  readonly title: string;
}

export type NavigationEvent =
  /** The user chose what the list shows. */
  | { readonly type: 'query'; readonly query: ListQuery }
  /** The user asked for more of the list. */
  | { readonly type: 'more' }
  /**
   * The user chose an artifact, or the page's own save made one; it opens
   * in the list of artifacts.
   */
  | { readonly type: 'choose'; readonly id: string }
  /** The page opened an artifact, or none, because of what it lists. */
  | { readonly type: 'open'; readonly id: string | undefined }
  /** The page's own move to the trash was answered. */
  | { readonly type: 'trashed'; readonly artifact: Named }
  /** The page's own recovery from the trash was answered. */
  | { readonly type: 'recovered'; readonly artifact: Named }
  /** The open artifact closed because the page learned it is in the trash. */
  | { readonly type: 'left'; readonly id: string };

export const START: Navigation = {
  query: ALL_ARTIFACTS,
  count: PAGE_SIZE,
  selected: undefined,
  notice: undefined,
};

/** The list of artifacts instead of the trash, with the same search. */
function outOfTrash(state: Navigation): Navigation {
  return state.query.deleted
    ? { ...state, query: showing(state.query, 'all'), count: PAGE_SIZE }
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
      // From the trash it opens in the list of artifacts. A page that left
      // the trash meanwhile keeps what it shows, which may hold a draft.
      return state.query.deleted
        ? { ...outOfTrash(state), selected: id, notice }
        : { ...state, notice };
    }
    case 'left':
      // That the page's own move put it there is said already; any other
      // notice about it, such as its recovery, is now out of date.
      return state.notice?.id === event.id && state.notice.toTrash
        ? state
        : {
            ...state,
            notice: {
              id: event.id,
              text: 'The open artifact was moved to the trash.',
              toTrash: true,
            },
          };
  }
}

/** What decides which artifact is open. */
export interface Opening {
  readonly selected: string | undefined;
  /** The trash is shown; its artifacts cannot be read, so none opens. */
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
 * artifact open. Otherwise it closes once the page knows it went to the
 * trash, or once a fresh list of every artifact no longer contains it; a
 * search, a filter, or a partial list does not close it. The first listed
 * artifact outside the trash then opens, also when a list answered for the
 * current request still names one the page knows went there.
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
