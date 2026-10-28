# 0014: The trash in the web workspace

Date: 2026-10-28. Status: accepted. Builds on
[0007](0007-live-web-workspace.md),
[0009](0009-web-list-and-metadata.md),
[0011](0011-web-history-and-restore.md), and
[0013](0013-deleting-artifacts-and-the-trash.md).

## Context

Decision 0013 added deleting to a trash, recovering, and listing the trash
to the API, MCP, the client, and the CLI. The web workspace could do none
of it, and showed an artifact deleted elsewhere as one it could not read.
This record covers roadmap A06b.

The requirements use the labels of [0004](0004-inbound-mcp-adapter.md):
**observed**, **source-only**, **inference**, and **decision**. A local,
unpublished trace maps each to its evidence.

## Requirements

| ID  | Requirement                                                       | Basis                                                                                                                                                                                         |
| --- | ----------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| W1  | The open artifact can be moved to the trash after a confirmation. | Observed: the hosted title menu offered deleting. Source-only: the client confirmed a delete in a dialog that said it cannot be undone. Decision: the dialog says the trash keeps it instead. |
| W2  | The trash can be listed, and an artifact recovered from it.       | Decision, from 0013 decisions 2 and 4: the hosted client had no trash to show.                                                                                                                |
| W3  | A delete is a conditional write like a rename.                    | Decision, from 0011 decision 6 and 0013 decision 1.                                                                                                                                           |
| W4  | A delete never drops a draft silently, here or elsewhere.         | Decision, from 0007 decision 8. Gap: what open pages showed after a hosted delete was not executed.                                                                                           |
| W5  | The page follows deletes and recoveries made elsewhere.           | Decision, from 0007 decision 4 and 0013 decision 5.                                                                                                                                           |

## Decisions

1. **Moving to the trash.** "Move to trash" sits with Pin and Rename and
   opens a dialog that names the artifact and says it leaves the list, that
   nothing reads, previews, or changes it there, agents included, until it
   is recovered from the trash, and that nothing is removed. The request
   is a metadata update with `deleted: true` (0013 decision 1) through the
   metadata gate of 0011 decision 6, which pin and rename share: it waits
   as they do, keeps the base its dialog opened on for a retry after a lost
   answer, says when the page learned of a newer version, and sends
   nothing across a possible replacement of the server's data.
2. **One rule for prepared requests.** Rename, restore, and the move to
   the trash decide the base of the next request with one plain function:
   the base the dialog opened on, unless the page has learned of a newer
   version and shows it, and a lost answer is resent as it was. 0011
   decision 6 stays as it is; only its code is shared.
3. **Drafts.** While the editor holds unsaved changes, moving to the trash
   is disabled with the reason, as restoring is, and so is showing the
   trash, which closes the open artifact.
4. **After the page's own delete.** The artifact leaves the list and the
   view at once, and the page says where it went, with a way to show the
   trash.
5. **The trash.** The list's "Show" choice gains "Trash". It lists the
   artifacts in the trash, with search and order, and its items do not
   open: an artifact there cannot be read. Showing the trash and leaving
   it, also by a recovery or the notice's "Show trash", change only what
   is shown; the search and order stay as their fields show them. Each
   artifact has "Recover", a metadata update with `deleted: false`
   against the metadata revision the list read, through a gate of that
   artifact's own with the same rules: it waits while the page knows of a
   newer version of it or the list predates a possible replacement. A
   refused recovery holds back no other artifact. A recovered artifact
   opens in the list of artifacts.
6. **Deleted elsewhere.** The page knows which artifacts are in the trash,
   as of the newest metadata revision it has seen: from the feed (0013
   decision 5), from a list, or from its own write. An open artifact the
   page knows went to the trash closes, also when the list it shows is
   partial, and the page says it went to the trash; nothing in the trash
   opens in its place. If the editor holds a draft, the artifact stays open
   and says that it is in the trash and that the draft cannot be saved
   unless it is recovered; the draft stays. "Recover" is offered there,
   since the trash cannot be shown with a draft. Its base is the metadata
   revision the page knows the trash state from. This is the one
   exception to 0011 decision 6, which prepares a write only from a
   version the page shows: an artifact in the trash cannot be read, so no
   newer version can be shown. A newer one refuses the recovery, and the
   feed then reports it.

## Costs and limits

- The trash shows no pin filter; the API has one.
- A recovery from the open artifact's notice and one from the trash are
  separate requests with keys of their own; a lost answer of either is
  retried where it was sent.
- What a delete elsewhere did is learned from the feed; without the feed,
  an open artifact that cannot be read says so without the reason, and a
  draft there can only be copied out.
