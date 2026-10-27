import { useSyncExternalStore } from 'react';
import type { Visibility } from './read-scheduler';

/** The document's visibility; a page in a background tab is hidden. */
export const pageVisibility: Visibility = {
  visible: () => document.visibilityState !== 'hidden',
  subscribe(onChange) {
    document.addEventListener('visibilitychange', onChange);
    return () => document.removeEventListener('visibilitychange', onChange);
  },
};

/** Whether the page is visible, updated when that changes. */
export function usePageVisible(): boolean {
  return useSyncExternalStore(pageVisibility.subscribe, pageVisibility.visible);
}
