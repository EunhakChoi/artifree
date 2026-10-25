# Artifree

**Open artifacts. Any agent.**

An independently written workspace for agent-made apps, documents, slides,
dashboards, designs, and motion. The goal is to run it on your own machine or
server and use it from a browser, CLI, HTTP API, or MCP-capable agent.

**Current stage: scaffolding.** The development web shell, inspection CLI, Rust
workspace, generated contracts, and verification tooling exist. Artifact storage,
publishing, execution, HTTP/MCP servers, authentication, and conversion workers
are not implemented. The inventory shown by the UI and CLI is a build-time
snapshot, not live server health.

## Quick start

Requirements: Node.js 22.12 or newer, npm 10 or newer, and rustup. The repository
pins its tested Rust toolchain in `rust-toolchain.toml`.

```sh
npm ci
npm run build
npm run dev
```

Open the URL printed by Vite. The dev server binds to loopback by default.
From another terminal:

```sh
npm run cli -- --help
npm run cli -- describe --json
cargo run -p artifree-server -- --describe
```

`artifree-server` currently prints help or an inventory and exits. It does not
listen on a port. Use the built CLI directly when consuming JSON:

```sh
node apps/cli/dist/main.js describe --json
```

## Architecture

| Layer          | Location                                                     | Choice                                                                  |
| -------------- | ------------------------------------------------------------ | ----------------------------------------------------------------------- |
| Presentation   | `apps/web`, `apps/cli`, `packages/runtime-sdk`               | TypeScript fixed; React/Vite currently used for the web shell           |
| Logic          | `crates/artifree-core`, `artifree-domain`, `artifree-server` | Rust fixed; HTTP framework and async runtime undecided                  |
| Provider       | `crates/artifree-ports`, `artifree-providers`                | Rust integration boundary; adapter tools and worker languages undecided |
| Wire contracts | `crates/artifree-contracts`, `packages/contracts`            | Rust DTOs → JSON Schema → generated TypeScript                          |

The application core depends on domain types and outbound ports. Concrete
providers implement those ports and are wired by the server composition root.
Inbound HTTP and MCP adapters will call the same application commands.

See the [architecture document](docs/architecture.md), the interactive Korean
[HTML design](docs/architecture.html), and the [decision record](docs/decisions/0001-foundation.md).
Download `docs/architecture.html` and open it in a browser; it is standalone and
loads no external resources. Its TypeScript source is in `docs/architecture/`.

## Verification

```sh
npm run check
npm run build
npx playwright install chromium
npm run test:smoke
```

Checks cover formatting, generated-contract drift and schema validity, allowed
Rust crate dependencies, actual inventory source paths, strict TypeScript,
Rust formatting, Clippy, and Cargo tests. Smoke checks exercise the Rust/CLI
contract, invalid CLI arguments, the built web shell, and HTML interactions at
desktop and mobile widths. They do not establish product feature parity.

After changing wire DTOs or the scaffold inventory:

```sh
npm run generate:contracts
npm run build
```

Both generated contracts and lockfiles are committed. There is no mandatory
database, model account, Docker daemon, or cloud subscription for this scaffold.

## Planned deployment profiles

- **Local temporary:** a process-local session and temporary files.
- **Local persistent:** SQLite and a local data directory.
- **Self-hosted:** a Rust API, separate workers, and local or external storage.
- **Hosted web:** static UI on Vercel or another host, with a separately deployed
  Rust API and workers. Function memory is not a durable session store.

Tunnels, a VPN, and reverse proxies provide access to these deployments; they
do not replace the API, storage, authentication, or worker lifecycle.

## Scope and provenance

All code, names, UI, schemas, and sample content in this repository are written
for Artifree. No third-party product bundle, decompiled source, captured account
data, cookies, private URLs, or credentials belong here. An independent
implementation does not by itself establish a formally separated clean-room
process. This scaffold makes no claim of compatibility with a proprietary
internal API or of complete feature equivalence.

Licensed under [MIT](LICENSE).
