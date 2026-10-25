/** Proposed capability vocabulary. A name is not a grant of permission. */
export type RuntimeCapability =
  'model.generate' | 'data.read' | 'data.write' | 'files.read' | 'tools.call';

/**
 * Future server-issued binding. Runtime verification and the iframe bridge are
 * not implemented. Never trust this object merely because it typechecks.
 */
export interface RuntimeSessionBinding {
  readonly sessionId: string;
  readonly artifactId: string;
  readonly revisionId: string;
  readonly capabilities: readonly RuntimeCapability[];
}
