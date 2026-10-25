/* Generated from artifree-contracts. Run npm run generate:contracts; do not edit. */

export type Layer = 'presentation' | 'logic' | 'provider' | 'shared';
export type ComponentStatus = 'scaffold' | 'planned';

/**
 * A build-time scaffold inventory, not a live server health response.
 */
export interface ServiceDescription {
  async_runtime?: string | null;
  components: Component[];
  contract_version: string;
  http_listening: boolean;
  name: string;
  phase: string;
  server_framework?: string | null;
  version: string;
}
export interface Component {
  id: string;
  language: string;
  layer: Layer;
  name: string;
  path: string;
  status: ComponentStatus;
  summary: string;
}
