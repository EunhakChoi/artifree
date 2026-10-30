/** Capability names; only `metadata.read` is currently supported. A name is not a grant. */
export type RuntimeCapability =
  | 'metadata.read'
  | 'model.generate'
  | 'data.read'
  | 'data.write'
  | 'files.read'
  | 'tools.call';

/**
 * Proposed capability binding. The server uses its own connection and view
 * binding for metadata grants; this type grants no permission.
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

/** The first runtime operation, subject to a current metadata grant. */
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
