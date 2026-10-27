# 0003: First working slice — local HTML artifacts

Date: 2026-10-27. Status: accepted. Supersedes the deferred HTTP framework,
async runtime, and local storage choices of [0001](0001-foundation.md).

## Context

The first vertical slice connects an agent-facing API, durable storage, a web
preview, and editing: create a file package → conditional durable save →
revision-pinned read → isolated preview → the same operations from the CLI and
the web. It must run locally without Docker, a cloud account, or a database
server, and keep the core independent of the framework and the store.

## Decisions

1. **HTTP: Axum 0.8 on Tokio.** Only `artifree-server` depends on them. The
   application core and the store stay synchronous; the server runs commands on
   Tokio's blocking pool. Axum's default features are disabled except HTTP/1,
   JSON, and the Tokio integration.
2. **Local storage: one SQLite file through `rusqlite` with bundled SQLite.**
   Metadata, immutable revisions, request-key results, and file bytes share
   one database, so a commit is one `BEGIN IMMEDIATE` transaction and needs no
   cross-store atomicity. WAL mode with `synchronous=FULL` makes a returned
   commit durable. Identical file content is stored once (SHA-256 addressed).
   Each operation opens its own connection; SQLite's file lock serializes
   writers across threads and processes, so no in-process lock is needed.
3. **Revisions and retries.** A save names its base revision and is rejected
   with `revision_conflict` unless the base is current. Mutating requests carry
   an `Idempotency-Key`, scoped to the actor. The same key and request content
   (a length-prefixed SHA-256 fingerprint) replays the original result even
   after later commits; the same key with different content is rejected.
   Rejected requests do not reserve their key. Keys are kept indefinitely for
   now.
4. **Authorization.** One local bearer token, created with owner-only file
   permissions on Unix in the data directory, maps to the single local owner.
   The bearer scheme is matched case-insensitively. On Unix the data directory
   is created with mode 700, and the server refuses to start on an existing
   directory that group or other users can access, because the database and
   its WAL in it would be readable without the token. It reports the mode
   rather than changing it. On Windows the directory inherits its parent's
   access control. Unmatched routes and methods also authenticate first and
   answer with contract-shaped errors (`404 not_found`,
   `405 invalid_request` with `Allow`).
   The core still checks the actor against the artifact owner on every read and
   write, and reports inaccessible artifacts as not found. This is not
   multi-user authentication.
5. **Isolated preview.** Artifact files are served by a second listener on a
   separate origin under a 256-bit ticket bound to actor, artifact, and
   revision, expiring after 15 minutes by default. Each request rechecks access
   through the core. Responses carry CSP `sandbox` without `allow-same-origin`,
   restrict loads and connections to the content origin, set `nosniff` and
   `no-referrer`, and limit `frame-ancestors`. The web UI embeds previews in a
   sandboxed iframe. Content has no network egress, data, model, or file
   capability; the runtime SDK remains types only.
6. **Paths and limits.** Package paths are portable relative names: ASCII
   letters, digits, `-`, `_`, `.`; no hidden, `.`/`..`, empty, or reserved
   device segments; supported media type by extension; no case-insensitive
   collision or file/directory conflict. Limits: 256 files, 5 MiB per file,
   25 MiB per package, 36 MiB per request body.
7. **Wire contracts.** HTTP API v1 bodies are Rust DTOs exported as one JSON
   Schema (`contracts/generated/api-v1.schema.json`) and generated TypeScript.
   The TypeScript client validates responses at runtime; the server validates
   and converts every request into domain types.

## Dependencies added

Rust: `axum`, `tokio` (server); `rusqlite` with bundled SQLite (providers);
`sha2` (core fingerprints, provider content addresses); `getrandom` (IDs,
tokens, tickets); `base64` (upload decoding); `tower` (router tests only).
Each edge is listed in the boundary checker. `cargo-deny` now scopes its graph
to the native targets the server ships, because wasm-only SQLite dependencies
are never built, and allows the permissive Zlib (foldhash) and BSD-3-Clause
(part of Axum's router, matchit) licenses.

Web: shadcn/ui `field`, `input`, `label`, `textarea`, `native-select`, and
`empty` components through the official CLI.

## Costs and limits

- Single user and loopback by default. Exposing the API beyond loopback needs
  TLS and a real authentication design first.
- Preview tickets live in process memory and end with the process.
- Request keys and unreferenced content are not garbage-collected yet.
- PostgreSQL/object storage, publishing, sharing, restore, fork, MCP, build
  workers, and runtime capabilities are not implemented.
- Sandboxed previews prevent access to the workspace origin and the network,
  but do not limit CPU or memory use by artifact scripts.
