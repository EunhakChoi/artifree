# 0022. Production Web and API Serving

Date: 2026-11-01

## Status

Accepted (O01)

## Context

Artifree previously relied on the Vite development server for serving the web
workspace during local development, while `artifree-server` ran the API and
sandboxed preview listeners. To deploy and use Artifree in production without
Node.js or Vite, `artifree-server` must serve production web assets
(`apps/web/dist`) and the API together, while supporting explicit LAN
configuration and preserving existing security boundaries:

1. Preview origin isolation on a dedicated port in a sandboxed iframe without
   `allow-same-origin`.
2. MCP streamable HTTP endpoint (`/mcp`) confined to loopback clients only,
   preventing LAN peers from accessing agent tools.
3. Authenticated REST API with CORS headers allowing cross-origin web apps
   while forbidding the preview content origin and `null` origin.
4. CLI and Web connection resolution with deterministic precedence.
5. Supervisor health checks via an explicit unauthenticated `/healthz` endpoint.
6. Safe static asset delivery, symlink containment, and SPA client route fallback.

## Decision

1. **Integrated Web Asset Serving (`--web-dir` and `ARTIFREE_WEB_DIR`).**
   `artifree-server` optionally serves static web assets from a specified
   directory using `WebAssets`. Startup verifies that the path exists, is a
   directory, contains `index.html`, and that `index.html` does not resolve
   outside the root via symlinks. The readiness event includes `web_url`
   matching the API URL when web assets are enabled.
   Path traversal is prevented using path component inspection and canonical
   containment verification. Files under `/assets/*` are served with
   `Cache-Control: public, max-age=31536000, immutable`; other assets and
   `index.html` are served with `Cache-Control: no-cache`. Extensionless paths
   fall back to `index.html` (HTTP 200) for SPA client routing. Only `GET` and
   `HEAD` methods are accepted; other methods return `405 Method Not Allowed`.

2. **Strict API and Web Routing Separation.**
   The web router is mounted as an outer fallback handler outside the
   authenticated `/v1` API route tree. Requests to `/v1/*` or `/mcp` strictly
   require bearer token authentication; unmatched or unauthorized API paths
   return standard JSON error contracts (`401 unauthenticated`, `404 not_found`,
   `405 invalid_request`), never falling back to HTML. Unauthenticated route
   probing on `/v1/*` is rejected with 401 before route resolution.

3. **CORS on API Endpoints.**
   An outer CORS layer on `/v1/*` handles preflight `OPTIONS` requests with
   `204 No Content`, advertising methods (`GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS`),
   request headers (`Authorization, Content-Type, If-Match, Idempotency-Key, Last-Event-ID`),
   and exposing protocol headers (`Access-Control-Expose-Headers: Content-Range, Retry-After, Location`).
   Requests from the preview `content_origin` or `"null"` origin are rejected
   with `403 Forbidden`.

4. **Dedicated `/healthz` Endpoint.**
   An explicit `/healthz` route answers `GET` with `200 OK` ("ok\n") and `HEAD`
   with `200 OK` (empty body). It is mounted outside admission rate limits and
   authentication, and is excluded from SPA fallback.

5. **Explicit LAN Configuration (`--lan <ip>` and `ARTIFREE_LAN`).**
   Passing `--lan <ip>` configures the server for local network accessibility:
   - Derives API listen socket (`<ip>:7410`, bracketed if IPv6 `[<ip>]:7410`).
   - Derives preview content listen socket (`<ip>:7411`, bracketed if IPv6).
   - Derives preview content origin (`http://<ip>:7411` or `http://[<ip>]:7411`).
   - Dynamically derives CSP `frame-ancestors` including the LAN IP while
     retaining loopback origins (`http://127.0.0.1:* http://localhost:* http://[::1]:*`).
     Explicit flags (`--api-listen`, `--content-listen`, `--content-origin`,
     `--frame-ancestors`) retain higher precedence over derived LAN defaults.

6. **MCP Loopback Confinement via Peer Socket Address.**
   To prevent LAN peers from reaching `/mcp` by forging the `Host: 127.0.0.1:7410`
   header, the `/mcp` handler extracts `ConnectInfo<SocketAddr>` and verifies
   that `peer.ip().is_loopback()`. Non-loopback peers are rejected with `403 Forbidden`.

7. **CLI Connection Profiles.**
   The CLI resolves connection configuration using strict precedence:
   - Base URL: `--server <url>` > `ARTIFREE_SERVER` > `ARTIFREE_URL` > profile `server` > `http://127.0.0.1:7410`.
   - Access token: `--token <val>` > `--token-file <path>` > `ARTIFREE_TOKEN` > `ARTIFREE_TOKEN_FILE` > profile `token` > profile `token_file`.
   - Profile file candidate paths: `ARTIFREE_PROFILES_FILE` > `./artifree-profiles.json` > `~/.config/artifree/profiles.json` > `~/.artifree/profiles.json`.
   - Profile existence is checked via `Object.hasOwn` to prevent prototype pollution or unintended matching of `Object.prototype` methods.

8. **Web Workspace Connection and Token Isolation.**
   The Web UI resolves the server address via URL query parameter (`?server=<url>`),
   session storage (`artifree.serverUrl`), connect form input, or defaults to the
   current page origin. Tokens are stored in session storage scoped to the target
   origin (`artifree.accessToken.<origin>`). Auto-restoration of stored tokens is
   suppressed when `?server=` is present in the URL, preventing token exfiltration.
   Submitting an empty server URL in the connect form resets the connection to the
   page's current origin and clears the stored server URL.

9. **Draft Recovery in Non-Secure LAN Contexts.**
   Plain HTTP connections over LAN IP addresses are non-secure contexts in
   browsers, where the Web Locks API (`navigator.locks`) is disabled. Under
   decision 0018, draft store operations fail safely with `LOCKS_REQUIRED`
   to prevent uncoordinated multi-tab corruption. Users can edit and save
   artifacts normally, while uncommitted draft persistence is withheld until
   HTTPS or localhost is used.

## Consequences

- Standalone deployment requires only the compiled `artifree-server` binary and
  `apps/web/dist` assets, eliminating Node and Vite from production environments.
- Preview origin isolation, API bearer authentication, and MCP loopback
  confinement are strictly maintained across both localhost and LAN modes.
- Supervisors can monitor service health through `/healthz` without tokens.
