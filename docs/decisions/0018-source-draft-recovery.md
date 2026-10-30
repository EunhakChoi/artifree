# 0018: Keeping a source draft in the browser

Date: 2026-10-29. Status: accepted. Builds on
[0007](0007-live-web-workspace.md), [0010](0010-revision-restore-and-digests.md),
[0011](0011-web-history-and-restore.md), and
[0017](0017-code-workspace.md).

## Context

A web draft (0017) lives in the page's memory, together with the bytes its
renames moved. A reload, a closed tab, or a crash of the page loses it. A
save whose page closed before the answer arrived may have committed. A new
attempt cannot tell, because the idempotency key that would replay the
save is gone as well. Roadmap A10 keeps the draft and that key, binds them
to the artifact, the base revision, and the tab that wrote them, makes
recovering or discarding an explicit choice, and handles a server that
moved on meanwhile.

A first attempt tied this tab's owner id, its lock, and the order of its
writes to views and connections, so each review found another view or
connection that lost them. This record ties all three to the page.

The requirements use the labels of [0004](0004-inbound-mcp-adapter.md). A
local, unpublished trace maps each to its evidence.

## Requirements

| ID  | Requirement                                                                      | Basis                                                                                                                                                                                                                                                                                                 |
| --- | -------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| D1  | A draft outlives a reload, a closed tab, and a crash of the page.                | Source-only: the inspected general-artifact paths did not show a dedicated source-draft store; the SDK tells pages to keep unsent input themselves. Observed: its app-data outbox and document staging kept writes across a crash and committed them from the recovered tab. Decision: items 1 and 3. |
| D2  | A save whose answer was lost before the page closed can be replayed once.        | Observed: the original replayed a pending app-data write with the same key after recovery. Source-only: its file saves carried no key. Decision: item 6.                                                                                                                                              |
| D3  | A draft belongs to the tab that wrote it while that tab is open.                 | Source-only: the original's outbox tied rows to a tab owner and included an orphan-adoption path. Observed: a fresh owner did not adopt the tested pending row. Decision: item 2.                                                                                                                     |
| D4  | Recovering or discarding is the user's explicit choice.                          | Gap: a choice was not observed in the examined cases; recovery used the browser's session restore and an automatic resend. Decision: item 4, since a source draft changes files and is not resent without the user.                                                                                   |
| D5  | A recovered draft is checked against what the server holds now.                  | Decision: item 5, with 0010's digests and 0007's rules.                                                                                                                                                                                                                                               |
| D6  | A browser that refuses to keep or read drafts says so; nothing is lost silently. | Observed: under an injected quota failure the original warned in telemetry only, kept the text on screen, and committed once the fault cleared. Decision: items 4 and 7.                                                                                                                              |
| D7  | Source drafts are not mixed with app data or document staging.                   | Decision: item 1.                                                                                                                                                                                                                                                                                     |

## Decisions

1. **One store for source drafts, owned by the page.** The store is the
   IndexedDB database `artifree-source-drafts`, object store `drafts`,
   keyed by owner id and artifact.
   - A record holds:
     - the draft as 0017 defines it, including bytes a rename moved;
     - the artifact's title, to name the record;
     - when it was kept;
     - a count of the draft's changes;
     - the last save sent: its key, the count it carried, and the digest of
       its serialized request;
     - the request digest and key of each save attempted during the draft;
     - whether a save may have committed.
   - Nothing else is stored there. IndexedDB is used because a draft can
     hold up to 25 MiB of moved bytes.
   - The page creates one object for its drafts in its entry module, before
     anything renders. That object claims the owner id, lists drafts, and
     orders writes. Views and connections only call it, so a reconnect, a
     disconnected page, or a view replacing another changes neither the
     owner nor the order.
2. **A tab owns its drafts.**
   - The owner id lives in session storage, so a reload keeps it. Before
     rendering, the page starts claiming a Web Lock named after it and holds
     that lock until it closes, also while no server is connected.
   - A duplicated tab copies the id, finds its lock held, and takes a new
     one. A reload that finds the previous page still holding the lock does
     the same; the previous page's drafts then belong to a closed tab.
   - Nothing is listed or written before the page holds its id.
     A failed claim is reported and a later lookup can retry it.
   - A draft of an owner whose lock is held is left to that tab. If Web
     Locks are unavailable, the browser cannot distinguish a reload from a
     duplicate tab with copied session storage, or safely tell whether
     another owner is still open. Draft reads and writes fail instead of
     risking another tab's record. The lookup warns and offers an explicit
     choice to edit without kept drafts; edits then warn that they are not
     kept. Existing records are left untouched. The same choice is offered
     if `crypto.randomUUID` is unavailable and a new owner id is needed.
     An incomplete lock snapshot also fails the lookup: it cannot establish
     which owners are open. The user may explicitly edit without the lookup,
     using this page's separately held owner.
   - Continuing another tab's draft writes it under this owner and removes
     the other record in the same transaction. If the write fails, the
     other record, the only copy, stays until a later write succeeds.
   - A listed draft can change owners or contents before the user acts on
     it. Adoption and removal hold that owner's Web Lock during the
     transaction and compare the listed storage version inside it. If the
     owner is active, the record changed, or it is already gone, the
     operation fails without deleting or replacing the record. Records
     created before storage versions were added use their recorded time and
     edit count for this comparison until their next write.
   - The lock callback returns a failure to its caller, which then rejects
     the operation. Firefox's browser automation reports a rejected lock
     callback as a page error even when the request's rejection is handled.
     Returning the failure keeps it visible to the user without that report;
     the lock still covers the entire transaction.
3. **What is kept, and when.**
   - The write point of 0017 keeps the draft after every change it applies.
     The draft's end removes the record: a save, a discard, or "Load
     latest".
   - Each artifact has one write queue per page. Keeps run one at a time,
     and only the last state asked for while a keep is in flight is written.
     Discarding either this tab's record or another tab's is ordered with
     those keeps. A discard of another tab's record also separates earlier
     and later keeps, and a new list waits for it; a replaced view cannot
     offer a record while its deletion is still pending. A failed discard
     reports the failure, leaves the record available, and does not block a
     later retry. If an earlier adoption failed, a successful discard of
     that owner's record ends the pending adoption as well, so a new draft
     does not try to replace a record that was removed.
   - Before a save is sent, the draft, its key, and "may have committed" are
     kept, and the save waits for that write. The request is sent even if
     the write failed.
   - Waiting for that write introduces a new window before the request. If
     a feed reset or a move to the trash invalidates the base in that window,
     the request is not sent; the last known save state is kept instead.
     "Load latest" waits until the save is no longer in progress.
   - A refused save keeps what was known before it. A success removes the
     record. An answer from an earlier feed epoch changes nothing (0017,
     item 6).
   - A write the browser has not finished when the page closes is lost, and
     the record keeps the state before it. A change made in the moment
     before closing may be missing, and a draft ended then may be offered
     again; item 4 leaves that to the user.
4. **Recovering is chosen, and nothing is edited before the choice.**
   - When the source view of an artifact opens, it waits for this page's
     queued writes, then looks for a draft the page may continue. It offers
     "Recover draft" or "Discard kept draft" before the editor appears.
   - This tab's own draft is offered before another tab's, because
     continuing the other would replace it.
   - If the drafts cannot be read, the editor waits as well, with "Try
     again" and "Edit without them". The second option says that a kept
     draft may be replaced.
   - While a discard is pending, neither choice can be made again. A
     discard that fails keeps the choice and says why.
   - Ending an open draft, including undoing its last change, loading the
     latest revision, and completing a save, waits for removal of its kept
     record. If removal fails, the editor retains the draft and offers a
     retry or an explicit choice to continue while the record remains. A
     successful save is reported as such even when local removal fails.
     If the feed resets while a saved draft is being removed, the draft is
     kept again and the stale answer does not replace it.
   - Drafts kept for other artifacts are named above the list, with a way to
     open or discard each, because a reload opens the first listed artifact.
     If that list cannot be read, it says so and offers a retry instead of
     appearing empty.
   - The recovery view owns the workspace's unsaved lock through the
     editor's lifetime. Replaying a newly mounted editor's effects in React
     development Strict Mode does not end the recovered draft or release
     that lock; leaving the recovery view does.
5. **The base revision is checked by its digests.**
   - A recovered draft counts as written on its revision only if the server
     lists that revision with the same paths and SHA-256 digests (0010).
     Then it is a 0017 draft like any other: a newer revision shows "saved
     elsewhere", and saving it is refused as a conflict.
   - Otherwise the revision is no longer on the server as it was, as after
     a replacement of its data (0011). The draft's text can be edited and
     copied, nothing of the revision is read or moved, and saving is
     disabled.
6. **A lost save replays.**
   - If the draft was kept as sent, the recovered draft reuses the kept key
     for the same request and starts as "may already be saved". No "saved
     elsewhere" notice shows for a revision that may be its own.
   - Saving it again replays the commit. A changed draft is a new request
     with a new key. If edits are undone, the digest identifies the original
     request even after a reload or another request was refused. Older kept
     records without a request digest can reuse their key only while their
     recorded change count still matches.
7. **A refusal to keep is shown.** When a write fails, for example over the
   quota, the source view says the draft is not kept and is lost if the
   page closes. Editing continues in memory, and the next change is kept
   once the browser allows it.

## Alternatives

- **Recover automatically**, as the original's outbox resends. A source
  draft replaces files. Resending it without the user could commit text
  they meant to drop, and a draft on an older revision needs their choice.
- **Owner and write order per connection or per view.** This was tried
  first. A reconnect, a page without a connection, or a view replacing
  another then claimed a second id or ran a second queue. The page is the
  only lifetime that matches the lock.
- **localStorage.** About 5 MiB per origin, strings only, and synchronous;
  moved bytes do not fit.
- **One shared record per artifact.** Two open tabs would overwrite each
  other's drafts.
- **Keeping only text, and reading moved bytes again on recovery.** It would
  read a revision again after the page closed, which item 5 has to check
  anyway, and could not recover a rename of a file the server no longer
  has.

## Consequences

- Drafts stay in the browser profile until they are recovered, discarded,
  or saved; clearing site data removes them. They are not encrypted beyond
  what the profile offers.
- Draft recovery requires Web Locks. An insecure, non-loopback HTTP origin
  may lack both Web Locks and `crypto.randomUUID`; the latter also prevents
  the existing client from generating save keys. Use a secure context for
  durable editing.
- A draft is kept per browser profile, not per user account, and per
  origin, which serves one server. The server still checks every save.
- A record holds the draft's moved bytes, so a large draft uses that much
  storage.
- Keeping is not synchronous with editing: closing a page moments after a
  change, or after a draft's end, can lose that write (item 3). The browser
  smoke waits for the store before it reloads or closes a tab.
- Two tabs that try to continue the same closed tab's draft cannot both
  remove that listed record: the later transaction must find the same
  stored version or fail without changing either record.
