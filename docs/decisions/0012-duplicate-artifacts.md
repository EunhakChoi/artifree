# 0012: Duplicating an artifact from a revision

Date: 2026-10-28. Status: accepted. Builds on
[0003](0003-local-html-artifact-slice.md),
[0008](0008-artifact-metadata-and-lists.md),
[0010](0010-revision-restore-and-digests.md), and
[0011](0011-web-history-and-restore.md).

## Context

A restore (0010) brings an earlier revision back into the same history.
Sometimes the earlier state should instead become a separate artifact, so
that both can change without affecting each other. Roadmap A05 adds that
duplicate to the API, MCP, the TypeScript client, the CLI, and the web
workspace.

Artifacts have no application data or uploaded assets yet. A duplicate
therefore copies files only, and says so; R06 and R09 extend it when those
parts exist.

The requirements use the labels of [0004](0004-inbound-mcp-adapter.md):
**observed**, **source-only**, **inference**, and **decision**. A local,
unpublished trace maps each to its evidence.

## Requirements

| ID  | Requirement                                                                  | Basis                                                                                                                                                                                                                                                                                 |
| --- | ---------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| D1  | Any revision of an artifact can be copied into a new artifact.               | Observed: the hosted service copied an artifact with a separate request that could name an earlier version, and without one copied the latest.                                                                                                                                        |
| D2  | The copy starts its own history; neither artifact's changes reach the other. | Observed: the copy had a new identifier and one version, and the original's history was unchanged. Decision: revision 1 of the copy has the files, byte for byte; later saves are checked to stay apart.                                                                              |
| D3  | The copy has a name that tells it apart, and is not pinned.                  | Observed: the copy was named after the original followed by " (Copy)", and was not starred; whether a starred original passes its star on was not captured. Decision: the same default name, a title of the caller's choice, and no pin.                                              |
| D4  | What a copy received is stated.                                              | Observed: the hosted copy also received application data and uploaded assets. Source-only: the client reported data or assets that were not copied. Decision: the response lists what was copied, files only for now, and the page says so.                                           |
| D5  | A duplicate is safe to retry.                                                | Gap: the retry behavior of the hosted copy was not captured. Decision: the idempotency key of every create (0003).                                                                                                                                                                    |
| D6  | Only someone who may read the artifact may copy it, and owns the copy.       | Observed: the hosted copy belonged to the caller alone. Source-only: the client refused copies for several reasons, such as someone else's artifact. Decision: the access policy of every read (0003); other refusals have no counterpart here, and the request limits of 0005 apply. |

## Decisions

1. **A duplicate is a create.** `POST
/v1/artifacts/{id}/revisions/{revision}/duplicate` with an optional
   `{"title": "…"}` and an `Idempotency-Key` creates an artifact owned by
   the caller, through `ArtifactService::duplicate` and the store's
   ordinary create. Its revision 1 has the files of `revision`, byte for
   byte; their content is shared, as all equal content is. It answers
   `201` with the copy, its revision 1, `copied`, and `replayed`, and the
   copy's `Location`. The feed records a `created` change, so clients that
   follow changes (0006, 0007) need nothing new. The revision is named in
   the path, like a restore's, so a request never depends on what is
   current when it arrives.
2. **Name and pin.** Without a title the copy is named after the source's
   current title followed by " (Copy)", shortened by whole characters to
   stay within 200. The copy is not pinned. Its history starts at
   revision 1 and does not say where it came from.
3. **What is copied.** `copied` lists the parts the copy received:
   `["files"]` in this version. A part added later is added to this list,
   and clients reject a part they do not know rather than report a copy
   as complete. The source is not touched.
4. **Refusals create nothing.** A missing or inaccessible artifact, or a
   revision that is 0 or after the current one, is `404 not_found`; it is
   reported before a malformed body. An invalid title is `422`, an
   unknown field `400`, and a missing key `400`. A duplicate is not
   conditional: it needs no base revision, since it changes nothing that
   exists.
5. **Retries.** The request fingerprint covers the operation, the
   artifact, the revision, and the title, or that the default title was
   asked for. Resending the same duplicate with its key returns the
   original copy, also after the source was renamed or revised. The key
   is refused for any other request.
6. **Contracts.** The request body is a schema definition but not part of
   the API schema's root, as the list query is: with every field optional,
   the empty object is a body, and the root would accept any object.
7. **Other clients.** The MCP tool `duplicate_artifact`, the client method
   `duplicateArtifact`, and the CLI command `artifree artifact duplicate
<id> <revision> [--title <title>]` call the same operation. The CLI
   reports an unknown outcome with the key to retry, as for a create.
8. **The web workspace.** "Duplicate" in the artifact's header opens a
   dialog that offers the revision the preview shows; the revision can be
   changed there. The dialog says that the copy has its own history,
   starts with the chosen revision's files, is named "… (Copy)", is not
   pinned, and that only files are copied. The request names no base, so
   it shares no write gate of 0011 decision 6, only its key reuse:
   sending the same revision again reuses its key, and a lost answer is
   an unknown outcome that a retry resolves to one copy. The copy then
   opens. While the editor holds unsaved changes, duplicating is disabled
   with the reason, since the copy would open in place of the draft
   (0007 decision 8). Like a restore, nothing is prepared from a read made
   before the server's data may have been replaced, since the same number
   may then name another revision: the control waits until the artifact
   is read again, and a dialog open across a replacement sends nothing
   more, saying so, and that the copy may have been made if its answer
   was lost (0011 decision 6). Opened again, it offers the latest
   revision, since the preview's choice is forgotten (0011 decision 8).

## Costs and limits

- Files only. Application data and uploaded assets do not exist yet; R06
  and R09 must copy them when they do, and until then a duplicate is not
  a copy of everything.
- The copy does not record its source; a list cannot show where a copy
  came from.
- The web page names the copy by default and offers no title; a rename
  afterwards changes it.
