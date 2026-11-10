import { defineCall, ok } from './call';

/** The metadata call (decision 0020). */
export const METADATA_CALLS = [
  defineCall({
    type: 'metadata',
    keys: ['type', 'id'],
    parse: () => ({}),
    async serve(call, deps, signal) {
      const { client, view } = deps;
      const result = await client.runtimeMetadata(view, { signal });
      return ok(call.id, {
        metadata: {
          artifactId: result.artifact_id,
          title: result.title,
          currentRevision: result.current_revision,
          metadataRevision: result.metadata_revision,
        },
      });
    },
  }),
] as const;
