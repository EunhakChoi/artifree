# 0010: Restoring a revision, and file digests

Date: 2026-10-28. Status: accepted. Builds on
[0003](0003-local-html-artifact-slice.md),
[0006](0006-change-feed.md), and
[0008](0008-artifact-metadata-and-lists.md).

## Context

Revisions are immutable and kept, but the only way back to an earlier one
was to read every file and save them again. Comparing two revisions meant
reading every file of both, since a manifest gave only paths and sizes.
Roadmap A04 adds comparison and restore. Its delivery is split in two:
**A04a** (this change) adds restore and file digests to the application
core, HTTP, MCP, the TypeScript client, and the CLI; **A04b** adds
comparison and restore to the web workspace.

The requirements use the labels of [0004](0004-inbound-mcp-adapter.md):
**observed**, **source-only**, **inference**, and **decision**. A local,
unpublished trace maps each to its evidence.

## Requirements

| ID  | Requirement                                                              | Basis                                                                                                                                                                                                                                                                                                                                                                      |
| --- | ------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| H1  | An earlier revision's files can become the current files again.          | Gap: the analysis captured no restore of an ordinary artifact, and none appeared in the client code it read. Observed: the only way back that the hosted service showed was a copy of an earlier version as a new artifact, which left the original's history unchanged; its separate document editor restores by adding a new version. Decision: restore adds a revision. |
| H2  | A restore never changes or removes a revision.                           | Observed: the hosted history kept every version, newest first. Decision: a restore commits the next revision; every revision stays readable.                                                                                                                                                                                                                               |
| H3  | A restore is conditional and safe to retry.                              | Observed: a hosted save against an older base version failed with a conflict that named the current version. Decision: the base revision and idempotency key of every save (0003).                                                                                                                                                                                         |
| H4  | A restore changes the files only.                                        | Observed: application data was not tied to a version; a copy of an older version received the current data. Decision: the title and pin stay (0008 decision 1); data, permissions, and publishing do not exist yet and will not be restored implicitly when they do.                                                                                                       |
| H5  | Two revisions can be compared without reading files that did not change. | Gap: no comparison of ordinary artifacts appeared in the analysis. Decision: manifests carry a digest of each file.                                                                                                                                                                                                                                                        |

## Decisions

1. **Restore is a commit.** `POST
/v1/artifacts/{id}/revisions/{revision}/restore` with
   `{"base_revision": n}` and an `Idempotency-Key` commits the files of
   `revision`, byte for byte, as the next revision, through
   `ArtifactService::restore` and the store's ordinary conditional commit.
   It answers `201` with the commit response and the new revision's
   `Location`, like a save. The feed records a `revised` change, so
   clients that follow changes (0006, 0007) need nothing new.
2. **Refusals write nothing.** They are checked in this order, so each
   request has one answer: the artifact, a base that cannot be current,
   the restored revision, an unchanged restore, then in the store a
   replay before a stale base.
   - A missing or inaccessible artifact, or a revision that is 0 or after the
     current one, is `404 not_found`.
   - A base that is not current is `409 revision_conflict` with
     `current_revision`, after a replay check as for a save.
   - Restoring the files a current base already has is
     `422 invalid_request`, as is a save that changes nothing. Against a
     stale base the conflict is reported instead, since the files the
     restore would replace are no longer the base's.
   - A missing key is `400`.
3. **Retries.** The request fingerprint covers the operation, the artifact,
   the base, and the restored revision. Resending the same restore with
   its key returns the original receipt, also after later commits. The key
   is refused for a save or for a restore of another revision. As for a
   save, a request refused before the store, such as an unchanged
   restore, is refused whatever its key.
4. **What a restore leaves alone.** The title and pin, and every existing
   revision. A restored revision does not record which revision it came
   from; its files and digests show it.
5. **Digests.** Every revision file in a manifest carries `sha256`, the
   lowercase hex SHA-256 of its bytes, which the store already keeps to
   share content. Equal digests mean equal bytes, so a client compares two
   manifests and reads only files whose digests differ. Clients reject a
   manifest without a valid digest.
6. **Other clients.** The MCP tool `restore_revision`, the client method
   `restoreRevision`, and the CLI command `artifree artifact restore <id>
<revision> --base <n>` call the same operation. The CLI reports an
   unknown outcome with the key to retry, as for a save.
7. **Comparison is a presentation.** No comparison endpoint is added:
   manifests with digests and pinned file reads are enough for the web
   workspace (A04b) and for agents. Text differences are computed by the
   client that shows them.

## Costs and limits

- A restore holds both packages in memory to compare them, as a save holds
  its base; packages are at most 25 MiB. A retry reads the restored
  package before the store finds its replay.
- The history does not say that a revision was restored, or from which
  revision.
- The web workspace gains comparison and restore in A04b.
- Application data, uploads, permissions, and publishing are not part of
  a revision and are not restored (roadmap R06, R09, S03).
