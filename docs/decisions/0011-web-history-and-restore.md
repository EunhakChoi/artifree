# 0011: Comparing and restoring revisions in the web workspace

Date: 2026-10-28. Status: accepted. Builds on
[0007](0007-live-web-workspace.md),
[0009](0009-web-list-and-metadata.md), and
[0010](0010-revision-restore-and-digests.md); amends decisions 5 and 9 of
0007 and decision 5 of 0009.

## Context

Decision 0010 added restore and file digests to the API, MCP, client, and
CLI. The web workspace previewed any revision but could neither compare two
nor restore one. This record covers roadmap A04b.

Two earlier attempts were closed after three review rounds each. Their
defects had one cause: what the page shows can be older than what it
knows, and each write or read checked only part of the difference. A
dialog sent a request prepared before a feed `reset` replaced the data, or
before the latest state arrived after a conflict, or after the page's own
write; a pin and a rename raced on one metadata revision; a choice of
revision or a file read outlived the data it was made from. Decisions 6
and 9 below replace those guards with one rule for writes and one for
reads.

The requirements use the labels of [0004](0004-inbound-mcp-adapter.md):
**observed**, **source-only**, **inference**, and **decision**. A local,
unpublished trace maps each to its evidence.

## Requirements

| ID  | Requirement                                                                    | Basis                                                                                                                                                                    |
| --- | ------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| V1  | Two revisions can be compared file by file and line by line.                   | Gap: the analysis found no comparison of ordinary artifacts; only the separate document editor shipped a differ. Decision: decisions 2 and 3.                            |
| V2  | A comparison reads only what it shows.                                         | Decision, from 0010 decision 5: manifests carry digests.                                                                                                                 |
| V3  | An earlier revision can be restored from the page.                             | Source-only: the analyzed client's frame menu offers a version history; what choosing a version does was not captured. Observed: a history listed versions newest first. |
| V4  | No conditional write from the page is prepared from a version known to be old. | Decision, from 0007 decision 8 and 0009 decision 5: decision 6.                                                                                                          |
| V5  | Nothing is read or written for data that may have been replaced.               | Decision, from 0007 decisions 5 and 8: decisions 6 and 9.                                                                                                                |
| V6  | A restore never drops a draft silently.                                        | Decision, from 0007 decision 8.                                                                                                                                          |

## Decisions

1. **A history tab.** Next to Preview and Source, "History" compares a
   revision with another: by default the one before the latest with the
   latest, whose menu entry "Latest (N)" follows new revisions. It reads
   nothing until opened.
2. **Files by digest.** The two manifests give the files added, removed, or
   changed, in path order, and how many are unchanged, without reading any
   file. A same-size file with other bytes is changed. A long path is cut
   short in the list, with the whole path on hover and above its
   comparison.
3. **Lines of one text file.** The first changed file, or the one chosen,
   is compared line by line when both sides are text of at most 1 MiB and
   valid UTF-8: the fewest added and removed lines (Myers), with three
   unchanged lines of context. Lines keep their line feeds, so a carriage
   return, a byte order mark, or a missing final line feed is a difference,
   shown as `␍`, `⟨BOM⟩`, and `⟨no line feed⟩`. Every run of unchanged lines
   left out, before, between, or after changes, is shown with its count.
   Lines form a table with both line numbers and a mark that is also
   spoken, so color is never the only signal. Past 20,000 lines a side or
   2,000 changed lines, and for other files, only the sizes are shown.
4. **Restore.** "Restore revision N" restores the revision on the left
   after a confirmation, which says the next revision gets those files
   byte for byte and that every revision, the title, and the pin stay. The
   revision and the base are those the dialog opened on (but see decision
   6). The detail view holds the request, so its key outlives the tab.
5. **Drafts.** While the editor holds unsaved changes, restoring is
   disabled with the reason; comparing stays available.
6. **When a conditional write is prepared.** Pin, rename, restore, and save
   name a version as their base. Writes that name the same kind of version
   share a gate: pin and rename the metadata revision, restore and save the
   source revision.
   - A pin, rename, or restore is prepared only from what the page shows,
     and only when that is at least every version the page has learned
     of: from a notice or a list (0007 decision 4), from its own
     successful write, which the workspace learns as known (0009 decision
     6), and from a conflict, which proves at least the next version
     exists and may name the current one. Until the page shows such a
     version, the control waits, and the page says why.
   - A conflict that names a current version at or below the refused base
     means the server's data is behind what the page read: nothing is
     awaited, and the read the conflict causes shows what there is.
   - A save keeps 0007 decision 8: a draft's base is fixed, and a draft the
     page knows is outdated may still be saved, which the server refuses.
     The notice that a newer revision was saved shows as soon as the page
     knows of it, also if reading it failed. A save's refusal teaches the
     source gate, and its success is learned as known, so a restore waits
     for both.
   - One write of a gate is sent at a time, so the page's own pin never
     refuses its rename; the other control says why it waits.
   - A dialog keeps the base it opened on, so a retry after a lost answer
     is the same request. Once the page has learned of a newer version and
     shows it, the dialog says what changed and prepares the next request
     from it; a rename dialog the user has not typed in offers the title
     read then, never a refused one.
   - A request whose answer was lost is sent again as it was, also after
     the page learned of a newer version, which may be that very write:
     it is a replay under its key, refused without writing if its base is
     stale (as for saves, 0007 decision 8).
   - Nothing is prepared while what is shown was read before the server's
     data may have been replaced (a `reset`, or a `ready` after the page
     read without a stream; 0007 decision 8). A dialog open when that
     happens says so and sends nothing more; if its request was already
     sent and the answer lost, it also says the change may have been made,
     and offers no retry. Opened again, it starts from what the page reads
     then. What the gate learned before no longer holds, since replaced data
     may never reach those versions.
     This amends 0009 decision 5, which prepared a rename's retry after a
     conflict from whatever the view held when it was sent.
7. **Plain functions.** The comparison of manifests and lines, the runs
   left out, and the rule for when a write waits are plain code without
   React, tested on their own.
8. **Choices follow the read they came from.** A revision or file chosen
   in the history tab or the preview menu belongs to the read it was
   chosen from. Once the page shows a read made after a possible
   replacement of the server's data, the choice is forgotten and the
   default shown, since the same number can name other content and later
   reach that number again. This amends 0007 decision 9.
9. **Content follows fresh metadata.** While what is shown was read before
   a possible replacement, the page starts no preview, file, or comparison
   read for it, and shows none it read meanwhile, since the same revision
   and path may name other bytes. Each says it waits for the artifact to
   be read again. A draft stays, as 0007 decision 8 says. This amends 0007
   decision 5, which keyed content by the epoch of the metadata shown.

## Costs and limits

- One file is compared at a time; the tab forgets its choices when it
  closes, and every view forgets them after a possible replacement of the
  server's data, also when nothing was replaced.
- A write waits while a read the page needs is outstanding; if that read
  fails, the artifact says so and offers to try again.
- After a possible replacement, previews and the source disappear until
  the artifact is read again, a brief wait locally.
- A notice that "this was prepared" before a change also appears after the
  page's own write, which it cannot tell from another client's.
- A restored revision does not record where it came from (0010).
- The browser checks forge a `reset` by resuming the stream from another
  feed's cursor and forge conflicts that name versions not yet read; no
  real backup is restored.
