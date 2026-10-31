# 0022: Production web and API serving

Date: 2026-11-01. Status: accepted. Builds on
[0003](0003-local-html-artifact-slice.md),
[0004](0004-inbound-mcp-adapter.md), and
[0005](0005-request-limits.md).

## Context

Artifree previously required Vite development server proxies to run the web
presentation alongside `artifree-server`. Running without Vite requires the
server to directly host production web assets while maintaining the security
boundaries established in earlier decisions:

- Preview frames must remain on a distinct origin and port in a sandbox
  without `allow-same-origin` (0003).
- Inbound MCP endpoints must remain confined to local agent processes
  regardless of network exposure (0004).
- The web management interface and CLI need flexible server targeting
  (localhost and LAN) without exposing credentials to untrusted repositories
  or malicious network peers.

## Decision

1. **Production asset serving and streaming.** `artifree-server` optionally
   serves production web assets when `--web-dir <dir>` or `ARTIFREE_WEB_DIR` is
   configured. Assets are served asynchronously via non-blocking chunked
   streaming (Tokio `FileStream` with 64 KiB chunks) and proper `Content-Length`
   headers, preventing large bundle or media transfers from stalling the async
   runtime worker threads. Fingerprinted assets under `/assets/*` receive
   immutable cache headers (`public, max-age=31536000, immutable`), while
   `index.html` is marked `no-cache`.
2. **SPA routing fallback and mime isolation.** Client-side SPA navigation routes
   (GET requests without file extensions that do not match existing static files
   or API endpoints) fall back to `index.html`. Unmatched requests with file
   extensions return 404 Not Found rather than HTML, preventing MIME mismatch
   parse errors when browsers request missing scripts or stylesheets.
3. **Path traversal and symlink guards.** Asset file resolution canonicalizes
   paths and strictly rejects requests attempting to escape `web_dir`, whether
   via `..` traversal segments or symlinks pointing outside the configured
   directory.
4. **Default loopback and explicit LAN mode.** The server defaults to loopback
   interfaces (`127.0.0.1:7410` for API, `127.0.0.1:7411` for preview). Explicit
   LAN binding is configured via `--lan <ip>` (or `ARTIFREE_LAN`), supporting
   both IPv4 and IPv6 addresses. When LAN binding is enabled, a secondary
   loopback listener is automatically bound so local CLI and agent workflows
   continue unhindered on `127.0.0.1` (or `[::1]`). If the primary listen
   address is an unspecified wildcard (`0.0.0.0` or `::`), loopback derivation
   is skipped to prevent `EADDRINUSE` port collision.
5. **Origin isolation and canonicalization.** The preview origin remains on a
   distinct port (default 7411). When LAN mode is enabled, `frame-ancestors` and
   CORS rules are derived to admit the LAN web origin while preserving IPv4/IPv6
   loopback origins. Both the configured content origin and incoming `Origin`
   headers are canonicalized (stripping default ports 80/443 and trailing slashes,
   normalizing scheme and host case) before validation. API CORS rejects `null`
   and cross-origin requests originating from the preview origin.
6. **MCP loopback confinement.** Inbound MCP endpoints (`/mcp` and tools) enforce
   loopback peer connection verification (`peer.ip().is_loopback()`). Remote
   network clients requesting MCP tools receive 403 Forbidden, preserving local
   agent security even when API and web endpoints are exposed to the LAN.
7. **CLI connection resolution and untrusted checkout protection.** The CLI
   resolves connection parameters with strict precedence: command-line flag >
   environment variable > profile configuration > default (`http://127.0.0.1:7410`).
   To prevent untrusted repository checkouts from hijacking CLI traffic or
   exfiltrating credentials, working-tree or checkout profile files are NEVER
   loaded implicitly; they require an explicit profile file option (`--profile-file <path>`)
   or environment variable (`ARTIFREE_PROFILES_FILE`). Specifying only a profile
   name (`--profile <name>`) strictly resolves from user-level configuration
   directories (`~/.config/artifree/` and `~/.artifree/`).
8. **Web origin-scoped tokens and explicit targeting.** The web application
   supports targeting custom servers. Access tokens stored in `sessionStorage`
   are scoped by origin (`artifree.accessToken.<origin>`). URL query parameters
   (`?server=<url>`) allow explicit server targeting but suppress automatic
   token restoration to prevent cross-server credential leaks. Empty server URL
   inputs default back to the current hosting origin.

## Limits

This decision enables standalone production serving and LAN access for the
single-actor server. It does not implement multi-user authentication, cloud
storage providers, TLS termination, or public internet exposure (reserved for
subsequent roadmap milestones). Preview frames remain isolated by origin and CSP
sandbox as specified in 0003 and 0019.
