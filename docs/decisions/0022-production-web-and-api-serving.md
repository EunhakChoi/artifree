# 0022: Production web asset and API serving

Date: 2026-10-31. Status: accepted. Builds on
[0003](0003-local-html-artifact-slice.md), [0004](0004-inbound-mcp-adapter.md), and
[0018](0018-source-draft-recovery.md).

## Context

Artifree previously required the Vite development server to serve frontend
assets and forward `/v1` requests to `artifree-server`. For self-hosted and
personal server deployments, running a separate Node development process is
unnecessary overhead and exposes development-mode behaviors.

A single server process must serve pre-built production web assets while
maintaining strict separation between unauthenticated web assets, bearer-token
authenticated API endpoints, and loopback-restricted MCP tools. Preview origins
must remain strictly isolated in an opaque sandbox, and client tools (Web UI and
CLI) require deterministic server address and profile resolution without
sacrificing local-first security defaults.

## Decision

1. **Integrated Web Asset Serving (`--web-dir` and `ARTIFREE_WEB_DIR`).**
   `artifree-server` optionally serves static web assets from a specified
   directory. Startup verifies that the path exists, is a directory, and
   contains `index.html` before binding to listening sockets. The readiness
   line stdout includes `web_url` matching the API URL when enabled.
   Requests are normalized with percent-decoding and null-byte rejection. Path
   traversal is prevented using component validation (rejecting `..`, `.`, root
   escapes) and canonical path verification (`starts_with`). Static assets
   under `/assets/*` are served with `Cache-Control: public, max-age=31536000,
immutable`. Missing paths with a file extension return `404 Not Found`,
   while extensionless paths fall back to `index.html` (HTTP 200) for
   client-side SPA routing. Only `GET` and `HEAD` methods are accepted; other
   methods return `405 Method Not Allowed` with `Allow: GET, HEAD`.
2. **Strict API and Web Routing Separation.**
   The web router is mounted as an outer fallback handler outside the
   authenticated `/v1` API route tree. Requests to `/v1/*` or `/mcp` strictly
   require bearer token authentication; any unmatched or unauthorized API path
   returns standard JSON error contracts (`401 unauthenticated`, `404 not_found`,
   `405 invalid_request`), never falling back to HTML. Non-API asset requests do
   not require authentication headers, allowing standard web browsers to fetch
   the client application shell without pre-shared HTTP headers.
3. **Explicit LAN Configuration (`--lan <ip>` and `ARTIFREE_LAN`).**
   Passing `--lan <ip>` configures the server for local network accessibility by
   deriving the API listen socket (`<ip>:7410`), preview content listen socket
   (`<ip>:7411`), preview content origin (`http://<ip>:7411`), and CSP frame
   ancestors (`http://127.0.0.1:* http://localhost:* http://<ip>:*`). IPv6
   addresses are properly bracketed (`[<v6>]:port`) for socket parsing.
   Explicit flags (`--api-listen`, `--content-listen`, `--content-origin`,
   `--frame-ancestors`) retain higher precedence over derived LAN defaults.
   When `--content-listen` is overridden without an explicit `--content-origin`,
   the preview origin is derived from the actual listener port rather than forced
   to 7411.
4. **Preserved Sandbox Isolation, MCP Loopback Policy, and CORS.**
   Previews continue to be hosted on an isolated origin (port 7411 by default)
   and rendered inside an iframe CSP sandbox (`allow-scripts allow-forms
allow-modals allow-downloads` without `allow-same-origin`).
   The MCP endpoint (`/mcp`) strictly enforces loopback-only communication:
   external `Host` headers and any `Origin` header are rejected with `403
Forbidden`. LAN clients access the system through the REST API and Web UI;
   inbound MCP remains reserved for local loopback agents.
   For cross-origin web workspace clients (such as local dev servers or LAN
   portals accessing `/v1/*`), the server handles CORS `OPTIONS` preflights
   before authentication and includes standard CORS headers, while explicitly
   rejecting requests from the preview sandbox origin (`content_origin`) or
   `null` origin with `403 Forbidden`.
5. **CLI Server Address and Profile Precedence.**
   The CLI resolves connection configuration using strict precedence:
   - Base URL: `--server <url>` > `ARTIFREE_URL` > profile `server` >
     `http://127.0.0.1:7410`.
   - Access token: `--token <token>` > `--token-file <path>` >
     `ARTIFREE_TOKEN` > `ARTIFREE_TOKEN_FILE` > profile `token` > profile
     `token_file` (resolved relative to the configuration file directory).
   - Config file: `--config <path>` > `ARTIFREE_CONFIG_FILE` >
     `./.artifree/profiles.json` > `./.artifree/config.json` >
     `~/.config/artifree/profiles.json`.
6. **Web Workspace Connection Priority and Token Isolation.**
   The Web UI resolves the server address via URL query parameter (`?server=<url>`),
   followed by session storage (`artifree.serverUrl`), connect form input, and
   defaults to the page's current origin (`""`). Access tokens are stored per
   server origin in session storage (`artifree.accessToken.<origin>`), and external
   servers specified via `?server=` never reuse stored credentials to prevent
   token exfiltration attacks from crafted links.
7. **Draft Recovery in Non-Secure LAN Contexts.**
   Plain HTTP connections over LAN IP addresses are non-secure contexts in
   browsers, where the Web Locks API (`navigator.locks`) is disabled. Under
   decision 0018, draft store operations fail safely with `LOCKS_REQUIRED`
   to prevent uncoordinated multi-tab corruption. Users can edit and save
   artifacts normally, while uncommitted draft persistence is withheld until
   HTTPS or localhost is used.

## Consequences

- Standalone deployment requires only the compiled `artifree-server` binary and
  `apps/web/dist` assets, eliminating Node and Vite from production environments.
- Security boundaries (origin separation for previews, authentication for APIs,
  and loopback restriction for MCP) remain strictly enforced.
- Remote HTTPS reverse proxies, token rotation, and multi-user authentication
  remain intentionally deferred to O02 and S01.
