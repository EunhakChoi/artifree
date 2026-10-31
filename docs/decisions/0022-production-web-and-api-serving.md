# 0022: Production web asset and API serving

Date: 2026-10-31. Status: accepted. Builds on
[0003](0003-local-html-artifact-slice.md), [0004](0004-inbound-mcp-adapter.md),
and [0005](0005-request-limits.md).

## Context

Running Artifree previously required a separate Vite development server for
the web interface alongside the Rust API server. In a production or private
server environment, users need a self-contained execution model where static
web assets and API endpoints are served by a single binary without Node.js or
Vite at runtime.

At the same time, existing security boundaries must remain strictly enforced:
the preview frame must continue to run on an isolated origin with a restrictive
CSP sandbox; cross-origin API access must support authorized web clients while
denying the preview origin and null origins; loopback-only MCP policies must
not be weakened by LAN exposure; and CLI tools and web workspaces must support
deterministic connection profiles without risk of credential leakage.

## Decision

1. **Integrated Web Asset Serving.** `artifree-server` serves built static web
   assets when configured with `--web-dir <dir>` or `ARTIFREE_WEB_DIR`.
   Static assets under `/assets/*` are served with MIME detection and immutable
   cache headers (`Cache-Control: public, max-age=31536000, immutable`).
   Non-asset requests outside `/v1/*`, `/mcp`, and `/healthz` fall back to
   `index.html` with `no-cache` headers to support client-side SPA routing.
   Path traversal is strictly prevented by rejecting paths with `..` or
   non-canonical components.
2. **Local and Explicit LAN Binding.** The server defaults to loopback
   (`127.0.0.1:7410` for API/web, `127.0.0.1:7411` for preview). When explicit
   LAN access is requested via `--lan <ip>` or `ARTIFREE_LAN`, the server binds
   the specified IP for API/web and preview (with IPv6 properly bracketed in
   URIs). The preview origin remains on a distinct port (default 7411) from the
   API/web origin. The CSP `frame-ancestors` directive for preview documents
   is dynamically derived from the effective API listener address.
3. **CORS and Admission Rate Limiting.** Outer CORS middleware on `/v1/*`
   permits cross-origin browser clients while strictly rejecting the preview
   origin (`content_origin`) and `null` origin. Unauthenticated CORS preflight
   `OPTIONS` requests return `204 No Content` with allowed methods (`GET`,
   `POST`, `PUT`, `PATCH`, `DELETE`, `HEAD`, `OPTIONS`) and allowed headers
   (`Authorization`, `Content-Type`, `If-Match`, `Idempotency-Key`,
   `Last-Event-ID`). Unmatched `/v1/*` routes require bearer authentication
   before 404 to prevent route enumeration, pass through the admission
   limiter, and return JSON 404 with CORS headers.
4. **Strict Isolation of MCP and Preview.** MCP (`/mcp`) remains strictly
   confined to loopback hosts (`localhost`, `127.0.0.1`, `[::1]`) and rejects
   any request bearing an `Origin` header with 403 Forbidden. LAN exposure of
   the API does not expose MCP to the network. Preview documents are served
   with sandbox `allow-scripts allow-forms allow-modals allow-downloads` without
   `allow-same-origin`.
5. **CLI Connection Profiles.** The CLI resolves server connection parameters
   with strict precedence: CLI flags (`--server`, `--token`, `--token-file`) >
   environment variables (`ARTIFREE_SERVER`, `ARTIFREE_TOKEN`) > profile configuration
   (`~/.artifree/profiles.json`) > defaults (`http://127.0.0.1:7410`).
   Configuration files undergo strict runtime type and shape validation, throwing
   descriptive `UsageError` on invalid types, missing requested profiles, or
   missing config files when an explicit profile is requested.
6. **Web Workspace Server Configuration.** The web workspace stores tokens in
   origin-scoped `sessionStorage` (`artifree.accessToken.<origin>`) to prevent
   cross-origin credential exfiltration. Submitting a blank server URL explicitly
   clears the stored server override, returning to origin-relative requests.

## Limits

This decision covers single-binary production serving and private LAN operation.
Public Internet exposure, HTTPS reverse proxy automation, token issuance/rotation,
multi-tenant authentication, and background workers are deferred to subsequent
roadmap items (O02+). LAN HTTP environments lack secure contexts, which disables
browser Web Locks and limits multi-tab draft recovery as documented in ADR 0018.
