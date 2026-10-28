# 0014: The trash in the web workspace

Date: 2026-10-28. Status: accepted. Builds on
[0007](0007-live-web-workspace.md),
[0009](0009-web-list-and-metadata.md),
[0011](0011-web-history-and-restore.md), and
[0013](0013-deleting-artifacts-and-the-trash.md).

## Context

Decision 0013 added the trash to the API, MCP, the client, and the CLI.
Roadmap A06b adds it to the web workspace:

- moving the open artifact to the trash;
- showing the trash and recovering from it;
- following moves and recoveries made elsewhere, by an agent or another
  tab, without losing a draft.

Two earlier versions of this slice did not pass review.

The first kept separate states for what the page lists, which artifact is
open, and what it says about one that left. Write answers acted on values
captured when the write was sent. The second put that state in one
reducer (decision 5), but left two rules implicit, and its findings came
from them:

- **When an answer from before a reset counts.** It compared the epoch a
  write was sent in with a copy of the current epoch updated after each
  render. An answer could settle in between and be taken as current.
- **How an artifact in the trash is read.** Reads were removed one view at
  a time. A refused recovery waited for the change feed to say what changed,
  so with the feed down it never could be sent again.

This record states both rules (decisions 4 and 6), and carries every case
those reviews found as a requirement.

The requirements use the labels of [0004](0004-inbound-mcp-adapter.md):
**observed**, **source-only**, **inference**, and **decision**. A local,
unpublished trace maps each to its evidence.

## Requirements

| ID  | Requirement                                                                                           | Basis                                                                                                                                                                                                                                 |
| --- | ----------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| W1  | The open artifact can be moved to the trash after a confirmation that says so.                        | Observed: the hosted title menu offered a delete next to rename and duplicate. Source-only: its confirmation named the artifact and said the delete cannot be undone. Decision: the wording says what the trash keeps instead (0013). |
| W2  | The trash can be shown, and an artifact recovered from it.                                            | Decision, from 0013 decision 5. The hosted interface had no trash to compare with.                                                                                                                                                    |
| W3  | A move and a recovery are conditional writes that are safe to retry.                                  | Decision, from 0011 decision 6 and 0013 decisions 1 and 4.                                                                                                                                                                            |
| W4  | A draft is never dropped by a move, a recovery, or a move made elsewhere.                             | Decision, from 0007 decision 8. Gap: what an open hosted page did after a delete elsewhere was not executed.                                                                                                                          |
| W5  | A move made elsewhere closes the open artifact and says why, also before a re-read.                   | Decision, from 0007 decision 4 and 0013 decision 6.                                                                                                                                                                                   |
| W6  | Nothing the page learned before the server's data may have been replaced decides what it shows after. | Decision, from 0007 decision 4 and 0011 decision 9.                                                                                                                                                                                   |
| W7  | Nothing depends on the change feed to make progress; the feed only makes the page learn sooner.       | Decision, from 0007 decision 3.                                                                                                                                                                                                       |

## Decisions

1. **Moving to the trash.** The artifact header gains "Move to trash".
   - It opens a confirmation naming the artifact.
   - It sends `{deleted: true}` against the metadata revision the page
     shows, through the metadata gate that rename and pin share (0011
     decision 6).
   - It is disabled, with the reason, while the editor holds a draft,
     since the move closes the artifact.
2. **One rule for prepared requests.** A dialog prepares its request from
   what it opened on, so a request whose answer was lost is sent again as
   it was, under its key.
   - Once the page has learned of a newer version and shows it, the next
     request is prepared from that.
   - Rename, restore, and the move apply this one plain function
     (`prepare` in `write-gate.ts`); 0011 decision 6 is unchanged.
3. **The trash.** The list's "Show" field gains "Trash", disabled while the
   editor holds a draft.
   - It lists the artifacts in the trash with the search and order shown.
     None of them opens, since none can be read.
   - Each has "Recover": a metadata update with `deleted: false` against
     the revision the list read, through a gate of that artifact's own. A
     conflict proves something about one artifact and holds back no other.
   - The list's fields stay once any list was read: when nothing is listed,
     since every artifact may be in the trash, and while the list of
     another search or view loads or fails, so neither typing nor the way
     back is lost.
4. **What the page knows of the trash, and for how long.** The page records,
   per artifact it saw in the trash, whether it still is, as of the newest
   metadata revision it saw. It learns this from:
   - a `deleted` or `recovered` notice;
   - a list;
   - the answer to its own write;
   - the read after a refused recovery (decision 6).

   Like the versions the page knows, the record holds for one feed epoch
   (0007 decision 4); a new epoch forgets it.

   An answer to a write sent in an earlier epoch is neither learned nor
   moves the page. What the page reads again says where the artifact is.
   Whether an epoch is still current is decided by the feed handler as each
   event arrives, not by what the page has rendered. So an answer that
   settles between a `reset` and its render is not taken as current.

   A list leaves out an artifact the record says left its view after the
   list was read, so the page's own move removes it from the list at once.

5. **Where the page is, in one place.** One state, changed by events
   through a plain reducer (`navigation.ts`), holds:
   - what is listed: search, order, view, and how many;
   - which artifact is open;
   - the notice about an artifact that left the view.

   Write answers are events that say what happened. The reducer applies
   them to the page as it is when they arrive:
   - A recovery answered while the trash is shown opens the artifact in the
     list of artifacts, with the search and order shown then.
   - One answered after the page left the trash changes nothing the page
     shows, which may hold a draft; it only says that it was recovered.

6. **An open artifact in the trash.** An open artifact the page knows is in
   the trash closes, also when its list is partial or not read again. The
   page says why, and the first listed artifact outside the trash opens.

   If the editor holds a draft, the artifact stays open with the draft:
   - Saving is disabled.
   - Discarding stays available.
   - The card says it is in the trash and offers "Recover", since the trash
     cannot be shown with a draft.
   - Nothing is read for it. The detail's own read pauses, the preview and
     the history say why instead of mounting, and they mount afresh once it
     is recovered.

   That recovery's base is the metadata revision the page learned the move
   from. This is the one exception to 0011 decision 6, which prepares a
   write only from a version the page shows: an artifact in the trash
   cannot be read.

   A refusal names the current metadata revision, but not whether the
   artifact is still in the trash, and the feed may be down. So the page
   reads the artifact once:
   - Found: it is outside the trash, and the draft can be saved again.
   - Not found: it is in the trash at the revision the refusal named, and
     the next recovery is sent against that.
   - If that read fails, the card says so and offers to read again.

7. **Notices.** The page says what its own move or recovery did, and why an
   open artifact closed.
   - When an artifact the page itself moved closes, the move is not
     reported again as one made elsewhere.
   - Any other notice about that artifact, such as its recovery, is
     replaced by why it closed.
   - A notice that an artifact went to the trash offers "Show trash" while
     there is no draft.

   An empty list says why:
   - A complete list of every artifact outside the trash says there are
     none, and that the trash is shown from "Show".
   - A list with more to show, whose artifacts all left the view, says so
     and offers "Show more".
   - Any other says that nothing matches.

## Costs and limits

- The trash shows no pin filter; the API has one.
- Without the feed, the page learns of a move made elsewhere only from a
  list or a refused write. An open artifact that can no longer be read says
  so without the reason, and a draft there can only be copied out.
- A recovery from the open artifact and one from the trash are separate
  requests with keys of their own; a lost answer of either is retried where
  it was sent.
- An answer to the page's own move or recovery that arrives after a reset is
  not reported. If the page reads that the artifact is gone, it says so only
  as a failed read.
