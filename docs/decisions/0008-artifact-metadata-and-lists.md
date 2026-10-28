# 0008: Artifact metadata and list queries

Date: 2026-10-28. Status: accepted; decision 8 added the same day. Builds
on [0003](0003-local-html-artifact-slice.md) and
[0006](0006-change-feed.md).

## Context

An artifact's title was fixed at creation, nothing marked the artifacts a
user returns to, and `GET /v1/artifacts` returned every artifact in one
response. Roadmap A03 adds renaming, pins, search, order, and bounded pages
for the API, MCP, CLI, and web workspace. Its delivery is split in two:
**A03a** (this change) implements them in storage, the application core,
HTTP, MCP, the TypeScript client, and the CLI; **A03b** adds them to the
web workspace, which until then lists the first page of up to 500 artifacts.
It already follows metadata changes made elsewhere (decision 7).

The requirements use the labels of [0004](0004-inbound-mcp-adapter.md):
**observed**, **source-only**, **inference**, and **decision**. A local,
unpublished trace maps each to its evidence.

## Requirements

| ID  | Requirement                                                         | Basis                                                                                                                                                                                                                                                                                                |
| --- | ------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| M1  | Renaming changes the title without changing any source revision.    | Observed: the hosted rename answered with a new version that carried the new title, while the earlier version kept the old one. Source-only: a local worker of the analyzed client changes the stored title only and creates no version. Decision: a rename creates no source revision (decision 1). |
| M2  | Pinning marks an artifact without changing it.                      | Observed: pinning and unpinning answered without a body and added no version. Source-only: pins are per-user favorites with a list filter. Decision: one pin per artifact while each artifact has one owner; per-user pins come with sharing (roadmap S01).                                          |
| M3  | Metadata changes are conditional and safe to retry.                 | Source-only: the local worker accepts optional title preconditions and refuses a mismatch. Decision: every change names its base metadata revision and carries an idempotency key, like a save.                                                                                                      |
| M4  | Lists can be searched, filtered by pin, ordered, and read in pages. | Observed: the hosted list accepted `limit` and `sort` among its query names and answered with `next_page_token`. Source-only: the client also sends `q` and `page_token`. Decision: the parameters and bounds below; the analysis did not establish the hosted defaults, limits, or token format.    |
| M5  | A caller written before pages keeps receiving the list it received. | Decision: without parameters, the order and contents stay as before for up to 500 artifacts.                                                                                                                                                                                                         |
| M6  | Clients that follow changes learn of metadata changes.              | Decision: a metadata change is a committed change in the feed of decision 0006.                                                                                                                                                                                                                      |

## Decisions

1. **Metadata has its own revision.** An artifact's title and pin form its
   metadata, versioned by a `metadata_revision` that starts at 1 and
   advances only when they change. Source revisions stay file packages: a
   rename never conflicts with an editor's draft, never changes what any
   revision renders, and never changes `updated_at`, which remains the
   time of the latest source revision. The hosted service records the title
   per version instead; an Artifree source revision is a package of files,
   and a title in it would make two people's unrelated changes conflict.
2. **One conditional, retryable update.** `PATCH /v1/artifacts/{id}` takes
   `base_metadata_revision` and `title`, `pinned`, or both, with an
   `Idempotency-Key`, through `ArtifactService::update`. A stale base is
   `409 metadata_conflict` with `current_metadata_revision`, and nothing is
   written. A request without either field is `422`. An update to the values
   already stored writes no metadata revision and no change, but records its
   key, so a retry replays it. The MCP tool `update_artifact` and the CLI
   commands `rename`, `pin`, and `unpin` call the same operation.
3. **Receipts stay as of their request.** Storage keeps every metadata
   revision, and each request key records the source and metadata revision
   its request produced. A replayed save reports the title it was committed
   under, and a replayed rename the title it set, also after later changes.
4. **List queries.** `GET /v1/artifacts` accepts `q`, `sort`, `pinned`,
   `limit`, and `page_token`; unknown parameters are `400`.
   - `q`: 1-200 characters, without control characters, contained in the
     title. ASCII letters match regardless of case and other characters
     exactly, which is what SQLite compares without an extension.
   - `sort`: `updated` (default), the latest source revision first, or
     `created`, the newest first; equal times are ordered by identifier.
   - `pinned`: `true` or `false` keeps only pinned or unpinned artifacts.
   - `limit`: 1-500, default 500. The default is the maximum so that a
     caller written before pages keeps its result for up to 500 artifacts.
   - `next_page_token` appears only when more artifacts follow. The token
     continues after the last artifact's sort time and identifier, and
     names the order and a digest of the filters: a token used with
     another order or other filters is `400`, while the page size may
     change between pages. Clients treat it as opaque.
   - A page is read in one query, so it never exceeds the limit. Artifacts
     that change while a caller pages by update time can appear twice or be
     missed, which a client that needs a stable view resolves by reading
     from the start again.
5. **The change feed.** A metadata change appends a `metadata_updated`
   change with the current source revision and the new metadata revision.
   Every change event now carries `metadata_revision`. A client of 0006
   that does not know the kind reads the artifact again, as decision 9 of
   0006 provides.
6. **Schema version 3.** Opening a version 1 or 2 database adds the pin and
   metadata revision columns, a `metadata_revisions` table filled with each
   artifact's current title as revision 1, and the metadata revision of
   each request key and change (1 for existing rows). The request key and
   change tables are rebuilt because their constraints change; the
   migration keeps the feed's head in `sqlite_sequence`, so cursors stay
   valid and new changes continue after it. A newer schema is still
   refused.
7. **Clients.** `ArtifreeClient.listArtifacts` takes the query and returns
   the page with its token; `updateArtifact` sends the update. `artifree
artifact list` prints every matching artifact, reading page after page,
   unless `--limit` or `--page-token` asks for one page. `rename`, `pin`,
   and `unpin` require `--base-metadata`. The web workspace knows each
   artifact's revision and metadata revision: a change counts as news,
   in the sense of decision 0007, when either is newer than known, so a
   rename elsewhere reads the list and the open artifact once and a repeated
   notice reads nothing.
8. **Commit times follow commit order.** The update order of decision 4
   and the creation order compare times taken from the system clock, which
   can be set back. While the browser checks ran on WSL 2, it moved back
   by over a second between two requests, and an artifact saved last then
   sorted below older ones. A
   commit takes the clock's time, or one millisecond after the last commit
   in the feed when the clock reads that time or earlier, inside the same
   write transaction. `created_at`, `updated_at`, and `committed_at` then
   increase with the feed sequence, so the last change sorts first and
   ties come only from data written before this rule. A clock set back by
   an hour makes commits an hour ahead of it until it catches up.

## Costs and limits

- Search folds only ASCII letters; `é` does not match `É`.
- Paging by update time is not a snapshot (decision 4).
- Commit times can run ahead of the clock after it is set back
  (decision 8); they are ordering times, not exact wall-clock readings.
- Pins belong to the artifact, not to a user.
- The metadata history has no API yet; it serves receipts.
- The web workspace gains search, order, pins, more pages, and renaming in
  A03b. Until then it shows the first 500 artifacts.
