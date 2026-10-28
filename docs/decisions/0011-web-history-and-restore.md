# 0011: Comparing and restoring revisions in the web workspace

Date: 2026-10-28. Status: accepted. Builds on
[0007](0007-live-web-workspace.md),
[0009](0009-web-list-and-metadata.md), and
[0010](0010-revision-restore-and-digests.md).

## Context

Decision 0010 added restore and file digests to the API, MCP, client, and
CLI. The web workspace previewed any revision but could neither compare two
nor restore one. This record covers roadmap A04b.

A first attempt was closed after review found twice that a dialog sent a
conditional request prepared from view state that was changing underneath
it: after a feed `reset` had replaced the data it was read from, and after
a conflict, before the latest state had arrived. The rename and pin of 0009
had the same gap. Decision 6 below settles how every conditional write in
the page is prepared.

The requirements use the labels of [0004](0004-inbound-mcp-adapter.md):
**observed**, **source-only**, **inference**, and **decision**. A local,
unpublished trace maps each to its evidence.

## Requirements

| ID  | Requirement                                                    | Basis                                                                                                                                                                 |
| --- | -------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| V1  | Two revisions can be compared file by file and line by line.   | Gap: the analysis found no comparison of ordinary artifacts; only the separate document editor shipped a differ. Decision: the comparison below.                      |
| V2  | A comparison reads only what it shows.                         | Decision, from 0010 decision 5: manifests carry digests.                                                                                                              |
| V3  | An earlier revision can be restored from the page.             | Source-only: the analyzed client's frame menu offers a version history; what choosing a version does was not captured. Decision: restore through 0010.                |
| V4  | A conditional write from the page never sends a stale request. | Decision, from 0007 decision 8 and 0009 decision 5: requests are prepared only from what the page has read since the last possible replacement and past any conflict. |
| V5  | A restore never drops a draft silently.                        | Decision, from 0007 decision 8.                                                                                                                                       |

## Decisions

1. **A history tab.** Next to the preview and source tabs, "History"
   compares a revision with another. It offers the revision before the
   latest against the latest ("Latest (N)" follows new revisions), and
   reads nothing until it is opened.
2. **Files by digest.** The two manifests give the files added, removed, or
   changed (another digest), in path order, and how many are unchanged.
   No file is read for this.
3. **Lines of one text file.** The first differing file, or the one
   chosen, is compared line by line when both sides are text media types
   of at most 1 MiB and valid UTF-8; only its two versions are read. A
   line ends after its line feed, so a carriage return, a byte order mark,
   and a missing final line feed are differences, and they are shown as
   `␍`, `⟨BOM⟩`, and `⟨no line feed⟩`. The comparison is the shortest edit
   script (Myers), with three lines of context; beyond 20,000 lines a side
   or 2,000 changed lines, and for other files, only sizes are shown. Lines
   are a table with both line numbers and a `+` or `-` mark, so color is
   never the only signal.
4. **Restore.** "Restore revision N" restores the revision on the left
   after a confirmation: the next revision will have its files, byte for
   byte, and the revisions, title, and pin stay. The revision and the base
   are fixed when the dialog opens, so newer revisions arriving meanwhile
   change neither. The detail view holds the request, so its key and any
   refused base outlive the tab. The page learns its own restore like its
   own save (0007 decision 4).
5. **Drafts.** While the editor holds unsaved changes, restoring is
   disabled with the reason; comparing stays available.
6. **Preparing a conditional write.** Rename, pin, and restore share one
   component, and each request is prepared as follows.
   - From the artifact the page shows when the dialog opens, or when the
     pin is pressed. The same request is sent again with its key, so a
     retry after a lost answer applies at most once (0009 decision 5).
   - Not while what is shown was read before the server's data may have
     been replaced (a `reset`, or a `ready` after the page read without a
     stream; 0007 decision 8). Pin, rename, and restore are disabled until
     the artifact is read again.
   - Not from a dialog that was open when such a replacement happened: it
     says so and sends nothing, since replaced data can hold other content
     under the same numbers and the base check would pass. Opened again,
     it starts from what the page reads then.
   - After a conflict, not until the page has read a version past the
     refused base, and at least the version the conflict named. Until
     then, the control is disabled and a notice says the page is reading
     the latest; the notice stays while the refusal does, also in a dialog
     opened again. The next request is then prepared against what the page
     read: a rename dialog the user has not typed in offers the title read
     then, never the refused one. If that read fails, the detail view's
     retry recovers it. A refusal lapses when the server's data may have
     been replaced, since the versions it names may no longer exist.
7. **Plain functions.** The comparison of manifests and lines, and the
   rule for when a refusal is read past, are plain code without React,
   tested on their own.

## Costs and limits

- One file is compared at a time; the tab forgets its choices when it
  closes.
- A conflict whose latest state cannot be read leaves the control disabled
  until a read succeeds.
- A pin whose answer was lost, followed by a reset to data with the same
  metadata revision, still shows "Outcome unknown"; pinning again sends a
  request prepared from the new read.
- A restored revision does not record where it came from (0010).
