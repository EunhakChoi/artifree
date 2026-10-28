# 0013: Deleting artifacts, and the trash

Date: 2026-10-28. Status: accepted. Builds on
[0003](0003-local-html-artifact-slice.md),
[0006](0006-change-feed.md),
[0008](0008-artifact-metadata-and-lists.md), and
[0012](0012-duplicate-artifacts.md).

## Context

Artifacts could be created, changed, and copied, but not removed. Roadmap
A06 adds deleting with a trash from which an artifact can be recovered;
removing data for good is O05. Its delivery is split in two, as A03 and
A04 were: **A06a** (this change) adds deleting, recovering, and the trash
to the application core, the store, HTTP, MCP, the TypeScript client, and
the CLI; **A06b** adds them to the web workspace.

The requirements use the labels of [0004](0004-inbound-mcp-adapter.md):
**observed**, **source-only**, **inference**, and **decision**. A local,
unpublished trace maps each to its evidence.

## Requirements

| ID  | Requirement                                                                  | Basis                                                                                                                                                                                                                                                                                                                      |
| --- | ---------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| T1  | An owner can delete an artifact; afterwards it cannot be read.               | Observed: the hosted service deleted an artifact with a separate request, which succeeded without a body, and a read of the same identifier then failed as not found. Source-only: an agent tool for deleting was listed, and the client fell back to the HTTP request.                                                    |
| T2  | A deleted artifact can be recovered.                                         | Gap: no recovery was observed; the client's confirmation said a delete cannot be undone. Source-only: the list query accepted an option to include soft-deleted artifacts. Inference: deleted artifacts were kept for a while. Decision: a trash, since agents can delete (0004) and a mistaken delete must be reversible. |
| T3  | Nothing changes or reveals a deleted artifact, previews and copies included. | Gap: what open pages, issued preview links, and agents saw after a delete was not executed. Decision: every read and write of a deleted artifact is refused like a missing one, except recovering it and listing the trash.                                                                                                |
| T4  | A delete that races a save has one outcome, and both are safe to retry.      | Source-only: the client handled a conflict on delete separately; the server's behavior was not executed. Decision: the base metadata revision and idempotency key of every metadata update (0008), in one store transaction.                                                                                               |
| T5  | Clients that follow changes learn of deletes and recoveries.                 | Decision, from 0006: two change kinds.                                                                                                                                                                                                                                                                                     |
| T6  | Deleting one artifact never removes bytes another one uses.                  | Observed: deleting a copy left the original's assets and storage use as they were. Decision: nothing is removed until O05; content stays shared (0012).                                                                                                                                                                    |

## Decisions

1. **Deleting is a metadata change.** Whether an artifact is in the trash
   is part of its metadata, like its title and pin (0008). `PATCH
/v1/artifacts/{id}` with `{"base_metadata_revision": n, "deleted":
true}` and an `Idempotency-Key` moves the artifact to the trash, and
   `"deleted": false` recovers it, through `ArtifactService::update` and
   the store's conditional metadata update. Either commits the next
   metadata revision; source revisions stay as they are. The answer is
   the update response, whose artifact carries `deleted_at_ms`: the time
   it was deleted, or `null`. A metadata update may also rename or pin in
   the same request.
2. **What a deleted artifact refuses.** Every read and write of a deleted
   artifact is `404 not_found`, as for a missing one: reading it, its
   revisions, or its files, previews (also links issued before the
   delete, which are checked on every read, 0003), saves, restores,
   copies, renames, and pins. Only a metadata update with
   `"deleted": false` reaches it, and the trash list shows it. The store
   checks this in the transaction of each write, so a save and a delete
   are ordered: one commits first, and the other is refused or conflicts.
   A copy's source must be in place when the copy commits. A request
   there that could never have committed, such as changes that do not
   form a valid package, is no retry and is `404` too, not `422`.
3. **Retries.** A retry with the key of an earlier request returns what
   that request did, also after the artifact was deleted: a replay comes
   before the trash check. The fingerprint of a metadata update covers
   whether it deletes, recovers, or keeps the trash state. A delete of an
   artifact already in the trash, with another key, is `404`.
4. **The trash.** `GET /v1/artifacts` lists artifacts outside the trash;
   with `deleted=true` it lists only those in the trash, with the same
   search, pin filter, orders, and pages. Page tokens are bound to the
   filter. Nothing leaves the trash on its own: it keeps artifacts and
   their bytes until O05 adds removing them for good.
5. **Changes.** The feed records a delete as `deleted` and a recovery as
   `recovered`, with the new metadata revision; any other metadata change
   stays `metadata_updated`. Clients that do not know a kind read the
   artifact again (0006, 0007), which now answers `404` while it is in the
   trash.
6. **Storage.** Schema version 5 adds `deleted_at` to the artifact and to
   each metadata revision, so a replay reports the trash state as of its
   request. The change kinds are a checked list, so the change table is
   rebuilt, keeping every sequence and the feed's head.
7. **Other clients.** The MCP tools `update_artifact` and `list_artifacts`
   take `deleted` like HTTP; there is no separate tool, so agents delete
   and recover through the same conditional update as a rename. The
   client's `updateArtifact` and `listArtifacts` take it too. The CLI adds
   `artifree artifact delete <id> --base-metadata <n>`, `recover`, and
   `list --deleted`.
8. **Access.** Only an artifact's owner may delete or recover it, as for
   every write (0003). The web workspace does not offer it until A06b; a
   delete made elsewhere shows there as an artifact that cannot be read.

## Costs and limits

- Deleted artifacts keep their storage until O05; the trash has no limit
  and no expiry.
- A retry of a request made before a delete replays it, so its answer
  names an artifact that is now in the trash.
- A copy made before its source was deleted stays; deleting a copy leaves
  its source alone.
- Until A06b, the web workspace cannot delete, list the trash, or recover,
  and an artifact deleted elsewhere shows as one it cannot read.
