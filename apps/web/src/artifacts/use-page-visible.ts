import { useSyncExternalStore } from 'react';

function subscribe(onChange: () => void): () => void {
  document.addEventListener('visibilitychange', onChange);
  return () => document.removeEventListener('visibilitychange', onChange);
}

/** Whether the page is visible now; a page in a background tab is not. */
export function pageVisible(): boolean {
  return document.visibilityState !== 'hidden';
}

/** Whether the page is visible, updated when that changes. */
export function usePageVisible(): boolean {
  return useSyncExternalStore(subscribe, pageVisible);
}
