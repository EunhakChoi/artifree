# 0023: Single-owner token management

Date: 2026-11-01. Status: accepted. Builds on
[0001](0001-foundation.md),
[0003](0003-local-html-artifact-slice.md),
[0004](0004-inbound-mcp-adapter.md), and
[0022](0022-production-web-and-api-serving.md).

## Context

Artifree previously authenticated all HTTP API and MCP requests through a single
bearer token read from the server's `access-token` file. Enabling multiple agents
and external client integrations without sharing the root secret requires a
manageable credential lifecycle.

At the same time, Artifree's design intentionally models a single local owner
(decision 0001). Introducing token management must not expand the system into a
complex multi-user permission model, generic role-based access control (RBAC), or
fragmented resource ownership.

## Decision

1. **Single-owner authentication semantics.** Both the primary bootstrap token
   and issued agent tokens authenticate the same built-in owner actor (`local`).
   Tokens do not represent separate user accounts; all artifacts, revisions,
   handoffs, and change streams belong to the single owner.
2. **Administrative authority boundary.** Administrative actions—issuing,
   listing, rotating, and revoking tokens via `/v1/tokens`—require administrative
   authority (`RequireAdmin`). Only the bootstrap owner token possesses
   `is_admin: true`. Requests to `/v1/tokens` using agent tokens are rejected
   with `403 Forbidden` (`ErrorCode::Forbidden`).
3. **Secret exposure and storage safeguards.**
   - Plaintext secrets are generated from cryptographically secure random bytes
     (`art_` followed by 64 hex characters) and returned in API responses
     strictly once upon creation (`POST /v1/tokens`) or rotation
     (`POST /v1/tokens/{id}/rotate`).
   - SQLite storage retains only the SHA-256 hex digest (`secret_hash`), a masked
     display prefix (`art_12345678...`), creation timestamp, and optional
     revocation/last-used timestamps in a `STRICT` table. Plaintext secrets are
     never stored in the database.
   - `AccessToken` implements a custom `Debug` format (`AccessToken(..)`) to
     prevent credential leakage in server logs.
4. **Cascading revocation and rotation.**
   - Rotating or revoking an agent token takes effect immediately: subsequent
     requests with previous bearer secrets fail with `401 Unauthorized`.
   - Revocation cascades to active stateful resources tied to the revoked token:
     active Server-Sent Events (SSE) change streams are woken and terminated
     immediately via Tokio `watch` channels; outstanding preview tickets and
     runtime sessions created under that token are invalidated.
5. **Restart persistence.**
   - Token metadata and secret hashes are stored in the SQLite database
     (`tokens` table in `artifree.sqlite3`).
   - Active tokens continue to authenticate across server restarts, and revoked
     tokens remain revoked.
   - The bootstrap token remains pinned to the local file `access-token`; it
     cannot be rotated or revoked through the API (`403 Forbidden`).
6. **Client and CLI integration.**
   - The `@artifree/client` SDK exposes `listTokens()`, `createToken()`,
     `rotateToken()`, and `revokeToken()` with runtime schema checks.
   - The `artifree token` CLI command provides `list`, `create`, `rotate`, and
     `revoke` subcommands supporting `--json`, connection profiles, and
     environment variable overrides.

## Consequences

- Agents receive scoped credentials that can be individually revoked or rotated
  without breaking other tools or changing the server's root token.
- Resource management remains unified under the single local owner without
  unnecessary multi-tenant or RBAC complexity.
- Revocation actively tears down open streaming connections and live sandbox
  tickets rather than waiting for natural session expiry.
