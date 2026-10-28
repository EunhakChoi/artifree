# 0014: The trash in the web workspace

Date: 2026-10-28. Status: accepted. Builds on
[0007](0007-live-web-workspace.md),
[0009](0009-web-list-and-metadata.md),
[0011](0011-web-history-and-restore.md), and
[0013](0013-deleting-artifacts-and-the-trash.md).

## Context

Decision 0013 added the trash to the API, MCP, the client, and the CLI.
Roadmap A06b adds it to the web workspace: moving the open artifact to the
trash, showing the trash and recovering from it, and following deletes made
elsewhere, by an agent or another tab, without losing a draft.

A first version of this slice did not pass review. Each finding came from
the same cause: where the page is (which artifacts it lists, which one is
open, and what it says about one that left) was held in separate states,
and the answer to a write acted on the values captured when the write was
sent. A recovery answered after the user searched restored the old search;
one write state served every artifact in the trash; and a list kept an
artifact the page had already moved. This record decides that state as one
unit (decision 5).

The requirements use the labels of [0004](0004-inbound-mcp-adapter.md):
**observed**, **source-only**, **inference**, and **decision**. A local,
unpublished trace maps each to its evidence.

## Requirements

| ID  | Requirement                                                                           | Basis                                                                                                                                                                                                                                 |
| --- | ------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| W1  | The open artifact can be moved to the trash after a confirmation that says so.        | Observed: the hosted title menu offered a delete next to rename and duplicate. Source-only: its confirmation named the artifact and said the delete cannot be undone. Decision: the wording says what the trash keeps instead (0013). |
| W2  | The trash can be shown, and an artifact recovered from it.                            | Decision, from 0013 decision 5. The hosted interface had no trash to compare with.                                                                                                                                                    |
| W3  | A move and a recovery are conditional writes that are safe to retry.                  | Decision, from 0011 decision 6 and 0013 decisions 1 and 4.                                                                                                                                                                            |
| W4  | A draft is never dropped by a move, a recovery, or a delete made elsewhere.           | Decision, from 0007 decision 8. Gap: what an open hosted page did after a delete elsewhere was not executed.                                                                                                                          |
| W5  | A delete made elsewhere closes the open artifact and says why, also before a re-read. | Decision, from 0007 decision 4 and 0013 decision 6.                                                                                                                                                                                   |

## Decisions

1. **Moving to the trash.** The artifact header gains "Move to trash",
   which opens a confirmation naming the artifact. It sends `{deleted:
true}` against the metadata revision the page shows, through the
   metadata gate that rename and pin share (0011 decision 6). The button is
   disabled with the reason while the editor holds a draft, since the move
   closes the artifact.
2. **One rule for prepared requests.** A dialog prepares its request from
   what it opened on, so a request whose answer was lost is sent again as
   it was, under its key. Once the page has learned of a newer version and
   shows it, the next request is prepared from that. Rename, restore, and
   the move to the trash apply this one plain function (`prepare` in
   `write-gate.ts`); 0011 decision 6 is unchanged.
3. **The trash.** The list's "Show" field gains "Trash", disabled while
   the editor holds a draft. It lists the artifacts in the trash with the
   search and order the fields show; none of them opens, since none can be
   read. Each has "Recover", a metadata update with `deleted: false`
   against the revision the list read, through a gate of that artifact's
   own: a conflict proves something about one artifact and holds back no
   other. The fields stay when nothing is listed, since every artifact may
   be in the trash, and while the list of another search or view loads or
   fails, so neither typing nor the way back is lost.
4. **What the page knows of the trash.** The page records, per artifact
   it saw in the trash, whether it still is, as of the newest metadata
   revision it saw: from a `deleted` or `recovered` notice, from a list, or
   from the answer to its own write. Like the versions the page knows, the
   record holds for one feed epoch (0007 decision 4): it is forgotten
   whenever the server's data may have been replaced, and an answer to a
   move or recovery sent before then is not learned and does not move the
   page either; what the page reads again says where the artifact is. A list leaves out an artifact the
   record says left its view after the list was read, so the page's own
   move removes it from the list at once.
5. **Where the page is, in one place.** What is listed (search, order,
   what is shown, how many), which artifact is open, and the notice about
   one that left the view are one state, changed by events through a plain
   reducer (`navigation.ts`). The answers to writes are events too: they
   say what happened, and the reducer applies them to the page as it is
   when they arrive. A recovery answered while the trash is still shown
   opens the artifact in the list of artifacts with the search and order
   the fields show then; one answered after the page left the trash changes
   nothing the page shows, which may hold a draft, and only says that it
   was recovered.
6. **Deleted elsewhere.** An open artifact the page knows went to the trash
   closes, also when the list it shows is partial or not yet read again,
   and the page says that it went to the trash; the first listed artifact
   outside the trash opens. If the editor holds a draft, the artifact stays
   open with the draft, saving is disabled, and the card says it is in the
   trash and offers "Recover", since the trash cannot be shown with a
   draft. That recovery's base is the metadata revision the page learned
   the move from. This is the one exception to 0011 decision 6, which
   prepares a write only from a version the page shows: an artifact in the
   trash cannot be read. A newer change refuses it, and the feed then
   reports that change, which the recovery waits for; nothing is read
   again for the refusal, since the artifact cannot be read. While it is in the trash, the preview and the
   history are not mounted and say why, since every read of the artifact
   answers `not_found`; they mount afresh once it is recovered, so no
   answer read in the trash outlives the recovery.
7. **Notices.** The page says what its own move or recovery did, and why
   an open artifact closed. The page's own move is not reported again as
   one made elsewhere when the artifact closes; any other notice about it,
   such as its recovery, is replaced by the reason it closed. A notice that an artifact went to the trash offers "Show
   trash" while there is no draft. An empty list says why: a complete list of every artifact outside the
   trash says there are none and that the trash is shown from the "Show"
   field, since every artifact may be in it; a list with more to show
   whose artifacts all left the view says so and offers "Show more"; any
   other says that nothing matches.

## Costs and limits

- The trash shows no pin filter; the API has one.
- Without the feed, the page learns of a delete made elsewhere only from a
  list; an open artifact that can no longer be read says so without the
  reason, and a draft there can only be copied out.
- A recovery from the open artifact and one from the trash are separate
  requests with keys of their own; a lost answer of either is retried
  where it was sent.
