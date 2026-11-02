# Artifree architecture

Status: early development, 2026-10-31. Local HTML artifacts work end to end;
see [decision 0003](decisions/0003-local-html-artifact-slice.md).

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
`Retry-After` ([decision 0007](decisions/0007-live-web-workspace.md)).

## Repository map

```text
apps/
  web/                 TypeScript · React/Vite · Tailwind 4 · shadcn/ui (Base UI) workspace
  cli/                 TypeScript · artifact commands and describe
packages/
  contracts/           generated TypeScript wire types and build inventory
  client/              HTTP API v1 client with runtime response checks
  runtime-sdk/         bound metadata read; other capability types proposed
crates/
  artifree-domain/     identifiers, portable file packages, version counters; no dependencies
  artifree-core/       artifact service, owner policy, job and worker logic; documents planned
  artifree-ports/      artifact and job store contracts; other families planned
  artifree-providers/  SQLite artifact store, ZIP source archives; models, tools, build, export planned
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
- Access policy, comments, and publication targets have their own lifecycles.

Source rollback must not silently roll back app data or permissions. File
packages and structured document models need different edit operations.

The metadata runtime runs opted-in content separately from the account UI. Its
short-lived view binds the actor, artifact, source revision, view generation,
and initialization; the server checks access and the current source on each
read ([0019](decisions/0019-runtime-metadata-bridge.md)). A process-local
connection grant requires explicit consent for `metadata.read` and is checked
again on every call; other capabilities remain proposed
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

| Method and path                                                                    | Purpose                                           |
| ---------------------------------------------------------------------------------- | ------------------------------------------------- |
| `GET /v1/artifacts`                                                                | Owned artifacts: search, pin filter, order, pages |
| `POST /v1/artifacts`                                                               | Create revision 1 (`Idempotency-Key`)             |
| `GET /v1/artifacts/{id}`                                                           | Metadata and current revision                     |
| `PATCH /v1/artifacts/{id}`                                                         | Conditional rename or pin (`Idempotency-Key`)     |
| `GET /v1/artifacts/{id}/revisions`                                                 | History, newest first                             |
| `POST /v1/artifacts/{id}/revisions`                                                | Conditional commit (`Idempotency-Key`)            |
| `GET /v1/artifacts/{id}/revisions/{n}`                                             | One revision manifest                             |
| `GET /v1/artifacts/{id}/revisions/{n}/files/{path}`                                | Raw bytes of a pinned file, or one `Range`        |
| `POST /v1/artifacts/{id}/revisions/{n}/previews`                                   | Short-lived preview URL on the content origin     |
| `POST /v1/artifacts/{id}/revisions/{n}/runtime-views`                              | Issue a bound, short-lived runtime view           |
| `POST /v1/artifacts/{id}/revisions/{n}/runtime-views/{view}/{generation}/metadata` | Read the view's current metadata                  |
| `DELETE /v1/runtime-views/{view}/{generation}`                                     | Close one runtime view generation                 |
| `POST /v1/runtime-sessions`                                                        | Open an ephemeral runtime connection              |
| `DELETE /v1/runtime-sessions/{session}`                                            | Close the connection and revoke its grants        |
| `PUT /v1/runtime-sessions/{session}/artifacts/{id}/revisions/{n}/grant`            | Replace an observed connection grant              |
| `GET /v1/agent-sessions`                                                           | List the actor's registered inboxes               |
| `POST /v1/handoffs`                                                                | Stage a pinned source selection                   |
| `GET /v1/handoffs/{id}`                                                            | Read a handoff's state                            |
| `POST /v1/handoffs/{id}/send`                                                      | Confirm sending the staged delivery               |
| `POST /v1/handoffs/{id}/cancel`                                                    | Cancel before the agent may have received it      |
| `GET /v1/changes`                                                                  | Committed changes as server-sent events           |
| `POST /v1/artifacts/{id}/jobs`                                                     | Queue a `verify_revision` job (`Idempotency-Key`) |
| `GET /v1/artifacts/{id}/jobs`                                                      | One artifact's jobs, newest first, pages          |
| `GET /v1/jobs`                                                                     | The actor's jobs, newest first, pages             |
| `GET /v1/jobs/{id}`                                                                | One job: state, attempts, result or failure       |
| `POST /v1/jobs/{id}/cancel`                                                        | Stop a job (`Idempotency-Key`); not a rollback    |
| `GET /v1/artifacts/{id}/data`                                                      | Read your application data at a `?path=`          |
| `PUT /v1/artifacts/{id}/data`                                                      | Replace a value (`Idempotency-Key`, base version) |
| `PATCH /v1/artifacts/{id}/data`                                                    | Merge-patch a value (`Idempotency-Key`)           |
| `DELETE /v1/artifacts/{id}/data`                                                   | Remove a value (`Idempotency-Key`, with a body)   |

Errors use `{ "error": { "code", "message", "current_revision"?, "current_metadata_revision"?, "current_data_version"? } }` with
`invalid_request` (400/405/409/422; 405 carries `Allow`, and 409 can identify a stale runtime generation), `unauthenticated` (401), `not_found` (404),
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
reconnects after lost connections, `429`, and `5xx`, and stops on other
failures.

## MCP

`POST /mcp` serves MCP Streamable HTTP through the official Rust SDK, `rmcp`,
statelessly and with JSON responses, for protocol versions up to 2026-07-28.
It requires the same bearer token, accepts only loopback `Host` names, and
refuses requests carrying an `Origin`. Twenty-two tools are served. Eleven
mirror the artifact HTTP routes: `list_artifacts`, `get_artifact`,
`create_artifact`, `save_revision`, `restore_revision`, `duplicate_artifact`,
`update_artifact`, `list_revisions`, `get_revision`, `read_file`, and
`create_preview`. Four mirror the job routes: `enqueue_job`, `get_job`,
`list_jobs`, and `cancel_job` ([0030](decisions/0030-persistent-jobs-and-worker.md)).
Four mirror the application data routes: `get_data`, `set_data`, `update_data`,
and `delete_data` ([0031](decisions/0031-app-data.md)); the three that write
take their value as JSON text and are marked destructive, since nothing keeps
what they replace. Results
are the HTTP API v1 bodies; failures are those error bodies with
`isError: true`. MCP requests share the per-actor request limits with HTTP;
a refused call is an HTTP `429`. The generated catalog is
`contracts/generated/mcp-tools.json`; [the MCP guide](mcp.md) shows Codex and
Pi configuration. The three handoff tools register an agent inbox,
poll it, and acknowledge a delivered selection. They are not page callbacks;
an agent has to poll. All these are agent tools, not page capabilities.

## Next implementation slices

The [PR roadmap](roadmap.md) is the implementation plan. A01 (committed
change notifications and the deferred CLI retry diagnostic) and A02 (the web
workspace following committed changes) are implemented, and A03 adds renaming,
pins, search, order, and pages to the API, MCP, client, CLI
([0008](decisions/0008-artifact-metadata-and-lists.md)), and web workspace
([0009](decisions/0009-web-list-and-metadata.md)). A04a adds restoring an
earlier revision and file digests to the API, MCP, client, and CLI
([0010](decisions/0010-revision-restore-and-digests.md)), and A04b compares
and restores revisions in the web workspace, preparing no conditional write
from a version older than the page has learned of and reading nothing for
data that may have been replaced
([0011](decisions/0011-web-history-and-restore.md)). A05 duplicates a
revision's files into a new artifact with its own history through the API,
MCP, client, CLI, and web workspace
([0012](decisions/0012-duplicate-artifacts.md)). A06a deletes artifacts to a
trash and recovers them through the API, MCP, client, and CLI: deleting is a
conditional metadata update, and an artifact in the trash answers every
other request as an unknown one, except the replay of a request made before
the delete ([0013](decisions/0013-deleting-artifacts-and-the-trash.md)). A07
exports one revision's files as a deterministic, uncompressed ZIP archive
whose first entry is a manifest of paths, sizes, digests, and media types,
through the API, client, CLI, and web workspace; it is a read with the
access and trash rules of every read
([0015](decisions/0015-source-exports.md)). A11 reads part of a pinned
file: one byte range over HTTP, served by one parser of the `Range` field;
parts cut between characters over MCP, with the next offset and the whole
file's size and SHA-256; and a client that accepts only the exact part it
asked for ([0016](decisions/0016-bounded-file-reads.md)). A06b adds the
trash to the web workspace: the open artifact moves there after a
confirmation, the trash is shown from the list and recovered from, and
moves made elsewhere close the open artifact unless a draft holds it, which
nothing then reads for; what the page knows of the trash, like the versions
it knows, lasts one feed epoch, and answers to writes from an earlier epoch
move nothing ([0014](decisions/0014-web-trash.md)). A09 edits an
artifact's files in the web workspace: a draft spans files, adds, renames,
and deletes them, and is saved as one revision against its base or not at
all; every change passes one write point that decides by the state it
lands in, renames carry the bytes they move, and paths are checked with
the package rules the build inventory carries
([0017](decisions/0017-code-workspace.md)). A10 keeps that draft in the
browser's IndexedDB: the page starts claiming a tab owner id and its Web
Lock before anything renders and orders each artifact's writes, the source
view offers a kept draft for recovery or discard before the editor
starts, a recovered draft counts as written on its revision only if the
server lists it with the same digests, and a save whose answer was lost
replays with its kept key. Without Web Locks, draft reads and writes are
refused with an explicit warning to protect other tabs' records
([0018](decisions/0018-source-draft-recovery.md)).
R04 adds the first bound runtime metadata read ([0019](decisions/0019-runtime-metadata-bridge.md)),
and R05 adds connection-scoped metadata consent and grants
([0020](decisions/0020-runtime-grants.md)). A08 stages a saved source
selection and, after user confirmation, delivers it to an explicitly registered
MCP inbox ([0021](decisions/0021-agent-selection-handoff.md)). Its receipt
acknowledgement is separate from model work and its state is process-local.
O05 removes an artifact in the trash for good (`POST
/v1/artifacts/{id}/purge`, administrative credential only, conditional on its metadata revision and
retryable by key) and deletes file content no revision file refers to
(`GET` for the dry run and its set digest, `DELETE` conditional on that digest
and retryable by key, `/v1/content/unreferenced`). A purge keeps a tombstone so that
request keys, change feed sequences, and foreign keys stay valid, and it has no
MCP tool, because an agent could not undo it
([0029](decisions/0029-retention-and-cleanup.md)).
R01 adds persistent jobs and a worker process
([0030](decisions/0030-persistent-jobs-and-worker.md)). One job kind,
`verify_revision`, pins an actor, artifact, revision, and request key, and
recomputes the digest and size of each file of the revision from the store.
The job states and the lease live in SQLite (schema 8); a worker claims a job
under a lease and a fence that rises on every claim, so a worker that stalled
and was replaced cannot write a result. Execution is at least once; one result
is recorded. `artifree-server worker run` is a subcommand of the server binary
that opens the database directly (no HTTP, no credential); nothing starts it.
The job runs trusted code in that process: it is not R02's isolated execution,
and no builds, models, or untrusted code run. Job changes are not in the change
feed, and the web workspace has no job view. Agent tokens may enqueue, read,
list, and cancel jobs (the four MCP tools), at most 16 open jobs per actor.
R06 adds application data ([0031](decisions/0031-app-data.md)): one JSON
object per actor and artifact with its own `data_version` (schema 9),
unrelated to the source revision and untouched by saving or restoring one.
Paths are `/`-separated segments that address object members; values are strict
JSON, canonicalized, and limited (256 KiB per value, 1 MiB and 10,000 values per
space, 32 levels). A write is conditional on `base_data_version` and keyed, in
the key space shared with artifact, cleanup, and job requests; a write that
changes nothing keeps the version. `update` is an RFC 7396 merge patch, `set`
can store `null`, and a path through a value that is not an object is
`data_path_conflict`. HTTP, the MCP tools, the client, and the CLI share one
core service, and issued agent tokens may use them. A duplicate copies the
data only with `copy_data`. Data is not in the change feed, a revision, or an
export, a purge erases it, and **no page can read or write it: the runtime
bridge grants only `metadata.read`**. Subscriptions and queries (R07) and
assets (R09) are not implemented.

The roadmap separately covers artifact management, local/server deployment,
isolated builds, the runtime SDK and app data, optional providers, structured
artifact types and conversions, and multi-user sharing/publication. Publishing
must choose and document whether its address follows the head or a pinned
revision. PostgreSQL and external object storage are conditional extensions;
single-owner hosting can keep SQLite.
