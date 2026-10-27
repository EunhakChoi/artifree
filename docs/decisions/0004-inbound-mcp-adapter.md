# 0004: Inbound MCP adapter for agents

Date: 2026-10-27. Status: accepted. Builds on
[0003](0003-local-html-artifact-slice.md); its storage, revision, retry, and
preview rules apply unchanged.

## Context

External agents such as Codex and Pi should be able to use the implemented
HTML artifact behavior directly: list, read, create, save, inspect history,
read pinned files, and issue preview links. The HTTP API and CLI already do
this. The MCP adapter must reuse the same application behavior rather than
add a second write path.

The requirements below were written for Artifree after a private behavioral
analysis of an existing hosted artifact product. This repository holds no
captures, source, or account data from that analysis, and Artifree claims no
compatibility with it. Each requirement states its basis:

- **Observed**: seen in a live trace or request and response.
- **Source-only**: read in shipped code, not seen running.
- **Inference**: an interpretation of observations.
- **Decision**: chosen for Artifree; may differ from the observed product.

A local, unpublished trace file maps each requirement ID to the analysis
reports and raw evidence it rests on.

## Requirements

| ID  | Requirement                                                                                                                                                                   | Basis                                                                                                                                                                                                                 |
| --- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| M1  | Agent tools and the runtime capabilities of an artifact page are separate surfaces. This adapter exposes agent tools only; previews keep no capabilities.                     | Observed: agent tools and page capability declarations were separate lists, and an agent edited documents while the page's own declaration allowed export only.                                                       |
| M2  | Creating returns the artifact identifier and its committed revision. A link that renders the artifact is a separate, short-lived result.                                      | Observed: the creation result carried an identifier and a viewer link but no version. Decision: return revisions, because saves are conditional on them; issue expiring preview links, because nothing is published.  |
| M3  | A save names its base revision. A stale base rejects the whole save with the current revision, and a failed multi-file save writes nothing.                                   | Observed: a stale whole-snapshot base was a conflict, and a batch with one conflicting precondition created no version. Source-only: a desktop agent tool required a base version. Decision: no merge of stale edits. |
| M4  | A retry key that repeats the same input returns the original result; the same key with different input is refused; a request rejected by validation does not reserve its key. | Observed for document operation identifiers: replay, conflict, and a corrected request accepted under the same identifier after a validation rejection.                                                               |
| M5  | Failures a model can correct are tool results with a machine-readable code and, for conflicts, the current revision. An unknown tool is a protocol error.                     | Observed: a conflict came back as an error tool result with structured content. MCP specification: tool execution errors versus protocol errors.                                                                      |
| M6  | An artifact the caller may not access looks absent and leaks no content.                                                                                                      | Observed: writes by a non-writer were refused as not found or forbidden. Decision 0003: inaccessible means not found.                                                                                                 |
| M7  | Agents can list revisions, read one revision's manifest, and read the exact bytes of a file at a pinned revision.                                                             | Observed: no agent tool for history was found. Source-only: a desktop read tool returned text only. Decision: expose history and exact bytes, as HTTP and the CLI already do.                                         |
| M8  | Content read from an artifact is presented as data, not instructions.                                                                                                         | Observed: read results were wrapped as content written by others.                                                                                                                                                     |
| M9  | A call whose answer is lost may still have committed; the agent can retry without duplicating it.                                                                             | Source-only: a desktop adapter separated "not sent" from "no answer" and advised checking state. Decision: retry keys make the retry itself safe.                                                                     |
| M10 | Transport and authentication suit external agents.                                                                                                                            | Observed: no supported external-agent authentication path was found. Inference: the transport is a design choice. Decision: items 1-4 below.                                                                          |

## Decisions

1. **Transport: MCP Streamable HTTP at `/mcp` on the API listener.** The MCP
   endpoint runs in the server process, so a preview link it issues is served
   by the same process that holds the ticket; a separate stdio process could
   not issue working links. Codex and Pi both connect to Streamable HTTP
   servers with a bearer token. A stdio transport is not provided.
2. **SDK: the official Rust SDK, `rmcp` 3.8**, with only its server and
   Streamable HTTP server features and no macros. The tests exercise the
   2025-11-25 `initialize` handshake and the stateless 2026-07-28 revision;
   the SDK also accepts the 2025-03-26 and 2025-06-18 handshakes. Clients that
   need the older HTTP+SSE transport (2024-11-05) are not served. The endpoint is stateless for every version (no
   `Mcp-Session-Id`) and answers with JSON rather than an event stream; `GET`
   is `405`. Nothing depends on transport state: retries use tool keys.
3. **Authentication and actor mapping.** A middleware checks the same bearer
   token as HTTP before the SDK sees the request (`401 unauthenticated`
   otherwise) and attaches the actor to the request. Tools read the actor
   from there; no tool takes an actor or owner argument. The token maps to the
   single local owner, as in decision 0003. OAuth and the MCP authorization
   flows are not implemented.
4. **Request guards.** The SDK accepts only loopback `Host` names, which
   stops DNS rebinding, and the endpoint refuses any request with an `Origin`
   header, because MCP clients are not web pages and the web workspace does
   not use MCP. Both answer `403`, after authentication. The request body limit
   equals the API's (36 MiB), so the largest package fits; the SDK default of
   4 MiB would not. An oversized body gets the SDK's plain-text `413`, not
   the API's `payload_too_large` JSON body.
5. **Shared operations.** HTTP handlers and MCP tools call the same server
   functions (`crates/artifree-server/src/operations.rs`), which convert wire
   values, run one `ArtifactService` command, and map errors. Validation,
   owner checks, conflicts, request fingerprints, limits, and error codes are
   therefore identical, and a key used over HTTP replays over MCP and the
   reverse.
6. **Tools.** Eight tools: `list_artifacts`, `get_artifact`, `create_artifact`,
   `save_revision`, `list_revisions`, `get_revision`, `read_file`, and
   `create_preview`. Arguments are closed objects; `create_artifact` and
   `save_revision` require `idempotency_key`. Successful results are the HTTP
   API v1 bodies; `read_file` returns UTF-8 text when the bytes are valid
   UTF-8 and base64 otherwise, both exact. Failures return the HTTP error body
   with `isError: true`. Each output schema therefore accepts the result or
   the error body (`anyOf`): MCP requires structured content to match it, and
   1.x SDK clients validate error results too. Annotations mark reads as read-only, commits as
   idempotent, and nothing as destructive or open-world.
7. **Contracts.** Tool arguments, `FileContent`, and the catalog are Rust
   types in `artifree-contracts`. Their schemas are generated without `$ref`
   and without non-standard integer formats, published as
   `contracts/generated/mcp-tools.json`, and drift-checked like the other
   contracts. The server lists exactly this catalog (cache hint: 5 minutes).
8. **Build changes.** `rmcp` requires Rust 1.88, so the minimum supported Rust
   version rises from 1.85 to 1.88. `sha2` moves to 0.11 so that it and
   `rmcp` share one `cpufeatures`; SHA-256 output, and so every stored
   fingerprint and content address, is unchanged. `syn` 2 remains for
   build-time proc macros of `tracing` and the Windows time-zone bindings and
   is allowed as a duplicate with that reason in `deny.toml`.

## Not in scope

Web auto-refresh after agent edits, the runtime SDK bridge, outbound MCP
providers, React/TSX builds, sharing, publishing, stdio transport, and
selection or edit-intent handoff to agent sessions.

## Costs and limits

- Single user and loopback only. Exposing the endpoint beyond loopback needs
  TLS, real authentication, and a `Host` policy first.
- A preview link is a bearer credential returned to the agent and, through
  it, possibly to its model provider. It expires (15 minutes by default) and
  rechecks access on every request.
- File content passes through the agent's model context. `read_file` returns
  whole files up to the 5 MiB limit; there is no ranged read.
- Agents must generate retry keys. A key reused for different content is
  refused, not silently accepted.
- The adapter was verified with the official TypeScript MCP SDK, 2.x in both
  protocol eras and 1.x for error results, and manually with Codex CLI 0.164. Pi's configuration follows its
  documentation and was not executed in this change.
