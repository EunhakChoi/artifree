import type { RuntimeView } from '@artifree/contracts';

/**
 * What every call a runtime view makes for its frame shares (decision 0052,
 * rule 5): the transport the client lends a call, the route and body of a
 * frame request, and the names of the frame routes. The client itself builds
 * the metadata route here, so this module, with all twelve route names, is in
 * the main bundle; the workspace's request gate is to read the names there
 * (rule 9). The comparison of received bytes is kept out of it, in the
 * on-demand `runtime-bytes` module.
 */

/**
 * What a frame call needs from the client. Every request is sent as the
 * workspace on the frame's behalf; `check` fails with the client's own error
 * once the view's connection ended, and `json` checks before and after.
 */
export interface FrameTransport {
  /** Fails if the runtime connection of the view was closed or replaced. */
  readonly check: () => void;
  readonly json: <T>(
    path: string,
    body: Record<string, unknown>,
    parse: (value: unknown) => T | undefined,
    signal: AbortSignal | undefined,
  ) => Promise<T>;
  readonly post: (
    path: string,
    body: Record<string, unknown>,
    signal: AbortSignal | undefined,
  ) => Promise<Response>;
  readonly bytes: (
    response: Response,
    what: string,
  ) => Promise<Uint8Array<ArrayBuffer>>;
}

/**
 * The tail of every frame route, below a view's generation. Each is named
 * once in the client, and the calls build their route from these names. The
 * workspace's request gate still writes the same tails out in its own
 * pattern; decision 0052, rule 9 (not yet implemented) has it build that
 * pattern from this list.
 */
export const FRAME_ROUTES = {
  metadata: 'metadata',
  sourceList: 'source/list',
  sourceRead: 'source/read',
  sourceOffer: 'source/offer',
  sourceDownload: 'source/download',
  sourceWrite: 'source/write',
  assetsList: 'assets/list',
  assetsRead: 'assets/read',
  assetsUpload: 'assets/upload',
  assetsDelete: 'assets/delete',
  assetsOffer: 'assets/offer',
  assetsDownload: 'assets/download',
} as const;

/** The tail of one frame route. */
export type FrameRouteTail = (typeof FRAME_ROUTES)[keyof typeof FRAME_ROUTES];

/** Every frame route tail, in the order they are named above. */
export const FRAME_ROUTE_TAILS: readonly FrameRouteTail[] =
  Object.values(FRAME_ROUTES);

/** The path of one of a view's frame routes. */
export function frameRoute(view: RuntimeView, tail: FrameRouteTail): string {
  return `/v1/artifacts/${encodeURIComponent(view.artifact_id)}/revisions/${view.revision}/runtime-views/${view.view_id}/${view.generation}/${tail}`;
}

/** A request body: the view's secret, then what the call adds. */
export function frameBody(
  view: RuntimeView,
  rest: Record<string, unknown>,
): Record<string, unknown> {
  return { initialization: view.initialization, ...rest };
}
