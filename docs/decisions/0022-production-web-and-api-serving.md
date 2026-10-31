# 0022. Production Web Asset Serving and Connection Profiles

Date: 2026-10-31

## Status

Accepted

## Context

Artifree previously required running two separate processes for full web operation:
the Rust backend (`artifree-server serve`) and the Vite frontend dev server (`npm run dev`).
While effective for development, a production personal server requires:

1. Serving compiled frontend assets (`apps/web/dist`) directly from `artifree-server` without Node.js or Vite running in production (roadmap item O01).
2. Preserving origin separation: previews must remain on an isolated content origin (`http://127.0.0.1:7411` or `--lan <ip>:7411`), sandboxed without `allow-same-origin`, unable to access storage or bearer credentials.
3. Supporting LAN operation (`--lan <ip>` and `ARTIFREE_LAN`) for local network devices without weakening loopback security on MCP.
4. Supporting cross-origin web workspace clients with proper CORS preflight handling while strictly forbidding preview frames (`content_origin`) and `null` origin from accessing `/v1/*`.
5. Defining connection profiles and strict resolution precedence for the Web UI and CLI.
6. Graceful lifecycle handling, readiness line reporting (`web_url`), and fail-safe draft recovery under plain HTTP LAN non-secure contexts.

## Decision

1. **Production Web Asset Serving with Immutable Caching.**
   `artifree-server` accepts `--web-dir <path>` (and `ARTIFREE_WEB_DIR`). On startup,
   the server verifies the directory exists and contains `index.html`, failing fast
   with a descriptive error if missing or invalid.
   Assets in `assets/` are served with `Cache-Control: public, max-age=31536000, immutable`.
   Other static files are served with `Cache-Control: no-cache` and strict MIME types.
   Percent decoding and path canonicalization prevent directory traversal attacks.

2. **Root-Relative Asset Resolution and SPA Fallback.**
   `apps/web/vite.config.ts` builds assets with root-relative paths (`base: '/'`), and
   `apps/web/index.html` specifies `<base href="/" />`.
   When a client-side route (e.g., `/artifacts/art-12345` or `/trash`) is navigated to,
   the server performs SPA fallback by serving `index.html`.
   Because assets are referenced root-relatively, the browser requests `/assets/...` from
   the server root, preventing nested 404s. As defense-in-depth, the server also resolves
   any subpath request ending in `assets/<filename>` to the root asset directory.
   Missing assets with file extensions return `404 Not Found` rather than SPA fallback.

3. **Origin Separation and Sandbox Boundaries.**
   The preview server remains on an independent listener (default port 7411), isolated
   from the API and web origins. Preview iframes use CSP sandbox
   `allow-scripts allow-forms allow-modals allow-downloads` without `allow-same-origin`.
   CSP `frame-ancestors` dynamically permits embedding from the active API/Web origin.

4. **Outer CORS Middleware and MCP Loopback Policy.**
   An outer CORS middleware wraps all `/v1/*` API endpoints (including unrecognized routes
   handled by fallback):
   - Preflight `OPTIONS` requests on `/v1/*` are answered before authentication with
     `204 No Content` and standard access control headers.
   - Cross-origin requests from the preview origin (`content_origin`) or `null` origin
     are strictly rejected with `403 Forbidden`.
   - Unrecognized `/v1/*` routes return `404 JSON` with CORS headers.
   - MCP (`/mcp`) remains strictly restricted to local loopback; requests with foreign
     `Host` headers or any `Origin` header are rejected with `403 Forbidden`.

5. **CLI Server Address and Profile Precedence.**
   The CLI resolves connection configuration using strict precedence:
   - Base URL: `--server <url>` > `ARTIFREE_URL` > profile `server` > `http://127.0.0.1:7410`.
   - Access token: `--token <token>` > `--token-file <path>` > `ARTIFREE_TOKEN` >
     `ARTIFREE_TOKEN_FILE` > profile `token` > profile `token_file` (resolved relative
     to the configuration file directory).
   - Config file: `--config <path>` > `ARTIFREE_CONFIG_FILE` > `./.artifree/profiles.json` >
     `./.artifree/config.json` > `~/.config/artifree/profiles.json`.

6. **Web Workspace Connection Priority and Token Isolation.**
   The Web UI resolves the server address via URL query parameter (`?server=<url>`),
   followed by session storage (`artifree.serverUrl`), connect form input, and defaults
   to same-origin (`""`). Access tokens are stored per server origin
   (`artifree.accessToken.<origin>`), and external servers specified via `?server=`
   never automatically reuse stored credentials to prevent token exfiltration attacks.

7. **Draft Recovery in Non-Secure LAN Contexts.**
   Plain HTTP connections over LAN IP addresses are non-secure contexts in browsers,
   where `navigator.locks` is disabled. Under decision 0018, draft store operations fail
   safely with `LOCKS_REQUIRED` to prevent uncoordinated multi-tab corruption. Users can
   edit and save artifacts normally, while uncommitted draft persistence is withheld
   until HTTPS or localhost is used.

## Consequences

- Standalone deployment requires only the compiled `artifree-server` binary and
  `apps/web/dist` assets, eliminating Node and Vite from production environments.
- Security boundaries (origin separation for previews, authentication for APIs, and
  loopback restriction for MCP) remain strictly enforced.
- Remote HTTPS reverse proxies, token rotation, and multi-user authentication remain
  intentionally deferred to O02 and S01.
