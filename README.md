# Artifree

**Open artifacts. Any agent.**

An independently written workspace for agent-made apps, documents, slides,
dashboards, designs, and motion. The goal is to run it on your own machine or
server and use it from a browser, CLI, HTTP API, or MCP-capable agent.

**Current stage: early development.** One vertical slice works: HTML
file-package artifacts are created, saved as immutable revisions with conflict
detection and safe retries, and previewed in a sandbox on a separate origin —
through the HTTP API, MCP tools for agents, the CLI, and the web workspace,
stored in a local SQLite file. It is single-user and listens on loopback by
default. A runtime preview may read its own artifact metadata, list and read
the files of its pinned revision, and ask the user to save one of them or the
source archive, each through a bound bridge after its own connection-scoped
user grant; it cannot change anything. One persistent job, `verify_revision`, re-checks a revision's stored files
against their digests in a separate worker process the operator starts
(`artifree-server worker run`). Publishing, sharing, build and conversion
workers, sandboxed execution of untrusted code, and multi-user authentication
are not implemented. The module inventory shown by the UI and
CLI is a build-time snapshot, not live server health.

## Quick start

Requirements: Node.js 22.12 or newer, npm 10 or newer, and rustup. The repository
pins its tested Rust toolchain in `rust-toolchain.toml`.

```sh
npm ci
npm run build
cargo run -p artifree-server -- serve --data-dir .artifree/data --web-dir apps/web/dist
```

The server prints one JSON readiness line with the API and web URL (default
`http://127.0.0.1:7410`), the preview origin (`http://127.0.0.1:7411`), and the
token file. On Unix the server creates the data directory readable only by
you and refuses an existing one that other users can access.

Open `http://127.0.0.1:7410` in a browser and paste the token from
`.artifree/data/access-token` to use the workspace directly without Vite.

When running behind an HTTPS reverse proxy (e.g. Caddy, Nginx, or Traefik terminating TLS),
configure external origins and trusted proxy networks explicitly:

```sh
cargo run -p artifree-server -- serve \
  --data-dir .artifree/data \
  --web-dir apps/web/dist \
  --api-origin https://artifree.example.com \
  --content-origin https://preview.example.com \
  --trusted-proxy 127.0.0.1/32 \
  --allowed-host artifree.example.com \
  --allow-remote-mcp
```

Container and service files can use the variables `ARTIFREE_API_ORIGIN`,
`ARTIFREE_CONTENT_ORIGIN`, `ARTIFREE_FRAME_ANCESTORS`, `ARTIFREE_ALLOWED_ORIGINS`
and others instead of flags (a flag wins; see `artifree-server --help`). A static
copy of the web workspace on another host can be built with
`VITE_ARTIFREE_SERVER=https://api.example.com npm run build` to start with that
server ([remote access](docs/remote-access.md)). Allow-lists are read at start-up;
changing them needs a restart.

The proxy must append (or overwrite) the client address in `X-Forwarded-For` or
`Forwarded` and strip the header it does not use. A request with no client
address, or a chain made only of trusted proxies, has an unknown client, and
`/mcp` refuses unknown clients unless `--allow-remote-mcp` is set. Local agents
should connect directly to the loopback listener, not through a trusted proxy
(decision 0024).

Linux x64 and Windows x64 release archives (server, built web workspace, a
bundled CLI that needs Node.js 22.12 or newer, and a SHA-256 manifest) are built
by CI and checked by unpacking them into an empty directory, starting, saving,
previewing, and restarting, and carry the license texts of the code inside.
A tag-triggered workflow can publish them with attestations
([decision 0037](docs/decisions/0037-release-publishing-and-supply-chain.md)),
but **nothing has been published or signature-checked yet**. See
[release packages](docs/release-packages.md). A [container image](docs/container.md)
built from the Linux archive is checked in CI the same way (nothing is
published), and the [remote access guides](docs/remote-access.md) cover a
reverse proxy with TLS, tunnels, a private network, and a static web UI on
another host; none of those third-party services was run.

The `backup` command makes a consistent copy of a running server's database
and proves that it restores; `restore` builds a new data directory from it,
and starting a newer build on an older data directory upgrades it. See the
[backup guide](docs/backup-restore.md) ([decision 0027](docs/decisions/0027-backup-restore-and-schema-upgrade.md)).
The access token is not part of a backup; a restored directory gets a new
one. Windows and container use of these commands is not verified outside CI.

A one-shot session ([decision 0038](docs/decisions/0038-one-shot-session.md))
is the other storage mode: `artifree-server serve --one-shot-ttl-seconds 3600`
(instead of `--data-dir`) runs the same server on a new private temporary data
directory and stops after that many seconds (1 to 604800), on Ctrl+C, or on
SIGTERM, and then deletes the directory with the database and token in it. The lifetime is counted from the start of `serve`, so startup time is part of it and `session_ttl_seconds` is the total lifetime, not the time after readiness. A
new session starts empty: it has a new access token, and tokens, preview
addresses, and jobs of an earlier session do not exist in it and do not
authenticate. `--session-root <dir>` chooses where session directories are
made (default: the system temporary directory). The readiness line says
`"mode":"one_shot"` (`"persistent"` for `--data-dir`) and the server prints a
line on stderr saying that nothing is kept; **the HTTP API, MCP tools, and
contracts do not report the mode.** A session that was killed (SIGKILL, power
loss) leaves its directory behind; the next one-shot start removes such a
directory, but only on Linux and only when the marker in it shows that its owner
process is gone. The lifetime is the lifetime of the data, not of any tunnel or
public address: there is no tunnel, and a tunnel in front of a session keeps its
own lifetime. **Not implemented:** sweeping on macOS and Windows, a sweep
without starting another session, SIGHUP handling, secure erasure, a worker for
one-shot data, any API or web indicator of the mode, and Windows verification.

Nothing is removed unless asked. `artifact purge <id> --base-metadata <n> --yes`
removes an artifact that is already in the trash for good (its files and
titles; request keys, change sequences, and copies stay), and `content cleanup`
reports file content that no revision refers to. `content cleanup --apply`
reviews that set, then deletes exactly it under a request key; a retry with the
same `--idempotency-key` and `--base-digest` prints the first run's report
again, and a set that changed since the review is refused with
`content_conflict` ([decision 0029](docs/decisions/0029-retention-and-cleanup.md)). A
purge cannot be undone, is refused to tokens issued to the owner profile (they
get `403`), is not offered to MCP agents, and does not reach earlier backups.
Purge is allowed to the owner's access token and to a token issued to another
user, who purges only their own trashed artifacts (another user's answers `404`,
as a missing one does); content cleanup needs the owner's access token, and the
bytes of purged artifacts of any user are reclaimed only by that cleanup
([decision 0036](docs/decisions/0036-multi-user-identity.md)). Request keys, those of a cleanup too, are
never expired.

Jobs are the first background work ([decision 0030](docs/decisions/0030-persistent-jobs-and-worker.md)).
`artifree job enqueue <id> --revision <n> --idempotency-key <key>` queues a
`verify_revision` job, which recomputes the SHA-256 and size of every file of
that revision from the database and reports `ok` or the first 50 mismatches;
`job show`, `job list`, and `job cancel <job> --idempotency-key <key>` follow
and stop it. Nothing starts a worker: the server does not, and the container
runs only the server, so a job stays `queued` until an operator runs
`artifree-server worker run --data-dir <dir>` (`--once` stops when nothing is
due). A job runs trusted server code, with no sandbox, and it uses no network; it is
not the isolated execution that builds of React sources or model calls will
need. Cancelling stops work and never rolls anything back. Job changes are
not in the change feed and the web workspace has no job view; poll `job show`.

Each artifact can keep application data: one JSON object per owner, with its own
`data_version` that is unrelated to the artifact's revisions
([decision 0031](docs/decisions/0031-app-data.md)). `artifree data get <id>
[--path todos/first]` reads it; `data set`, `data update` (a JSON Merge Patch),
and `data delete` need `--path`, `--base-data-version` (the version you read),
and an optional `--idempotency-key`. A stale base fails with `data_conflict`
and the current version, and a write that changes nothing keeps the version.
Values are strict JSON sent as written (a repeated member name or a number
with a magnitude beyond 2^53-1, in any spelling such as `1e16`, is refused;
`1.0` is stored as `1`), at most 256 KiB each and 1 MiB in all. The client
(`JsonText`) and the CLI send the text exactly as given; a JavaScript value that
`JSON.stringify` would change silently (`NaN`, `undefined`, a `Date`) is refused
before it is sent. `data delete` needs a request body, which a proxy that
strips the body of a `DELETE` makes a `400` (nothing is deleted); `data update
--patch null` removes the same value below the root, but not at the root (a
null patch there is refused; `data set --path "" --value {}` empties it). Saving or
restoring a revision does not change the data, and `artifact duplicate
--copy-data` gives a copy its own copy of it. Issued agent tokens and MCP tools
(`get_data`, `set_data`, `update_data`, `delete_data`) may read and write it;
there is no history, so an overwrite cannot be undone, and a purge erases it.
**A page running in a preview cannot read or write this data** (the runtime
bridge has no data capability), it is not in a revision or an export, and the web
workspace has no data view or subscriptions; poll `data get`.

For development with live reloading, Vite is still available:

```sh
npm run dev
```

Open the URL printed by Vite and paste the token from
`.artifree/data/access-token`. Vite forwards `/v1` to the API
(`ARTIFREE_API_URL` overrides the target). Agents and scripts use the CLI:

```sh
export ARTIFREE_TOKEN_FILE=.artifree/data/access-token
node apps/cli/dist/main.js artifact create --title "Demo" --dir ./site
node apps/cli/dist/main.js artifact save <id> --base 1 --put index.html=./site/index.html
node apps/cli/dist/main.js artifact preview <id>
node apps/cli/dist/main.js artifact --help
node apps/cli/dist/main.js token list
node apps/cli/dist/main.js token create "My Agent"
```

Results are JSON on stdout; failures go to stderr with exit code 1 (API) or 2
(usage). A save names its base revision and fails with `revision_conflict` if
another save won. `artifact edit` is the separate command for changing some
files when the head moved on: each file must still be as it was in `--base`, or
the whole edit fails with `file_conflict` and writes nothing
([decision 0035](docs/decisions/0035-per-file-conditional-edit.md)); it is not
a merge. Retrying with the same `--idempotency-key` returns the
original result instead of committing twice. The HTTP API is summarized in the
[architecture document](docs/architecture.md#http-api-v1).

MCP agents such as Codex and Pi connect to `http://127.0.0.1:7410/mcp` with the
same token:

```sh
export ARTIFREE_TOKEN="$(cat .artifree/data/access-token)"
codex mcp add artifree --url http://127.0.0.1:7410/mcp --bearer-token-env-var ARTIFREE_TOKEN
```

[Using Artifree from MCP agents](docs/mcp.md) covers the tools, Pi's
configuration, examples, and retry rules.

## Architecture

| Layer          | Location                                                     | Choice                                                                  |
| -------------- | ------------------------------------------------------------ | ----------------------------------------------------------------------- |
| Presentation   | `apps/web`, `apps/cli`, `packages/runtime-sdk`               | TypeScript; web uses React/Vite, Tailwind CSS 4, shadcn/ui with Base UI |
| Logic          | `crates/artifree-core`, `artifree-domain`, `artifree-server` | Rust; Axum on Tokio for HTTP                                            |
| Provider       | `crates/artifree-ports`, `artifree-providers`                | Rust; local SQLite storage, other adapters and workers undecided        |
| Wire contracts | `crates/artifree-contracts`, `packages/contracts`            | Rust DTOs → JSON Schema → generated TypeScript                          |

The application core depends on domain types and outbound ports. Concrete
providers implement those ports and are wired by the server composition root.
Inbound HTTP and MCP adapters call the same application commands.

See the [architecture document](docs/architecture.md), the interactive Korean
[HTML design](docs/architecture.html), and the decision records
([0001](docs/decisions/0001-foundation.md),
[0002](docs/decisions/0002-ui-and-quality.md),
[0003](docs/decisions/0003-local-html-artifact-slice.md),
[0004](docs/decisions/0004-inbound-mcp-adapter.md),
[0005](docs/decisions/0005-request-limits.md),
[0006](docs/decisions/0006-change-feed.md),
[0007](docs/decisions/0007-live-web-workspace.md),
[0008](docs/decisions/0008-artifact-metadata-and-lists.md),
[0009](docs/decisions/0009-web-list-and-metadata.md),
[0010](docs/decisions/0010-revision-restore-and-digests.md),
[0011](docs/decisions/0011-web-history-and-restore.md),
[0012](docs/decisions/0012-duplicate-artifacts.md),
[0013](docs/decisions/0013-deleting-artifacts-and-the-trash.md),
[0014](docs/decisions/0014-web-trash.md),
[0015](docs/decisions/0015-source-exports.md),
[0016](docs/decisions/0016-bounded-file-reads.md),
[0017](docs/decisions/0017-code-workspace.md),
[0018](docs/decisions/0018-source-draft-recovery.md),
[0019](docs/decisions/0019-runtime-metadata-bridge.md),
[0020](docs/decisions/0020-runtime-grants.md),
[0021](docs/decisions/0021-agent-selection-handoff.md),
[0022](docs/decisions/0022-production-web-and-api-serving.md),
[0023](docs/decisions/0023-single-owner-token-management.md),
[0024](docs/decisions/0024-https-proxy-and-remote-access.md),
[0025](docs/decisions/0025-release-packages.md),
[0026](docs/decisions/0026-container-image.md),
[0027](docs/decisions/0027-backup-restore-and-schema-upgrade.md),
[0028](docs/decisions/0028-static-web-defaults.md),
[0029](docs/decisions/0029-retention-and-cleanup.md),
[0030](docs/decisions/0030-persistent-jobs-and-worker.md),
[0031](docs/decisions/0031-app-data.md), and
[0032](docs/decisions/0032-runtime-source-read-and-download.md),
[0033](docs/decisions/0033-runtime-source-write.md),
[0034](docs/decisions/0034-room-and-presence.md),
[0035](docs/decisions/0035-per-file-conditional-edit.md),
[0036](docs/decisions/0036-multi-user-identity.md),
[0037](docs/decisions/0037-release-publishing-and-supply-chain.md), and
[0038](docs/decisions/0038-one-shot-session.md)).
Download `docs/architecture.html` and open it in a browser; it is standalone and
loads no external resources. Its TypeScript source is in `docs/architecture/`.

The [PR roadmap](docs/roadmap.md) records the remaining work, dependencies,
acceptance checks, and evidence requirements. The committed change feed (A01)
and the web workspace following it (A02) are implemented, and so are renaming,
pins, search, order, and pages through the API, MCP, CLI, and web workspace
(A03), revision comparison and restore through the API, MCP, CLI, and web
workspace (A04), duplicating a revision into an artifact with its own
history (A05), deleting artifacts to a trash and recovering them through
the API, MCP, client, and CLI (A06a) and the web workspace (A06b), and
exporting a revision's source as a ZIP archive with a manifest through the
API, client, CLI, and web workspace (A07), and editing an artifact's files
in the web workspace, saved as one revision (A09), with the draft kept in
the browser and recovered by choice after a reload or a closed tab (A10);
the browser must provide Web Locks to keep drafts safely. R04 adds the first
runtime metadata read, R05 adds an explicit connection-scoped metadata grant,
R11a adds read-only source listing and reading and user-confirmed download
requests with their own grants, R11b lets a preview save one file under a
separate edit grant, and R12 adds an in-memory room per artifact for presence and
ephemeral events (HTTP and the TypeScript client only).
The source editor can stage a saved selection and, after review, send it to a
registered MCP agent inbox (A08). The agent must poll and acknowledge; receipt
does not establish that a model completed the requested work.

An HTML artifact can opt into the metadata bridge by placing the marker as
the first child of its head, before any script:

```html
<!doctype html>
<html lang="en">
  <head>
    <!-- artifree-runtime -->
    <script>
      window.artifree.currentMetadata().then((metadata) => {
        console.log(metadata.title);
      });
    </script>
  </head>
  <body></body>
</html>
```

The opt-in prefix must use a doctype followed by `html` and `head` opening
tags; attributes in these tags must have quoted values. Other source previews
remain byte-for-byte ordinary previews. The bridge is available in the web
workspace, requires explicit metadata access consent, that workspace and server,
and is not included in source
exports. A preview is isolated from the workspace, not the network: it can
navigate its own frame elsewhere. TypeScript consumers can import the runtime
SDK for its types and should check that `window.artifree` is present; ordinary
previews do not install it.

An opted-in entry without a manifest declares `metadata.read` for compatibility;
this is a declaration, not a grant. An optional `artifree-runtime.json` source
file specifies a complete declaration, for example:

```json
{ "sdk": "artifree.app.v1", "capabilities": ["metadata.read"] }
```

An omitted `capabilities` field or an empty list declares none. The workspace
shows declared, supported and granted capabilities separately. Grants apply to
one connection and source binding, can be revoked, and expire or disappear on
server restart. Two more capabilities are read-only (decision 0032) and one
saves (decision 0033):

```json
{
  "sdk": "artifree.app.v1",
  "capabilities": [
    "metadata.read",
    "source.read",
    "source.download",
    "source.write"
  ]
}
```

`source.read` lets the preview list the files of its pinned revision and read
them in parts (`window.artifree.source.list()` and `.read({ path, offset,
length })`; a part has at most 1 MiB, and every part carries the whole file's
SHA-256 and size). `source.download` lets it ask (`.requestDownload({ path })`,
or without a path for the source archive) for the workspace to offer a file;
the user confirms in the workspace, never in the frame, and the receipt
`started` means only that the bytes were handed to the browser. Each is its own
declaration, consent, and grant: reading does not allow a download request.
The server checks the connection's grant and the artifact's current head on
every call, so a revoked grant or a newer revision is refused. `source.write` lets it save
one file of its own artifact (`.write({ path, content, retryId })`, at most 1
MiB) as the next revision. It needs its own "Allow source editing" consent, which
the workspace explains beside the control; it is not implied by reading and does
not imply it. The server saves through the artifact service with the view's
revision as the base and a key only a frame can produce, reads the new revision
back, and returns the path, revision, size, and SHA-256, which the client, the
workspace, and the frame each check against the bytes they sent. A save moves
the head, so the workspace shows the next revision in a new view that holds no
grant: the user consents again for each save. Only the owner exists as a
principal who can save a given artifact (its own user); there are no viewer roles. **Not implemented:** data access from a
preview, subscriptions, uploaded assets, replacing a view in place, deleting or
renaming files, several files in one save, a prompt for each save, roles (a
viewer is not refused because none exists), and any claim that a preview cannot
use the network. No MCP tool was added or changed.

### Rooms and presence (decision 0034)

A runtime connection (a browser tab) can join the room of an artifact as a
named `slot` participant, keep it with a heartbeat, send short ephemeral events,
and leave. The server tells `isMe` (my participant) and `sameTab` (my
connection) apart from facts it issued; nothing a peer sends is used. A lease
lasts 15 s; a join in the same slot resumes the participant under a new epoch
and the old one is refused. Limits: 64 rooms, 16 participants per room, 4 per
tab, 4 events per second per participant (burst 8), 128 kept events, 512-byte
events, 256-byte states, and a room request budget apart from the owner's. A
late joiner never receives older events. Rooms are memory of one server process:
a restart empties them, nothing is stored, and events have no delivery
guarantee. **Not implemented:** roles and sharing (S02), a room
capability for a sandboxed frame, a web or CLI surface, a push stream,
subscriptions (R07), an outbox (R08), and any MCP tool.

### Users (decision 0036)

The owner is user one (`local`) and can register more users
(`artifree user create <label>`, `GET/POST /v1/users`) and issue a token to one
of them (`artifree token create <name> --user <id>`). A token belongs to one
user for its whole life, and a user reaches only their own artifacts, revisions,
trash, jobs, application data, change feed, previews, runtime connections and
rooms: another user's name answers the `404` an unknown name does. Only the
bootstrap token manages users and tokens; no MCP tool does. Disabling a user
makes its tokens `401` and closes its open streams, links, connections and
rooms; enabling lets tokens that were not revoked in again. A disabled
user's queued jobs wait unclaimed and run once the user is enabled again (a job
already running finishes). **Not implemented:** roles, sharing and per-object
grants (S02), OAuth or any external identity, passwords or login sessions, a
web screen for users, user deletion, per-user storage quotas, and cleanup of
content as a user (cleanup is administrator-only).

## Verification

```sh
npm run check
npm run build
npx --no-install playwright install chromium firefox
npm run test:smoke
```

Checks cover zero-warning code/design/CSS lint, strict TypeScript including
dependency declarations, generated-contract drift and rejection, both language
dependency boundaries, actual inventory paths, Rust formatting/pedantic Clippy,
rustdoc, and behavioral tests for storage, conflicts, retries, crash recovery,
authorization, and path validation. Browser checks run the workspace against a
real server, probe the preview sandbox, and exercise keyboard and dialog focus,
light/dark accessibility, desktop/mobile layouts, and standalone HTML.
CI also checks Windows, Rust's minimum version, known dependency vulnerabilities,
licenses, and workflow syntax. See [the quality policy](docs/quality.md) and
[contributor instructions](.github/CONTRIBUTING.md) for commands and limits.

After changing wire DTOs or the scaffold inventory:

```sh
npm run generate:contracts
npm run generate:runtime
npm run build
```

Both generated contracts and lockfiles are committed. There is no mandatory
database server, model account, Docker daemon, or cloud subscription; SQLite is
compiled into the server.

## Planned deployment profiles

- **Local temporary:** a process-local session and temporary files.
- **Local persistent (implemented, single user):** SQLite and a local data
  directory.
- **Self-hosted:** a Rust API, separate workers, and local or external storage.
- **Hosted web:** static UI on Vercel or another host, with a separately deployed
  Rust API and workers. Function memory is not a durable session store.

Tunnels, a VPN, and reverse proxies provide access to these deployments; they
do not replace the API, storage, authentication, or worker lifecycle.

## Scope and provenance

Application code, names, schemas, and sample content are written for Artifree.
UI components are adapted from MIT-licensed shadcn/ui; their source attribution
is retained in [third-party notices](THIRD_PARTY_NOTICES.md).
No proprietary product bundle, decompiled source, captured account
data, cookies, private URLs, or credentials belong here. An independent
implementation does not by itself establish a formally separated clean-room
process. Artifree makes no claim of compatibility with a proprietary internal
API or of complete feature equivalence.

Licensed under [MIT](LICENSE).
