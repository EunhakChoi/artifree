# 0011: The web workspace compares and restores revisions

Date: 2026-10-28. Status: accepted. Builds on
[0007](0007-live-web-workspace.md),
[0009](0009-web-list-and-metadata.md), and
[0010](0010-revision-restore-and-digests.md).

## Context

Decision 0010 added restore and file digests to the API, MCP, client, and
CLI. The web workspace could preview any revision but neither compare two
nor restore one. This record covers roadmap A04b: both in the web
workspace, which keeps following committed changes as 0007 describes.

The requirements use the labels of [0004](0004-inbound-mcp-adapter.md):
**observed**, **source-only**, **inference**, and **decision**. A local,
unpublished trace maps each to its evidence.

## Requirements

| ID  | Requirement                                                 | Basis                                                                                                                                                                                       |
| --- | ----------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| V1  | Two revisions can be compared file by file.                 | Gap: no comparison of ordinary artifacts appeared in the analysis; only the separate document editor shipped a differ. Decision: the comparison below.                                      |
| V2  | A comparison reads only what changed.                       | Decision, from 0010 H5: manifests carry digests.                                                                                                                                            |
| V3  | An earlier revision can be restored from the page.          | Source-only: the analyzed client's frame menu offers a version history; what choosing a version does was not captured. Decision: restore through 0010.                                      |
| V4  | A restore from the page is safe to retry and never guesses. | Decision, from 0009 decision 5: a request prepared when the dialog opened, the same key for the same request, an unknown outcome after a lost answer, and a conflict that reads the latest. |
| V5  | A restore never drops a draft silently.                     | Decision, from 0007 decision 8.                                                                                                                                                             |

## Decisions

1. **A history tab.** Next to the preview and source tabs, "History"
   compares a revision with another: by default the revision before the
   latest with the latest ("Latest (N)" follows new revisions). The tab
   reads nothing until it is opened, and opens again on these defaults.
2. **Files by digest.** The manifests the page already holds give the
   files that were added, removed, or changed (different digest), in path
   order, and how many are unchanged. No file is read for this.
3. **Lines of one file.** The first changed file, or the one chosen, is
   shown line by line when both sides are text media types of at most
   1 MiB and valid UTF-8; only that file's two versions are read. Lines end
   at line feeds; a carriage return stays in its line and is shown as `␍`,
   and a byte order mark as `⟨BOM⟩`, so a change of line endings or of the
   mark is visible. A file added or removed without lines says so. The comparison is the shortest
   edit script (Myers), with three lines of context around changes; past
   20,000 lines a side or 2,000 changed lines it shows only the sizes, as
   it does for other files. Changed and removed lines carry `+` and `-`,
   so color is never the only signal.
4. **Restore.** "Restore revision N" restores the revision shown on the
   left after a confirmation that says the next revision will have its
   files, byte for byte, and that revisions, the title, and the pin stay.
   The revision to restore and the base, the current revision, are fixed
   when the dialog opens, though the default choice follows new revisions,
   so the dialog never changes what it restores. Sending the same request
   again reuses its key, as for a rename (0009 decision 5); its identity
   includes the restored revision, so a restore of another revision is a
   new request. The detail view holds the request, so its key survives
   closing and opening the tab. A lost answer is an
   unknown outcome that a retry applies at most once. A conflict says that
   a revision was saved meanwhile and nothing was restored, reads the
   artifact again, and prepares the next attempt against what it reads.
   The page learns its own restore like its own save (0007 decision 4).
5. **Drafts.** While the editor holds unsaved changes, restoring is
   disabled with the reason, as switching files is: a restore would make
   the draft's base stale. Comparing stays available.
6. **One write pattern.** Rename, pin, and restore share one component
   for conditional writes: key reuse for an identical request, the
   conflict code of each, and the same outcome messages. The comparison
   is plain functions without React, tested on their own.
7. **Replaced data.** A restore or rename dialog records when it opens how
   often the server's data may have been replaced so far (a `reset`, or a
   `ready` after a lost stream; 0007 decision 8). If that changes while it
   is open, it says so and sends nothing, since a replacement can hold
   other files or metadata under the same revision numbers and the base
   check would pass. Opened again, it is prepared from what the page reads
   then. While what is shown was read before such a replacement, pin,
   rename, and restore are disabled until it is read again.

## Costs and limits

- One changed file is shown at a time.
- Text over 1 MiB, binary files, and very large changes show sizes only.
- The tab forgets the chosen revisions when it is closed.
- The restored revision does not say where it came from (0010).
