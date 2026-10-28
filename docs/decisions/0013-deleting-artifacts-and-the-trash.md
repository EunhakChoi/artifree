# 0013: Deleting artifacts to a trash

Date: 2026-10-28. Status: accepted. Builds on
[0003](0003-local-html-artifact-slice.md),
[0006](0006-change-feed.md),
[0008](0008-artifact-metadata-and-lists.md), and
[0012](0012-duplicate-artifacts.md).

## Context

Artifacts could be created, changed, restored, and copied, but not removed.
Roadmap A06 adds deleting. Agents can call every write that people can
(0004), so a delete must be safe to retry and possible to undo. This record
covers the API, the store, MCP, the TypeScript client, and the CLI (A06a);
the web workspace follows in A06b.

The requirements use the labels of [0004](0004-inbound-mcp-adapter.md):
**observed**, **source-only**, **inference**, and **decision**. A local,
unpublished trace maps each to its evidence.

## Requirements

| ID  | Requirement                                                              | Basis                                                                                                                                                                                                                                  |
| --- | ------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| T1  | After a delete, the artifact cannot be read.                             | Observed: a hosted delete succeeded without a body, and a read of the same identifier then failed as not found. Source-only: agents had a delete tool, with the HTTP request as a fallback.                                            |
| T2  | A delete can be undone.                                                  | Source-only: the hosted client said a delete cannot be undone, and its list query could include soft-deleted artifacts. Inference: deleted artifacts were kept for a while. Decision: a trash that people and agents can recover from. |
| T3  | Nothing reads, previews, or changes an artifact in the trash.            | Gap: links, previews, and open pages after a hosted delete were not executed. Decision.                                                                                                                                                |
| T4  | A delete racing a save has one outcome, and both are safe to retry.      | Source-only: the hosted client handled a conflict on delete separately. Gap: the server side was not executed. Decision: the conditions and keys of every write (0003, 0008).                                                          |
| T5  | Clients that follow changes learn of deletes and recoveries.             | Decision, from 0006.                                                                                                                                                                                                                   |
| T6  | A delete removes no bytes, and deleting a copy leaves its source intact. | Observed: deleting a hosted copy left the original's assets and storage use as they were. Decision: bytes stay until removal for good (O05).                                                                                           |

## Decisions

1. **Deleting is a metadata change.** Whether an artifact is in the trash
   is metadata, like its title and pin. `PATCH /v1/artifacts/{id}` with
   `{"base_metadata_revision": n, "deleted": true}` and an
   `Idempotency-Key` moves it to the trash; `"deleted": false` takes it
   out. Both go through `ArtifactService::update` and the store's
   conditional metadata update, so there is no second write path; each
   commits the next metadata revision and leaves source revisions alone.
   The same request may also rename or pin. The artifact carries
   `deleted_at_ms`: when it went to the trash, or `null`.
2. **One rule for the trash.** While an artifact is in the trash, every
   request that names it gets the answer the same request gets for an
   unknown artifact, with two exceptions: a request that recovers it, and
   the retry of a request that committed before the delete, which replays.
   A request valid in itself is therefore `404 not_found`, with the same
   body; one invalid in itself, such as an empty title, an unsafe path, or
   a missing key, gets the same validation error as for any artifact,
   since it is refused before an artifact is looked up and tells nothing
   about it. This covers reads (the artifact, its history, manifests,
   files, previews, and preview links issued before, which are checked on
   every read, 0003) and writes (saves, restores, copies, renames, pins, a
   second delete, and a key reused for another request). The rule does not
   depend on the order of checks that need the artifact: the store answers
   a write in the trash with its replay or `404`, and the core answers
   `404` where one of its own checks fails in the trash, since a retry
   passes them all: its base and revision existed, and its changes formed
   a package.
3. **Writes are ordered.** The store checks the trash in each write's
   transaction, after looking up its key. A save and a delete are ordered:
   one commits first, and the other conflicts or is refused. A copy's
   source must be outside the trash when the copy commits.
4. **Retries.** The fingerprint of a metadata update covers whether it
   deletes, recovers, or leaves the trash alone, so a delete's key never
   recovers. A replay reports the artifact as of its request, trash state
   included. A recovery is an ordinary metadata update: a stale base is a
   `metadata_conflict`, and a reused key `request_key_reused`.
5. **The trash.** `GET /v1/artifacts` lists artifacts outside the trash;
   with `deleted=true` it lists only those in it, with the same search,
   pin filter, orders, and pages. Page tokens are bound to that choice;
   tokens of the default list keep their form. Nothing leaves the trash on
   its own.
6. **Changes.** The feed records a delete as `deleted` and a recovery as
   `recovered`, with the new metadata revision; other metadata changes stay
   `metadata_updated`. A client that does not know a kind reads the
   artifact again (0006), which answers `404` while it is in the trash.
7. **Storage.** Schema version 5 adds `deleted_at` to artifacts and to
   metadata revisions, so a replay can report the trash state of its
   request. The change kinds are a checked list, so the change table is
   rebuilt, keeping every sequence and the feed's head. Rows are kept;
   removing them for good is O05.
8. **Other clients.** The MCP tools `update_artifact` and `list_artifacts`
   take `deleted` as HTTP does; there is no separate tool, so agents delete
   and recover through the conditional update they already use for a
   rename, and the tool count stays the same. The client's `updateArtifact`
   and `listArtifacts` take it too, and its response checks require
   `deleted_at_ms` to be `null` or a time. The CLI adds
   `artifree artifact delete <id> --base-metadata <n>`, `recover`, and
   `list --deleted`.
9. **Access.** Only an artifact's owner may delete or recover it, as for
   every write (0003). Until A06b the web workspace offers neither, and
   shows an artifact deleted elsewhere as one it cannot read.

## Alternatives

- **`DELETE /v1/artifacts/{id}`.** It would need its own base and key
  rules and its own feed entry, and could not undo itself. A metadata
  update already has all three.
- **Answering by the order of checks.** Answering a write in the trash with
  whatever check failed first makes the answer depend on where each check
  runs, and lets a request tell that the artifact exists. One rule for the
  trash avoids both.

## Costs and limits

- The trash has no limit and no expiry; deleted artifacts keep their
  storage until O05.
- A replay of a request made before a delete names an artifact that is now
  in the trash.
- A copy made before its source was deleted stays; deleting a copy leaves
  its source alone.
