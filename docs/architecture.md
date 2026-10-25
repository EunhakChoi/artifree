# Artifree architecture

Status: initial scaffolding, 2026-10-26.

The [interactive HTML design](architecture.html) maps the design to actual source
paths. All product workflows in that design are proposals. The UI, CLI inventory,
Rust workspace, generated contracts, and checks are the implemented scaffold.

## Repository map

```text
apps/
  web/                 TypeScript · React/Vite workspace shell
  cli/                 TypeScript · help/version/describe
packages/
  contracts/           generated TypeScript wire types and scaffold inventory
  client/              future HTTP SDK configuration types
  runtime-sdk/         proposed capability and session-binding types
crates/
  artifree-domain/     source vocabulary; no dependencies
  artifree-core/       artifacts, policy, documents, jobs, agent handoff boundaries
  artifree-ports/      outbound boundary vocabulary
  artifree-providers/  storage, models, tools, build, export module locations
  artifree-contracts/  Serde/Schemars wire DTOs and JSON Schema exporter
  artifree-server/     composition root, descriptor, future HTTP/MCP adapters
contracts/generated/  generated JSON Schema and inventory snapshot
docs/architecture/    TypeScript, CSS, and HTML source for the standalone design
scripts/              contract generation, architecture build, checks, smoke
```

## Layer dependency direction

```text
TypeScript clients → inbound HTTP/MCP → Rust application core → outbound ports
                                                ↓                    ↑
                                           domain types       concrete providers

Rust server composition root wires the adapters, core, and providers together.
```

There are no HTTP listeners, MCP transports, provider operation implementations,
or async-runtime dependencies yet. The first working slice will define narrow
operation signatures and choose suitable integration libraries. Keep runtime
dependencies out of domain types rather than creating a generic runtime wrapper.

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

## Next implementation slice

Choose HTTP/runtime libraries and implement one local HTML artifact end to end:
validated file package → conditional durable source save → pinned revision read
→ isolated preview → the same operation from CLI. Include meaningful tests for
authorization, traversal rejection, concurrent writes, duplicate request keys,
and recovery. Introduce build and conversion workers before running untrusted
toolchains. Expand to structured document types and optional AI capabilities in
subsequent slices.
