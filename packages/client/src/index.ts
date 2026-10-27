/** HTTP API v1 client for browsers and Node.js. */
export { ArtifreeClient, encodeFile, newIdempotencyKey } from './client.js';
export type { ClientConfiguration, MutationOptions } from './client.js';
export type {
  ChangeFeedEvent,
  FeedChange,
  FollowOptions,
} from './change-feed.js';
export { ArtifreeApiError } from './errors.js';
export type { FailureCode } from './errors.js';
