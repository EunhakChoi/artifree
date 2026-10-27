/* Generated from artifree-contracts. Run npm run generate:contracts; do not edit. */

/**
 * A build-time module inventory, not a live server health response.
 */
export interface ServiceDescription {
  /**
   * Selected async runtime, or null while undecided.
   */
  async_runtime?: string | null;
  /**
   * Modules included in this build-time inventory.
   */
  components: Component[];
  /**
   * Version of this wire contract.
   */
  contract_version: string;
  /**
   * Whether this build can serve the HTTP API (`artifree-server serve`).
   * The inventory itself is static and does not probe a running server.
   */
  http_api: boolean;
  /**
   * Display name of the application.
   */
  name: string;
  /**
   * Product implementation stage.
   */
  phase: string;
  /**
   * Selected HTTP framework, or null while undecided.
   */
  server_framework?: string | null;
  /**
   * Version of the compiled descriptor binary.
   */
  version: string;
}
/**
 * A repository component and an honest description of its implementation stage.
 */
export interface Component {
  /**
   * Stable inventory key, unique within the descriptor.
   */
  id: string;
  /**
   * Implemented language or an explicitly open integration choice.
   */
  language: string;
  /**
   * Layer that owns the module's responsibility.
   */
  layer: 'presentation' | 'logic' | 'provider' | 'shared';
  /**
   * Human-readable module label.
   */
  name: string;
  /**
   * Repository-relative path to the source or module boundary.
   */
  path: string;
  /**
   * Implementation stage; this is not runtime health.
   */
  status: 'implemented' | 'scaffold' | 'planned';
  /**
   * Actual scaffold behavior or the proposed responsibility.
   */
  summary: string;
}
