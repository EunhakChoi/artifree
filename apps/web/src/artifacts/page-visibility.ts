import { useSyncExternalStore } from 'react';
import type { Visibility } from './request-gate';

/** The document's visibility, as the browser reports it. */
export const pageVisibility: Visibility = {
  visible: () => document.visibilityState === 'visible',
  subscribe(onChange) {
    document.addEventListener('visibilitychange', onChange);
    return () => document.removeEventListener('visibilitychange', onChange);
  },
};

/** Whether the page is visible, updated when its tab is shown or hidden. */
export function usePageVisible(): boolean {
  return useSyncExternalStore(pageVisibility.subscribe, pageVisibility.visible);
}
