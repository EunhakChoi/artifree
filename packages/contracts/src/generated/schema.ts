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
   * Whether `artifree-server serve` also answers MCP over Streamable HTTP
   * at `/mcp` on the API listener.
   */
  mcp_api: boolean;
  /**
   * Display name of the application.
   */
  name: string;
  package_rules: PackageRules;
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
/**
 * The rules this build applies to an artifact's files.
 */
export interface PackageRules {
  /**
   * The document a preview opens. Every package has it.
   */
  entry_path: string;
  /**
   * Largest file, in bytes.
   */
  max_file_bytes: number;
  /**
   * Most files in one package.
   */
  max_files: number;
  /**
   * Largest sum of the sizes of a package's files, in bytes.
   */
  max_package_bytes: number;
  /**
   * Longest path, in UTF-8 bytes.
   */
  max_path_bytes: number;
  /**
   * Most `/`-separated segments in a path.
   */
  max_path_segments: number;
  /**
   * Longest segment, in bytes.
   */
  max_segment_bytes: number;
  /**
   * Each accepted file extension, lowercase, with the `Content-Type` a
   * file with it is served with. Case is ignored when a path is checked.
   */
  media_types: {
    [k: string]: string;
  };
}
