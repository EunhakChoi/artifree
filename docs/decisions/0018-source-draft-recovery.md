# 0018: Keeping a source draft in the browser

Date: 2026-10-29. Status: accepted. Builds on
[0007](0007-live-web-workspace.md), [0010](0010-revision-restore-and-digests.md),
[0011](0011-web-history-and-restore.md), and
[0017](0017-code-workspace.md).

## Context

A web draft (0017) lives in the page's memory, with the bytes its renames
moved. A reload, a closed tab, or a crash loses it. A save whose page
closed before the answer may have committed, and a new attempt cannot
tell, because the idempotency key that would replay it is gone too. Roadmap
A10 keeps the draft and that key, binds them to the artifact, the base
revision, and the tab that wrote them, makes recovering or discarding an
explicit choice, and handles a server that moved on meanwhile.

The requirements use the labels of [0004](0004-inbound-mcp-adapter.md). A
local, unpublished trace maps each to its evidence.

## Requirements

| ID  | Requirement                                                               | Basis                                                                                                                                                                                                                                                                     |
| --- | ------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| D1  | A draft outlives a reload, a closed tab, and a crash of the page.         | Source-only: the original has no store for artifact source drafts; its SDK tells pages to keep unsent input themselves. Observed: its app-data outbox and document staging kept writes across a crash and committed them from the recovered tab. Decision: items 1 and 2. |
| D2  | A save whose answer was lost before the page closed can be replayed once. | Observed: the original replayed a pending app-data write with the same key after recovery. Source-only: its file saves carried no key. Decision: item 3.                                                                                                                  |
| D3  | A draft belongs to the tab that wrote it while that tab is open.          | Source-only: the original's outbox tied rows to a tab owner in session storage and a Web Lock, and adopted rows of closed owners. Decision: item 2.                                                                                                                       |
| D4  | Recovering or discarding is the user's explicit choice.                   | Gap: no original path offered a choice; recovery was the browser's own session restore plus automatic resend. Decision: item 4, since a source draft changes files and is not resent without the user.                                                                    |
| D5  | A recovered draft is checked against what the server has now.             | Decision: items 5 and 6, with 0010's digests and 0007's rules.                                                                                                                                                                                                            |
| D6  | A browser that refuses to keep the draft says so and keeps editing.       | Observed: under an injected quota failure the original warned in telemetry, kept the text on screen, and committed once the fault cleared; no user-facing notice. Decision: item 7.                                                                                       |
| D7  | Source drafts are not mixed with app data or document staging.            | Decision: item 1.                                                                                                                                                                                                                                                         |

## Decisions

1. **One store for source drafts.** IndexedDB database
   `artifree-source-drafts`, object store `drafts`, keyed by tab owner and
   artifact. A record holds the draft as 0017 defines it, including bytes
   a rename moved, the artifact's title to name it, when it was kept, a
   count of the draft's changes, the last save sent (its key and the count
   it carried), and whether a save may have committed. Nothing else is
   stored there, and no other data shares it. IndexedDB because a draft
   can hold up to 25 MiB of moved bytes.
2. **A tab owns its drafts.** The owner id lives in session storage, so a
   reload keeps it, and the tab holds a Web Lock named after it while it is
   open; a duplicated tab finds the lock held and takes a new id, as does a
   reload that finds the previous page still holding it, whose drafts are
   then another tab's to continue once it is gone. Nothing is listed or
   written before the tab holds its id. A draft of another owner whose
   lock is held is left to that tab. One whose lock is free, or where the
   browser cannot tell (no Web Locks outside a secure context), may be
   continued; continuing it rewrites it under this tab's owner and removes
   the other record in the same transaction, so the other record stays
   until a write of this tab succeeds. The id and its lock belong to the
   page, so disconnecting and connecting again keeps them.
3. **What is kept, and when.** The write point of 0017 keeps the draft
   after every change it applies, and the draft's end (a save, a discard,
   "Load latest") removes it. Writes run one at a time and only the last
   state asked for is written, so a write that finishes late never brings
   back an ended draft. Before a save is sent, the draft, the key, and
   "may have committed" are kept and that write is awaited; the request
   follows even if the write failed. A refused save keeps what was known
   before it; a success removes the record; an answer from an earlier feed
   epoch changes nothing (0017, item 6). A write the browser has not
   finished when the page closes is lost, and the record keeps the state
   before it: a change made in the last moment before closing may be
   missing, and a draft ended then may be offered again, which item 4
   leaves to the user.
4. **Recovering is chosen.** When the source view of an artifact has no
   draft, it looks for a draft this tab may continue and, if there is one,
   offers "Recover draft" or "Discard kept draft" before showing the
   editor, which waits for the choice. Drafts
   kept for other artifacts are named above the list, with a way to open
   or discard each, since a reload opens the first listed artifact.
5. **The base revision is checked by its digests.** A recovered draft
   counts as written on its revision only if the server lists that
   revision with the same paths and SHA-256 digests (0010); then it is a
   draft of 0017 like any other: a newer revision shows "saved elsewhere",
   and saving it is refused as a conflict. Otherwise the revision is not
   on the server as it was, as after a replacement of its data (0011):
   the draft's text can be edited and copied, nothing of the revision is
   read or moved, and saving is disabled.
6. **A lost save replays.** If the draft was kept as sent, the recovered
   draft reuses the kept key for the same request and starts as "may
   already be saved", so no "saved elsewhere" notice shows for a revision
   that may be its own. Saving it again replays the commit; a changed
   draft is a new request with a new key.
7. **A refusal to keep is shown.** When a write fails, for example over
   the quota, the source view says the draft is not kept and is lost if
   the page closes; editing continues in memory, and the next change is
   kept once the browser allows it.

## Alternatives

- **Recover automatically**, as the original's outbox resends. A source
  draft replaces files; resending it without the user could commit text
  they meant to drop, and a draft on an older revision needs their choice.
- **localStorage.** About 5 MiB per origin, strings only, and synchronous;
  moved bytes do not fit.
- **One shared record per artifact.** Two open tabs would overwrite each
  other's drafts.
- **Keeping only text, and reading moved bytes again on recovery.** It
  would reread a revision after the page closed, which item 5 has to
  check anyway, and could not recover a rename of a file the server no
  longer has.

## Consequences

- Drafts stay in the browser profile until recovered, discarded, or saved;
  clearing site data removes them. They are not encrypted beyond what the
  profile offers.
- A draft is kept per browser profile, not per user account; the server
  still checks every save.
- A record holds the draft's moved bytes, so a large draft uses that much
  storage.
- Keeping is not synchronous with editing: closing a page within moments
  of a change or of a draft's end can lose that write (item 3). The browser
  smoke waits for the store before it reloads or closes a tab.
