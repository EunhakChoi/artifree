# Artifree architecture

Status: early development, 2026-11-10. Local HTML artifacts work end to end;
see [decision 0003](decisions/0003-local-html-artifact-slice.md). This document
describes structure. What each slice implemented and what remains is recorded in
the [roadmap](roadmap.md) and the [decision records](decisions/).

The [interactive HTML design](architecture.html) maps the design to actual source
paths and marks each module and flow step as working, scaffold, or planned.
Working today: creating HTML file-package artifacts, conditional revision
commits with retry keys, revision-pinned reads, and sandboxed previews, through
the HTTP API, MCP tools for agents
([decision 0004](decisions/0004-inbound-mcp-adapter.md)), the CLI, and the web
workspace, stored in local SQLite. Clients can follow committed changes as an
event stream ([decision 0006](decisions/0006-change-feed.md)), and the web
workspace follows it, keeping pinned previews and unsaved drafts, and sends
its requests through one gate that holds every read for a `429`'s
`Retry-After` ([decision 0007](decisions/0007-live-web-workspace.md)). On the
same store and service there are also application data, uploaded assets,
persistent jobs with a worker process, user-isolated access for several users,
and a runtime bridge through which an opted-in preview frame uses eight
separately granted capabilities; see [Implemented areas](#implemented-areas).

## Repository map

```text
apps/
  web/                 TypeScript · React/Vite · Tailwind 4 · shadcn/ui (Base UI) workspace
  cli/                 TypeScript · artifact commands and describe
packages/
  contracts/           generated TypeScript wire types and build inventory
  client/              HTTP API v1 client with runtime response checks; opt-in outbox entry point
  runtime-sdk/         frame side of the runtime bridge: metadata, source and uploaded assets; data and rooms proposed
crates/
  artifree-domain/     identifiers, portable file packages, version counters; no dependencies
  artifree-core/       artifact, data and asset services, asset validation, rooms, jobs and worker logic; documents planned
  artifree-ports/      artifact, data, asset, job, token and user store contracts
  artifree-providers/  SQLite stores, backup and restore, ZIP source archives, execution-host inspection; models, tools, build, export planned
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
         SqliteStore: one transaction per commit
                      (revision CAS + files + request key + change feed row)
```

## State and authority

- A source revision versions an artifact's authoritative source.
- A document sequence versions structured document operations.
- An app-data version describes separately persisted runtime data.
- An asset version describes one actor/artifact upload space independently.
- Access policy, comments, and publication targets have their own lifecycles.

Source rollback must not silently roll back app data or permissions. File
packages and structured document models need different edit operations.

The metadata runtime runs opted-in content separately from the account UI. Its
short-lived view binds the actor, artifact, source revision, view generation,
and initialization; the server checks access and the current source on each
read ([0019](decisions/0019-runtime-metadata-bridge.md)). A process-local
connection grant requires explicit consent for `metadata.read` and is checked
again on every call. R11a adds read-only `source.read` and
`source.download`, each declared, consented, and granted on its own: the server
checks the binding, the connection's grant of that one capability, the live
artifact, and the artifact's current head on every call and again before it
returns bytes, answers `404` for a binding that does not match, `403` for a
missing grant, and `409` for a head that moved, and limits frame traffic apart
from the owner's. The workspace asks the server for an offer before it shows the
user a prompt, and a download only starts after the user confirms
([0032](decisions/0032-runtime-source-read-and-download.md)). R11b adds
`source.write` under its own consent: the server saves one file through the
artifact service with the view's revision as the base and a key in a namespace
only a frame can produce, reads the new revision back, and every side compares
the receipt with the bytes it sent. The save moves the head, so the next view
needs a new grant
([0033](decisions/0033-runtime-source-write.md)). R09d adds `assets.read`,
`assets.write` and `assets.download` in the same way, each its own declaration,
consent and grant: reads are assembled from parts and verified by digest,
writes carry the frame's own asset base and a key only a frame can produce, and
a download asks the server for an offer before the user is prompted
([0045](decisions/0045-uploaded-assets.md)). R12 adds rooms, which
are not a capability and not storage: an in-memory, bounded room per actor and
artifact, where each runtime connection (a tab) joins as named participants
under a 15 s lease, and the server computes `isMe` and `sameTab` from the
connection it issued. Events are ephemeral and never replayed to a late joiner;
a restart empties every room; a frame cannot reach one yet (R15), and there
are no roles
([0034](decisions/0034-room-and-presence.md)). With its own `data.read` grant a frame reads the
application data of its artifact, as JSON text the host never parses
([0053](decisions/0053-runtime-app-data.md)); it cannot write it yet (R14b).
The other capabilities remain proposed
([0020](decisions/0020-runtime-grants.md)). Separate ports alone do not isolate cookie
scope. Public hosting needs an explicit origin, credential, and sandbox policy.

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

| Method and path                                                                    | Purpose                                                 |
| ---------------------------------------------------------------------------------- | ------------------------------------------------------- |
| `GET /v1/artifacts`                                                                | Owned artifacts: search, pin filter, order, pages       |
| `POST /v1/artifacts`                                                               | Create revision 1 (`Idempotency-Key`)                   |
| `GET /v1/artifacts/{id}`                                                           | Metadata and current revision                           |
| `PATCH /v1/artifacts/{id}`                                                         | Conditional rename or pin (`Idempotency-Key`)           |
| `GET /v1/artifacts/{id}/revisions`                                                 | History, newest first                                   |
| `POST /v1/artifacts/{id}/revisions`                                                | Conditional commit (`Idempotency-Key`)                  |
| `POST /v1/artifacts/{id}/edits`                                                    | Per-file conditional commit (`Idempotency-Key`)         |
| `GET /v1/artifacts/{id}/revisions/{n}`                                             | One revision manifest                                   |
| `GET /v1/artifacts/{id}/revisions/{n}/files/{path}`                                | Raw bytes of a pinned file, or one `Range`              |
| `POST /v1/artifacts/{id}/revisions/{n}/previews`                                   | Short-lived preview URL on the content origin           |
| `POST /v1/artifacts/{id}/revisions/{n}/runtime-views`                              | Issue a bound, short-lived runtime view                 |
| `POST /v1/artifacts/{id}/revisions/{n}/runtime-views/{view}/{generation}/metadata` | Read the view's current metadata                        |
| `POST .../runtime-views/{view}/{generation}/source/list`                           | List the pinned revision's files (`source.read`)        |
| `POST .../runtime-views/{view}/{generation}/source/read`                           | Read one part of a file (`source.read`)                 |
| `POST .../runtime-views/{view}/{generation}/source/offer`                          | Ask what a download would be (`source.download`)        |
| `POST .../runtime-views/{view}/{generation}/source/download`                       | The bytes of a confirmed download                       |
| `POST .../runtime-views/{view}/{generation}/assets/list`                           | List the artifact's assets (`assets.read`)              |
| `POST .../runtime-views/{view}/{generation}/assets/read`                           | Read one part of an asset (`assets.read`)               |
| `POST .../runtime-views/{view}/{generation}/assets/upload`                         | Upload one asset (`assets.write`)                       |
| `POST .../runtime-views/{view}/{generation}/assets/delete`                         | Delete one asset (`assets.write`)                       |
| `POST .../runtime-views/{view}/{generation}/assets/offer`                          | Ask what an asset download would be (`assets.download`) |
| `POST .../runtime-views/{view}/{generation}/assets/download`                       | The bytes of a confirmed asset download                 |
| `POST .../runtime-views/{view}/{generation}/data/read`                             | Read the application data at a path (`data.read`)       |
| `DELETE /v1/runtime-views/{view}/{generation}`                                     | Close one runtime view generation                       |
| `POST /v1/runtime-sessions`                                                        | Open an ephemeral runtime connection                    |
| `DELETE /v1/runtime-sessions/{session}`                                            | Close the connection and revoke its grants              |
| `PUT /v1/runtime-sessions/{session}/artifacts/{id}/revisions/{n}/grant`            | Replace an observed connection grant                    |
| `GET /v1/agent-sessions`                                                           | List the actor's registered inboxes                     |
| `POST /v1/handoffs`                                                                | Stage a pinned source selection                         |
| `GET /v1/handoffs/{id}`                                                            | Read a handoff's state                                  |
| `POST /v1/handoffs/{id}/send`                                                      | Confirm sending the staged delivery                     |
| `POST /v1/handoffs/{id}/cancel`                                                    | Cancel before the agent may have received it            |
| `GET /v1/changes`                                                                  | Committed changes as server-sent events                 |
| `POST /v1/artifacts/{id}/jobs`                                                     | Queue a `verify_revision` job (`Idempotency-Key`)       |
| `GET /v1/artifacts/{id}/jobs`                                                      | One artifact's jobs, newest first, pages                |
| `GET /v1/jobs`                                                                     | The actor's jobs, newest first, pages                   |
| `GET /v1/jobs/{id}`                                                                | One job: state, attempts, result or failure             |
| `POST /v1/jobs/{id}/cancel`                                                        | Stop a job (`Idempotency-Key`); not a rollback          |
| `GET /v1/artifacts/{id}/data`                                                      | Read your application data at a `?path=`                |
| `PUT /v1/artifacts/{id}/data`                                                      | Replace a value (`Idempotency-Key`, base version)       |
| `PATCH /v1/artifacts/{id}/data`                                                    | Merge-patch a value (`Idempotency-Key`)                 |
| `DELETE /v1/artifacts/{id}/data`                                                   | Remove a value (`Idempotency-Key`, JSON body)           |
| `GET /v1/artifacts/{id}/data/members`                                              | One page of an object's member names (`?path=`)         |

The table lists the main routes. The contract in
`crates/artifree-contracts/src/api.rs` and the generated schema are
authoritative; restore, duplicate, export, purge, content cleanup, users,
tokens, rooms and the outbox binding are described in their decision records.

Private upload HTTP routes (0045): `GET/POST
/v1/artifacts/{id}/assets`, `GET/DELETE /v1/artifacts/{id}/assets/{asset_id}`,
and `GET /v1/artifacts/{id}/assets/{asset_id}/content`. Upload and delete require
an `Idempotency-Key` and `base_asset_version`. UTF-8 plain text/CSV and
complete static PNG (validated by `png`, `miniz_oxide` and `crc32fast`) and
complete sequential JPEG (an Artifree entropy walk, then `zune-jpeg`) are
admitted; other JPEG variants are `invalid_asset` (decision 0045, rule 3). Historical
MIME metadata and matching receipts remain readable/replayable before current
admission policy. Reads are authenticated scoped attachments with
no-store/nosniff; there is no public URL, and a frame reaches assets only through
the runtime view routes above. The client calls, the MCP tools, the CLI
commands, the web panel and the duplicate asset copy exist. The [content policy](asset-validation.md) records these limits.
SQLite schema 12 keeps immutable bytes, independent versions, receipts and
logical quotas in the same database as source files. Source and asset
associations both protect shared blobs from purge and cleanup.

Errors use `{ "error": { "code", "message", "current_revision"?, "current_metadata_revision"?, "current_data_version"?, "current_asset_version"? } }` with
`invalid_request` (400/405/409/422; 405 carries `Allow`, and 409 can identify a stale runtime generation), `unauthenticated` (401), `forbidden` (403, a runtime frame call whose capability is not granted), `not_found` (404),
`revision_conflict`, `metadata_conflict`, `data_conflict`, `data_path_conflict`, `job_ended`, and `too_many_jobs` (409), `request_key_reused` and `data_too_large` (422), `payload_too_large`
(413), `range_not_satisfiable` (416 with `Content-Range: bytes */{size}`), `rate_limited` (429 with
`Retry-After`; this attempt was not processed),
`unavailable` (503, retry with the same key), and `internal` (500).

Every request is authenticated first and then admitted under one per-actor
budget shared with MCP: a burst, a sustained rate, and a cap on concurrent
requests ([decision 0005](decisions/0005-request-limits.md)).
Bodies are defined in `crates/artifree-contracts/src/api.rs`.

`GET /v1/changes` sends `ready`, `change`, and `reset` events whose `id` is
an opaque cursor naming the database and a position in its feed. Each commit
records its change in its own transaction, so replays and failed commits
send nothing, and a client that reconnects with `Last-Event-ID` receives
every commit it missed, also after a restart; a cursor of another database
gets `reset`. Events name the artifact and revision to read again and carry
no content. An open stream takes one of the actor's `--change-streams` slots
instead of a request slot. The TypeScript client's `followChanges`
reconnects after lost connections, `429`, and `5xx` with jittered, growing
delays, and stops on other failures.

## MCP

`POST /mcp` serves MCP Streamable HTTP through the official Rust SDK, `rmcp`,
statelessly and with JSON responses, for protocol versions up to 2026-07-28.
It requires the same bearer token, accepts only loopback `Host` names, and
refuses requests carrying an `Origin`. Twenty-seven tools are served. Twelve
mirror the artifact HTTP routes: `list_artifacts`, `get_artifact`,
`create_artifact`, `save_revision`, `edit_files`, `restore_revision`, `duplicate_artifact`,
`update_artifact`, `list_revisions`, `get_revision`, `read_file`, and
`create_preview`. Four mirror the job routes: `enqueue_job`, `get_job`,
`list_jobs`, and `cancel_job` ([0030](decisions/0030-persistent-jobs-and-worker.md)).
Four mirror the application data routes: `get_data`, `set_data`, `update_data`,
and `delete_data` ([0031](decisions/0031-app-data.md)); the three that write
take their value as JSON text and are marked destructive, since nothing keeps
what they replace. Four mirror the asset routes: `list_assets`, `read_asset`,
`upload_asset`, and `delete_asset` ([0045](decisions/0045-uploaded-assets.md)); uploads
and deletes are marked destructive, and bytes travel as base64, never a host
path. Results
are the HTTP API v1 bodies; failures are those error bodies with
`isError: true`. MCP requests share the per-actor request limits with HTTP;
a refused call is an HTTP `429`. The generated catalog is
`contracts/generated/mcp-tools.json`; [the MCP guide](mcp.md) shows Codex and
Pi configuration. The three handoff tools register an agent inbox,
poll it, and acknowledge a delivered selection. They are not page callbacks;
an agent has to poll. All these are agent tools, not page capabilities.

## Implemented areas

The [PR roadmap](roadmap.md) is the implementation plan and holds the detail of
every slice. This table only points from an area to its records.

| Area                          | Roadmap                  | Decisions                          | Where                                                                                                                                                      |
| ----------------------------- | ------------------------ | ---------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Artifacts and revisions       | A03–A07, A11, X01        | 0003, 0008–0016, 0035              | `core/artifacts.rs`, `providers/storage.rs`                                                                                                                |
| Change feed and live web      | A01, A02                 | 0006, 0007                         | `server/changes.rs`, `client/change-feed.ts`, `apps/web`                                                                                                   |
| Code workspace and drafts     | A09, A10                 | 0017, 0018                         | `apps/web/src/artifacts`                                                                                                                                   |
| Agent tools and handoff       | A08                      | 0004, 0021                         | `server/mcp.rs`, `server/handoffs.rs`                                                                                                                      |
| Request limits                | O07                      | 0005, 0048                         | `server/limits.rs`, `server/http/unread.rs`                                                                                                                |
| Serving, access and tokens    | O01, O02, O03d           | 0022–0024, 0028                    | `server/serve.rs`, `server/proxy.rs`, `server/origin.rs`                                                                                                   |
| Packages and container        | O03a–O03c                | 0025, 0026, 0037                   | `scripts/package-release.mjs`, `deploy/`                                                                                                                   |
| Backup, retention, one-shot   | O04, O05, O06            | 0027, 0029, 0039                   | `providers/backup.rs`, `server/maintenance.rs`, `server/one_shot.rs`                                                                                       |
| Jobs and worker               | R01                      | 0030                               | `core/jobs.rs`, `core/worker.rs`, `providers/storage/jobs.rs`                                                                                              |
| Application data              | R06, R06b, R07           | 0031, 0040, 0046                   | `domain/data.rs`, `core/data.rs`, `providers/storage/data.rs`                                                                                              |
| Browser data outbox (partial) | R08a–R08c                | 0041, 0043, 0044, 0047, 0049       | `client/outbox/` (its own entry point; the public sender is off)                                                                                           |
| Uploaded assets               | R09                      | 0045                               | `core/assets.rs`, `core/asset_validation/`, `providers/storage/assets.rs`                                                                                  |
| Runtime bridge                | R04, R05, R11, R09d, R13 | 0019, 0020, 0032, 0033, 0045, 0052 | `server/runtime_*.rs`, `server/http/runtime.rs`, `client/runtime-*.ts`, `web/artifacts/frame-rpc.ts`, `web/artifacts/frame-calls/`, `packages/runtime-sdk` |
| Rooms                         | R12                      | 0034                               | `core/room.rs`, `server/rooms.rs`, `client/room.ts`                                                                                                        |
| Users                         | S01                      | 0036                               | `ports/user_store.rs`, `providers/users.rs`, `server/users.rs`                                                                                             |

Proposed and not implemented: untrusted execution
([0042](decisions/0042-linux-untrusted-execution.md) is a proposed boundary
with read-only host inspection only), React builds, view replacement, frame
access to application data and rooms, model and connector providers, structured
document types and conversions, roles and sharing, publishing, PostgreSQL and
external object storage. The module inventory in
`crates/artifree-server/src/inventory.rs` marks the same split and is a
build-time snapshot, not a health response.

## Structural notes

[Decision 0051](decisions/0051-roadmap-and-architecture-review.md) reviewed the
structure after R09. The layering above is kept. These notes describe where the
code is under strain and what new work should do about it; none of them is a
migration.

- **One capability, many places.** A runtime call is spread over the contract,
  the core service for a write, a server module and its handler in
  `http/runtime.rs`, the client's family module, re-export list and route
  name, the host's descriptor in `frame-calls/`, and the SDK's types and
  bootstrap. A new capability also needs its entry in `runtime_policy.rs`
  (`CapabilitySet` holds eight bits and all eight are used; R14b widens it), its descriptors
  registered in `frame-rpc.ts`, the permission control, and the prompt if it
  hands the user a file. The client, the host and the frame each check what
  they receive on purpose. R13
  ([0052](decisions/0052-runtime-bridge-consolidation.md)) removed the
  repetition around those checks; it did not reduce the number of files,
  because most are one per layer. Its tests catch some omissions (a route the
  gate does not know, limits that differ between layers) but not a layer left
  out of a new call, so follow an existing call through every place above.
- **Every user-owned state is scoped to one actor**: application data, assets
  and rooms by actor and artifact; request keys, jobs and the change sequence
  by actor. Access is
  the artifact's own user, decided in `core/policy.rs` and in the change feed's
  storage query. Sharing has to decide for each state whether it is per user or
  shared (S02s) before more state is built on that scope. Decision 0051 sets
  the direction for application data and assets: per user by default, shared
  only in a space the artifact declares. The scope of rooms under sharing is
  left to S02s.
- **Background work has no owner in the server process.** The worker is a
  subcommand that opens the database directly and nothing starts or supervises
  it; job changes are not in the change feed. Builds, the execution supervisor
  and automatic purge need the same scheduler, clock and supervision (R01b).
- **Application logic in the server crate.** The runtime sessions, policy,
  source and asset modules, preview tickets and handoffs are state machines
  that need no HTTP framework but live in `artifree-server` and depend on its
  state and on `SqliteStore`. Rooms show the intended arrangement: logic in
  `core/room.rs`, a thin adapter in `server/rooms.rs`. New framework-free logic
  follows rooms; existing modules are not moved for their own sake.
  `core/agent_handoff.rs` and `core/documents.rs` are still short "planned"
  modules, although handoffs are implemented in the server (0021).
- **Four files change with almost every feature** and are over two thousand
  lines each: `server/http.rs`, `server/operations.rs`,
  `contracts/src/api.rs` and `client/src/client.ts`. They are split by resource
  family when a change touches that family.
- **A new aggregate gets its own port and service module.** Data, assets and
  jobs did; documents and builds should, rather than widening `ArtifactStore`.
- **State in process memory**: runtime views and connections, grants, rooms,
  handoff inboxes, preview tickets and request budgets. A restart empties them
  and several instances cannot share them (X04).
- **One SQLite file, one connection per call.** A commit, its request key and
  its change row are one transaction. This is the design for a single server;
  PostgreSQL and object storage stay conditional extensions (X02, X03).
