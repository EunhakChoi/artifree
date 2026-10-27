# Artifree architecture

Status: early development, 2026-10-27. Local HTML artifacts work end to end;
see [decision 0003](decisions/0003-local-html-artifact-slice.md).

The [interactive HTML design](architecture.html) maps the design to actual source
paths and marks each module and flow step as working, scaffold, or planned.
Working today: creating HTML file-package artifacts, conditional revision
commits with retry keys, revision-pinned reads, and sandboxed previews, through
the HTTP API, MCP tools for agents
([decision 0004](decisions/0004-inbound-mcp-adapter.md)), the CLI, and the web
workspace, stored in local SQLite.

## Repository map

```text
apps/
  web/                 TypeScript · React/Vite · Tailwind 4 · shadcn/ui (Base UI) workspace
  cli/                 TypeScript · artifact commands and describe
packages/
  contracts/           generated TypeScript wire types and build inventory
  client/              HTTP API v1 client with runtime response checks
  runtime-sdk/         proposed capability and session-binding types
crates/
  artifree-domain/     identifiers, portable file packages, version counters; no dependencies
  artifree-core/       artifact service and owner policy; documents, jobs, handoff planned
  artifree-ports/      artifact store contract; other families planned
  artifree-providers/  SQLite artifact store; models, tools, build, export planned
  artifree-contracts/  Serde/Schemars wire DTOs and JSON Schema exporter
  artifree-server/     composition root, Axum HTTP API, MCP endpoint, preview origin
contracts/generated/  generated JSON Schemas (descriptor, API v1), MCP tool catalog, inventory snapshot
docs/architecture/    TypeScript, CSS, and HTML source for the standalone design
scripts/              contract generation, architecture build, checks, smoke
```

The web app owns its UI components under `apps/web/src/components/ui` and theme
tokens in `apps/web/src/style.css`. UI call sites use component variants and
semantic tokens. A second frontend can justify a shared UI package later.
Generated artifacts do not inherit management UI styling restrictions.

[Decision 0002](decisions/0002-ui-and-quality.md) records this stack and the
[quality gates](quality.md). They cover both language boundaries, rejection
contracts, source-linked design drift, zero-warning lint, Windows/MSRV, browser
accessibility, and dependency checks. [Decision 0003](decisions/0003-local-html-artifact-slice.md)
selects Axum on Tokio for HTTP and SQLite through `rusqlite` for local
storage; other providers remain candidates.

## Layer dependency direction

```text
TypeScript clients → inbound HTTP/MCP → Rust application core → outbound ports
                                                ↓                    ↑
                                           domain types       concrete providers

Rust server composition root wires the adapters, core, and providers together.
```

`ArtifactService` in the core is synchronous and generic over the
`ArtifactStore` port. The Axum server runs its commands on Tokio's blocking
pool; the core and the store have no async-runtime dependency. The MCP
adapter calls the same server operations as HTTP, and through them the same
service. Keep runtime dependencies out of domain
types rather than creating a generic runtime wrapper.

```text
CLI / scripts ─┐                       ┌─ API listener   /v1/...  (bearer token)
MCP agents ────┼─► artifree-server ────┤                 /mcp     (same token, MCP)
web (proxy) ───┘                       └─ content listener /p/<ticket>/...  (sandbox CSP)
                     │
                     ▼
         ArtifactService (owner check, fingerprint, apply changes)
                     │
                     ▼
         SqliteStore: one transaction per commit (revision CAS + files + request key)
```

## State and authority

- A source revision versions an artifact's authoritative source.
- A document sequence versions structured document operations.
- An app-data version describes separately persisted runtime data.
- Access policy, comments, and publication targets have their own lifecycles.

Source rollback must not silently roll back app data or permissions. File
packages and structured document models need different edit operations.

The future runtime runs untrusted content separately from the account UI. Its
session must bind actor, artifact, revision, and granted capabilities, and the
server must check current authority. Separate ports alone do not isolate cookie
scope. Public hosting needs an explicit origin, credential, and sandbox policy.
No such runtime enforcement exists in this scaffold.

## Local and server operation

Provider selection should preserve commit, conflict, retry, and authorization
semantics across deployments. Local persistence candidates are SQLite and files;
server candidates are PostgreSQL and object storage. They require separate
migrations and acceptance checks. A SQL commit and a blob write are not a shared
atomic transaction.

Temporary sessions require explicit TTL, cleanup, and ownership. Serverless
instances need external shared state even for short-lived sessions. A Vercel
frontend can connect to the Rust API and isolated workers hosted elsewhere.
Tunnels and private networks control reachability; application permissions stay
in the Rust core.

## HTTP API v1

| Method and path                                     | Purpose                                       |
| --------------------------------------------------- | --------------------------------------------- |
| `GET /v1/artifacts`                                 | Owned artifacts, most recently updated first  |
| `POST /v1/artifacts`                                | Create revision 1 (`Idempotency-Key`)         |
| `GET /v1/artifacts/{id}`                            | Metadata and current revision                 |
| `GET /v1/artifacts/{id}/revisions`                  | History, newest first                         |
| `POST /v1/artifacts/{id}/revisions`                 | Conditional commit (`Idempotency-Key`)        |
| `GET /v1/artifacts/{id}/revisions/{n}`              | One revision manifest                         |
| `GET /v1/artifacts/{id}/revisions/{n}/files/{path}` | Raw bytes of a pinned file (attachment)       |
| `POST /v1/artifacts/{id}/revisions/{n}/previews`    | Short-lived preview URL on the content origin |

Errors use `{ "error": { "code", "message", "current_revision"? } }` with
`invalid_request` (400/405/422; 405 carries `Allow`), `unauthenticated` (401), `not_found` (404),
`revision_conflict` (409), `request_key_reused` (422), `payload_too_large`
(413), `unavailable` (503, retry with the same key), and `internal` (500).
Bodies are defined in `crates/artifree-contracts/src/api.rs`.

## MCP

`POST /mcp` serves MCP Streamable HTTP through the official Rust SDK, `rmcp`,
statelessly and with JSON responses, for protocol versions up to 2026-07-28.
It requires the same bearer token, accepts only loopback `Host` names, and
refuses requests carrying an `Origin`. Eight tools mirror the HTTP routes:
`list_artifacts`, `get_artifact`, `create_artifact`, `save_revision`,
`list_revisions`, `get_revision`, `read_file`, and `create_preview`. Results
are the HTTP API v1 bodies; failures are those error bodies with
`isError: true`. The generated catalog is
`contracts/generated/mcp-tools.json`; [the MCP guide](mcp.md) shows Codex and
Pi configuration. These are agent tools, not page capabilities.

## Next implementation slices

- Publishing a pinned revision, restore, and fork, kept separate from app data
  and access policy.
- Multi-user authentication, sharing, and revocation tests.
- Build workers before running untrusted toolchains; then the runtime SDK
  bridge with server-checked capabilities.
- PostgreSQL/object storage held to the same store contract tests.
- Structured document types and optional AI capabilities.
