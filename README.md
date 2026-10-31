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
default. A runtime preview may read its own artifact metadata through a
bound bridge after a connection-scoped user grant. Publishing, sharing, build and conversion
workers, and multi-user authentication are not implemented. The module inventory shown by the UI and
CLI is a build-time snapshot, not live server health.

## Quick start

Requirements: Node.js 22.12 or newer, npm 10 or newer, and rustup. The repository
pins its tested Rust toolchain in `rust-toolchain.toml`.

```sh
npm ci
npm run build
cargo run -p artifree-server -- serve --data-dir .artifree/data
```

The server prints one JSON readiness line with the API URL (default
`http://127.0.0.1:7410`), the preview origin (`http://127.0.0.1:7411`), and the
token file. On Unix the server creates the data directory readable only by
you and refuses an existing one that other users can access. Keep it running
and, in another terminal:

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
```

Results are JSON on stdout; failures go to stderr with exit code 1 (API) or 2
(usage). A save names its base revision and fails with `revision_conflict` if
another save won. Retrying with the same `--idempotency-key` returns the
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
[0019](docs/decisions/0019-runtime-metadata-bridge.md), and
[0020](docs/decisions/0020-runtime-grants.md)).
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
runtime metadata read and R05 adds an explicit connection-scoped metadata grant;
agent handoff (A08) remains planned.

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
server restart. No other runtime capability is implemented.

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
[contributor instructions](CONTRIBUTING.md) for commands and limits.

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
