import type { DataSpace } from '@artifree/client';

/** The `--shared` option of the commands that name a data or asset space. */
export const sharedOption = { shared: { type: 'boolean' } } as const;

/** The space `--shared` names: the artifact's shared space, or your own. */
export function spaceOf(values: { shared?: boolean | undefined }): DataSpace {
  return values.shared === true ? 'shared' : 'own';
}

/**
 * `--shared, ` when it names the shared space, for a retry hint that lists
 * what to resend unchanged: a key replays only the space it was sent to.
 */
export function sharedHint(values: { shared?: boolean | undefined }): string {
  return values.shared === true ? '--shared, ' : '';
}
