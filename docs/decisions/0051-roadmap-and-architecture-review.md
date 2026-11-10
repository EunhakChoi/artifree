# 0051: Roadmap and architecture review after R09

Date: 2026-11-10. Status: **accepted for the documentation changes below**
(made by the change that adds this record). It adds no executable behavior,
contract, schema or dependency and changes no numbered rule of another record.
The items under "Open decisions" are **not decided**; they are listed so that
later work does not assume an answer.

## Context

R09 (uploaded assets, including the three runtime asset capabilities) is
complete in Chromium. Before choosing the next slice, the remaining roadmap
and the structure of the code on `main` were read again: the roadmap rows and
their prerequisites, the crate and package layout, the dependency directions,
the storage schema, the server's in-memory state, and the runtime bridge from
the frame to the store. The review read structure, imports, sizes and the key
modules; it did not audit every module's internals, and sizes are line counts.

## Findings

Roadmap:

1. Several statements were out of date: the header and the completed-work table
   (MCP tool count, "no other runtime feature", "no uploaded assets"), the
   status of R06b (merged), R08c-2cs (the acceptance of 0047 is merged), R09
   and R09b (every sub-row is done), R12's "the runtime SDK contains types
   only", and the milestone table.
2. Two large rows with little visible result gate almost everything that
   remains. R02 gates R03, and through T01 every type row; R08 gates R10, and
   through R10 the rows P02, P05 and S02. Three of those prerequisites were
   wider than the dependent row needs (decisions 2 to 4).
3. Some remaining work had no row: a frame's access to application data (R06,
   R07, R08, R11a and R11b each list it as not implemented), a frame's access
   to rooms (R12), the operation of background work (nothing starts or
   supervises the worker, job changes are not in the change feed, the web has
   no job view), and the return of Firefox to CI, which
   [quality](../quality.md#browser-engines) and `release.yml` already make a
   precondition of the first release.

Structure:

4. The layering holds and is enforced: domain, ports, core and providers
   depend in one direction, the core and store are synchronous, HTTP and MCP
   call the same operations, each kind of state has its own version, and the
   newer stores (data, assets, jobs) are separate ports.
5. Adding one runtime capability touches about twelve hand-written files in
   five packages (contract, capability set, server module, routes, client,
   host frame dispatch, request gate, prompt, permission control, SDK types
   and bootstrap). The host dispatch names every call in two places, limits
   are repeated in the host and the SDK, and the follow-ups of 0045 (repeated
   frame transport, repeated download flow, per-kind prompt branches, an
   eight-bit capability set with seven bits used) have the same cause. Seven
   capabilities exist; data, rooms and the P rows add at least seven more.
6. Every user-owned state is scoped to one actor: application data, assets
   and rooms by actor and artifact; request keys (cleanup runs included), jobs
   and the change sequence by actor alone. Access
   is "the artifact's own user", decided in `policy::may_access` and in the
   change feed's storage query. Sharing (S02) must decide, for each of those
   states, whether it is per user or shared. A frame data capability fixes
   that meaning in the SDK contract.
7. About six thousand lines of application state machines live in
   `artifree-server` (runtime sessions, policy, source, assets, preview
   tickets, handoffs) and depend on its `AppState` and on `SqliteStore`, while
   `artifree-core` still holds three-line "planned" modules for handoffs
   (implemented in the server by 0021) and documents. Rooms show the other
   arrangement: the logic in `core/room.rs`, the adapter in `server/rooms.rs`.
8. Four files are touched by nearly every feature and are over two thousand
   lines each: `http.rs`, `operations.rs`, the contract `api.rs`, and the
   client's `client.ts`.
9. The outbox is isolated behind its own entry point
   (`@artifree/client/outbox`); nothing in the web workspace or the CLI imports
   it. Its public handle is local-only and its sender is off, and no shipped
   feature writes application data from a page.
10. `docs/architecture.md` had grown into a per-slice history that repeated the
    roadmap and had fallen behind it.

## Decisions

1. **The track cut is unchanged** (A, O, R, P, T, S, X). A row is the starting
   point of its work, not a fixed pull request boundary: the split into pull
   requests is decided when a track starts and may change while it runs. No hardening or
   release track is added: Firefox returns before the first public release by
   the existing policy, and review follow-ups stay in the decision record of
   the slice that produced them.
2. **R10 no longer waits for R08.** R10 needs R08 only to cross-check queued
   outbox writes, and none exist while the public sender is off. That check
   moves to the change that turns the sender on, and the R08c row now says
   so. R10's prerequisites are R05 and R11b. If R14 is implemented first, as
   ordered below, R10 also cross-checks `data.write` and data subscriptions;
   the R10 row names them.
3. **S02 no longer waits for R10.** Revocation closes open views, which S01
   already does when a user is disabled; adopting a replacement view is not
   needed for it.
4. **T01 is split.** T01a is the type catalog (descriptor, required files,
   guide, contract pin, creation path) and T01b is the first React template.
   Only T01b needs R03. T02, T07, T09, T11 and T13 depend on T01a. T12 keeps
   its own R03 prerequisite and depends on T01a for the catalog. No other
   prerequisite of a T row changes.
5. **R03 still waits for R02.** [0042](0042-linux-untrusted-execution.md)
   states that R02 establishes the execution boundary before React builds. A
   narrower threat model for a first build would amend 0042 and is not made
   here.
6. **T03b's two-user check moves to S02.** Two users cannot reach one
   artifact until sharing exists, so "two users changing one document" cannot
   be checked on S01 alone. T03b keeps its prerequisites and is checked with
   two tabs of one user; S02 runs the two-user check if T03b exists by then.
   Making all of T03b wait for S02 would also make the document exports
   behind it wait for sharing.
7. **New roadmap rows**, each proposed and without a specification yet:
   - R13, a behavior-preserving consolidation of the runtime bridge before the
     next capability (finding 5). The three independent checks (client, host,
     frame) stay. It is an exception to the roadmap's entry rule 6 (one
     observable result per change), accepted because the result is checkable:
     the existing unit, integration and smoke checks pass unchanged, the diff
     removes repetition, and it adds no empty port or general wrapper. It is
     not the "move for its own sake" that decision 8 rules out; that concerns
     moving modules between crates.
   - R14, a frame's access to application data. It adds the ninth capability,
     so it widens the capability set.
   - R15, a frame's access to rooms.
   - R01b, the operation of background work: a scheduler inside the server,
     worker supervision, job changes in the change feed and a job view. It
     reuses the existing injectable clock (`artifree_core::jobs::Clock`).
     O05b's automatic purge, R02b and R03 share it and name it as a
     prerequisite.
   - S02s, the access model as a documentation-only decision (finding 6),
     ahead of S02's implementation and together with R14's specification.
   - O08, Firefox back in the CI matrix with every pending Firefox acceptance
     re-run, a prerequisite of O03c's first publication.
8. **Guidance for new code**, not a migration:
   - A state machine that needs no server framework goes into
     `artifree-core`, with a thin adapter in the server, as rooms do. Existing
     server modules are not moved for their own sake.
   - A new aggregate (documents, builds) gets its own port and service
     module; `ArtifactStore` does not grow to hold it.
   - The four large files are split by resource family when a change touches
     that family, not in one sweep.
9. **`docs/architecture.md` describes structure**; the per-slice history is
   the roadmap's. The architecture document keeps the repository map, the
   layer rules, state and authority, the runtime bridge model, the HTTP and
   MCP surfaces and the structural notes from this review.

## Recommended order

1. S02s (documentation only) and the R13 consolidation.
2. R14, then R10.
3. One of the directions under "Open decisions".
4. The remainder of R08 once R14 exists and it is decided whether a frame's
   writes use the outbox.

O05b's web purge can be done at any time, and R15 at any time after R13. O08
and O03c come before the first public release.

## Open decisions

These belong to the project owner and are not settled by this record.

1. **The direction after R10**: untrusted execution and React builds (R02,
   R03), sharing (S02), or structured types (T01a and T02). R02 then R03
   unblocks the most rows and is the longest stretch before a visible result.
2. **What "the planned feature set is complete" means** for Firefox's return
   and the first public release: all of P and T, or an earlier point.
3. **Whether the remainder of R08 is paused** until a feature needs it. The
   order above assumes it follows R14; nothing is removed either way.
4. **The access model for shared artifacts** (S02s): whether application data
   and assets are per user or shared. The recommendation to be examined in
   S02s is per user by default, with sharing only for a space the artifact
   declares, because it keeps the current schema and isolation tests and does
   not open every state at once.

## What this record does not do

It does not move code, change a contract, weaken a check or re-enable Firefox.
It does not claim that any proposed row is specified, and a row added here
still needs its own specification before code
([contribution guide](../../.github/CONTRIBUTING.md)).
