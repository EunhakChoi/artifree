# 0001: TypeScript presentation, Rust logic, replaceable providers

Date: 2026-10-26. Status: accepted foundation; integration libraries deferred.

## Context

Artifree should work with external agents, support local temporary and persistent
use, and grow into a self-hosted service. Agent-generated implementation changes
need strong automated feedback and explicit module contracts.

## Decisions

1. Presentation is TypeScript; logic is Rust. The server framework and async
   runtime are not fixed. There is no Tokio/Axum dependency in this scaffold.
2. Keep one modular Rust application and isolate future build/conversion workers.
   Do not create separate services for each domain module at this stage.
3. Define wire DTOs in Rust, generate JSON Schema with Schemars, validate the
   inventory, and generate TypeScript. Domain types stay independent of DTOs.
4. Use npm workspaces and Cargo workspaces. Commit both lockfiles. React/Vite are
   the current shell tools; they are not required for artifact-generated content.
5. Use crate dependencies as a checkable architecture boundary. Initial provider
   operations remain undefined until their first behavior contracts exist.
6. Make the current stage observable: the descriptor exits without listening,
   and both the UI and CLI label their data as a build-time scaffold inventory.
7. Use MIT licensing for the independently authored initial code and assets.

## Costs and limits

The TypeScript/Rust boundary needs generated contracts plus runtime validation;
neither replaces access checks. Rust does not prove transaction semantics,
authorization, deadlock freedom, or artifact isolation. Two languages mean two
toolchains. Storage adapters and worker tools need explicit deployment profiles.

## Deferred choices

HTTP framework, async runtime, MCP transport and SDK, authentication integration,
database libraries, storage implementations, job queue, document editors,
bundlers, conversion tools, and worker languages remain open. Choose them with a
working slice, record the result, and update source-linked design documentation.
