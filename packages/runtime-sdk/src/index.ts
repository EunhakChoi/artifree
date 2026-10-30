/** Proposed capability vocabulary. A name is not a grant of permission. */
export type RuntimeCapability =
  'model.generate' | 'data.read' | 'data.write' | 'files.read' | 'tools.call';

/**
 * Proposed capability binding for a future grant system. The implemented
 * metadata view uses a different binding; this type grants no permission.
 */
export interface RuntimeSessionBinding {
  readonly sessionId: string;
  readonly artifactId: string;
  readonly revisionId: string;
  readonly capabilities: readonly RuntimeCapability[];
}

/** Current metadata available to a frame for its own artifact. */
export interface RuntimeMetadata {
  readonly artifactId: string;
  readonly title: string;
  readonly currentRevision: number;
  readonly metadataRevision: number;
}

/** The first runtime operation. It grants no data or model access. */
export interface ArtifactRuntime {
  /** Reads current metadata through the workspace's bound runtime view. */
  currentMetadata(): Promise<RuntimeMetadata>;
}

declare global {
  interface Window {
    /** Present only after the artifact opts in to the runtime script. */
    artifree?: ArtifactRuntime;
  }
}
