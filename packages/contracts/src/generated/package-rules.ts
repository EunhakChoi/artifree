/* Generated from artifree-server --package-rules. Run npm run generate:contracts; do not edit. */

/**
 * The package rules this build enforces, for clients that check a package
 * before sending it. The server checks every request again.
 */
export interface PackageRules {
  /**
   * The file a preview opens; every package has it.
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
   * Largest package, all files together, in bytes.
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
   * Each lowercase file extension a path may end with, and the
   * `Content-Type` its file is served with. Extensions match without
   * regard to ASCII case.
   */
  media_types: {
    [k: string]: string;
  };
}

/** The rules this build's server enforces; it checks every request again. */
export const packageRules: PackageRules = {
  "entry_path": "index.html",
  "max_files": 256,
  "max_file_bytes": 5242880,
  "max_package_bytes": 26214400,
  "max_path_bytes": 240,
  "max_path_segments": 8,
  "max_segment_bytes": 100,
  "media_types": {
    "avif": "image/avif",
    "css": "text/css; charset=utf-8",
    "csv": "text/csv; charset=utf-8",
    "gif": "image/gif",
    "htm": "text/html; charset=utf-8",
    "html": "text/html; charset=utf-8",
    "ico": "image/x-icon",
    "jpeg": "image/jpeg",
    "jpg": "image/jpeg",
    "js": "text/javascript; charset=utf-8",
    "json": "application/json",
    "map": "application/json",
    "md": "text/markdown; charset=utf-8",
    "mjs": "text/javascript; charset=utf-8",
    "otf": "font/otf",
    "png": "image/png",
    "svg": "image/svg+xml",
    "ttf": "font/ttf",
    "txt": "text/plain; charset=utf-8",
    "wasm": "application/wasm",
    "webp": "image/webp",
    "woff": "font/woff",
    "woff2": "font/woff2",
    "xml": "application/xml"
  }
};
